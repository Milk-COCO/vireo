//! 用户可管的字形缓存（`gpu.text().glyphs()`）。
//!
//! 默认行为不变：字形画到哪、光栅化到哪，图集按 LRU 自己扩。这一层给需要
//! "别每帧重新光栅化"和"显存别失控"的场景提供**完整**的控制面：
//!
//! - **预算**：[`GlyphCache::set_budget`] 定一个字节上限，帧首超了就淘汰未钉住的 LRU；
//! - **预光栅化**：[`GlyphCache::prewarm`] 在加载阶段把字填进图集（不产生可见输出）；
//! - **钉子**：[`GlyphCache::hold`] 返回 [`GlyphHold`]，活着的期间这些字形不会被淘汰；
//!   `hold` 内部先 `prewarm` 再钉，所以"钉住的键一定已经在图集里"（否则钉住没意义）；
//! - **回收**：[`GlyphCache::trim`] / [`GlyphCache::clear_unpinned`] / [`GlyphCache::clear`]；
//! - **观测**：[`GlyphCache::stats`] / [`GlyphCache::entries`]。
//!
//! 与 `StableText` 的分工：`StableText` 管的是**排版结果**（shape 过的 Buffer），
//! 这里管的是**光栅化结果**（图集里的位图/距离场）。两者独立，可以一起用。

use std::sync::Arc;

use wgpu::{Device, Queue};

use crate::glyphon::{GlyphonCacheKey, GpuCacheStatus, PinRegistry};

use super::{TextDef, TextSystem};

/// 字形缓存视图。从 [`TextSystem::glyphs`] 拿。
///
/// 生命周期借用 `TextSystem`（也就是 `GpuContext`），所以不能跨线程保存。
pub struct GlyphCache<'a> {
    sys: &'a TextSystem,
}

/// 一条图集条目属于哪张图。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlyphAtlasKind {
    /// R8 图：覆盖度掩码 + 精确 SDF（两者共用一张，靠内容类型区分解释）。
    Mask,
    /// 彩色图（RGBA8）：彩色字形（emoji / COLR）。
    Color,
}

/// 条目里存的是哪种表示。决定它怎么被 shader 解释，也决定显存占用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlyphContentType {
    /// 覆盖度掩码（未带样式的文字、几何描边）。
    Mask,
    /// 单通道有符号距离场（带样式的文字：描边/阴影/渐变）。
    Sdf,
    /// 彩色位图。
    Color,
}

impl GlyphContentType {
    fn from_glyphon(content_type: crate::glyphon::ContentType) -> Self {
        match content_type {
            crate::glyphon::ContentType::Mask => Self::Mask,
            crate::glyphon::ContentType::Sdf => Self::Sdf,
            crate::glyphon::ContentType::Color => Self::Color,
        }
    }
}

/// 单条字形缓存的快照（调试/内存审计用）。
#[derive(Clone, Debug)]
pub struct GlyphEntryInfo {
    /// 在哪张图集里。
    pub atlas: GlyphAtlasKind,
    /// 哪种表示。
    pub content: GlyphContentType,
    /// 图集内左上角（像素）。
    pub x: u16,
    /// 图集内左上角（像素）。
    pub y: u16,
    /// 条目宽（像素）。
    pub width: u16,
    /// 条目高（像素）。
    pub height: u16,
    /// 该条目占的字节数（`宽 × 高 × 每像素字节`，不含 packer 空洞）。
    pub bytes: u64,
    /// 是否被 [`GlyphHold`] 钉住。
    pub pinned: bool,
    /// 是否属于最近一次 prepare 用到的字形（本帧在用 ⇒ 不会被淘汰）。
    pub in_use: bool,
    /// 缓存键摘要：`Text(gid=12 size=32.00 bin=(0,0))` 这类形式，给日志用。
    pub key: String,
}

/// 字形缓存的累计统计。计数是**自本次进程启动**累计的（`set_budget` 只重置告警计数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlyphCacheStats {
    /// R8 图里的条目数（覆盖度 + SDF）。
    pub mask_entries: u32,
    /// 彩色图里的条目数。
    pub color_entries: u32,
    /// 被钉住的条目数（去重）。
    pub pinned_entries: u32,
    /// R8 图的条目占用（字节）。
    pub mask_used_bytes: u64,
    /// 彩色图的条目占用（字节）。
    pub color_used_bytes: u64,
    /// R8 图纹理容量（字节，`边长²`）。
    pub mask_texture_bytes: u64,
    /// 彩色图纹理容量（字节，`边长² × 4`）。
    pub color_texture_bytes: u64,
    /// 当前预算（`None` = 不限制）。
    pub budget_bytes: Option<u64>,
    /// 累计光栅化次数（新造图集条目；命中不计数）。
    pub rasterizations: u64,
    /// 累计淘汰次数（LRU / 预算 / 清空 / 扩图集时丢条目）。
    pub evictions: u64,
    /// 累计扩图集次数。
    pub grows: u64,
    /// 累计触发淘汰的次数（预算回收 + 手动 `trim`）。
    pub trims: u64,
    /// 超预算但淘汰不动（全是钉住/在用）的告警次数（0/1，改预算后重置）。
    pub budget_warnings: u64,
}

impl GlyphCacheStats {
    /// 两张图合计的条目占用（字节）。
    pub fn used_bytes(&self) -> u64 {
        self.mask_used_bytes + self.color_used_bytes
    }

    /// 两张图合计的纹理容量（字节）。
    pub fn texture_bytes(&self) -> u64 {
        self.mask_texture_bytes + self.color_texture_bytes
    }

    /// 条目数合计。
    pub fn entries(&self) -> u32 {
        self.mask_entries + self.color_entries
    }
}

/// 把一批字形钉在图集里，活着期间不允许淘汰。
///
/// 由 [`GlyphCache::hold`] 创建；`drop` 即解钉（等价于 [`Self::release`]）。
/// 多个句柄钉同一批字形是引用计数语义：最后一个句柄释放才真正解钉。
#[must_use = "句柄一丢就解钉了；要保住就把它存起来"]
pub struct GlyphHold {
    pins: Arc<PinRegistry>,
    keys: Vec<GlyphonCacheKey>,
}

impl GlyphHold {
    /// 钉住的字形键数量（不是字节数）。
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// 主动解钉（与 `drop` 等价，便于提前释放）。
    pub fn release(self) {}
}

impl Drop for GlyphHold {
    fn drop(&mut self) {
        if !self.keys.is_empty() {
            self.pins.unpin(&self.keys);
        }
    }
}

/// 缓存键摘要（Debug 形式，长度可控）。
fn key_summary(key: &GlyphonCacheKey) -> String {
    let raw = format!("{:?}", key);
    const MAX: usize = 160;
    if raw.len() <= MAX {
        raw
    } else {
        let mut cut = MAX;
        while !raw.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &raw[..cut])
    }
}

impl GlyphCache<'_> {
    pub(crate) fn new(sys: &TextSystem) -> GlyphCache<'_> {
        GlyphCache { sys }
    }

    // ---- 观测 ----

    /// 累计统计 + 当前占用。
    pub fn stats(&self) -> GlyphCacheStats {
        let ctx = self.sys.ctx();
        let (rasterizations, evictions, grows) = ctx.text_atlas.counters();
        let mask_used = ctx.text_atlas.mask_atlas.live_bytes();
        let color_used = ctx.text_atlas.color_atlas.live_bytes();
        GlyphCacheStats {
            mask_entries: ctx.text_atlas.mask_atlas.glyph_cache.len() as u32,
            color_entries: ctx.text_atlas.color_atlas.glyph_cache.len() as u32,
            pinned_entries: ctx.text_atlas.pins().len() as u32,
            mask_used_bytes: mask_used,
            color_used_bytes: color_used,
            mask_texture_bytes: ctx.text_atlas.mask_atlas.capacity_bytes(),
            color_texture_bytes: ctx.text_atlas.color_atlas.capacity_bytes(),
            budget_bytes: ctx.glyph_budget,
            rasterizations,
            evictions,
            grows,
            trims: ctx.glyph_trims,
            budget_warnings: ctx.glyph_budget_warnings,
        }
    }

    /// 逐条快照（顺序按 LRU：最近用过的在前）。条目多时开销是 O(n)，别每帧调。
    pub fn entries(&self) -> Vec<GlyphEntryInfo> {
        let ctx = self.sys.ctx();
        let pins = ctx.text_atlas.pins().snapshot();
        let mut out = Vec::with_capacity(
            ctx.text_atlas.mask_atlas.glyph_cache.len()
                + ctx.text_atlas.color_atlas.glyph_cache.len(),
        );
        for (kind, atlas, generation) in [
            (
                GlyphAtlasKind::Mask,
                &ctx.text_atlas.mask_atlas,
                ctx.text_atlas.mask_atlas.generation,
            ),
            (
                GlyphAtlasKind::Color,
                &ctx.text_atlas.color_atlas,
                ctx.text_atlas.color_atlas.generation,
            ),
        ] {
            let bpp = match kind {
                GlyphAtlasKind::Mask => 1,
                GlyphAtlasKind::Color => 4,
            };
            for (key, details) in atlas.glyph_cache.iter() {
                let (x, y, content) = match details.gpu_cache {
                    GpuCacheStatus::InAtlas { x, y, content_type } => {
                        (x, y, GlyphContentType::from_glyphon(content_type))
                    }
                    GpuCacheStatus::SkipRasterization => (0, 0, GlyphContentType::Mask),
                };
                out.push(GlyphEntryInfo {
                    atlas: kind,
                    content,
                    x,
                    y,
                    width: details.width,
                    height: details.height,
                    bytes: u64::from(details.width) * u64::from(details.height) * bpp,
                    pinned: pins.contains(key),
                    in_use: details.last_used == generation,
                    key: key_summary(key),
                });
            }
        }
        out
    }

    /// 当前被钉住的条目数（去重）。
    pub fn pinned_count(&self) -> usize {
        self.sys.ctx().text_atlas.pins().len()
    }

    // ---- 预算 ----

    /// 设置字形缓存预算（两张图**合计**的条目占用，字节）。
    ///
    /// - `Some(bytes)`：帧首检查，超了就淘汰未钉住的 LRU 字形；
    /// - `None`（默认）：不限制。
    ///
    /// 预算按"条目面积累计"算，不含图集里 packer 的空洞 —— 所以纹理容量可能比预算大
    /// 一圈（图集是 2 的幂逐步翻倍的）。被 [`Self::hold`] 钉住的字形不与预算竞争：
    /// 全是钉住的条目时只会 warn 一次然后继续用现有的图。
    pub fn set_budget(&self, bytes: Option<u64>) {
        self.sys.ctx().set_glyph_budget(bytes);
    }

    /// 当前预算（`None` = 不限制）。
    pub fn budget(&self) -> Option<u64> {
        self.sys.ctx().glyph_budget()
    }

    // ---- 预光栅化 / 钉住 ----

    /// 把 `text` 用得上的字形提前光栅化进图集（**不产生可见输出**）。
    ///
    /// 返回本次新光栅化的条目数（0 = 全部命中）。`scale` 是逻辑→设备像素的缩放，
    /// 传窗口的 dpi 缩放（`Renderer::scale_factor()`）；样式里的 `Px`/`Dp` 靠它折算。
    ///
    /// 典型用法：加载界面时 `prewarm` 一遍要显示的文案，之后每帧都是纯命中。
    pub fn prewarm(
        &self,
        device: &Device,
        queue: &Queue,
        text: &str,
        options: &TextDef,
        scale: f32,
    ) -> usize {
        self.sys
            .ctx()
            .prewarm_text(device, queue, text, options, scale)
            .0
    }

    /// 预光栅化并钉住：返回的 [`GlyphHold`] 活着期间，这批字形不会被 LRU/预算淘汰。
    ///
    /// 适合"界面骨架字形"（标题、常用标签）：启动时 `hold` 住，之后既不会重新光栅化，
    /// 也不会被别的文案挤掉。同一批字形被多次 `hold` 是引用计数语义。
    pub fn hold(
        &self,
        device: &Device,
        queue: &Queue,
        text: &str,
        options: &TextDef,
        scale: f32,
    ) -> GlyphHold {
        let mut ctx = self.sys.ctx();
        let pins = ctx.text_atlas.pins().clone();
        let (_new, keys) = ctx.prewarm_text(device, queue, text, options, scale);
        pins.pin(&keys);
        GlyphHold { pins, keys }
    }

    // ---- 回收 ----

    /// 手动按预算淘汰一次，返回淘汰条数（没设预算就是 0）。
    pub fn trim(&self) -> usize {
        self.sys.ctx().trim_glyphs_to_budget()
    }

    /// 清掉所有**未钉住**的字形，返回清掉的条数。钉住的保持不动。
    pub fn clear_unpinned(&self) -> usize {
        let mut ctx = self.sys.ctx();
        let n = ctx.text_atlas.clear_unpinned();
        ctx.glyph_evicted = ctx.glyph_evicted.saturating_add(n as u64);
        n
    }

    /// 清掉全部字形（含钉住的），返回清掉的条数。
    ///
    /// 已存在的 [`GlyphHold`] 句柄不会 panic，只是解钉时找不到条目（no-op）；
    /// 之后画到的字形会重新光栅化。
    pub fn clear(&self) -> usize {
        let mut ctx = self.sys.ctx();
        let n = ctx.text_atlas.clear_all();
        ctx.glyph_evicted = ctx.glyph_evicted.saturating_add(n as u64);
        n
    }

    /// 把图集收缩到刚好装下现有条目（**拷贝搬运，不重新光栅化**）。
    ///
    /// 清掉一批字形后图集尺寸不会自己缩回去；这个调用把显存真正还给系统。
    /// 返回是否发生了收缩。条目在图集里的坐标会变，所以**只在两帧之间调**。
    pub fn compact(&self, device: &Device, queue: &Queue) -> bool {
        self.sys.ctx().compact_glyphs(device, queue)
    }
}
