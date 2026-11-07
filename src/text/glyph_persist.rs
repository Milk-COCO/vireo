//! 字形缓存的**跨进程**持久化（`glyphs().save()` / `glyphs().load()`）。
//!
//! 目标：第二次启动不用重新光栅化就能画出上次的字。
//!
//! 文件格式 `VRTX` v1（全部小端）：
//!
//! ```text
//! magic   [4] = "VRTX"
//! version u32 = 1
//! fonts   u32 count, 每条：digest u64 | name_len u16 | name(utf8)
//! entries u32 count，每条：
//!     atlas u8 (0 = R8 掩码/SDF 图, 1 = 彩色图)
//!     content u8 (0 = Mask, 1 = Sdf, 2 = Color)
//!     w u16 | h u16                         （条目尺寸）
//!     x u16 | y u16                         （保存时的图集位置，仅供阅读，装载时重排）
//!     top i16 | left i16                    （字形 bearing，定位要用）
//!     key_tag u8 (0 Text | 1 Styled | 2 CustomSdf | 3 Stroke | 4 Custom)
//!       Text:      base{font u32, gid u16, size u32, xbin u8, ybin u8, weight u16, flags u32}
//!       Styled:    base, band_q u16
//!       CustomSdf: custom{gid u16, w u16, h u16, xbin u8, ybin u8}, band_q u16
//!       Stroke:    base, width_q u16, join u8, cap u8, miter_q u16
//!       Custom:    custom
//!     像素 w*h*bpp（bpp：R8 图 1，彩色图 4；w/h 为 0 表示"不占图集的元数据条目"）
//! ```
//!
//! 两个刻意的设计：
//!
//! 1. **字体靠摘要认，不靠 `fontdb::ID`**：ID 是进程内的 slotmap 键，下次启动必然不同。
//!    所以每条字形记的是"文件内字体下标"，字体表里存 postscript 名 + 数据摘要；
//!    装载时按名字找候选再验摘要，认不出的字体整条跳过（不报错，重新光栅化即可）。
//! 2. **装载时重新打包**：`etagere` 不能"把某块直接占在指定坐标"，所以装载用一个新的
//!    打包器按面积从大到小重排，再按新坐标逐条 `write_texture`（像素在 CPU 侧，
//!    不存在纹理内自拷贝的重叠风险）。顺带得到一个无碎片的图集。

use std::io;
use std::path::Path;

use rustc_hash::FxHashMap;
use wgpu::{Device, Queue, TexelCopyBufferInfo, TexelCopyBufferLayout, TexelCopyTextureInfo};

use crate::glyphon::cosmic_text::{CacheKey, CacheKeyFlags, SubpixelBin};
use crate::glyphon::{
    ContentType, CustomGlyphCacheKey, GlyphDetails, GlyphonCacheKey, GpuCacheStatus, InnerAtlas,
    State, fontdb,
};

use super::TextContext;

const MAGIC: [u8; 4] = *b"VRTX";
const VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// 字节流读写
// ---------------------------------------------------------------------------

struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self {
            out: Vec::with_capacity(64 * 1024),
        }
    }
    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.out.extend_from_slice(v);
    }
    fn string(&mut self, s: &str) {
        let len = s.len().min(u16::MAX as usize) as u16;
        self.u16(len);
        self.bytes(&s.as_bytes()[..len as usize]);
    }
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    fn eof(&self) -> bool {
        self.at >= self.data.len()
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or_else(truncated)?;
        if end > self.data.len() {
            return Err(truncated());
        }
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> io::Result<i16> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> io::Result<String> {
        let len = self.u16()? as usize;
        let raw = self.take(len)?;
        Ok(String::from_utf8_lossy(raw).into_owned())
    }
}

fn truncated() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "VRTX 文件被截断")
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("VRTX: {msg}"))
}

// ---------------------------------------------------------------------------
// 键的序列化
// ---------------------------------------------------------------------------

const TAG_TEXT: u8 = 0;
const TAG_STYLED: u8 = 1;
const TAG_CUSTOM_SDF: u8 = 2;
const TAG_STROKE: u8 = 3;
const TAG_CUSTOM: u8 = 4;

fn bin_to_u8(bin: SubpixelBin) -> u8 {
    match bin {
        SubpixelBin::Zero => 0,
        SubpixelBin::One => 1,
        SubpixelBin::Two => 2,
        SubpixelBin::Three => 3,
    }
}

fn u8_to_bin(v: u8) -> SubpixelBin {
    match v {
        1 => SubpixelBin::One,
        2 => SubpixelBin::Two,
        3 => SubpixelBin::Three,
        _ => SubpixelBin::Zero,
    }
}

/// 字体数据摘要（采样 FNV-1a + 面下标）。**唯一**的字体身份判据。
///
/// 为什么采样而不是整份哈希：中文系统字体动辄 10–25MB，release 下逐字节 FNV 也要
/// ~1GB/s ⇒ 每个字体 20ms 量级，而 `save`/`load` 每次都要算一遍（性能用例实测
/// save 105 条要 60ms，几乎全是这里）。取「长度 + 面下标 + 头 64KiB + 尾 64KiB +
/// 中间等距采样 64KiB」，约 0.2ms/字体。
///
/// 碰撞风险：同名、同长度、且头尾与等距采样完全一致、只在未采样区段不同的两份字体
/// 才会误判 —— 对"复用已光栅化的字形"这个用途可以接受（装载还要求 postscript 名一致）。
fn font_digest(data: &[u8], index: u32) -> u64 {
    const SAMPLE: usize = 64 * 1024;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    };
    mix(&(data.len() as u64).to_le_bytes());
    mix(&index.to_le_bytes());
    let head = data.len().min(SAMPLE);
    mix(&data[..head]);
    if data.len() > SAMPLE {
        let tail_start = data.len().saturating_sub(SAMPLE).max(head);
        mix(&data[tail_start..]);
        let mid = &data[head..tail_start];
        if !mid.is_empty() {
            let step = (mid.len() / SAMPLE).max(1);
            let mut i = 0;
            while i < mid.len() {
                mix(&mid[i..i + 1]);
                i += step;
            }
        }
    }
    h
}

/// `CacheKey` 里除字体 ID 之外的部分（字体 ID 要按文件内下标换成当前进程的 ID）。
struct BaseParts {
    font_slot: u32,
    glyph_id: u16,
    font_size_bits: u32,
    x_bin: SubpixelBin,
    y_bin: SubpixelBin,
    font_weight: fontdb::Weight,
    flags: CacheKeyFlags,
}

fn write_base(w: &mut Writer, base: &CacheKey, font_slot: u32) {
    w.u32(font_slot);
    w.u16(base.glyph_id);
    w.u32(base.font_size_bits);
    w.u8(bin_to_u8(base.x_bin));
    w.u8(bin_to_u8(base.y_bin));
    w.u16(base.font_weight.0);
    w.u32(base.flags.bits());
}

fn read_base(r: &mut Reader<'_>) -> io::Result<BaseParts> {
    Ok(BaseParts {
        font_slot: r.u32()?,
        glyph_id: r.u16()?,
        font_size_bits: r.u32()?,
        x_bin: u8_to_bin(r.u8()?),
        y_bin: u8_to_bin(r.u8()?),
        font_weight: fontdb::Weight(r.u16()?),
        flags: CacheKeyFlags::from_bits_truncate(r.u32()?),
    })
}

fn write_custom(w: &mut Writer, c: &CustomGlyphCacheKey) {
    w.u16(c.glyph_id);
    w.u16(c.width);
    w.u16(c.height);
    w.u8(bin_to_u8(c.x_bin));
    w.u8(bin_to_u8(c.y_bin));
}

fn read_custom(r: &mut Reader<'_>) -> io::Result<CustomGlyphCacheKey> {
    Ok(CustomGlyphCacheKey {
        glyph_id: r.u16()?,
        width: r.u16()?,
        height: r.u16()?,
        x_bin: u8_to_bin(r.u8()?),
        y_bin: u8_to_bin(r.u8()?),
    })
}

/// 写出一个键。`font_slot` 把 `fontdb::ID` 映射成文件内下标。
fn write_key(
    w: &mut Writer,
    key: &GlyphonCacheKey,
    font_slot: &mut dyn FnMut(fontdb::ID) -> Option<u32>,
) -> io::Result<()> {
    let mut slot_of = |id: fontdb::ID| font_slot(id).ok_or_else(|| invalid("字体不在字体表里"));
    match key {
        GlyphonCacheKey::Text(base) => {
            w.u8(TAG_TEXT);
            write_base(w, base, slot_of(base.font_id)?);
        }
        GlyphonCacheKey::Styled { base, band_q } => {
            w.u8(TAG_STYLED);
            write_base(w, base, slot_of(base.font_id)?);
            w.u16(*band_q);
        }
        GlyphonCacheKey::CustomSdf { key, band_q } => {
            w.u8(TAG_CUSTOM_SDF);
            write_custom(w, key);
            w.u16(*band_q);
        }
        GlyphonCacheKey::Stroke {
            base,
            width_q,
            join,
            cap,
            miter_q,
        } => {
            w.u8(TAG_STROKE);
            write_base(w, base, slot_of(base.font_id)?);
            w.u16(*width_q);
            w.u8(*join);
            w.u8(*cap);
            w.u16(*miter_q);
        }
        GlyphonCacheKey::Custom(key) => {
            w.u8(TAG_CUSTOM);
            write_custom(w, key);
        }
    }
    Ok(())
}

/// 读回一个键；字体下标经 `fonts` 映射到当前的 `fontdb::ID`。
///
/// 返回 `None` 表示字体认不出来（整条跳过）。**注意**：跳过时像素也已经读掉了。
fn read_key(
    r: &mut Reader<'_>,
    fonts: &[Option<fontdb::ID>],
) -> io::Result<Option<GlyphonCacheKey>> {
    let resolve = |parts: &BaseParts| -> Option<fontdb::ID> {
        fonts.get(parts.font_slot as usize).copied().flatten()
    };
    let make_base = |parts: &BaseParts, font_id: fontdb::ID| -> CacheKey {
        CacheKey {
            font_id,
            glyph_id: parts.glyph_id,
            font_size_bits: parts.font_size_bits,
            x_bin: parts.x_bin,
            y_bin: parts.y_bin,
            font_weight: parts.font_weight,
            flags: parts.flags,
        }
    };
    let tag = r.u8()?;
    let key = match tag {
        TAG_TEXT => {
            let parts = read_base(r)?;
            let Some(font_id) = resolve(&parts) else {
                return Ok(None);
            };
            GlyphonCacheKey::Text(make_base(&parts, font_id))
        }
        TAG_STYLED => {
            let parts = read_base(r)?;
            let band_q = r.u16()?;
            let Some(font_id) = resolve(&parts) else {
                return Ok(None);
            };
            GlyphonCacheKey::Styled {
                base: make_base(&parts, font_id),
                band_q,
            }
        }
        TAG_CUSTOM_SDF => {
            let key = read_custom(r)?;
            let band_q = r.u16()?;
            GlyphonCacheKey::CustomSdf { key, band_q }
        }
        TAG_STROKE => {
            let parts = read_base(r)?;
            let width_q = r.u16()?;
            let join = r.u8()?;
            let cap = r.u8()?;
            let miter_q = r.u16()?;
            let Some(font_id) = resolve(&parts) else {
                return Ok(None);
            };
            GlyphonCacheKey::Stroke {
                base: make_base(&parts, font_id),
                width_q,
                join,
                cap,
                miter_q,
            }
        }
        TAG_CUSTOM => GlyphonCacheKey::Custom(read_custom(r)?),
        other => return Err(invalid(&format!("未知的键类型 {other}"))),
    };
    Ok(Some(key))
}

// ---------------------------------------------------------------------------
// GPU 读回
// ---------------------------------------------------------------------------

/// 把两张图集一起读回 CPU（去行对齐填充）。
///
/// **一次 encoder、一次 submit、一次 poll**：`device.poll(Wait)` 会等到队列里所有已提交
/// 的工作都完成，逐张图各 poll 一次等于把等待时间翻倍（性能用例实测过：两次 79ms、
/// 一张图集的读回结果：`data` 每行只有前 `row` 字节有效（行距 `stride` 是 256 对齐）。
struct ReadBack {
    data: Vec<u8>,
    stride: u32,
}

/// 图集里**真正用到**的右下角（图集是 2 的幂，字形通常只占左上角一小块）。
fn used_extent(atlas: &InnerAtlas) -> (u32, u32) {
    let mut w = 0u32;
    let mut h = 0u32;
    for (_, d) in atlas.glyph_cache.iter() {
        if let GpuCacheStatus::InAtlas { x, y, .. } = d.gpu_cache {
            w = w.max(u32::from(x) + u32::from(d.width));
            h = h.max(u32::from(y) + u32::from(d.height));
        }
    }
    (w.max(1).min(atlas.size), h.max(1).min(atlas.size))
}

/// 把两张图集一起读回 CPU。
///
/// - **只读用到的矩形**：2048² 图集里只有十几个字时，从 4MB 降到几十 KB；
/// - **一次 encoder / 一次 submit / 一次 poll**：`device.poll(Wait)` 会等队列里所有已提交
///   的工作完成，逐张图各 poll 一次等于把等待时间翻倍。
fn read_back_both(
    device: &Device,
    queue: &Queue,
    mask: &InnerAtlas,
    color: &InnerAtlas,
) -> (ReadBack, ReadBack) {
    struct Job {
        w: u32,
        h: u32,
        stride: u32,
    }
    let jobs: Vec<Job> = [mask, color]
        .iter()
        .map(|atlas| {
            let (w, h) = used_extent(atlas);
            let row = w * atlas.num_channels() as u32;
            let stride = (row + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
                & !(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1);
            Job { w, h, stride }
        })
        .collect();
    let textures = [&mask.texture, &color.texture];
    let buffers: Vec<wgpu::Buffer> = jobs
        .iter()
        .map(|job| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vireo glyph atlas readback"),
                size: u64::from(job.stride) * u64::from(job.h),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        })
        .collect();

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("vireo glyph atlas readback encoder"),
    });
    for i in 0..jobs.len() {
        let job = &jobs[i];
        encoder.copy_texture_to_buffer(
            TexelCopyTextureInfo {
                texture: textures[i],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            TexelCopyBufferInfo {
                buffer: &buffers[i],
                layout: TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(job.stride),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: job.w,
                height: job.h,
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit(Some(encoder.finish()));

    let slices: Vec<wgpu::BufferSlice<'_>> = buffers.iter().map(|b| b.slice(..)).collect();
    for slice in &slices {
        slice.map_async(wgpu::MapMode::Read, |_| {});
    }
    let _ = device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });

    let collect = |slice: &wgpu::BufferSlice<'_>, job: &Job| -> ReadBack {
        let mapped = slice.get_mapped_range().expect("glyph atlas readback");
        ReadBack {
            data: mapped.to_vec(),
            stride: job.stride,
        }
    };
    let mask_rb = collect(&slices[0], &jobs[0]);
    let color_rb = collect(&slices[1], &jobs[1]);
    for buffer in &buffers {
        buffer.unmap();
    }
    (mask_rb, color_rb)
}

// ---------------------------------------------------------------------------
// 保存 / 装载
// ---------------------------------------------------------------------------

/// 保存当前图集内容。返回写出的条目数。
pub(crate) fn save(
    ctx: &mut TextContext,
    device: &Device,
    queue: &Queue,
    path: &Path,
) -> io::Result<usize> {
    // 字体表：先收集所有条目用到的 font_id（`Custom*` 没有字体）。
    let mut font_ids: Vec<fontdb::ID> = Vec::new();
    {
        let atlas = &ctx.text_atlas;
        for (key, _) in atlas
            .mask_atlas
            .glyph_cache
            .iter()
            .chain(atlas.color_atlas.glyph_cache.iter())
        {
            let id = match key {
                GlyphonCacheKey::Text(base)
                | GlyphonCacheKey::Styled { base, .. }
                | GlyphonCacheKey::Stroke { base, .. } => Some(base.font_id),
                GlyphonCacheKey::Custom(_) | GlyphonCacheKey::CustomSdf { .. } => None,
            };
            if let Some(id) = id
                && !font_ids.contains(&id)
            {
                font_ids.push(id);
            }
        }
    }

    // 字体记录：摘要 + postscript 名（装载时按名字找候选，再验摘要）。
    let mut font_records: Vec<(u64, String)> = Vec::with_capacity(font_ids.len());
    let mut font_slot: FxHashMap<fontdb::ID, u32> = FxHashMap::default();
    for (i, id) in font_ids.iter().enumerate() {
        let name = ctx
            .font_system
            .db()
            .face(*id)
            .map(|f| f.post_script_name.clone())
            .unwrap_or_default();
        let digest = ctx
            .font_system
            .db()
            .with_face_data(*id, |data, index| font_digest(data, index))
            .unwrap_or(0);
        font_slot.insert(*id, i as u32);
        font_records.push((digest, name));
    }

    let (mask_px, color_px) = read_back_both(
        device,
        queue,
        &ctx.text_atlas.mask_atlas,
        &ctx.text_atlas.color_atlas,
    );

    let entry_count = ctx.text_atlas.mask_atlas.glyph_cache.len() as u32
        + ctx.text_atlas.color_atlas.glyph_cache.len() as u32;

    let mut w = Writer::new();
    w.bytes(&MAGIC);
    w.u32(VERSION);
    w.u32(font_records.len() as u32);
    for (digest, name) in &font_records {
        w.u64(*digest);
        w.string(name);
    }
    w.u32(entry_count);

    let mut written = 0usize;
    let mut lookup = |id: fontdb::ID| font_slot.get(&id).copied();
    let atlas = &ctx.text_atlas;
    for (atlas_kind, inner, rb) in [
        (0u8, &atlas.mask_atlas, mask_px),
        (1u8, &atlas.color_atlas, color_px),
    ] {
        let bpp = inner.num_channels();
        let stride = rb.stride as usize;
        for (key, details) in inner.glyph_cache.iter() {
            let (x, y, content) = match details.gpu_cache {
                GpuCacheStatus::InAtlas { x, y, content_type } => {
                    (x as usize, y as usize, content_type)
                }
                GpuCacheStatus::SkipRasterization => (0, 0, ContentType::Mask),
            };
            let w_px = details.width as usize;
            let h_px = details.height as usize;

            w.u8(atlas_kind);
            w.u8(match content {
                ContentType::Mask => 0,
                ContentType::Sdf => 1,
                ContentType::Color => 2,
            });
            w.u16(details.width);
            w.u16(details.height);
            w.u16(x as u16);
            w.u16(y as u16);
            w.i16(details.top);
            w.i16(details.left);
            write_key(&mut w, key, &mut lookup)?;

            // 像素（按行从读回的缓冲里切；`SkipRasterization` 没有像素）。
            // 读回只覆盖用到的矩形，越界的条目（不该出现）就跳过像素而不是 panic。
            if w_px > 0 && h_px > 0 {
                // 末行的行首 + 末行的 x 宽度（不能按整行 stride 加，会多算一行的尾部）。
                let need = (y + h_px - 1) * stride + (x + w_px) * bpp;
                if need <= rb.data.len() {
                    for row in 0..h_px {
                        let start = (y + row) * stride + x * bpp;
                        w.bytes(&rb.data[start..start + w_px * bpp]);
                    }
                } else {
                    log::warn!("vireo: 保存字形缓存时条目越出读回范围，已跳过该条像素");
                }
            }
            written += 1;
        }
    }
    debug_assert_eq!(written as u32, entry_count);

    std::fs::write(path, &w.out)?;
    log::info!(
        "vireo: 字形缓存已保存到 {}（{} 条 / {} 字体 / {} KiB）",
        path.display(),
        written,
        font_records.len(),
        w.out.len() / 1024
    );
    Ok(written)
}

/// 目标图集边长：装得下 `live` 字节的内容，并留一倍余量给后续增长。
fn pick_size(live: u64, bpp: u64, max: u32) -> u32 {
    let mut size = 256u32;
    let side = (live as f64 / bpp as f64).sqrt() * 2.0;
    while size < side.ceil() as u32 && size < max {
        size *= 2;
    }
    size.min(max)
}

/// 从文件装载字形缓存。返回成功装上的条目数。
///
/// 要求图集为空（先 `clear()`，或在新进程里）。字体认不出的条目会被跳过（不报错），
/// 装不上的部分之后重新光栅化。
pub(crate) fn load(
    ctx: &mut TextContext,
    device: &Device,
    queue: &Queue,
    path: &Path,
) -> io::Result<usize> {
    if !ctx.text_atlas.mask_atlas.glyph_cache.is_empty()
        || !ctx.text_atlas.color_atlas.glyph_cache.is_empty()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "装载字形缓存前必须先清空图集（glyphs().clear()）",
        ));
    }

    let data = std::fs::read(path)?;
    let mut r = Reader::new(&data);

    if r.take(4)? != MAGIC {
        return Err(invalid("文件头不是 VRTX"));
    }
    let version = r.u32()?;
    if version != VERSION {
        return Err(invalid(&format!("版本 {version} 不支持（当前 {VERSION}）")));
    }

    let font_count = r.u32()? as usize;
    let mut file_fonts: Vec<(u64, String)> = Vec::with_capacity(font_count.min(64));
    for _ in 0..font_count {
        let digest = r.u64()?;
        let name = r.string()?;
        file_fonts.push((digest, name));
    }

    // 字体解析：按名字找候选，再逐候选验摘要（只哈希候选，不哈希全库）。
    let faces: Vec<(String, fontdb::ID)> = ctx
        .font_system
        .db()
        .faces()
        .map(|f| (f.post_script_name.clone(), f.id))
        .collect();
    let mut fonts: Vec<Option<fontdb::ID>> = Vec::with_capacity(file_fonts.len());
    let mut missing = 0usize;
    for (digest, name) in &file_fonts {
        let mut found = None;
        for (face_name, id) in &faces {
            if face_name != name {
                continue;
            }
            let d = ctx
                .font_system
                .db()
                .with_face_data(*id, |data, index| font_digest(data, index))
                .unwrap_or(0);
            if d == *digest {
                found = Some(*id);
                break;
            }
        }
        if found.is_none() {
            missing += 1;
        }
        fonts.push(found);
    }

    struct Rec {
        atlas: u8,
        content: ContentType,
        w: u16,
        h: u16,
        top: i16,
        left: i16,
        key: GlyphonCacheKey,
        pixels: Vec<u8>,
    }

    let entry_count = r.u32()? as usize;
    let mut recs: Vec<Rec> = Vec::with_capacity(entry_count.min(4096));
    let mut skipped = 0usize;
    for _ in 0..entry_count {
        let atlas = r.u8()?;
        let content = match r.u8()? {
            0 => ContentType::Mask,
            1 => ContentType::Sdf,
            2 => ContentType::Color,
            other => return Err(invalid(&format!("未知的内容类型 {other}"))),
        };
        let w_px = r.u16()?;
        let h_px = r.u16()?;
        let _saved_x = r.u16()?;
        let _saved_y = r.u16()?;
        let top = r.i16()?;
        let left = r.i16()?;
        let key = read_key(&mut r, &fonts)?;
        let bpp = if atlas == 1 { 4 } else { 1 };
        let pixels = r.take(w_px as usize * h_px as usize * bpp)?;
        let Some(key) = key else {
            // 字体缺失：像素已经读掉，后面的记录不会错位。
            skipped += 1;
            continue;
        };
        recs.push(Rec {
            atlas,
            content,
            w: w_px,
            h: h_px,
            top,
            left,
            key,
            pixels: pixels.to_vec(),
        });
    }
    if !r.eof() {
        log::debug!("vireo: VRTX 文件尾部还有 {} 字节未使用", data.len() - r.at);
    }

    // 重新打包：面积从大到小（碎片最少）。
    let mut order: Vec<usize> = (0..recs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(u64::from(recs[i].w) * u64::from(recs[i].h)));
    let mask_live: u64 = recs
        .iter()
        .filter(|r| r.atlas == 0)
        .map(|r| u64::from(r.w) * u64::from(r.h))
        .sum();
    let color_live: u64 = recs
        .iter()
        .filter(|r| r.atlas == 1)
        .map(|r| u64::from(r.w) * u64::from(r.h) * 4)
        .sum();

    let state = State { device, queue };
    let max_dim = device.limits().max_texture_dimension_2d;
    let mask_size = pick_size(mask_live, 1, max_dim);
    let color_size = pick_size(color_live, 4, max_dim);

    // 复位两张图（尺寸按内容重算，装载出来的图集不背历史的账）。
    {
        let atlas = &mut ctx.text_atlas;
        atlas.mask_atlas.recreate(&state, mask_size);
        atlas.mask_atlas.packer = etagere::BucketedAtlasAllocator::new(etagere::size2(
            mask_size as i32,
            mask_size as i32,
        ));
        atlas.color_atlas.recreate(&state, color_size);
        atlas.color_atlas.packer = etagere::BucketedAtlasAllocator::new(etagere::size2(
            color_size as i32,
            color_size as i32,
        ));
    }

    // 一条待落地的条目（先排好位置，最后一次性提交 GPU 拷贝）。
    struct Placed {
        buffer_offset: u64,
        padded_row: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        is_color: bool,
    }

    let mut staged: Vec<u8> = Vec::new();
    // 行对齐到 256：`copy_buffer_to_texture` 的 `bytes_per_row` 必须是 256 的倍数。
    // 窄字形会浪费一些暂存空间（几十条加起来也就 MB 级），换来**一次**提交。
    let align_row = |row: u32| {
        (row + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1) & !(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
    };

    let mut placed = 0usize;
    let mut placed_items: Vec<Placed> = Vec::with_capacity(recs.len());
    for idx in order {
        let rec = &recs[idx];
        let is_color = rec.atlas == 1;
        let inner = if is_color {
            &mut ctx.text_atlas.color_atlas
        } else {
            &mut ctx.text_atlas.mask_atlas
        };
        let bpp = inner.num_channels() as u32;
        let w_px = u32::from(rec.w);
        let h_px = u32::from(rec.h);
        let has_pixels = w_px > 0 && h_px > 0;
        let (gpu_cache, atlas_id) = if has_pixels {
            let Some(allocation) = inner
                .packer
                .allocate(etagere::size2(w_px as i32, h_px as i32))
            else {
                skipped += 1;
                continue;
            };
            let min = allocation.rectangle.min;
            let row = w_px * bpp;
            let padded_row = align_row(row);
            let buffer_offset = staged.len() as u64;
            staged.resize(staged.len() + padded_row as usize * h_px as usize, 0u8);
            for y in 0..h_px as usize {
                let src = y * row as usize;
                let dst = buffer_offset as usize + y * padded_row as usize;
                staged[dst..dst + row as usize]
                    .copy_from_slice(&rec.pixels[src..src + row as usize]);
            }
            placed_items.push(Placed {
                buffer_offset,
                padded_row,
                x: min.x as u32,
                y: min.y as u32,
                w: w_px,
                h: h_px,
                is_color,
            });
            (
                GpuCacheStatus::InAtlas {
                    x: min.x as u16,
                    y: min.y as u16,
                    content_type: rec.content,
                },
                Some(allocation.id),
            )
        } else {
            (GpuCacheStatus::SkipRasterization, None)
        };
        let last_used = inner.generation;
        inner.glyph_cache.put(
            rec.key,
            GlyphDetails {
                width: rec.w,
                height: rec.h,
                gpu_cache,
                atlas_id,
                top: rec.top,
                left: rec.left,
                last_used,
            },
        );
        placed += 1;
    }

    // 一次性上传：一个 staging buffer + 一次 submit（逐条 `write_texture` 会让 wgpu 的
    // 暂存带反复等待，性能用例里实测 0.5 ms/条）。
    if !placed_items.is_empty() {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo glyph cache load staging"),
            size: staged.len() as u64,
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        staging
            .slice(..)
            .get_mapped_range_mut()
            .expect("staging map")
            .copy_from_slice(&staged);
        staging.unmap();

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vireo glyph cache load"),
        });
        for item in &placed_items {
            let atlas = if item.is_color {
                &ctx.text_atlas.color_atlas
            } else {
                &ctx.text_atlas.mask_atlas
            };
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: TexelCopyBufferLayout {
                        offset: item.buffer_offset,
                        bytes_per_row: Some(item.padded_row),
                        rows_per_image: None,
                    },
                },
                TexelCopyTextureInfo {
                    texture: &atlas.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: item.x,
                        y: item.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: item.w,
                    height: item.h,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(Some(encoder.finish()));
    }

    ctx.text_atlas.rebind(device);
    log::info!(
        "vireo: 字形缓存已从 {} 装载（{} 条，跳过 {} 条，字体缺失 {} 个）",
        path.display(),
        placed,
        skipped,
        missing
    );
    Ok(placed)
}
