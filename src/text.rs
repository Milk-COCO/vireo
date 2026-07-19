//! 文本渲染：glyphon + cosmic-text，使用系统字体。
//!
//! `prepare_texts` 对「内容相同」的条目复用已 shape 的 `Buffer`（跳过 harfrust），
//! 位置/颜色/transform 不参与缓存键。
//!
//! 缓存策略（均可配置，经 `GpuContext`）：
//! - TTL：`set_shape_cache_ttl(Some(d) | None)`，`None` = 不按时间回收
//! - 条数：`set_shape_cache_max_entries(Some(n) | None)`，`None` = 不限制
//! - 立即清空：`clear_shape_cache`

use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;

use crate::glyphon::{
    Buffer, Cache, FontSystem, Metrics, Shaping, SwashCache, TextArea, TextAtlas, TextBounds,
    TextRenderer, Viewport,
};
pub use crate::glyphon::Attrs;
pub use crate::glyphon::ColorMode;
pub use cosmic_text::{AttrsOwned, Family, FamilyOwned, Style, Weight};
use wgpu::{Device, MultisampleState, Queue, TextureFormat};

use crate::color::Color;
use crate::gpu::GpuContext;

/// 默认硬顶（可 `set_shape_cache_max_entries` 修改；`None` = 不限制条数）。
const DEFAULT_SHAPE_MAX_ENTRIES: usize = 4096;
/// 默认软目标：超过后优先清过期项（仅 TTL 启用时；随 hard 缩放）。
const DEFAULT_SHAPE_SOFT_CAP: usize = 512;
/// 默认 TTL：超过这么久未使用 → 视为过期（真实时间，与 FPS 无关）。
const DEFAULT_SHAPE_TTL: Duration = Duration::from_secs(2);
/// 两次 GC 之间的最短间隔（真实时间）。
const SHAPE_GC_INTERVAL: Duration = Duration::from_millis(250);
/// 空闲 Buffer 池上限。
const BUFFER_POOL_CAP: usize = 128;

/// 影响 layout/shape 的键（不含 x/y/color/clip/transform）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ShapeKey {
    text: String,
    font_size_bits: u32,
    max_width_bits: u32,
    align: u8,
    /// None = 默认 Attrs
    attrs: Option<AttrsOwned>,
}

impl ShapeKey {
    fn from_entry(entry: &TextEntry) -> Self {
        let max_width_bits = entry
            .options
            .max_width
            .map(|w| w.to_bits())
            .unwrap_or(u32::MAX);
        Self {
            text: entry.text.clone(),
            font_size_bits: entry.options.font_size.to_bits(),
            max_width_bits,
            align: entry.options.align as u8,
            attrs: entry.options.attrs.clone(),
        }
    }
}

struct ShapeCacheSlot {
    key: ShapeKey,
    buffer: Buffer,
    last_used: Instant,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct ShapeCacheStats {
    pub hits: u64,
    pub misses: u64,
}

pub struct TextContext {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub cache: Cache,
    pub text_atlas: TextAtlas,
    pub text_renderer: TextRenderer,
    pub viewport: Viewport,
    sample_count: u32,
    /// 已 shape 的 Buffer 槽位
    shape_slots: Vec<ShapeCacheSlot>,
    /// ShapeKey → slot 下标
    shape_map: FxHashMap<ShapeKey, u32>,
    /// 可复用的空 Buffer
    buffer_pool: Vec<Buffer>,
    /// 上次 GC 时刻（真实时间，与 FPS 无关）
    last_gc: Instant,
    /// `None` = 永不按时间自动回收；`Some(d)` = 超过 d 未使用则过期。
    shape_ttl: Option<Duration>,
    /// `None` = 不限制缓存条数；`Some(n)` = 最多 n 条，满则 LRU 换槽。
    shape_max_entries: Option<usize>,
    stats: ShapeCacheStats,
}

impl TextContext {
    /// 确保 TextRenderer 匹配给定 sample_count
    pub fn ensure_sample_count(&mut self, device: &Device, count: u32) {
        if self.sample_count != count {
            self.text_renderer = TextRenderer::new(
                &mut self.text_atlas,
                device,
                MultisampleState {
                    count,
                    ..Default::default()
                },
                None,
            );
            self.sample_count = count;
        }
    }

    /// 每帧绘制前调用一次（多 batch 共享）。
    /// TTL/GC 用真实时间，与 FPS 无关；`shape_ttl == None` 时不做自动回收。
    pub fn advance_frame(&mut self) {
        if self.shape_ttl.is_none() {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.last_gc) >= SHAPE_GC_INTERVAL {
            self.gc_stale_shapes(now);
            self.last_gc = now;
        }
    }

    /// 设置 shape 缓存 TTL。
    /// - `Some(d)`：超过 d 未使用则过期（未超软目标时实际用 d×4）
    /// - `None`：永不按时间自动回收（可用 `clear_shape_cache` 手动清空）
    pub fn set_shape_cache_ttl(&mut self, ttl: Option<Duration>) {
        self.shape_ttl = ttl;
    }

    /// 当前 shape 缓存 TTL（`None` = 不自动按时间回收）。
    pub fn shape_cache_ttl(&self) -> Option<Duration> {
        self.shape_ttl
    }

    /// 设置 shape 缓存最大条数。
    /// - `Some(n)`：最多 n 条不同 ShapeKey，满则 LRU 换槽
    /// - `None`：不限制条数（仅 TTL / `clear_shape_cache` 可缩小）
    pub fn set_shape_cache_max_entries(&mut self, max: Option<usize>) {
        self.shape_max_entries = max;
        if let Some(cap) = max {
            while self.shape_slots.len() > cap {
                let victim = self.evict_one_slot(Instant::now());
                self.remove_slot(victim);
            }
        }
    }

    fn remove_slot(&mut self, slot_i: usize) {
        let removed = self.shape_slots.swap_remove(slot_i);
        self.shape_map.remove(&removed.key);
        self.recycle_buffer(removed.buffer);
        if slot_i < self.shape_slots.len() {
            let moved_key = self.shape_slots[slot_i].key.clone();
            self.shape_map.insert(moved_key, slot_i as u32);
        }
    }

    /// 当前 shape 缓存最大条数（`None` = 不限制）。
    pub fn shape_cache_max_entries(&self) -> Option<usize> {
        self.shape_max_entries
    }

    /// 立即清空全部 shape 缓存，Buffer 尽量回池。
    pub fn clear_shape_cache(&mut self) {
        self.shape_map.clear();
        let slots = std::mem::take(&mut self.shape_slots);
        for slot in slots {
            self.recycle_buffer(slot.buffer);
        }
        self.stats = ShapeCacheStats::default();
    }

    fn soft_cap(&self) -> usize {
        match self.shape_max_entries {
            Some(hard) => hard.min(DEFAULT_SHAPE_SOFT_CAP).max(1),
            None => DEFAULT_SHAPE_SOFT_CAP,
        }
    }

    /// 缓存命中统计（调试/测试）。
    pub fn shape_cache_stats(&self) -> ShapeCacheStats {
        self.stats
    }

    pub fn reset_shape_cache_stats(&mut self) {
        self.stats = ShapeCacheStats::default();
    }

    /// 当前缓存条目数（调试）。
    pub fn shape_cache_len(&self) -> usize {
        self.shape_slots.len()
    }

    fn take_buffer(&mut self, metrics: Metrics) -> Buffer {
        if let Some(mut buf) = self.buffer_pool.pop() {
            buf.set_metrics(metrics);
            buf
        } else {
            Buffer::new(&mut self.font_system, metrics)
        }
    }

    fn recycle_buffer(&mut self, buffer: Buffer) {
        if self.buffer_pool.len() < BUFFER_POOL_CAP {
            self.buffer_pool.push(buffer);
        }
    }

    /// 生效的过期阈值：未超软目标时更宽松（ttl×4）。
    fn effective_ttl(&self, now_len: usize) -> Option<Duration> {
        let base = self.shape_ttl?;
        if now_len > self.soft_cap() {
            Some(base)
        } else {
            Some(base.saturating_mul(4))
        }
    }

    /// 回收「超过 TTL 未使用」的条目。TTL 为真实时间；`shape_ttl == None` 时为空操作。
    fn gc_stale_shapes(&mut self, now: Instant) {
        let Some(ttl) = self.effective_ttl(self.shape_slots.len()) else {
            return;
        };
        if self.shape_slots.is_empty() {
            return;
        }
        let mut i = 0usize;
        while i < self.shape_slots.len() {
            let age = now.saturating_duration_since(self.shape_slots[i].last_used);
            if age > ttl {
                self.remove_slot(i);
            } else {
                i += 1;
            }
        }
    }

    /// 在必须腾槽时：优先过期项（若启用 TTL），否则全局最久未用。
    fn evict_one_slot(&mut self, now: Instant) -> usize {
        if let Some(ttl) = self.shape_ttl {
            if let Some(i) = self
                .shape_slots
                .iter()
                .position(|s| now.saturating_duration_since(s.last_used) > ttl)
            {
                return i;
            }
        }
        let mut oldest_i = 0usize;
        let mut oldest_t = self.shape_slots[0].last_used;
        for (i, slot) in self.shape_slots.iter().enumerate().skip(1) {
            if slot.last_used < oldest_t {
                oldest_t = slot.last_used;
                oldest_i = i;
            }
        }
        oldest_i
    }

    fn replace_slot(&mut self, slot_i: usize, key: ShapeKey, buffer: Buffer, now: Instant) -> u32 {
        let old_key = self.shape_slots[slot_i].key.clone();
        self.shape_map.remove(&old_key);
        let old_buf = std::mem::replace(&mut self.shape_slots[slot_i].buffer, buffer);
        self.recycle_buffer(old_buf);
        self.shape_slots[slot_i].key = key.clone();
        self.shape_slots[slot_i].last_used = now;
        let idx = slot_i as u32;
        self.shape_map.insert(key, idx);
        idx
    }

    /// 获取或创建已 shape 的 buffer，返回 slot 下标。
    fn get_or_shape(&mut self, key: ShapeKey, entry: &TextEntry) -> u32 {
        let now = Instant::now();
        if let Some(&idx) = self.shape_map.get(&key) {
            self.shape_slots[idx as usize].last_used = now;
            self.stats.hits += 1;
            return idx;
        }

        self.stats.misses += 1;
        let line_height = entry.options.font_size * 1.2;
        let metrics = Metrics::new(entry.options.font_size, line_height);
        let mut buffer = self.take_buffer(metrics);
        buffer.set_size(entry.options.max_width, None);

        let attrs = entry
            .options
            .attrs
            .as_ref()
            .map(|a| a.as_attrs())
            .unwrap_or_else(Attrs::new);

        buffer.set_text(
            &entry.text,
            &attrs,
            Shaping::Advanced,
            Some(entry.options.align.into()),
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        // 增长：无硬顶或未满则 push；满硬顶则 LRU/过期换槽
        let under_cap = match self.shape_max_entries {
            None => true,
            Some(cap) => self.shape_slots.len() < cap,
        };
        if under_cap {
            if self.shape_ttl.is_some() && self.shape_slots.len() >= self.soft_cap() {
                self.gc_stale_shapes(now);
            }
            let still_under = match self.shape_max_entries {
                None => true,
                Some(cap) => self.shape_slots.len() < cap,
            };
            if still_under {
                let idx = self.shape_slots.len() as u32;
                self.shape_slots.push(ShapeCacheSlot {
                    key: key.clone(),
                    buffer,
                    last_used: now,
                });
                self.shape_map.insert(key, idx);
                return idx;
            }
        }

        // 仅 hard cap 已满时走到这里
        let victim = self.evict_one_slot(now);
        self.replace_slot(victim, key, buffer, now)
    }
}

impl TextContext {
    pub fn new(
        device: &Device,
        queue: &Queue,
        texture_format: TextureFormat,
        color_mode: ColorMode,
        transform_bgl: &wgpu::BindGroupLayout,
    ) -> Self {
        let mut font_system = FontSystem::new();
        font_system.db_mut().load_system_fonts();

        let swash_cache = SwashCache::new();
        let cache = Cache::new(device, transform_bgl);
        let mut text_atlas =
            TextAtlas::with_color_mode(device, queue, &cache, texture_format, color_mode);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            device,
            MultisampleState::default(),
            None,
        );
        let viewport = Viewport::new(device, &cache);

        Self {
            font_system,
            swash_cache,
            cache,
            text_atlas,
            text_renderer,
            viewport,
            sample_count: 1,
            shape_slots: Vec::with_capacity(64),
            shape_map: FxHashMap::default(),
            buffer_pool: Vec::with_capacity(16),
            last_gc: Instant::now(),
            shape_ttl: Some(DEFAULT_SHAPE_TTL),
            shape_max_entries: Some(DEFAULT_SHAPE_MAX_ENTRIES),
            stats: ShapeCacheStats::default(),
        }
    }
}

/// 文本水平对齐
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    End,
    Justified,
}

impl From<TextAlign> for cosmic_text::Align {
    fn from(a: TextAlign) -> Self {
        match a {
            TextAlign::Left => cosmic_text::Align::Left,
            TextAlign::Center => cosmic_text::Align::Center,
            TextAlign::Right => cosmic_text::Align::Right,
            TextAlign::End => cosmic_text::Align::End,
            TextAlign::Justified => cosmic_text::Align::Justified,
        }
    }
}

/// 文本渲染选项
#[derive(Clone)]
pub struct TextOptions {
    pub x: f32,
    pub y: f32,
    pub font_size: f32,
    pub color: Color,
    /// 最大宽度，超过则换行。None 表示不换行。
    pub max_width: Option<f32>,
    /// 水平对齐。需要配合 max_width 使用才有效果。
    pub align: TextAlign,
    /// 字体属性（family、weight、style 等）。None 使用默认 Attrs。
    pub attrs: Option<AttrsOwned>,
    /// 裁剪区域 (left, top, right, bottom)，物理像素坐标。None 不裁剪。超出区域的文本会被隐藏。
    pub clip: Option<(i32, i32, i32, i32)>,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            font_size: 16.0,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
            max_width: None,
            align: TextAlign::Left,
            attrs: None,
            clip: None,
        }
    }
}

impl TextOptions {
    pub fn x(mut self, x: f32) -> Self {
        self.x = x;
        self
    }

    pub fn y(mut self, y: f32) -> Self {
        self.y = y;
        self
    }

    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn max_width(mut self, w: f32) -> Self {
        self.max_width = Some(w);
        self
    }

    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    pub fn clip(mut self, left: i32, top: i32, right: i32, bottom: i32) -> Self {
        self.clip = Some((left, top, right, bottom));
        self
    }

    pub fn with_family(mut self, family: Family<'_>) -> Self {
        self.attrs
            .get_or_insert_with(|| AttrsOwned::new(&Attrs::new()))
            .family_owned = FamilyOwned::new(family);
        self
    }

    pub fn with_weight(mut self, weight: Weight) -> Self {
        let attrs = self
            .attrs
            .get_or_insert_with(|| AttrsOwned::new(&Attrs::new()));
        attrs.weight = weight;
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        let attrs = self
            .attrs
            .get_or_insert_with(|| AttrsOwned::new(&Attrs::new()));
        attrs.style = style;
        self
    }
}

/// 文本条目，pushed by draw_text
#[derive(Clone)]
pub struct TextEntry {
    pub text: String,
    pub options: TextOptions,
    /// Batch-local transform index (logical space). 0 = identity.
    pub(crate) transform_index: u32,
}

/// 文本条目列表，存储一组待渲染的文本。
///
/// 通过 `draw_text(&mut list, "text", options)` 添加条目，
/// 或直接调用 `push("text", options)`。
pub struct TextEntryList {
    pub entries: Vec<TextEntry>,
}

impl TextEntryList {
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(8),
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 从另一个 TextEntryList 复制条目
    pub fn new_from_entries(other: &Self) -> Self {
        Self {
            entries: other.entries.clone(),
        }
    }

    /// 添加文本条目（默认 transform_index = 0，恒等变换）。
    pub fn push(&mut self, text: &str, options: TextOptions) {
        self.entries.push(TextEntry {
            text: text.to_string(),
            options,
            transform_index: 0,
        });
    }

    /// 添加文本条目并指定 transform index。
    pub(crate) fn push_indexed(&mut self, text: &str, options: TextOptions, transform_index: u32) {
        self.entries.push(TextEntry {
            text: text.to_string(),
            options,
            transform_index,
        });
    }

    /// 准备文本条目（调用 glyphon prepare），返回 (vertex_start, vertex_count)，
    /// 用于后续 render_range() 分段绘制。多 batch 时逐个调用，最后用返回的范围渲染。
    ///
    /// `transform_table`：batch 的本地变换矩阵表（12 f32 / mat3x3）。
    /// `global_transforms`：全局矩阵表（物理空间），新增矩阵追加到此。
    /// `scale`：逻辑→物理像素缩放因子。
    pub fn prepare_texts(
        &self,
        gpu: &GpuContext,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
        transform_table: &[f32],
        global_transforms: &mut Vec<f32>,
    ) -> (u32, u32) {
        if self.entries.is_empty() {
            return (0, 0);
        }

        let mut text_ctx = gpu.text_ctx.borrow_mut();

        text_ctx.viewport.update(
            &gpu.queue,
            crate::glyphon::Resolution {
                width: physical_width,
                height: physical_height,
            },
        );

        let n = self.entries.len();
        // 阶段 1：resolve shape 缓存 → slot 下标
        let mut slot_indices: Vec<u32> = Vec::with_capacity(n);
        for entry in &self.entries {
            let key = ShapeKey::from_entry(entry);
            let idx = text_ctx.get_or_shape(key, entry);
            slot_indices.push(idx);
        }

        // 阶段 2：构建 TextArea（只读 slots）+ glyphon prepare
        // 预计算 per-entry 元数据，避免与 buffer 借用交织
        struct AreaMeta {
            left: f32,
            top: f32,
            color: crate::glyphon::Color,
            bounds: TextBounds,
            transform_index: u32,
        }

        let mut metas: Vec<AreaMeta> = Vec::with_capacity(n);
        for entry in &self.entries {
            let color = crate::glyphon::Color::rgba(
                (entry.options.color.r * 255.0) as u8,
                (entry.options.color.g * 255.0) as u8,
                (entry.options.color.b * 255.0) as u8,
                (entry.options.color.a * 255.0) as u8,
            );
            let bounds = match entry.options.clip {
                Some((l, t, r, b)) => TextBounds {
                    left: l,
                    top: t,
                    right: r,
                    bottom: b,
                },
                None => TextBounds::default(),
            };
            let phys_idx = {
                let base = entry.transform_index as usize * 12;
                if base + 12 <= transform_table.len() {
                    let t = &transform_table[base..base + 12];
                    let is_identity = t[0] == 1.0
                        && t[1] == 0.0
                        && t[4] == 0.0
                        && t[5] == 1.0
                        && t[8] == 0.0
                        && t[9] == 0.0;
                    if is_identity {
                        0
                    } else {
                        let idx = (global_transforms.len() / 12) as u32;
                        global_transforms.extend_from_slice(&[
                            t[0],
                            t[1],
                            0.0,
                            0.0,
                            t[4],
                            t[5],
                            0.0,
                            0.0,
                            t[8] * scale,
                            t[9] * scale,
                            1.0,
                            0.0,
                        ]);
                        idx
                    }
                } else {
                    0
                }
            };
            metas.push(AreaMeta {
                left: entry.options.x * scale,
                top: entry.options.y * scale,
                color,
                bounds,
                transform_index: phys_idx,
            });
        }

        let vertex_start;
        let vertex_count;
        {
            // 拆字段以便 areas 借用 shape_slots 同时 mut prepare
            let TextContext {
                ref mut font_system,
                ref mut swash_cache,
                ref mut text_atlas,
                ref mut text_renderer,
                ref viewport,
                ref shape_slots,
                ..
            } = *text_ctx;

            let mut areas: Vec<TextArea> = Vec::with_capacity(n);
            for (i, &slot_i) in slot_indices.iter().enumerate() {
                let meta = &metas[i];
                areas.push(TextArea {
                    buffer: &shape_slots[slot_i as usize].buffer,
                    left: meta.left,
                    top: meta.top,
                    scale,
                    bounds: meta.bounds,
                    default_color: meta.color,
                    custom_glyphs: &[],
                    transform_index: meta.transform_index,
                });
            }

            vertex_start = text_renderer.glyph_vertex_count();
            text_renderer
                .prepare(
                    &gpu.device,
                    &gpu.queue,
                    font_system,
                    text_atlas,
                    viewport,
                    areas,
                    swash_cache,
                )
                .expect("glyphon prepare failed");
            vertex_count = text_renderer.glyph_vertex_count() - vertex_start;
        }

        (vertex_start, vertex_count)
    }

    /// prepare + render 所有文本条目到 render pass（单 batch 便利方法）。
    /// 不使用 transform（所有文字用恒等矩阵）。
    pub fn draw(
        &self,
        gpu: &GpuContext,
        render_pass: &mut wgpu::RenderPass<'_>,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
    ) {
        if self.entries.is_empty() {
            return;
        }
        let mut global_transforms = Vec::new();
        let _ = self.prepare_texts(
            gpu,
            physical_width,
            physical_height,
            scale,
            &[],
            &mut global_transforms,
        );
        let text_ctx = gpu.text_ctx.borrow();
        text_ctx
            .text_renderer
            .render(
                &text_ctx.text_atlas,
                &text_ctx.viewport,
                render_pass,
                &gpu.transform_dummy_bind_group,
            )
            .expect("glyphon render failed");
    }
}

/// 单个字形的渲染四边形
#[derive(Debug, Clone, Copy)]
pub struct GlyphQuad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// 文本排版结果
pub struct TextLayout {
    pub quads: Vec<GlyphQuad>,
    /// 字形图集纹理的 bind group（指向 glyphon 的 atlas）
    pub atlas_bind_group: wgpu::BindGroup,
}

/// 往 batch.texts 中添加一条文本渲染指令
pub fn draw_text(list: &mut TextEntryList, text: &str, options: TextOptions) {
    list.push(text, options);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};
    use rustc_hash::FxHasher;

    #[test]
    fn shape_key_ignores_position_and_color() {
        let e1 = TextEntry {
            text: "Hello".into(),
            options: TextOptions::default().x(10.0).y(20.0).color(Color::new(1.0, 0.0, 0.0, 1.0)),
            transform_index: 0,
        };
        let e2 = TextEntry {
            text: "Hello".into(),
            options: TextOptions::default().x(99.0).y(0.0).color(Color::new(0.0, 1.0, 0.0, 1.0)),
            transform_index: 3,
        };
        assert_eq!(ShapeKey::from_entry(&e1), ShapeKey::from_entry(&e2));
    }

    #[test]
    fn shape_key_differs_on_font_size_and_text() {
        let base = TextEntry {
            text: "Hello".into(),
            options: TextOptions::default().font_size(16.0),
            transform_index: 0,
        };
        let sized = TextEntry {
            text: "Hello".into(),
            options: TextOptions::default().font_size(18.0),
            transform_index: 0,
        };
        let other = TextEntry {
            text: "World".into(),
            options: TextOptions::default().font_size(16.0),
            transform_index: 0,
        };
        assert_ne!(ShapeKey::from_entry(&base), ShapeKey::from_entry(&sized));
        assert_ne!(ShapeKey::from_entry(&base), ShapeKey::from_entry(&other));
    }

    #[test]
    fn shape_key_hash_stable() {
        let e = TextEntry {
            text: "稳定".into(),
            options: TextOptions::default().font_size(14.0),
            transform_index: 0,
        };
        let k = ShapeKey::from_entry(&e);
        let mut h1 = FxHasher::default();
        k.hash(&mut h1);
        let mut h2 = FxHasher::default();
        k.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn default_ttl_is_two_seconds() {
        assert_eq!(DEFAULT_SHAPE_TTL, Duration::from_secs(2));
    }

    #[test]
    fn default_max_entries_is_4096() {
        assert_eq!(DEFAULT_SHAPE_MAX_ENTRIES, 4096);
    }
}
