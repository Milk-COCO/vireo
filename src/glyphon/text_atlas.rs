use super::{
    Cache, ContentType, FontSystem, GlyphDetails, GpuCacheStatus, RasterizeCustomGlyphRequest,
    RasterizedCustomGlyph, State, SwashCache, text_render::GlyphonCacheKey,
};
use etagere::{Allocation, BucketedAtlasAllocator, size2};
use lru::LruCache;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use std::hash::BuildHasherDefault;
use std::sync::{Arc, Mutex};
use wgpu::{
    BindGroup, BlendState, DepthStencilState, Device, Extent3d, MultisampleState, Origin3d, Queue,
    RenderPipeline, Sampler, TexelCopyTextureInfo, Texture, TextureAspect, TextureDescriptor,
    TextureDimension, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
};

type Hasher = BuildHasherDefault<FxHasher>;

/// 用户 `hold` 住的字形键 —— 这些条目不允许被淘汰。
///
/// 用引用计数而不是布尔值：同一批字形可以被多个 [`GlyphHold`] 同时钉住，
/// 最后一个句柄释放时才真正解钉。与自己内部那张计数表共享（`Arc`），
/// 所以 `GlyphHold::drop` 不需要再拿到图集（图集在 `TextContext` 的锁里）。
///
/// [`GlyphHold`]: crate::text::GlyphHold
#[derive(Default)]
pub(crate) struct PinRegistry {
    counts: Mutex<FxHashMap<GlyphonCacheKey, u32>>,
}

impl PinRegistry {
    /// 当前钉住的字形数（去重后）。
    pub(crate) fn len(&self) -> usize {
        self.counts.lock().unwrap().len()
    }

    /// 快照成集合：淘汰路径一次拿好，避免逐条加锁。
    pub(crate) fn snapshot(&self) -> FxHashSet<GlyphonCacheKey> {
        self.counts.lock().unwrap().keys().copied().collect()
    }

    pub(crate) fn pin(&self, keys: &[GlyphonCacheKey]) {
        let mut counts = self.counts.lock().unwrap();
        for key in keys {
            *counts.entry(*key).or_insert(0) += 1;
        }
    }

    pub(crate) fn unpin(&self, keys: &[GlyphonCacheKey]) {
        let mut counts = self.counts.lock().unwrap();
        for key in keys {
            if let Some(n) = counts.get_mut(key) {
                *n -= 1;
                if *n == 0 {
                    counts.remove(key);
                }
            }
        }
    }
}

#[allow(dead_code)]
pub(crate) struct InnerAtlas {
    pub kind: Kind,
    pub texture: Texture,
    pub texture_view: TextureView,
    pub packer: BucketedAtlasAllocator,
    pub size: u32,
    pub glyph_cache: LruCache<GlyphonCacheKey, GlyphDetails, Hasher>,
    /// Monotonic frame counter, bumped by `trim`. A glyph whose `last_used` equals this value has
    /// been touched since the last `trim` and is considered in use for the current frame.
    pub generation: usize,
    pub max_texture_dimension_2d: u32,
    /// 用户钉住表（两张图集共享同一个实例）。
    pub pins: Arc<PinRegistry>,
    /// 累计光栅化（新造图集条目）次数。
    pub rasterizations: u64,
    /// 累计淘汰次数（LRU / 清空 / grow 丢条目）。
    pub evictions: u64,
    /// 累计扩图集次数。
    pub grows: u64,
    /// 累计缩图集次数（`compact`）。
    ///
    /// 和 `grows` 分开：否则"扩容检测"的用例/日志会被压缩误触发。
    pub compacts: u64,
    /// 当前条目占用字节（增量维护；见 `live_bytes()` 的注释）。
    pub live_bytes: u64,
}

/// 一条图集条目占的字节数（`宽 × 高 × 每像素字节`）。
fn entry_bytes(width: u16, height: u16, bpp: usize) -> u64 {
    u64::from(width) * u64::from(height) * bpp as u64
}

impl InnerAtlas {
    const INITIAL_SIZE: u32 = 256;

    fn new(state: &State, kind: Kind, pins: Arc<PinRegistry>) -> Self {
        let max_texture_dimension_2d = state.device.limits().max_texture_dimension_2d;
        let size = Self::INITIAL_SIZE.min(max_texture_dimension_2d);

        let packer = BucketedAtlasAllocator::new(size2(size as i32, size as i32));

        // Create a texture to use for our atlas
        let texture = state.device.create_texture(&TextureDescriptor {
            label: Some("glyphon atlas"),
            size: Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: kind.texture_format(),
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let texture_view = texture.create_view(&TextureViewDescriptor::default());

        let glyph_cache = LruCache::unbounded_with_hasher(Hasher::default());

        Self {
            kind,
            texture,
            texture_view,
            packer,
            size,
            glyph_cache,
            generation: 0,
            max_texture_dimension_2d,
            pins,
            rasterizations: 0,
            evictions: 0,
            grows: 0,
            compacts: 0,
            live_bytes: 0,
        }
    }

    /// 光栅化出一个新条目时记账（条目字节数也在这里累加）。
    pub(crate) fn note_rasterized(&mut self, width: u16, height: u16) {
        self.rasterizations = self.rasterizations.saturating_add(1);
        self.note_entry_added(width, height);
    }

    /// 记录一条新条目占的字节（装载路径用；不增加光栅化计数）。
    pub(crate) fn note_entry_added(&mut self, width: u16, height: u16) {
        self.live_bytes =
            self.live_bytes
                .saturating_add(entry_bytes(width, height, self.kind.num_channels()));
    }

    /// 当前占用（按条目面积累计，不含 packer 空洞）。
    ///
    /// **增量维护**（插入时加、淘汰时减）：预算检查每帧都要问一次，而淘汰循环里
    /// 每扔一条也要问一次 —— 全量扫就成了 O(n²)（732 条 × 每帧 768 次淘汰 ≈ 56 万次
    /// 迭代/帧，真实窗口里能量出来）。
    pub(crate) fn live_bytes(&self) -> u64 {
        self.live_bytes
    }

    /// 图集纹理容量（`size² × 通道数`）。
    pub(crate) fn capacity_bytes(&self) -> u64 {
        let bpp = self.kind.num_channels() as u64;
        u64::from(self.size) * u64::from(self.size) * bpp
    }

    /// 淘汰一条「不钉住」的图集条目，返回释放的字节数。
    ///
    /// `min_age_frames`：条目至少「这么多帧没被用到」才允许淘汰（`0` = 连本帧的也动，
    /// 实际不会传 0）。两处调用方的取值：
    /// - `try_allocate` / 硬预算：`2`（本帧与上一帧都在用的不动）；
    /// - 保留预算：`RETENTION_MIN_AGE_FRAMES`（1 秒级没用过才算"历史积压"），
    ///   这样间歇使用的字形（比如只在某些帧出现的数字）不会被反复扔掉重画。
    ///
    /// 只淘汰真正占图集的条目；只占缓存表的元数据条目由 [`Self::try_allocate`] 清理。
    pub(crate) fn evict_one_unpinned(&mut self, min_age_frames: usize) -> Option<u64> {
        let pinned = self.pins.snapshot();
        let generation = self.generation;
        let key = self
            .glyph_cache
            .iter()
            .rev()
            .find(|(k, d)| {
                if d.atlas_id.is_none() || pinned.contains(*k) {
                    return false;
                }
                let age = generation.saturating_sub(d.last_used);
                age >= min_age_frames.max(1)
            })
            .map(|(k, _)| *k)?;
        let details = self.glyph_cache.pop(&key)?;
        let bytes = entry_bytes(details.width, details.height, self.kind.num_channels());
        self.live_bytes = self.live_bytes.saturating_sub(bytes);
        if let Some(id) = details.atlas_id {
            self.packer.deallocate(id);
        }
        self.evictions = self.evictions.saturating_add(1);
        Some(bytes)
    }

    /// 清掉所有未钉住的条目（含只占缓存表的元数据条目），返回清掉的条数。
    pub(crate) fn clear_unpinned(&mut self) -> usize {
        let pinned = self.pins.snapshot();
        let keys: Vec<GlyphonCacheKey> = self
            .glyph_cache
            .iter()
            .filter(|(k, _)| !pinned.contains(*k))
            .map(|(k, _)| *k)
            .collect();
        for key in &keys {
            if let Some(details) = self.glyph_cache.pop(key) {
                self.live_bytes = self.live_bytes.saturating_sub(entry_bytes(
                    details.width,
                    details.height,
                    self.kind.num_channels(),
                ));
                if let Some(id) = details.atlas_id {
                    self.packer.deallocate(id);
                }
            }
        }
        self.evictions = self.evictions.saturating_add(keys.len() as u64);
        keys.len()
    }

    /// 清掉全部条目（钉住的也清；句柄随后自然失效）。
    pub(crate) fn clear_all(&mut self) -> usize {
        let n = self.glyph_cache.len();
        self.glyph_cache.clear();
        // packer 无法逐块回收，重建一个更省事（下一帧重新分配）。
        self.packer = BucketedAtlasAllocator::new(size2(self.size as i32, self.size as i32));
        self.live_bytes = 0;
        self.evictions = self.evictions.saturating_add(n as u64);
        n
    }

    /// 在图集里腾一块 `width × height`。
    ///
    /// `cold_only = true`：只淘汰冷条目（上一帧也没用到的）。调用方先试冷淘汰，
    /// 不行就扩图集；图集到顶了才用 `cold_only = false` 动热条目。
    pub(crate) fn try_allocate(
        &mut self,
        width: usize,
        height: usize,
        cold_only: bool,
    ) -> Option<Allocation> {
        let size = size2(width as i32, height as i32);
        let pinned = self.pins.snapshot();
        let generation = self.generation;
        let is_cold = |d: &GlyphDetails| !cold_only || d.last_used + 1 < generation;

        loop {
            if let Some(allocation) = self.packer.allocate(size) {
                return Some(allocation);
            }

            // 先扔掉"不占图集"的元数据条目（它们只占缓存表，扔掉也能让 LRU 前进），
            // 再按 LRU 扔真正占着图集块的。钉住的、本帧用过的都不动。
            if let Some(key) = self
                .glyph_cache
                .iter()
                .rev()
                .find(|(k, d)| {
                    d.atlas_id.is_none() && d.last_used != generation && !pinned.contains(*k)
                })
                .map(|(k, _)| *k)
            {
                self.glyph_cache.pop(&key);
                self.evictions = self.evictions.saturating_add(1);
                continue;
            }

            let Some(key) = self
                .glyph_cache
                .iter()
                .rev()
                .find(|(k, d)| {
                    d.atlas_id.is_some()
                        && d.last_used != generation
                        && !pinned.contains(*k)
                        && is_cold(d)
                })
                .map(|(k, _)| *k)
            else {
                // 全被钉住 / 全是本帧在用 / 只剩热条目 → 交给调用方决定扩图集还是 AtlasFull。
                return None;
            };

            if let Some(details) = self.glyph_cache.pop(&key) {
                self.live_bytes = self.live_bytes.saturating_sub(entry_bytes(
                    details.width,
                    details.height,
                    self.kind.num_channels(),
                ));
                if let Some(id) = details.atlas_id {
                    self.packer.deallocate(id);
                }
            }
            self.evictions = self.evictions.saturating_add(1);
        }
    }

    pub fn num_channels(&self) -> usize {
        self.kind.num_channels()
    }

    /// 扩图集：尺寸翻倍，已有条目用**纹理拷贝**搬过去（不再重新光栅化）。
    ///
    /// 为什么不再重新光栅化：`grow` 会触发在绘制中途，重新光栅化几十上百个字形是
    /// 毫秒级的可见卡顿；`etagere::grow` 只扩空闲空间、不动已分配块的坐标，所以
    /// 同一坐标上的一次 `copy_texture_to_texture` 就搬完了。
    ///
    /// 代价：失去"重新光栅化时顺手校验条目是否过期"的自愈能力。`rasterize_glyph`
    /// 是纯函数（同键同图），条目过期只可能来自"字体被移除 / 自定义光栅化器换实现"，
    /// 这两种情况由调用方显式 `clear` 处理。
    pub(crate) fn grow(
        &mut self,
        state: &State,
        font_system: &mut FontSystem,
        cache: &mut SwashCache,
        scale_factor: f32,
        mut rasterize_custom_glyph: impl FnMut(
            RasterizeCustomGlyphRequest,
        ) -> Option<RasterizedCustomGlyph>,
    ) -> bool {
        let _ = (
            &font_system,
            &cache,
            &mut rasterize_custom_glyph,
            scale_factor,
        );
        if self.size >= self.max_texture_dimension_2d {
            return false;
        }

        // Grow each dimension by a factor of 2. The growth factor was chosen to match the growth
        // factor of `Vec`.`
        const GROWTH_FACTOR: u32 = 2;
        let new_size = (self.size * GROWTH_FACTOR).min(self.max_texture_dimension_2d);

        self.packer.grow(size2(new_size as i32, new_size as i32));

        // Create a texture to use for our atlas
        let new_texture = state.device.create_texture(&TextureDescriptor {
            label: Some("glyphon atlas"),
            size: Extent3d {
                width: new_size,
                height: new_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: self.kind.texture_format(),
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        // 把旧图里的条目原坐标搬过去（packer 增长不移动已分配的块）。
        let old_texture = std::mem::replace(&mut self.texture, new_texture);
        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vireo glyph atlas grow"),
            });
        let mut copied = 0u32;
        for glyph in self.glyph_cache.iter().map(|(_, g)| g) {
            let GpuCacheStatus::InAtlas { x, y, .. } = glyph.gpu_cache else {
                continue;
            };
            let (w, h) = (u32::from(glyph.width), u32::from(glyph.height));
            if w == 0 || h == 0 {
                continue;
            }
            let size = Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            };
            encoder.copy_texture_to_texture(
                TexelCopyTextureInfo {
                    texture: &old_texture,
                    mip_level: 0,
                    origin: Origin3d {
                        x: u32::from(x),
                        y: u32::from(y),
                        z: 0,
                    },
                    aspect: TextureAspect::All,
                },
                TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: Origin3d {
                        x: u32::from(x),
                        y: u32::from(y),
                        z: 0,
                    },
                    aspect: TextureAspect::All,
                },
                size,
            );
            copied += 1;
        }
        if copied > 0 {
            state.queue.submit(Some(encoder.finish()));
        }
        // 旧纹理在这里 drop：wgpu 会等到提交完成后再真正释放。

        self.texture_view = self.texture.create_view(&TextureViewDescriptor::default());
        self.size = new_size;
        self.grows = self.grows.saturating_add(1);
        log::debug!(
            "glyphon: {:?} atlas grew to {}²（拷贝 {} 条条目，未重新光栅化）",
            self.kind,
            new_size,
            copied
        );

        true
    }

    /// 缩图集：把仍然保留的条目重新打包到更小的纹理里（拷贝，不重新光栅化）。
    ///
    /// 什么时候用：清掉一批字形（`clear_unpinned` / 句柄释放 + 预算淘汰）之后，
    /// 图集尺寸不会自己缩回去 —— 显存就一直是历史峰值。`compact` 把它收回来。
    ///
    /// 只在这些情况返回 `false`（图集保持原样）：尺寸已经最小、或腾挪空间不够
    /// （碎片导致重新打包失败）。
    ///
    /// 注意：**条目坐标会变**，所以只能在两帧之间调（已经准备好的顶点不保证还有效）。
    pub(crate) fn compact(&mut self, state: &State) -> bool {
        const MIN_SIZE: u32 = InnerAtlas::INITIAL_SIZE;
        // 条目面积（不含每像素字节数）：用增量维护的 `live_bytes` 换算，O(1)。
        // 和 `try_allocate` 的占用统计同一个来源，避免两套账。
        let live_area: u64 = self.live_bytes / self.kind.num_channels() as u64;
        if live_area == 0 {
            // 全空：直接回到初始尺寸，连拷贝都省了。
            if self.size <= MIN_SIZE {
                return false;
            }
            self.recreate(state, MIN_SIZE);
            self.packer = BucketedAtlasAllocator::new(size2(MIN_SIZE as i32, MIN_SIZE as i32));
            self.compacts = self.compacts.saturating_add(1);
            return true;
        }
        // 目标边长 = 1.2 × sqrt(内容面积)，向上取到 2 的幂。
        // 别留太大余量：边长翻倍 = 面积 ×4，616 KiB 内容会被装进 4 MiB 纹理
        // （性能用例里量到过"compact 后反而比原来大"的那种反效果）。
        let side = ((live_area as f64).sqrt() * 1.2).ceil() as u32;
        let mut target = MIN_SIZE;
        while target < side && target < self.max_texture_dimension_2d {
            target = (target * 2).min(self.max_texture_dimension_2d);
        }
        if target >= self.size {
            return false;
        }

        // 先在新 packer 里排好（大的先排，碎片最少）；任何一个排不下就整体放弃。
        let mut items: Vec<(GlyphonCacheKey, u16, u16)> = self
            .glyph_cache
            .iter()
            .filter(|(_, d)| d.atlas_id.is_some() && d.width > 0 && d.height > 0)
            .map(|(k, d)| (*k, d.width, d.height))
            .collect();
        items.sort_by_key(|(_, w, h)| std::cmp::Reverse(u64::from(*w) * u64::from(*h)));

        let mut new_packer = BucketedAtlasAllocator::new(size2(target as i32, target as i32));
        let mut placements: Vec<(GlyphonCacheKey, Allocation)> = Vec::with_capacity(items.len());
        for (key, w, h) in items {
            let Some(allocation) = new_packer.allocate(size2(i32::from(w), i32::from(h))) else {
                log::debug!(
                    "glyphon: compact 放弃（{}² 排不下 {} 条）",
                    target,
                    placements.len()
                );
                return false;
            };
            placements.push((key, allocation));
        }

        let new_texture = state.device.create_texture(&TextureDescriptor {
            label: Some("glyphon atlas"),
            size: Extent3d {
                width: target,
                height: target,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: self.kind.texture_format(),
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let old_texture = std::mem::replace(&mut self.texture, new_texture);

        let mut encoder = state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vireo glyph atlas compact"),
            });
        for (key, allocation) in &placements {
            let Some(details) = self.glyph_cache.peek(key) else {
                continue;
            };
            let GpuCacheStatus::InAtlas { x, y, content_type } = details.gpu_cache else {
                continue;
            };
            let (w, h) = (u32::from(details.width), u32::from(details.height));
            let min = allocation.rectangle.min;
            encoder.copy_texture_to_texture(
                TexelCopyTextureInfo {
                    texture: &old_texture,
                    mip_level: 0,
                    origin: Origin3d {
                        x: u32::from(x),
                        y: u32::from(y),
                        z: 0,
                    },
                    aspect: TextureAspect::All,
                },
                TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: Origin3d {
                        x: min.x as u32,
                        y: min.y as u32,
                        z: 0,
                    },
                    aspect: TextureAspect::All,
                },
                Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            // 拷贝指令已经记下旧坐标，这里就可以把条目改写到新位置。
            //
            // 用 `peek_mut` 而不是 `get_mut`：后者会把条目提升成 MRU，而 placements 是按
            // 面积从大到小排的 ⇒ 搬完之后「最大的字形最不容易被淘汰、最小的最容易被淘汰」，
            // LRU 顺序被压缩本身改写了。`peek_mut` 只改值不动链表。
            if let Some(details) = self.glyph_cache.peek_mut(key) {
                details.gpu_cache = GpuCacheStatus::InAtlas {
                    x: min.x as u16,
                    y: min.y as u16,
                    content_type,
                };
                details.atlas_id = Some(allocation.id);
            }
        }
        state.queue.submit(Some(encoder.finish()));

        self.packer = new_packer;
        self.texture_view = self.texture.create_view(&TextureViewDescriptor::default());
        self.size = target;
        self.compacts = self.compacts.saturating_add(1);
        log::debug!(
            "glyphon: {:?} atlas compacted to {}²（搬迁 {} 条条目）",
            self.kind,
            target,
            placements.len()
        );
        true
    }

    pub(crate) fn recreate(&mut self, state: &State, size: u32) {
        // 换纹理 = 旧内容全部作废（装载路径会要求图集为空，然后逐条 note_entry_added）。
        self.live_bytes = 0;
        self.texture = state.device.create_texture(&TextureDescriptor {
            label: Some("glyphon atlas"),
            size: Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: self.kind.texture_format(),
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.texture_view = self.texture.create_view(&TextureViewDescriptor::default());
        self.size = size;
    }

    pub(crate) fn trim(&mut self) {
        // Advance the frame counter so that glyphs touched before this point are no longer
        // considered in use and become eligible for eviction.
        self.generation = self.generation.wrapping_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Mask,
    Color { srgb: bool },
}

impl Kind {
    fn num_channels(self) -> usize {
        match self {
            Kind::Mask => 1,
            Kind::Color { .. } => 4,
        }
    }

    pub(crate) fn texture_format(self) -> wgpu::TextureFormat {
        match self {
            Kind::Mask => TextureFormat::R8Unorm,
            Kind::Color { srgb } => {
                if srgb {
                    TextureFormat::Rgba8UnormSrgb
                } else {
                    TextureFormat::Rgba8Unorm
                }
            }
        }
    }
}

/// The color mode of a [`TextAtlas`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Accurate color management.
    ///
    /// This mode will use a proper sRGB texture for colored glyphs. This will
    /// produce physically accurate color blending when rendering.
    Accurate,

    /// Web color management.
    ///
    /// This mode reproduces the color management strategy used in the Web and
    /// implemented by browsers.
    ///
    /// This entails storing glyphs colored using the sRGB color space in a
    /// linear RGB texture. Blending will not be physically accurate, but will
    /// produce the same results as most UI toolkits.
    ///
    /// This mode should be used to render to a linear RGB texture containing
    /// sRGB colors.
    Web,
}

/// 图集占用快照（一次绘制后可读，用来决定要不要缩样式/换字号）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextAtlasStats {
    /// 覆盖度 / 距离场图集边长（像素）。两者共用同一张 R8 图。
    pub mask_size: u32,
    /// 彩色图集边长（像素）。
    pub color_size: u32,
    /// R8 图集占用的字节数（`mask_size²`，SDF 与覆盖度共用）。
    pub mask_bytes: u64,
    /// 彩色图集占用的字节数（`color_size² × 4`）。
    pub color_bytes: u64,
    /// R8 图集里的字形条目数（覆盖度 + 精确 SDF）。
    pub mask_entries: u32,
    /// 彩色图集里的字形条目数。
    pub color_entries: u32,
}

impl TextAtlasStats {
    pub fn total_bytes(&self) -> u64 {
        self.mask_bytes + self.color_bytes
    }
}

/// An atlas containing a cache of rasterized glyphs that can be rendered.
pub struct TextAtlas {
    cache: Cache,
    pub(crate) bind_group: BindGroup,
    pub(crate) color_atlas: InnerAtlas,
    pub(crate) mask_atlas: InnerAtlas,
    pub(crate) format: TextureFormat,
    pub(crate) color_mode: ColorMode,
    pub(crate) default_base_texture: TextureView,
    pub(crate) default_base_sampler: Sampler,
    /// 两张图集共享的钉住表。
    pub(crate) pins: Arc<PinRegistry>,
    /// 收集"本次 prepare 触碰到的字形键"用的缓冲（`None` = 不收集）。
    pub(crate) collect: Option<Vec<GlyphonCacheKey>>,
}

impl TextAtlas {
    /// 当前图集占用（纹理尺寸与字节数）。带样式的字形与普通字形**共用** R8 图集。
    pub fn stats(&self) -> TextAtlasStats {
        let mask_size = self.mask_atlas.size;
        let color_size = self.color_atlas.size;
        TextAtlasStats {
            mask_size,
            color_size,
            mask_bytes: u64::from(mask_size) * u64::from(mask_size),
            color_bytes: u64::from(color_size) * u64::from(color_size) * 4,
            mask_entries: self.mask_atlas.glyph_cache.len() as u32,
            color_entries: self.color_atlas.glyph_cache.len() as u32,
        }
    }

    // ---- 用户可管的部分（`text().glyphs()` 背后）----

    /// 两张图集共享的钉住表。
    pub(crate) fn pins(&self) -> &Arc<PinRegistry> {
        &self.pins
    }

    /// 开始收集"被 prepare 触碰的字形键"。
    pub(crate) fn start_collect(&mut self) {
        self.collect = Some(Vec::new());
    }

    /// 结束收集，返回本次触碰到的全部键（**可能有重复**：同一字形被多个 pass 用到，
    /// 调用方按需去重；`PinRegistry` 是计数语义，重复钉没有害处）。
    pub(crate) fn take_collect(&mut self) -> Vec<GlyphonCacheKey> {
        self.collect.take().unwrap_or_default()
    }

    /// 记录一次 prepare 触碰（收集模式未开启时是空操作）。
    pub(crate) fn note_touched(&mut self, key: GlyphonCacheKey) {
        if let Some(keys) = self.collect.as_mut() {
            keys.push(key);
        }
    }

    /// 当前真实占用（按条目面积累计）。
    pub(crate) fn live_bytes(&self) -> u64 {
        self.mask_atlas.live_bytes() + self.color_atlas.live_bytes()
    }

    /// 两张图的纹理容量合计（字节）。
    pub(crate) fn capacity_bytes(&self) -> u64 {
        self.mask_atlas.capacity_bytes() + self.color_atlas.capacity_bytes()
    }

    /// 任一张图「利用率偏低」吗（`live×4 < capacity`，即 < 25%）。
    ///
    /// 逐图判断而不是看合计：mask 图巨大、color 图空着时，合计利用率可能仍不算低，
    /// 但真正该缩的是 mask。
    pub(crate) fn any_atlas_underutilized(&self) -> bool {
        [&self.mask_atlas, &self.color_atlas].iter().any(|a| {
            let capacity = a.capacity_bytes();
            capacity > 0 && a.live_bytes().saturating_mul(4) < capacity
        })
    }

    /// 累计缩图集次数（两张图求和）。
    pub(crate) fn compact_count(&self) -> u64 {
        self.mask_atlas.compacts + self.color_atlas.compacts
    }

    /// 保留预算淘汰「历史积压」时要求的**最小未使用帧数**。
    ///
    /// 只淘汰"这么久都没被用到"的条目，而不是"上一帧没用到的"：后者会把间歇使用的字形
    /// （只在某些帧出现的数字、只在某个页面出现的标签）反复扔掉再重画 —— 实测软预算
    /// 128 KiB + 27 KiB 工作集的 HUD 场景还有 8.2 次/帧的残余光栅化，就是这个原因。
    /// 120 帧 ≈ 60fps 下 2 秒、120fps 下 1 秒。
    pub(crate) const RETENTION_MIN_AGE_FRAMES: usize = 120;

    /// 按**总预算**淘汰未钉住的 LRU 条目，直到两图合计占用 ≤ `budget`。
    ///
    /// - `min_age_frames = RETENTION_MIN_AGE_FRAMES`（保留预算）：只扔"历史积压"，
    ///   当前工作集与间歇使用的字形都不动 ⇒ 实际使用中不会造成重光栅化；
    /// - `min_age_frames = 2`（硬预算）：冷条目扔完还不够就动热条目（第二遍 `1`）——
    ///   这是明确的"占用必须降下来"的请求，代价是下一帧要重新光栅化。
    pub(crate) fn trim_to_budget(&mut self, budget: u64, min_age_frames: usize) -> usize {
        let mut evicted = 0;
        // 硬预算两遍（先 2 帧、再 1 帧）；保留预算一遍（120 帧）。
        let passes: [usize; 2] = if min_age_frames <= 1 {
            [1, 1]
        } else if min_age_frames == 2 {
            [2, 1]
        } else {
            [min_age_frames, min_age_frames]
        };
        let single_pass = min_age_frames != 2;
        for age in passes {
            while self.live_bytes() > budget {
                let mask_live = self.mask_atlas.live_bytes();
                let color_live = self.color_atlas.live_bytes();
                // 先从占得多的那张图里淘汰，避免只清一侧。
                let freed = if mask_live >= color_live {
                    self.mask_atlas
                        .evict_one_unpinned(age)
                        .or_else(|| self.color_atlas.evict_one_unpinned(age))
                } else {
                    self.color_atlas
                        .evict_one_unpinned(age)
                        .or_else(|| self.mask_atlas.evict_one_unpinned(age))
                };
                match freed {
                    Some(_) => evicted += 1,
                    None => break,
                }
            }
            if single_pass {
                break;
            }
        }
        evicted
    }

    /// 清掉所有未钉住的字形。
    pub(crate) fn clear_unpinned(&mut self) -> usize {
        self.mask_atlas.clear_unpinned() + self.color_atlas.clear_unpinned()
    }

    /// 清掉全部字形（含钉住的）。
    pub(crate) fn clear_all(&mut self) -> usize {
        self.mask_atlas.clear_all() + self.color_atlas.clear_all()
    }

    /// 把两张图都收到"刚好装下现有条目"的尺寸（拷贝，不重新光栅化）。
    /// 返回是否有图集被收缩。
    pub(crate) fn compact(&mut self, state: &State) -> bool {
        let mask = self.mask_atlas.compact(state);
        let color = self.color_atlas.compact(state);
        if mask || color {
            self.rebind(state.device);
        }
        mask || color
    }

    /// 累计计数 `(rasterizations, evictions, grows)`。
    pub(crate) fn counters(&self) -> (u64, u64, u64) {
        (
            self.mask_atlas.rasterizations + self.color_atlas.rasterizations,
            self.mask_atlas.evictions + self.color_atlas.evictions,
            self.mask_atlas.grows + self.color_atlas.grows,
        )
    }
    /// Creates a new [`TextAtlas`].
    pub fn new(
        device: &Device,
        queue: &Queue,
        cache: &Cache,
        format: TextureFormat,
        default_base_texture: &TextureView,
        default_base_sampler: &Sampler,
    ) -> Self {
        Self::with_color_mode(
            device,
            queue,
            cache,
            format,
            ColorMode::Accurate,
            default_base_texture,
            default_base_sampler,
        )
    }

    /// Creates a new [`TextAtlas`] with the given [`ColorMode`].
    pub fn with_color_mode(
        device: &Device,
        queue: &Queue,
        cache: &Cache,
        format: TextureFormat,
        color_mode: ColorMode,
        default_base_texture: &TextureView,
        default_base_sampler: &Sampler,
    ) -> Self {
        let state = State { device, queue };
        let pins = Arc::new(PinRegistry::default());
        let color_atlas = InnerAtlas::new(
            &state,
            Kind::Color {
                srgb: match color_mode {
                    ColorMode::Accurate => true,
                    ColorMode::Web => false,
                },
            },
            pins.clone(),
        );
        let mask_atlas = InnerAtlas::new(&state, Kind::Mask, pins.clone());

        let bind_group = cache.create_atlas_bind_group(
            device,
            &color_atlas.texture_view,
            &mask_atlas.texture_view,
            default_base_texture,
            default_base_sampler,
        );

        Self {
            cache: cache.clone(),
            bind_group,
            color_atlas,
            mask_atlas,
            format,
            color_mode,
            default_base_texture: default_base_texture.clone(),
            default_base_sampler: default_base_sampler.clone(),
            pins,
            collect: None,
        }
    }

    pub(crate) fn bind_group_for_base_texture(
        &self,
        device: &Device,
        base_texture: &TextureView,
    ) -> BindGroup {
        self.cache.create_atlas_bind_group(
            device,
            &self.color_atlas.texture_view,
            &self.mask_atlas.texture_view,
            base_texture,
            &self.default_base_sampler,
        )
    }

    pub fn trim(&mut self) {
        self.mask_atlas.trim();
        self.color_atlas.trim();
    }

    pub(crate) fn grow(
        &mut self,
        state: &State,
        font_system: &mut FontSystem,
        cache: &mut SwashCache,
        content_type: ContentType,
        scale_factor: f32,
        rasterize_custom_glyph: impl FnMut(RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph>,
    ) -> bool {
        let did_grow = match content_type {
            // SDF 与覆盖度共用同一张 R8 图集（靠 `content_type` 区分解释方式）。
            ContentType::Mask | ContentType::Sdf => self.mask_atlas.grow(
                state,
                font_system,
                cache,
                scale_factor,
                rasterize_custom_glyph,
            ),
            ContentType::Color => self.color_atlas.grow(
                state,
                font_system,
                cache,
                scale_factor,
                rasterize_custom_glyph,
            ),
        };

        if did_grow {
            self.rebind(state.device);
        }

        did_grow
    }

    pub(crate) fn inner_for_content_mut(&mut self, content_type: ContentType) -> &mut InnerAtlas {
        match content_type {
            ContentType::Color => &mut self.color_atlas,
            ContentType::Mask | ContentType::Sdf => &mut self.mask_atlas,
        }
    }

    pub(crate) fn get_or_create_pipeline(
        &self,
        device: &Device,
        multisample: MultisampleState,
        depth_stencil: Option<DepthStencilState>,
        blend: BlendState,
        num_targets: u32,
    ) -> RenderPipeline {
        self.cache.get_or_create_pipeline(
            device,
            self.format,
            multisample,
            depth_stencil,
            blend,
            num_targets,
        )
    }

    pub(crate) fn create_material_pipeline(
        &self,
        device: &Device,
        material_layout: Option<&wgpu::BindGroupLayout>,
        fragment_source: &str,
        multisample: MultisampleState,
        depth_stencil: Option<DepthStencilState>,
        blend: BlendState,
        num_targets: u32,
    ) -> RenderPipeline {
        self.cache.create_material_pipeline(
            device,
            material_layout,
            fragment_source,
            self.format,
            multisample,
            depth_stencil,
            blend,
            num_targets,
        )
    }

    pub(crate) fn rebind(&mut self, device: &wgpu::Device) {
        self.bind_group = self.cache.create_atlas_bind_group(
            device,
            &self.color_atlas.texture_view,
            &self.mask_atlas.texture_view,
            &self.default_base_texture,
            &self.default_base_sampler,
        );
    }
}
