use std::sync::Arc;
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;

pub use crate::glyphon::Attrs;
pub use crate::glyphon::ColorMode;
use crate::glyphon::{
    Buffer, Cache, FontSystem, Metrics, Shaping, SwashCache, TextArea, TextAtlas, TextBounds,
    TextRenderer, Viewport,
};
use wgpu::{Device, MultisampleState, Queue, Sampler, TextureFormat, TextureView};

use super::{GlyphKey, TypesetCacheStats, TypesetKey, TypesetSlot};
use super::{TextAlign, TextDef, TextStencilMode, stencil_text_ds_pass, stencil_text_ds_test};

/// 默认硬顶（可 `set_typeset_max_entries` 修改；`None` = 不限制条数）。
const DEFAULT_SHAPE_MAX_ENTRIES: usize = 4096;
/// 默认软目标：超过后优先清过期项（仅 TTL 启用时；随 hard 缩放）。
const DEFAULT_SHAPE_SOFT_CAP: usize = 512;
/// 默认 TTL：超过这么久未使用 → 视为过期（真实时间，与 FPS 无关）。
const DEFAULT_SHAPE_TTL: Duration = Duration::from_secs(2);
/// 两次 GC 之间的最短间隔（真实时间）。
const SHAPE_GC_INTERVAL: Duration = Duration::from_millis(250);
/// 空闲 Buffer 池上限。
const BUFFER_POOL_CAP: usize = 128;

/// 样式表缓冲的容量记录（缓冲本体在 `Viewport` 里）。
struct StyleBuffers {
    styles_cap: u64,
    layers_cap: u64,
    stops_cap: u64,
}

pub struct TextContext {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub cache: Cache,
    pub text_atlas: TextAtlas,
    pub text_renderer: TextRenderer,
    /// 创建 atlas 时用的默认基底纹理/采样器，AtlasFull 重建 atlas 时需要。
    pub(crate) base_texture: TextureView,
    pub(crate) base_sampler: Sampler,
    pub viewport: Viewport,
    last_viewport: Option<(u32, u32)>,
    /// 上次写入的 dpi（与 `last_viewport` 一起决定要不要重写 `Params`）。
    last_dpi: Option<u32>,
    /// 文本样式表 / 图层表的容量记录（缓冲本体在 `Viewport` 里，写入走它）。
    style_buffers: Option<StyleBuffers>,
    sample_count: u32,
    /// 已 shape 的 Buffer 槽位
    pub(crate) typeset_slots: Vec<TypesetSlot>,
    /// TypesetKey → slot 下标
    typeset_map: FxHashMap<TypesetKey, u32>,
    /// 可复用的空 Buffer
    buffer_pool: Vec<Buffer>,
    /// 上次 GC 时刻（真实时间，与 FPS 无关）
    last_gc: Instant,
    /// `None` = 永不按时间自动回收；`Some(d)` = 超过 d 未使用则过期。
    typeset_ttl: Option<Duration>,
    /// `None` = 不限制缓存条数；`Some(n)` = 最多 n 条，满则 LRU 换槽。
    typeset_max_entries: Option<usize>,
    /// 按字符和完整 shape 样式缓存 resolved glyph 元数据。
    /// 不缓存位图；光栅结果仍由 glyph atlas 管理。默认无 cap/TTL/LRU。
    pub(crate) glyph_cache: FxHashMap<GlyphKey, Arc<ResolvedGlyphCluster>>,
    /// 本帧 prepare 中引用的 slot，禁止淘汰
    frame_pinned: Vec<u32>,
    stats: TypesetCacheStats,
    /// 字形缓存预算（两张图集的**合计**占用，字节；`None` = 不限制）。
    /// 超出时在帧首淘汰未钉住的 LRU 字形。
    pub(crate) glyph_budget: Option<u64>,
    /// 超预算但淘汰不动（全是钉住/本帧在用）时只 warn 一次。
    pub(crate) glyph_budget_warnings: u64,
    /// 用户/预算触发的淘汰次数与条数。
    pub(crate) glyph_trims: u64,
    pub(crate) glyph_evicted: u64,
    /// 观测到的**单帧占用峰值**（帧首读数取 max）。这是"预算该给多大"的实测依据：
    /// 预算小于它就会每帧重光栅化（`set_budget` 会据此 warn 一次）。
    pub(crate) glyph_peak_bytes: u64,
}

#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct ResolvedTextGlyph {
    pub(crate) glyph: crate::glyphon::LayoutGlyph,
    pub(crate) line_y: f32,
    pub(crate) line_top: f32,
    pub(crate) line_height: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedGlyphCluster {
    pub(crate) glyphs: Arc<[Arc<ResolvedTextGlyph>]>,
    pub(crate) advance: f32,
}

impl TextContext {
    /// 确保 TextRenderer 匹配给定 sample_count（默认无 DS；随后由
    /// `ensure_text_stencil_mode` 按本帧 DS 切换）。
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

    /// 同步文字管线到真实 surface 格式（macOS Metal surface 常为 Bgra8UnormSrgb）。
    /// 首次窗口创建时调用一次：atlas 格式 + 文字管线目标格式与新格式对齐。
    /// 此后的 stencil 模式切换（`ensure_text_stencil_mode`）会按新格式重建管线。
    pub(crate) fn ensure_text_format(&mut self, device: &Device, format: TextureFormat) {
        if self.text_atlas.format == format {
            return;
        }
        self.text_atlas.format = format;
        self.ensure_text_stencil_mode(device, TextStencilMode::None);
    }

    /// 设置文字管线 DS 模式（与当前 render pass / 是否测裁切一致）。
    /// 默认混合（ALPHA）；逐段 blend 由 draw 循环经 [`Self::ensure_text_pipeline`] 覆盖。
    pub(crate) fn ensure_text_stencil_mode(&mut self, device: &Device, mode: TextStencilMode) {
        self.ensure_text_pipeline(device, mode, wgpu::BlendState::ALPHA_BLENDING, 1);
    }

    /// 设置文字管线（含混合）：DS 由 mode 定，blend 逐段传。
    /// draw 循环对每段调一次（缓存命中＋句柄交换，无重建）。
    /// `num_targets`：本 pass 的颜色附件数（多附件 pass 需要目标数匹配的管线变体）。
    pub(crate) fn ensure_text_pipeline(
        &mut self,
        device: &Device,
        mode: TextStencilMode,
        blend: wgpu::BlendState,
        num_targets: u32,
    ) {
        let ds = match mode {
            TextStencilMode::None => None,
            TextStencilMode::Pass => stencil_text_ds_pass(),
            TextStencilMode::Test => stencil_text_ds_test(),
        };
        let pipeline = self.text_atlas.get_or_create_pipeline(
            device,
            MultisampleState {
                count: self.sample_count,
                ..Default::default()
            },
            ds,
            blend,
            num_targets,
        );
        self.text_renderer.set_pipeline(pipeline);
    }

    /// 兼容旧名：强制文字管线带 stencil Test。
    pub fn ensure_text_stencil(&mut self, device: &Device) {
        self.ensure_text_stencil_mode(device, TextStencilMode::Test);
    }

    /// 预热文字管线：强制 swash cache / atlas / 上传 lazy 初始化。
    /// 用单字符 "A" 跑一次 prepare，触发首帧 33ms 的 text shape 成本。
    /// 调用前 `ensure_sample_count` 必须已跑过。
    pub fn preheat(
        &mut self,
        device: &Device,
        queue: &Queue,
        physical_width: u32,
        physical_height: u32,
    ) {
        let mut buf = Buffer::new(&mut self.font_system, Metrics::new(16.0, 20.0));
        let attrs = Attrs::new();
        buf.set_text("A", &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.font_system, true);

        self.viewport.update(
            queue,
            crate::glyphon::Resolution {
                width: physical_width,
                height: physical_height,
            },
            1.0,
        );

        self.text_renderer.clear();
        let _ = self.text_renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.text_atlas,
            &self.viewport,
            [TextArea {
                buffer: &buf,
                left: 0.0,
                top: 0.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: physical_width as i32,
                    bottom: physical_height as i32,
                },
                default_color: crate::glyphon::Color::rgb(255, 255, 255),
                custom_glyphs: &[],
                transform_index: 0,
                base_uv_rect: [0.0, 0.0, 1.0, 1.0],
                style_index: 0,
                style_layers_only: 0,
                sdf_band: 0.0,
                style_layers: 0,
                style_above_mask: 0,
                style_reach: 0.0,
                vector_stroke: None,
            }],
            &mut self.swash_cache,
        );
    }

    /// 每帧绘制前调用一次（多 batch 共享）。
    /// TTL/GC 用真实时间，与 FPS 无关；`typeset_ttl == None` 时不做自动回收。
    pub fn advance_frame(&mut self) {
        // 图集代数推进：上一帧用过的字形从这一帧起可被淘汰。
        // （此前从没推进过，`try_allocate` 因此永远认为"全都在用" → 只扩不淘。）
        self.text_atlas.trim();
        // 记下"上一帧结束时图集有多大"—— 这就是单帧工作集的实测值（预算该给多大看它）。
        self.glyph_peak_bytes = self.glyph_peak_bytes.max(self.text_atlas.live_bytes());
        self.enforce_glyph_budget();
        if self.typeset_ttl.is_none() {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.last_gc) >= SHAPE_GC_INTERVAL {
            let t0 = Instant::now();
            self.gc_stale_shapes(now);
            let us = t0.elapsed().as_micros() as u64;
            self.stats.gc_runs = self.stats.gc_runs.saturating_add(1);
            self.stats.last_gc_us = us;
            self.stats.total_gc_us = self.stats.total_gc_us.saturating_add(us);
            self.last_gc = now;
        }
    }

    /// 设置字形缓存预算（两张图集**合计**占用，字节；`None` = 不限制）。
    pub fn set_glyph_budget(&mut self, bytes: Option<u64>) {
        // 预算小于已观测到的单帧峰值 ⇒ 必然每帧淘汰+重光栅化（真实窗口里 20ms/帧级别）。
        // 这里只提示，不擅自改用户给的数字：`stats().peak_used_bytes` 是他自己调参的依据。
        if let Some(budget) = bytes
            && budget < self.glyph_peak_bytes
        {
            log::warn!(
                "vireo: 字形缓存预算 {} KiB 小于观测到的单帧占用峰值 {} KiB —— \
                 会每帧淘汰并在下一帧重新光栅化（建议 ≥ 1.5× 峰值）",
                budget / 1024,
                self.glyph_peak_bytes / 1024
            );
        }
        self.glyph_budget = bytes;
        self.glyph_budget_warnings = 0;
    }

    pub fn glyph_budget(&self) -> Option<u64> {
        self.glyph_budget
    }

    /// 按预算淘汰未钉住的字形（帧首调用；没设预算就是空操作）。
    fn enforce_glyph_budget(&mut self) {
        let Some(budget) = self.glyph_budget else {
            return;
        };
        if self.text_atlas.live_bytes() <= budget {
            return;
        }
        let evicted = self.text_atlas.trim_to_budget(budget) as u64;
        self.glyph_evicted = self.glyph_evicted.saturating_add(evicted);
        self.glyph_trims = self.glyph_trims.saturating_add(1);
        if self.text_atlas.live_bytes() > budget && self.glyph_budget_warnings == 0 {
            self.glyph_budget_warnings = 1;
            log::warn!(
                "vireo: 字形缓存超出预算（{} 字节），剩下的都是被 hold 住或本帧在用的字形；\
                 图集只会继续扩，释放句柄后才会回落",
                budget
            );
        }
    }

    /// 手动按预算淘汰一次，返回淘汰条数（不受帧首自动回收影响）。
    pub fn trim_glyphs_to_budget(&mut self) -> usize {
        let Some(budget) = self.glyph_budget else {
            return 0;
        };
        let evicted = self.text_atlas.trim_to_budget(budget);
        self.glyph_evicted = self.glyph_evicted.saturating_add(evicted as u64);
        self.glyph_trims = self.glyph_trims.saturating_add(1);
        evicted
    }

    /// 把图集收缩到刚好装下现有条目（拷贝搬运，不重新光栅化）。
    /// 返回是否有图集被收缩。
    pub fn compact_glyphs(&mut self, device: &Device, queue: &Queue) -> bool {
        let state = crate::glyphon::State { device, queue };
        self.text_atlas.compact(&state)
    }

    /// 把 `text` 用得上的字形光栅化进图集，返回 `(新光栅化条目数, 本次触碰到的键)`。
    ///
    /// 顶点会被丢弃（`prepare_discard`），所以这次调用**不产生可见输出**，
    /// 只负责把图集填好并记录键集合 —— `prewarm` / `hold` 共用这条路径。
    pub(crate) fn prewarm_text(
        &mut self,
        device: &Device,
        queue: &Queue,
        text: &str,
        def: &TextDef,
        scale: f32,
    ) -> (usize, Vec<crate::glyphon::GlyphonCacheKey>) {
        use crate::glyphon::TextArea;

        let before = self.text_atlas.counters().0;
        let slot = self.get_or_shape_text(text, def);
        let buffer = self.typeset_slots[slot as usize].buffer.clone();

        // 还没建过视口（首帧之前就 prewarm）时给一个安全的占位分辨率：
        // prepare 只用它算顶点（这里会丢弃），但 0 会让 Params 显示成空窗口。
        let res = self.viewport.resolution();
        if res.width == 0 || res.height == 0 {
            self.viewport.update(
                queue,
                crate::glyphon::Resolution {
                    width: 1024,
                    height: 1024,
                },
                scale,
            );
        }

        // 样式只影响"会创建哪些图集条目"（band / 层数 / above 位图 / 外扩 / 几何描边），
        // 而键本身只由 (字形, 尺寸, band_q) 决定，所以这里按绘制时的算法复算一遍；
        // 样式表下标随便给非零值 —— 顶点随即丢弃，不会进 shader。
        // **无样式时必须给 0**：`style_index == 0` 才是「覆盖度掩码」那条路径，
        // 给了非零值会生成 SDF 条目，跟真正绘制（`Text` 键）对不上。
        let dpi = scale as f64;
        let styled = def.style.as_ref().is_some_and(|s| s.needs_style());
        let (
            style_index,
            style_layers_only,
            sdf_band,
            style_layers,
            style_above_mask,
            style_reach,
            vector_stroke,
        ) = match def.style.as_ref() {
            Some(style) if styled => (
                1,
                2,
                crate::text::band_for_reach(style.reach_px(dpi)),
                style.layers.len().min(8) as u32,
                super::prepare::above_mask(style),
                style.reach_px(dpi),
                super::prepare::stroke_px(style, dpi),
            ),
            _ => (0, 0, 0.0, 0, 0, 0.0, None),
        };

        self.text_atlas.start_collect();
        let area = TextArea {
            buffer: &buffer,
            left: 0.0,
            top: 0.0,
            scale,
            // 预光栅化不裁剪：越界字形也要进图集（prepare 会先光栅化再判边界）。
            bounds: crate::glyphon::TextBounds {
                left: i32::MIN / 2,
                top: i32::MIN / 2,
                right: i32::MAX / 2,
                bottom: i32::MAX / 2,
            },
            default_color: crate::glyphon::Color::rgb(255, 255, 255),
            custom_glyphs: &[],
            transform_index: 0,
            base_uv_rect: [0.0, 0.0, 1.0, 1.0],
            style_index,
            style_layers_only,
            sdf_band,
            style_layers,
            style_above_mask,
            style_reach,
            vector_stroke,
        };

        let result = self.text_renderer.prepare_discard(
            device,
            queue,
            &mut self.font_system,
            &mut self.text_atlas,
            &self.viewport,
            [area],
            &mut self.swash_cache,
        );
        let keys = self.text_atlas.take_collect();
        if let Err(err) = result {
            log::warn!("vireo: 预光栅化失败（{:?}），图集可能已满", err);
        }
        let new = self.text_atlas.counters().0.saturating_sub(before) as usize;
        (new, keys)
    }

    /// 设置 shape 缓存 TTL。
    pub fn set_typeset_ttl(&mut self, ttl: Option<Duration>) {
        self.typeset_ttl = ttl;
    }

    /// 当前 shape 缓存 TTL（`None` = 不自动按时间回收）。
    pub fn typeset_ttl(&self) -> Option<Duration> {
        self.typeset_ttl
    }

    /// 设置 shape 缓存最大条数。
    pub fn set_typeset_max_entries(&mut self, max: Option<usize>) {
        self.typeset_max_entries = max;
        if let Some(cap) = max {
            loop {
                self.scavenge_dead_liveness();
                let non_held = self
                    .typeset_slots
                    .iter()
                    .filter(|s| s.liveness.is_none())
                    .count();
                if non_held <= cap {
                    break;
                }
                match self.evict_one_slot(Instant::now()) {
                    Some(victim) => self.remove_slot(victim),
                    None => break,
                }
            }
        }
    }

    fn remove_slot(&mut self, slot_i: usize) {
        let removed = self.typeset_slots.swap_remove(slot_i);
        debug_assert!(
            removed
                .liveness
                .as_ref()
                .is_none_or(|a| Arc::strong_count(a) <= 1),
            "remove_slot called on actively held slot"
        );
        self.typeset_map.remove(&removed.key);
        if let Ok(inner) = Arc::try_unwrap(removed.buffer) {
            self.recycle_buffer(inner);
        }
        if slot_i < self.typeset_slots.len() {
            let moved_key = self.typeset_slots[slot_i].key.clone();
            self.typeset_map.insert(moved_key, slot_i as u32);
        }
    }

    /// 当前 shape 缓存最大条数（`None` = 不限制）。
    pub fn typeset_max_entries(&self) -> Option<usize> {
        self.typeset_max_entries
    }

    /// 立即清空全部 shape 缓存，Buffer 尽量回池。
    pub fn clear_typeset_cache(&mut self) {
        self.scavenge_dead_liveness();
        let old_slots = std::mem::take(&mut self.typeset_slots);
        let mut new_slots: Vec<TypesetSlot> = Vec::with_capacity(old_slots.len());
        for slot in old_slots {
            if slot.liveness.is_some() {
                new_slots.push(slot);
            } else if let Ok(inner) = Arc::try_unwrap(slot.buffer) {
                self.recycle_buffer(inner);
            }
        }
        self.typeset_map.clear();
        for (new_i, slot) in new_slots.iter().enumerate() {
            self.typeset_map.insert(slot.key.clone(), new_i as u32);
        }
        self.typeset_slots = new_slots;
        self.glyph_cache.clear();
        self.frame_pinned.clear();
        self.stats = TypesetCacheStats::default();
    }

    /// 清空 `Glyphs` 的 resolved glyph 元数据缓存。
    pub fn clear_glyph_cache(&mut self) {
        self.glyph_cache.clear();
    }

    /// 当前 `Glyphs` resolved glyph 元数据缓存条目数。
    pub fn glyph_cache_len(&self) -> usize {
        self.glyph_cache.len()
    }

    fn pin_slot(&mut self, slot: u32) {
        if !self.frame_pinned.contains(&slot) {
            self.frame_pinned.push(slot);
        }
    }

    pub(crate) fn touch_slot(&mut self, slot: u32) {
        self.typeset_slots[slot as usize].last_used = Instant::now();
        self.pin_slot(slot);
    }

    pub(crate) fn begin_prepare_pins(&mut self) {
        self.frame_pinned.clear();
    }

    fn soft_cap(&self) -> usize {
        match self.typeset_max_entries {
            Some(hard) => hard.clamp(1, DEFAULT_SHAPE_SOFT_CAP),
            None => DEFAULT_SHAPE_SOFT_CAP,
        }
    }

    /// 缓存命中统计（调试/测试）。
    pub fn typeset_stats(&self) -> TypesetCacheStats {
        self.stats
    }

    pub fn reset_typeset_stats(&mut self) {
        self.stats = TypesetCacheStats::default();
    }

    /// 当前缓存条目数（调试）。
    pub fn typeset_len(&self) -> usize {
        self.typeset_slots.len()
    }

    pub(crate) fn take_buffer(&mut self, metrics: Metrics) -> Buffer {
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
        let base = self.typeset_ttl?;
        if now_len > self.soft_cap() {
            Some(base)
        } else {
            Some(base.saturating_mul(4))
        }
    }

    /// 回收「超过 TTL 未使用」的条目。
    fn gc_stale_shapes(&mut self, now: Instant) {
        let Some(ttl) = self.effective_ttl(self.typeset_slots.len()) else {
            return;
        };
        if self.typeset_slots.is_empty() {
            return;
        }
        self.scavenge_dead_liveness();
        let mut i = 0usize;
        while i < self.typeset_slots.len() {
            if self.typeset_slots[i].liveness.is_some() {
                i += 1;
                continue;
            }
            let age = now.saturating_duration_since(self.typeset_slots[i].last_used);
            if age > ttl {
                self.remove_slot(i);
            } else {
                i += 1;
            }
        }
    }

    /// 在必须腾槽时：优先过期项，否则全局最久未用。
    fn evict_one_slot(&mut self, now: Instant) -> Option<usize> {
        self.scavenge_dead_liveness();
        let pinned = |i: usize| self.frame_pinned.iter().any(|&p| p as usize == i);
        let held = |i: usize| self.typeset_slots[i].liveness.is_some();
        if let Some(ttl) = self.typeset_ttl
            && let Some(i) = self.typeset_slots.iter().enumerate().position(|(i, s)| {
                !pinned(i) && !held(i) && now.saturating_duration_since(s.last_used) > ttl
            })
        {
            return Some(i);
        }
        let mut oldest_i = None;
        let mut oldest_t = Instant::now();
        for (i, slot) in self.typeset_slots.iter().enumerate() {
            if pinned(i) || held(i) {
                continue;
            }
            if oldest_i.is_none() || slot.last_used < oldest_t {
                oldest_t = slot.last_used;
                oldest_i = Some(i);
            }
        }
        oldest_i
    }

    fn replace_slot_rc(
        &mut self,
        slot_i: usize,
        key: TypesetKey,
        buffer: Arc<Buffer>,
        line_width: f32,
        now: Instant,
    ) -> u32 {
        let old_key = self.typeset_slots[slot_i].key.clone();
        self.typeset_map.remove(&old_key);
        let old_buf = std::mem::replace(&mut self.typeset_slots[slot_i].buffer, buffer);
        if let Ok(inner) = Arc::try_unwrap(old_buf) {
            self.recycle_buffer(inner);
        }
        self.typeset_slots[slot_i].key = key.clone();
        self.typeset_slots[slot_i].line_width = line_width;
        self.typeset_slots[slot_i].last_used = now;
        let idx = slot_i as u32;
        self.typeset_map.insert(key, idx);
        idx
    }

    /// 获取或创建已 shape 的 buffer，返回 slot 下标。
    pub(crate) fn get_or_shape(&mut self, key: TypesetKey, options: &TextDef) -> u32 {
        let now = Instant::now();
        if let Some(&idx) = self.typeset_map.get(&key) {
            self.typeset_slots[idx as usize].last_used = now;
            self.stats.hits += 1;
            self.pin_slot(idx);
            return idx;
        }

        self.stats.misses += 1;
        let line_height = options.font_size * 1.2;
        let metrics = Metrics::new(options.font_size, line_height);
        let mut buffer = self.take_buffer(metrics);
        buffer.set_size(options.max_width, None);

        let attrs = options
            .attrs
            .as_ref()
            .map(|a| a.as_attrs())
            .unwrap_or_else(Attrs::new);

        buffer.set_text(
            &key.text,
            &attrs,
            Shaping::Advanced,
            Some(options.align.into()),
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let line_width = buffer
            .line_layout(&mut self.font_system, 0)
            .map(|layout| layout.iter().map(|run| run.w).sum::<f32>())
            .unwrap_or(0.0);

        let buffer = Arc::new(buffer);
        let under_cap = match self.typeset_max_entries {
            None => true,
            Some(cap) => {
                self.scavenge_dead_liveness();
                let non_held = self
                    .typeset_slots
                    .iter()
                    .filter(|s| s.liveness.is_none())
                    .count();
                non_held < cap
            }
        };
        if under_cap {
            let idx = self.typeset_slots.len() as u32;
            self.typeset_slots.push(TypesetSlot {
                key: key.clone(),
                buffer,
                line_width,
                liveness: None,
                last_used: now,
            });
            self.typeset_map.insert(key, idx);
            self.pin_slot(idx);
            return idx;
        }

        if let Some(victim) = self.evict_one_slot(now) {
            let idx = self.replace_slot_rc(victim, key, buffer, line_width, now);
            self.pin_slot(idx);
            return idx;
        }

        let idx = self.typeset_slots.len() as u32;
        self.typeset_slots.push(TypesetSlot {
            key: key.clone(),
            buffer,
            line_width,
            liveness: None,
            last_used: now,
        });
        self.typeset_map.insert(key, idx);
        self.pin_slot(idx);
        idx
    }

    pub(crate) fn get_or_shape_text(&mut self, text: &str, options: &TextDef) -> u32 {
        let mut opts = options.clone();
        opts.max_width = None;
        opts.align = TextAlign::Left;
        let key = TypesetKey::from_text(text, &opts);
        self.get_or_shape(key, &opts)
    }

    /// 扫描全槽，清理已死的 liveness 标记。
    pub(crate) fn scavenge_dead_liveness(&mut self) {
        for slot in &mut self.typeset_slots {
            if let Some(arc) = &slot.liveness
                && Arc::strong_count(arc) <= 1
            {
                slot.liveness = None;
            }
        }
    }

    /// 将槽标为 live，返回供 [`StableText`] 持有的 `Arc<()>`。
    pub(crate) fn mark_slot_live(&mut self, slot: u32) -> Arc<()> {
        let s = &mut self.typeset_slots[slot as usize];
        s.liveness.get_or_insert_with(|| Arc::new(())).clone()
    }

    /// 当前活跃 held 槽数。
    pub fn typeset_held_count(&mut self) -> usize {
        self.scavenge_dead_liveness();
        self.typeset_slots
            .iter()
            .filter(|s| s.liveness.is_some())
            .count()
    }

    /// 从文本创建 [`StableText`](super::StableText)。
    pub(crate) fn make_stable(&mut self, text: &str, options: &TextDef) -> super::StableText {
        let mut opts = options.clone();
        if opts.max_width.is_none() {
            opts.align = TextAlign::Left;
        }
        let key = TypesetKey::from_text(text, &opts);
        let slot = self.get_or_shape(key, &opts);
        let liveness = self.mark_slot_live(slot);
        let line_width = self.slot_line_width(slot);
        let buffer = self.typeset_slots[slot as usize].buffer.clone();
        let mut line_count = 0u32;
        let mut resolved_glyphs = Vec::new();
        for run in buffer.layout_runs() {
            line_count += 1;
            resolved_glyphs.extend(run.glyphs.iter().cloned().map(|glyph| {
                Arc::new(ResolvedTextGlyph {
                    glyph,
                    line_y: run.line_y,
                    line_top: run.line_top,
                    line_height: run.line_height,
                })
            }));
        }
        super::StableText {
            buffer,
            resolved_glyphs: Arc::from(resolved_glyphs),
            line_width,
            font_size: opts.font_size,
            liveness,
            text: text.to_string(),
            line_count: line_count.max(1),
        }
    }

    pub(crate) fn peek_shape_slot(&self, key: &TypesetKey) -> Option<u32> {
        self.typeset_map.get(key).copied()
    }

    pub(crate) fn slot_line_width(&self, slot: u32) -> f32 {
        let i = slot as usize;
        if i >= self.typeset_slots.len() {
            return 0.0;
        }
        self.typeset_slots[i].line_width
    }

    /// 按需 shape 单个字符并缓存完整 resolved glyph cluster。
    pub(crate) fn resolve_glyph(
        &mut self,
        ch: char,
        options: &TextDef,
    ) -> Arc<ResolvedGlyphCluster> {
        let key = GlyphKey::from_char(ch, options);
        if let Some(glyph) = self.glyph_cache.get(&key) {
            return glyph.clone();
        }
        let mut opts = options.clone();
        opts.max_width = None;
        opts.align = TextAlign::Left;
        let text = ch.to_string();
        let idx = self.get_or_shape_text(&text, &opts);
        let advance = self.slot_line_width(idx);
        let buffer = &self.typeset_slots[idx as usize].buffer;
        let mut glyphs = Vec::new();
        for run in buffer.layout_runs() {
            glyphs.extend(run.glyphs.iter().cloned().map(|glyph| {
                Arc::new(ResolvedTextGlyph {
                    glyph,
                    line_y: run.line_y,
                    line_top: run.line_top,
                    line_height: run.line_height,
                })
            }));
        }
        let resolved = Arc::new(ResolvedGlyphCluster {
            glyphs: Arc::from(glyphs),
            advance,
        });
        self.glyph_cache.insert(key, resolved.clone());
        resolved
    }

    pub(crate) fn ensure_viewport(&mut self, queue: &Queue, width: u32, height: u32, dpi: f32) {
        let next = (width, height);
        if self.last_viewport == Some(next) && self.last_dpi == Some(dpi.to_bits()) {
            return;
        }
        self.viewport
            .update(queue, crate::glyphon::Resolution { width, height }, dpi);
        self.last_viewport = Some(next);
        self.last_dpi = Some(dpi.to_bits());
    }

    /// 上传本帧的样式表 / 图层表，并按需重建 `group(1)` 的 bind group。
    ///
    /// 无样式帧：两张表都是空的（`style_count == 0`），着色器不会读它们。
    pub(crate) fn ensure_styles(
        &mut self,
        device: &Device,
        queue: &Queue,
        cache: &crate::glyphon::Cache,
        table: &crate::text::style::StyleTable,
    ) {
        let styles_bytes: &[u8] = bytemuck::cast_slice(&table.styles);
        let layers_bytes: &[u8] = bytemuck::cast_slice(&table.layers);
        let stops_bytes: &[u8] = bytemuck::cast_slice(&table.stops);
        let style_need = (styles_bytes.len().max(16) as u64).next_power_of_two();
        let layer_need = (layers_bytes.len().max(16) as u64).next_power_of_two();
        let stop_need = (stops_bytes.len().max(16) as u64).next_power_of_two();

        let needs_rebuild = match &self.style_buffers {
            None => true,
            Some(b) => {
                style_need > b.styles_cap || layer_need > b.layers_cap || stop_need > b.stops_cap
            }
        };
        if needs_rebuild {
            let styles = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glyphon text styles"),
                size: style_need,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let layers = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glyphon text layers"),
                size: layer_need,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let stops = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glyphon text stops"),
                size: stop_need,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.viewport
                .rebind_style_buffers(device, cache, styles, layers, stops);
            let b = self.style_buffers.get_or_insert_with(|| StyleBuffers {
                styles_cap: 0,
                layers_cap: 0,
                stops_cap: 0,
            });
            // `rebind_style_buffers` 把缓冲移交给 Viewport；这里只记容量，
            // 实际写入走 Viewport 暴露的缓冲句柄。
            b.styles_cap = style_need;
            b.layers_cap = layer_need;
            b.stops_cap = stop_need;
        }
        self.viewport
            .write_style_buffers(queue, styles_bytes, layers_bytes, stops_bytes);
        self.viewport.set_style_count(queue, table.style_count());
    }
}

impl TextContext {
    pub fn new(
        device: &Device,
        queue: &Queue,
        texture_format: TextureFormat,
        color_mode: ColorMode,
        transform_bgl: &wgpu::BindGroupLayout,
        default_base_texture: &wgpu::TextureView,
        default_base_sampler: &wgpu::Sampler,
        noflat: bool,
    ) -> Self {
        let mut font_system = FontSystem::new();
        font_system.db_mut().load_system_fonts();
        #[cfg(target_os = "android")]
        crate::text::android_fonts::install(&mut font_system);

        let swash_cache = SwashCache::new();
        let cache = Cache::new(device, transform_bgl, noflat);
        let mut text_atlas = TextAtlas::with_color_mode(
            device,
            queue,
            &cache,
            texture_format,
            color_mode,
            default_base_texture,
            default_base_sampler,
        );
        let text_renderer =
            TextRenderer::new(&mut text_atlas, device, MultisampleState::default(), None);
        let viewport = Viewport::new(device, &cache);

        Self {
            font_system,
            swash_cache,
            cache,
            text_atlas,
            text_renderer,
            base_texture: default_base_texture.clone(),
            base_sampler: default_base_sampler.clone(),
            viewport,
            last_viewport: None,
            last_dpi: None,
            style_buffers: None,
            sample_count: 1,
            typeset_slots: Vec::with_capacity(64),
            typeset_map: FxHashMap::default(),
            buffer_pool: Vec::with_capacity(16),
            last_gc: Instant::now(),
            typeset_ttl: Some(DEFAULT_SHAPE_TTL),
            typeset_max_entries: Some(DEFAULT_SHAPE_MAX_ENTRIES),
            glyph_cache: FxHashMap::default(),
            frame_pinned: Vec::with_capacity(32),
            stats: TypesetCacheStats::default(),
            glyph_budget: None,
            glyph_budget_warnings: 0,
            glyph_trims: 0,
            glyph_evicted: 0,
            glyph_peak_bytes: 0,
        }
    }
}
