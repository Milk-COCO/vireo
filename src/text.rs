//! 文本渲染：glyphon + cosmic-text，使用系统字体。
//!
//! `prepare_texts` 对「内容相同」的条目复用已 shape 的 `Buffer`（跳过 harfrust），
//! 位置/颜色/transform 不参与缓存键。
//!
//! 缓存策略（均可配置，经 `GpuContext`）：
//! - TTL：`set_shape_cache_ttl(Some(d) | None)`，`None` = 不按时间回收
//! - 条数：`set_shape_cache_max_entries(Some(n) | None)`，`None` = 不限制
//! - 立即清空：`clear_shape_cache`

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;

use crate::glyphon::{
    Buffer, Cache, FontSystem, Metrics, Shaping, SwashCache, TextArea, TextAtlas,
    TextBounds, TextRenderer, Viewport,
};
pub use crate::glyphon::Attrs;
pub use crate::glyphon::ColorMode;
pub use cosmic_text::{AttrsOwned, Family, FamilyOwned, FeatureTag, Style, Weight};
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
    fn from_text(text: &str, options: &TextOptions) -> Self {
        let max_width_bits = options
            .max_width
            .map(|w| w.to_bits())
            .unwrap_or(u32::MAX);
        Self {
            text: text.to_string(),
            font_size_bits: options.font_size.to_bits(),
            max_width_bits,
            align: options.align as u8,
            attrs: options.attrs.clone(),
        }
    }

    fn from_entry(entry: &TextEntry) -> Self {
        Self::from_text(&entry.text, &entry.options)
    }
}

pub(crate) struct ShapeCacheSlot {
    key: ShapeKey,
    buffer: Arc<Buffer>,
    line_width: f32,
    /// [`StableText`] 活跃标记。
    /// - `None`：未被 StableText 使用，可正常淘汰。
    /// - `Some(arc)`：曾/正被 StableText 使用。`Arc::strong_count > 1` 表示还有活的 StableText；
    ///   等于 1 表示所有 StableText 均已 drop（死标记），下次扫描会清掉。
    liveness: Option<Arc<()>>,
    last_used: Instant,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct ShapeCacheStats {
    pub hits: u64,
    pub misses: u64,
    /// 自动 GC 调用次数（TTL 扫描）
    pub gc_runs: u64,
    /// 最近一次 GC 耗时（微秒）
    pub last_gc_us: u64,
    /// 累计 GC 耗时（微秒）
    pub total_gc_us: u64,
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
    pub(crate) shape_slots: Vec<ShapeCacheSlot>,
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
    /// HUD 数字表是否已为当前字号/attrs 预热（不存 slot 下标，避免 LRU 后失效）
    digit_table_ready: bool,
    digit_step: f32,
    digit_metrics_bits: u32,
    digit_attrs: Option<AttrsOwned>,
    /// 本帧 prepare 中引用的 slot，禁止淘汰
    frame_pinned: Vec<u32>,
    stats: ShapeCacheStats,
}

/// 文字管线与 render pass DS attachment 的匹配方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextStencilMode {
    /// pass 无 DS attachment
    None,
    /// pass 有 DS，文字不测模板（Always；用于 UI / unclipped）
    Pass,
    /// pass 有 DS，Equal+Keep 测模板（裁切区内文字）
    Test,
}

/// Equal+Keep：只画在当前 stencil ref 内，不写 stencil。
pub(crate) fn stencil_text_ds_test() -> Option<wgpu::DepthStencilState> {
    Some(wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState {
            front: wgpu::StencilFaceState {
                compare: wgpu::CompareFunction::Equal,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op: wgpu::StencilOperation::Keep,
            },
            back: wgpu::StencilFaceState {
                compare: wgpu::CompareFunction::Equal,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op: wgpu::StencilOperation::Keep,
            },
            read_mask: 0xff,
            write_mask: 0x00,
        },
        bias: wgpu::DepthBiasState::default(),
    })
}

/// Always：有 DS attachment 时透传（不测不写），避免 UI/unclipped 被 Equal(0) 误裁。
pub(crate) fn stencil_text_ds_pass() -> Option<wgpu::DepthStencilState> {
    Some(wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState {
            front: wgpu::StencilFaceState::IGNORE,
            back: wgpu::StencilFaceState::IGNORE,
            read_mask: 0,
            write_mask: 0,
        },
        bias: wgpu::DepthBiasState::default(),
    })
}

impl TextContext {
    /// 确保 TextRenderer 匹配给定 sample_count（默认无 DS；随后由 `ensure_text_ds` 切换）。
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

    /// 设置文字管线 DS 模式（与当前 render pass / 是否测裁切一致）。
    pub(crate) fn ensure_text_stencil_mode(&mut self, device: &Device, mode: TextStencilMode) {
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
        );
        self.text_renderer.set_pipeline(pipeline);
    }

    /// 按帧粗选：无 DS 或默认 Test（细粒度用 [`ensure_text_stencil_mode`]）。
    pub fn ensure_text_ds(&mut self, device: &Device, use_stencil: bool) {
        self.ensure_text_stencil_mode(
            device,
            if use_stencil {
                TextStencilMode::Test
            } else {
                TextStencilMode::None
            },
        );
    }

    /// 兼容旧名：强制文字管线带 stencil Test。
    pub fn ensure_text_stencil(&mut self, device: &Device) {
        self.ensure_text_stencil_mode(device, TextStencilMode::Test);
    }

    /// 预热文字管线：强制 swash cache / atlas / 上传 lazy 初始化。
    /// 用单字符 "A" 跑一次 prepare，触发首帧 33ms 的 text shape 成本。
    /// 调用前 `ensure_sample_count` 必须已跑过。
    pub fn preheat(&mut self, device: &Device, queue: &Queue, physical_width: u32, physical_height: u32) {
        // 单字符 "A"：触发 cosmic_text shaping + swash 光栅化 + atlas 上传
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
            }],
            &mut self.swash_cache,
        );
    }

    /// 每帧绘制前调用一次（多 batch 共享）。
    /// TTL/GC 用真实时间，与 FPS 无关；`shape_ttl == None` 时不做自动回收。
    pub fn advance_frame(&mut self) {
        if self.shape_ttl.is_none() {
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

    /// 设置 shape 缓存最大条数（仅约束**非 held** 槽数；活跃 stable 槽不被该 cap 回收）。
    /// - `Some(n)`：最多 n 条非 held ShapeKey；满则 LRU 换槽
    /// - `None`：不限制条数（仅 TTL / `clear_shape_cache` 可缩小）
    pub fn set_shape_cache_max_entries(&mut self, max: Option<usize>) {
        self.shape_max_entries = max;
        if let Some(cap) = max {
            loop {
                self.scavenge_dead_liveness();
                let non_held = self
                    .shape_slots
                    .iter()
                    .filter(|s| s.liveness.is_none())
                    .count();
                if non_held <= cap {
                    break;
                }
                match self.evict_one_slot(Instant::now()) {
                    Some(victim) => self.remove_slot(victim),
                    None => break, // 全是 held，无法继续 shrink
                }
            }
        }
    }

    fn remove_slot(&mut self, slot_i: usize) {
        let removed = self.shape_slots.swap_remove(slot_i);
        debug_assert!(
            removed.liveness.as_ref().map_or(true, |a| Arc::strong_count(a) <= 1),
            "remove_slot called on actively held slot"
        );
        self.shape_map.remove(&removed.key);
        if let Ok(inner) = Arc::try_unwrap(removed.buffer) {
            self.recycle_buffer(inner);
        }
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
    /// 跳过 [`StableText`] 仍在活跃持有的槽，不回收它们的 Buffer。
    pub fn clear_shape_cache(&mut self) {
        self.scavenge_dead_liveness();
        // 重建：只保留活跃持有的槽（其余回池 + 从 map 移除）
        let old_slots = std::mem::take(&mut self.shape_slots);
        let mut new_slots: Vec<ShapeCacheSlot> = Vec::with_capacity(old_slots.len());
        for slot in old_slots {
            if slot.liveness.is_some() {
                // 活跃 held（scavenge 后 liveness.is_some() = 真 held）
                new_slots.push(slot);
            } else if let Ok(inner) = Arc::try_unwrap(slot.buffer) {
                self.recycle_buffer(inner);
            }
        }
        // 重建 map 下标（被保留槽的下标在新 vec 中已变）
        self.shape_map.clear();
        for (new_i, slot) in new_slots.iter().enumerate() {
            self.shape_map.insert(slot.key.clone(), new_i as u32);
        }
        self.shape_slots = new_slots;
        self.digit_table_ready = false;
        self.digit_step = 0.0;
        self.frame_pinned.clear();
        self.stats = ShapeCacheStats::default();
    }

    fn pin_slot(&mut self, slot: u32) {
        if !self.frame_pinned.contains(&slot) {
            self.frame_pinned.push(slot);
        }
    }

    fn begin_prepare_pins(&mut self) {
        self.frame_pinned.clear();
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
    /// 跳过 [`StableText`] 活跃持有的槽。
    fn gc_stale_shapes(&mut self, now: Instant) {
        let Some(ttl) = self.effective_ttl(self.shape_slots.len()) else {
            return;
        };
        if self.shape_slots.is_empty() {
            return;
        }
        self.scavenge_dead_liveness();
        let mut i = 0usize;
        while i < self.shape_slots.len() {
            if self.shape_slots[i].liveness.is_some() {
                i += 1;
                continue;
            }
            let age = now.saturating_duration_since(self.shape_slots[i].last_used);
            if age > ttl {
                self.remove_slot(i);
            } else {
                i += 1;
            }
        }
    }

    /// 在必须腾槽时：优先过期项（若启用 TTL），否则全局最久未用。
    /// 跳过本帧 pin 的槽与 [`StableText`] 活跃持有的槽。
    /// 返回 `None` 表示所有可用槽都被持有或 pin —— 调用方应放弃替换。
    fn evict_one_slot(&mut self, now: Instant) -> Option<usize> {
        self.scavenge_dead_liveness();
        let pinned = |i: usize| self.frame_pinned.iter().any(|&p| p as usize == i);
        let held = |i: usize| self.shape_slots[i].liveness.is_some();
        if let Some(ttl) = self.shape_ttl {
            if let Some(i) = self.shape_slots.iter().enumerate().position(|(i, s)| {
                !pinned(i) && !held(i) && now.saturating_duration_since(s.last_used) > ttl
            }) {
                return Some(i);
            }
        }
        let mut oldest_i = None;
        let mut oldest_t = Instant::now();
        for (i, slot) in self.shape_slots.iter().enumerate() {
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

    fn replace_slot_rc(&mut self, slot_i: usize, key: ShapeKey, buffer: Arc<Buffer>, line_width: f32, now: Instant) -> u32 {
        let old_key = self.shape_slots[slot_i].key.clone();
        self.shape_map.remove(&old_key);
        let old_buf = std::mem::replace(&mut self.shape_slots[slot_i].buffer, buffer);
        if let Ok(inner) = Arc::try_unwrap(old_buf) {
            self.recycle_buffer(inner);
        }
        self.shape_slots[slot_i].key = key.clone();
        self.shape_slots[slot_i].line_width = line_width;
        self.shape_slots[slot_i].last_used = now;
        let idx = slot_i as u32;
        self.shape_map.insert(key, idx);
        idx
    }

    /// 获取或创建已 shape 的 buffer，返回 slot 下标。
    fn get_or_shape(&mut self, key: ShapeKey, options: &TextOptions) -> u32 {
        let now = Instant::now();
        if let Some(&idx) = self.shape_map.get(&key) {
            self.shape_slots[idx as usize].last_used = now;
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

        // 增长：未满则 push。prepare 进行中禁止 GC/swap_remove（会弄乱已 pin 的下标）。
        // 淘汰仅 replace 原位，不 swap_remove。
        let buffer = Arc::new(buffer);
        // max_entries 只约束非 held 槽（held 槽不受限）
        let under_cap = match self.shape_max_entries {
            None => true,
            Some(cap) => {
                self.scavenge_dead_liveness();
                let non_held = self
                    .shape_slots
                    .iter()
                    .filter(|s| s.liveness.is_none())
                    .count();
                non_held < cap
            }
        };
        if under_cap {
            let idx = self.shape_slots.len() as u32;
            self.shape_slots.push(ShapeCacheSlot {
                key: key.clone(),
                buffer,
                line_width,
                liveness: None,
                last_used: now,
            });
            self.shape_map.insert(key, idx);
            self.pin_slot(idx);
            return idx;
        }

        // 非 held 已满：原位替换最久未用的非 held 槽（不 swap_remove）
        if let Some(victim) = self.evict_one_slot(now) {
            let idx = self.replace_slot_rc(victim, key, buffer, line_width, now);
            self.pin_slot(idx);
            return idx;
        }

        // 所有非 held 槽都被 pin（本帧 prepare 中），无法淘汰；
        // 仍允许 push 新槽（突破 cap），下帧 unpin 后自动回归。
        let idx = self.shape_slots.len() as u32;
        self.shape_slots.push(ShapeCacheSlot {
            key: key.clone(),
            buffer,
            line_width,
            liveness: None,
            last_used: now,
        });
        self.shape_map.insert(key, idx);
        self.pin_slot(idx);
        idx
    }

    fn get_or_shape_text(&mut self, text: &str, options: &TextOptions) -> u32 {
        // 段 shape 不使用 max_width（横拼由调用方控制）
        let mut opts = options.clone();
        opts.max_width = None;
        opts.align = TextAlign::Left;
        let key = ShapeKey::from_text(text, &opts);
        self.get_or_shape(key, &opts)
    }

    /// 扫描全槽，清理已死的 liveness 标记（`strong_count == 1` = 所有 StableText 均已 drop）。
    /// 在 GC/evict/clear/cap-check 前调用。
    fn scavenge_dead_liveness(&mut self) {
        for slot in &mut self.shape_slots {
            if let Some(arc) = &slot.liveness {
                if Arc::strong_count(arc) <= 1 {
                    slot.liveness = None;
                }
            }
        }
    }

    /// 将槽标为 live，返回供 [`StableText`] 持有的 `Arc<()>`。
    /// 惰性创建 liveness 标记（0→1 幂等）。
    fn mark_slot_live(&mut self, slot: u32) -> Arc<()> {
        let s = &mut self.shape_slots[slot as usize];
        s.liveness
            .get_or_insert_with(|| Arc::new(()))
            .clone()
    }

    /// 当前活跃 held 槽数（有至少一个 [`StableText`] 正在持有）。
    /// O(n) 扫描（n = shape_slots.len()）。
    pub fn shape_cache_held_count(&mut self) -> usize {
        self.scavenge_dead_liveness();
        self.shape_slots.iter().filter(|s| s.liveness.is_some()).count()
    }

    /// 从文本创建 [`StableText`]（走统一 cache 通路，与 `draw_text` 共享缓存）。
    ///
    /// 支持 `max_width` 与 `align`（见 [`StableText`] 文档）。
    /// 当 `max_width: None` 时行为同旧版：单行左对齐。
    ///
    /// 当非 held 槽已达 `shape_max_entries` 且无法淘汰时，仍会 push 新槽（突破 cap）；
    /// 此时 `shape_cache_len()` 可能大于 max_entries。
    pub fn make_stable(&mut self, text: &str, options: &TextOptions) -> StableText {
        // 直接走统一 get_or_shape（绕过 get_or_shape_text 的强制 None/Left）
        let mut opts = options.clone();
        if opts.max_width.is_none() {
            opts.align = TextAlign::Left; // 无 max_width 时仍强制左对齐（兼容旧版）
        }
        let key = ShapeKey::from_text(text, &opts);
        let slot = self.get_or_shape(key, &opts);
        let liveness = self.mark_slot_live(slot);
        let line_width = self.slot_line_width(slot);
        let buffer = self.shape_slots[slot as usize].buffer.clone();
        StableText { buffer, line_width, liveness, text: text.to_string() }
    }

    /// 查 shape cache 但不插入（用于 Dynamic 只读 hit）。
    fn peek_shape_slot(&self, key: &ShapeKey) -> Option<u32> {
        self.shape_map.get(key).copied()
    }

    /// 已 shape buffer 首行宽度（逻辑像素）。
    fn slot_line_width(&self, slot: u32) -> f32 {
        let i = slot as usize;
        if i >= self.shape_slots.len() {
            return 0.0;
        }
        self.shape_slots[i].line_width
    }

    /// 确保 0-9 + 常用数学符号已 shape；digit_step 取 0-9 最大宽（tabular）。
    fn ensure_digit_table(&mut self, options: &TextOptions) -> f32 {
        let bits = options.font_size.to_bits();
        let attrs = options.attrs.clone();
        let need = !self.digit_table_ready
            || self.digit_metrics_bits != bits
            || self.digit_attrs != attrs;
        if !need {
            // 仍 pin 住表项，防止本帧后续淘汰
            let mut opts = options.clone();
            opts.max_width = None;
            opts.align = TextAlign::Left;
            Self::add_tnum(&mut opts);
            for ch in HUD_DIGIT_TABLE.chars() {
                let s = ch.to_string();
                let _ = self.get_or_shape_text(&s, &opts);
            }
            return self.digit_step;
        }
        let mut opts = options.clone();
        opts.max_width = None;
        opts.align = TextAlign::Left;
        Self::add_tnum(&mut opts);
        let mut max_w = 0.0f32;
        for ch in HUD_DIGIT_TABLE.chars() {
            let s = ch.to_string();
            let idx = self.get_or_shape_text(&s, &opts);
            if ch.is_ascii_digit() {
                max_w = max_w.max(self.slot_line_width(idx));
            }
        }
        self.digit_table_ready = true;
        self.digit_step = max_w;
        self.digit_metrics_bits = bits;
        self.digit_attrs = attrs;
        max_w
    }

    fn digit_slot(&mut self, d: u32, options: &TextOptions) -> u32 {
        debug_assert!(d < 10);
        let s = ((b'0' + d as u8) as char).to_string();
        let mut opts = options.clone();
        opts.max_width = None;
        opts.align = TextAlign::Left;
        Self::add_tnum(&mut opts);
        self.get_or_shape_text(&s, &opts)
    }

    /// 在 opts 的 attrs 上加 `tnum` OpenType feature（字体级等宽数字）。
    /// 仅影响支持 tnum 的字体；不支持的字体会忽略此 feature。
    fn add_tnum(opts: &mut TextOptions) {
        let attrs = opts.attrs.get_or_insert_with(|| AttrsOwned::new(&Attrs::new()));
        attrs.font_features.enable(FeatureTag::new(b"tnum"));
    }

    fn glyph_slot_char(&mut self, ch: char, options: &TextOptions) -> u32 {
        let s = ch.to_string();
        let mut opts = options.clone();
        opts.max_width = None;
        opts.align = TextAlign::Left;
        self.get_or_shape_text(&s, &opts)
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
            digit_table_ready: false,
            digit_step: 0.0,
            digit_metrics_bits: 0,
            digit_attrs: None,
            frame_pinned: Vec::with_capacity(32),
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

/// 文本定义（WHAT）——决定文字"长什么样"，在绘制前定型。
/// 与位置（`Pos`）、覆盖（`TextOverride`）区分。
/// `font_size` 为必填，其余可选。
#[derive(Clone, Debug)]
pub struct TextDef {
    pub font_size: f32,
    pub max_width: Option<f32>,
    pub align: TextAlign,
    pub attrs: Option<AttrsOwned>,
}

impl TextDef {
    pub fn new(font_size: f32) -> Self {
        Self {
            font_size,
            max_width: None,
            align: TextAlign::Left,
            attrs: None,
        }
    }

    pub fn max_width(mut self, w: f32) -> Self {
        self.max_width = Some(w);
        self
    }

    pub fn align(mut self, a: TextAlign) -> Self {
        self.align = a;
        self
    }

    pub fn attrs(mut self, a: AttrsOwned) -> Self {
        self.attrs = Some(a);
        self
    }
}

/// 文本绘制覆盖（OVERRIDE）——覆盖 batch 文本状态机。
/// 与 `ShapeOverride` 语义对称。`None` = 保持 batch 状态。
#[derive(Clone, Debug, Default)]
pub struct TextOverride {
    /// `Some` = 覆盖 batch.text_color；`None` = 保持
    pub color: Option<Color>,
    /// `None` = 保持 batch.text_clip；`Some(None)` = 清除裁剪；`Some(Some(b))` = 设置
    pub clip: Option<Option<crate::glyphon::TextBounds>>,
}

impl TextOverride {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn color(mut self, c: Color) -> Self {
        self.color = Some(c);
        self
    }

    pub fn clip(mut self, l: i32, t: i32, r: i32, b: i32) -> Self {
        self.clip = Some(Some(crate::glyphon::TextBounds { left: l, top: t, right: r, bottom: b }));
        self
    }

    pub fn clear_clip(mut self) -> Self {
        self.clip = Some(None);
        self
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

/// Digits 预 shape 表：`0-9` + 常用数学/HUD 符号（单字符各一条 ShapeKey）。
/// 数字步进宽取 0-9 最大宽；符号用自身 glyph 宽。
const HUD_DIGIT_TABLE: &str = "0123456789.,+-*/%=:()[]{}±×÷°−eE";

#[inline]
fn is_hud_digit_char(ch: char) -> bool {
    ch.is_ascii_digit()
        || matches!(
            ch,
            '.' | ',' | '+' | '-' | '*' | '/' | '%' | '=' | ':' | '(' | ')' | '[' | ']' | '{'
                | '}' | '±' | '×' | '÷' | '°' | '−' | 'e' | 'E'
        )
}

/// 已 shape 的文字句柄，跨帧复用，与 `draw_text` 共享同一 cache。
///
/// **线程安全**：`StableText: Send + Sync`（内部仅 `Arc` + 字符串 + `f32`），
/// 可跨线程传递（但 [`GpuContext`] 本身不是 `Send`，创建与使用应在同一线程）。
/// ## 生命周期
/// - **Buffer 内存**：由 `StableText` 内的 `Arc<Buffer>` 保活；`drop` 后回收。
/// - **cache 槽**：[`GpuContext::make_stable_text`] 把对应槽标为「live」(`liveness: Arc<()>`)，
///   **直至所有 `StableText` clone 均 drop** 才会变为可淘汰。
///   同一文案的多次 `make_stable_text` 共享同一 liveness 标记（0→1 创建一次）。
///   GC/evict/clear 使用 `Arc::strong_count` 探测 liveness 标记是否还活着：
///   `strong_count > 1` = 仍有 `StableText` 持有；`== 1` = 已死（`slot` 自身唯一持有），可淘汰。
/// - **`Clone`**：复制 `Arc<Buffer>` 与 `Arc<()>`（liveness 标记），增加 `strong_count`。
///   所有 clone 共享同一 cache 槽，只有**所有** clone 均 drop 后槽才变为可淘汰。
///
/// ## 与 [`TextPart::Static`]/[`TextPart::Dynamic`] 的差异
/// | 维度 | `Static`/`Dynamic` | `Stable` |
/// |------|-------------------|----------|
/// | 缓存条目 | `draw_text` 自动管 | 用户 `make_stable_text` 显式创建 |
/// | 跨帧复用 | TTL/LRU 可能 evict | **所有 handle 均 drop 前**永不 evict |
///
/// ## `max_width` / `align` 支持
/// - 当 `max_width: None`（默认）：**单行左对齐**，同旧版行为。
/// - 当 `max_width: Some(w)`：文本在 w 逻辑像素处换行，**`align` 生效**（`Left` / `Center` / `Right`）。
///
/// ## 限制
/// - **不支持 `Digits` 切分**：整段是单 buffer，渲染时不走 `tnum` 加速。
///   需要等宽数字请用 [`TextPart::Digits`] 或 [`crate::text::HudLine`]。
///
/// ## 绘制 API
/// - [`DrawBatch::text_stable`]：每帧传 `x/y/color`；其余已在 `make_stable_text` 时定型。
/// - [`TextEntryList::push_stable`]：添加到 `TextEntryList`（与 `text_parts` 配合使用）。
///
/// ```ignore
/// let h = gpu.make_stable_text("Score: {}", TextOptions::default().font_size(20.0));
/// batch.text_stable(&h, 16.0, 16.0, WHITE);
/// ```
#[derive(Clone)]
pub struct StableText {
    pub(crate) buffer: Arc<Buffer>,
    pub(crate) line_width: f32,
    pub(crate) liveness: Arc<()>,
    /// 原文案（创建时传入的字符串），用于调试和用户侧去重。
    pub(crate) text: String,
}

impl StableText {
    /// 原文案（`make_stable_text` 时传入的字符串）。
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Debug for StableText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StableText")
            .field("text", &self.text)
            .field("line_width", &self.line_width)
            .field("buffer_strong_count", &Arc::strong_count(&self.buffer))
            .field("live_handle_count", &Arc::strong_count(&self.liveness))
            .finish()
    }
}

/// HUD 文本段（单行 LTR；不保证与整段 `draw_text` 像素级一致）。
///
/// 四种类型的差异：
/// - [`TextPart::Static`]：内容稳定，走整段 shape 缓存，同内容可命中。
/// - [`TextPart::Dynamic`]：内容会变但仍需整段 shape，适合不能拆成 Digits 的短句。
/// - [`TextPart::Digits`]：HUD 数字专用加速路径。**强制 `tnum`（等宽数字）**，
///   每个字宽度 = 0-9 最大宽。代码内自带大字表，无需整段 reshape。
///   若要比例数字，用 [`TextPart::Dynamic`] 或 [`TextPart::Static`]。
/// - [`TextPart::Stable`]：预 shape 句柄，不走 cache 查询。详见 [`StableText`]。
#[derive(Clone, Debug)]
pub enum TextPart<'a> {
    /// 内容稳定（标签、说明）。走整段 shape 缓存，同内容可 hit。
    Static(&'a str),
    /// 内容会变的任意文案。仍走整段 shape；字符串一变就 miss。
    /// 适合低频改动的短句，或无法用 Digits 的动态字。
    Dynamic(&'a str),
    /// 强制 `tnum` 等宽的 HUD 数字/符号。单槽预 shape，不整段 reshape。
    /// 仅接受 `0-9` + 数学符号（.,+-*/%=:()[]等）；其它字符跳过。
    /// 需要比例数字时请用 [`TextPart::Dynamic`] 或 [`TextPart::Static`]。
    Digits(&'a str),
    /// 预 shape 稳定文本，直接使用 [`StableText`] 的 `Arc<Buffer>`，不走 cache 查询。
    /// 可在多帧间复用而无需重新 shape。
    ///
    /// **与外层 `TextOptions` 的关系**（`draw_text_parts` / `push_parts`）：
    /// - **生效**：`x`/`y`（行起点；段内用 `StableText.line_width` 横拼）、`color`、`clip`、`transform`
    /// - **不生效**（已在 `make_stable_text` 时定型）：`font_size`、`attrs`
    /// - **`max_width`/`align`**：已在 `make_stable_text` 时定型（见 [`StableText`] 文档），段内不重新生效
    Stable(&'a StableText),
}

/// 拥有所有权的 HUD 段（`split_hud` / [`HudLine`]）。
///
/// 主轴 Static/Dynamic；[`HudPart::Digits`] 为数字优化。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudPart {
    Static(String),
    Dynamic(String),
    Digits(String),
}

/// Bevy 式组织：一条 HUD 行 = 若干 span，跨帧复用，只改 Dynamic/Digits 槽。
///
/// ```ignore
/// let mut line = HudLine::new()
///     .text("分数: ")
///     .digits("0")
///     .text("  模式: ")
///     .dynamic("Both");
/// // 每帧
/// line.set_digits(1, &score.to_string());
/// line.draw(&mut batch.texts, opts);
/// ```
#[derive(Clone, Debug, Default)]
pub struct HudLine {
    spans: Vec<HudPart>,
}

impl HudLine {
    pub fn new() -> Self {
        Self { spans: Vec::new() }
    }

    pub fn text(mut self, s: impl Into<String>) -> Self {
        self.spans.push(HudPart::Static(s.into()));
        self
    }

    pub fn dynamic(mut self, s: impl Into<String>) -> Self {
        self.spans.push(HudPart::Dynamic(s.into()));
        self
    }

    /// 数字优化槽（见 [`TextPart::Digits`]）。
    pub fn digits(mut self, s: impl Into<String>) -> Self {
        self.spans.push(HudPart::Digits(s.into()));
        self
    }

    pub fn spans(&self) -> &[HudPart] {
        &self.spans
    }

    pub fn set_text(&mut self, index: usize, s: impl Into<String>) {
        self.spans[index] = HudPart::Static(s.into());
    }

    pub fn set_dynamic(&mut self, index: usize, s: impl Into<String>) {
        self.spans[index] = HudPart::Dynamic(s.into());
    }

    pub fn set_digits(&mut self, index: usize, s: impl Into<String>) {
        self.spans[index] = HudPart::Digits(s.into());
    }

    /// 原地改 Dynamic/Digits 槽的字符串（不清空类型）。
    pub fn write_slot(&mut self, index: usize, s: &str) {
        match &mut self.spans[index] {
            HudPart::Static(buf) | HudPart::Dynamic(buf) | HudPart::Digits(buf) => {
                buf.clear();
                buf.push_str(s);
            }
        }
    }

    pub fn draw(&self, list: &mut TextEntryList, options: TextOptions) {
        list.push_hud_parts(&self.spans, options);
    }

    pub(crate) fn draw_indexed(
        &self,
        list: &mut TextEntryList,
        options: TextOptions,
        transform_index: u32,
    ) {
        list.push_hud_parts_indexed(&self.spans, options, transform_index);
    }
}

/// 将 HUD 字符串切成 Static / Digits 段（启发式；Dynamic 需手写或 [`HudLine`]）。
///
/// 规则：
/// - 连续 `0-9` 与常用数学符号 → [`HudPart::Digits`]（优化）
/// - 空格：已在 Digits 段内则并入；否则归 Static
/// - 其余 → [`HudPart::Static`]（假定标签稳定）
///
/// 例：`"分数: 42"` → `Static("分数")` + `Digits(": 42")`。
///
/// 空串返回空 `Vec`。不保证与整段 `draw_text` 像素级一致。
pub fn split_hud(s: &str) -> Vec<HudPart> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<HudPart> = Vec::new();
    let mut cur = String::new();
    let mut cur_digits: Option<bool> = None;

    let flush = |out: &mut Vec<HudPart>, cur: &mut String, cur_digits: &mut Option<bool>| {
        if cur.is_empty() {
            *cur_digits = None;
            return;
        }
        let part = match *cur_digits {
            Some(true) => HudPart::Digits(std::mem::take(cur)),
            _ => HudPart::Static(std::mem::take(cur)),
        };
        *cur_digits = None;
        out.push(part);
    };

    for ch in s.chars() {
        let is_d = if is_hud_digit_char(ch) {
            true
        } else if ch == ' ' {
            matches!(cur_digits, Some(true))
        } else {
            false
        };
        match cur_digits {
            Some(d) if d == is_d => cur.push(ch),
            Some(_) => {
                flush(&mut out, &mut cur, &mut cur_digits);
                cur_digits = Some(is_d);
                cur.push(ch);
            }
            None => {
                cur_digits = Some(is_d);
                cur.push(ch);
            }
        }
    }
    flush(&mut out, &mut cur, &mut cur_digits);
    out
}

#[derive(Clone, Debug)]
pub(crate) enum OwnedTextPart {
    Static(String),
    Dynamic(String),
    Digits(String),
    /// (buffer, cached_line_width)
    Stable(Arc<Buffer>, f32),
}

impl OwnedTextPart {
    fn from_part(p: &TextPart<'_>) -> Self {
        match p {
            TextPart::Static(s) => Self::Static((*s).to_string()),
            TextPart::Dynamic(s) => Self::Dynamic((*s).to_string()),
            TextPart::Digits(s) => Self::Digits((*s).to_string()),
            TextPart::Stable(h) => Self::Stable(h.buffer.clone(), h.line_width),
        }
    }

    fn from_hud(p: HudPart) -> Self {
        match p {
            HudPart::Static(s) => Self::Static(s),
            HudPart::Dynamic(s) => Self::Dynamic(s),
            HudPart::Digits(s) => Self::Digits(s),
        }
    }
}

/// 文本条目，pushed by draw_text / draw_text_parts / text_handle
#[derive(Clone)]
pub struct TextEntry {
    pub text: String,
    pub options: TextOptions,
    /// Batch-local transform index（逻辑空间）。
    /// `draw_text` 默认 0：在全局表中约定为**单位矩阵**（见 `Renderer::draw` 预留槽 0），
    /// 不是「首个 batch 矩阵」。有 batch transform 时请用 `batch.text()` / `push_indexed`。
    pub(crate) transform_index: u32,
    /// `Some` = HUD 多段（忽略 `text`）；`None` = 普通整段 `text`
    pub(crate) parts: Option<Vec<OwnedTextPart>>,
    /// `Some` = StableText 直接绘制（忽略 `text` 和 `parts`）。
    pub(crate) stable: Option<Arc<Buffer>>,
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
            parts: None,
            stable: None,
        });
    }

    /// 添加文本条目并指定 transform index。
    pub(crate) fn push_indexed(&mut self, text: &str, options: TextOptions, transform_index: u32) {
        self.entries.push(TextEntry {
            text: text.to_string(),
            options,
            transform_index,
            parts: None,
            stable: None,
        });
    }

    /// 使用预 shape 的 [`StableText`] 直接添加条目（跳过 cache 查询）。
    pub fn push_stable(&mut self, stable: &StableText, options: TextOptions) {
        self.push_stable_indexed(stable, options, 0);
    }

    pub(crate) fn push_stable_indexed(
        &mut self,
        stable: &StableText,
        options: TextOptions,
        transform_index: u32,
    ) {
        self.entries.push(TextEntry {
            text: String::new(),
            options,
            transform_index,
            parts: None,
            stable: Some(stable.buffer.clone()),
        });
    }

    /// HUD 多段文字（默认无 transform）。
    pub fn push_parts(&mut self, parts: &[TextPart<'_>], options: TextOptions) {
        self.push_parts_indexed(parts, options, 0);
    }

    pub(crate) fn push_parts_indexed(
        &mut self,
        parts: &[TextPart<'_>],
        options: TextOptions,
        transform_index: u32,
    ) {
        let owned: Vec<OwnedTextPart> = parts.iter().map(OwnedTextPart::from_part).collect();
        self.entries.push(TextEntry {
            text: String::new(),
            options,
            transform_index,
            parts: Some(owned),
            stable: None,
        });
    }

    /// 拥有权 span 列表（[`HudLine`] / [`split_hud`]）。
    pub fn push_hud_parts(&mut self, parts: &[HudPart], options: TextOptions) {
        self.push_hud_parts_indexed(parts, options, 0);
    }

    pub(crate) fn push_hud_parts_indexed(
        &mut self,
        parts: &[HudPart],
        options: TextOptions,
        transform_index: u32,
    ) {
        if parts.is_empty() {
            return;
        }
        let owned: Vec<OwnedTextPart> = parts.iter().cloned().map(OwnedTextPart::from_hud).collect();
        self.entries.push(TextEntry {
            text: String::new(),
            options,
            transform_index,
            parts: Some(owned),
            stable: None,
        });
    }

    /// 按 [`split_hud`] 规则自动切分后加入（默认无 transform）。
    pub fn push_hud(&mut self, text: &str, options: TextOptions) {
        self.push_hud_indexed(text, options, 0);
    }

    pub(crate) fn push_hud_indexed(
        &mut self,
        text: &str,
        options: TextOptions,
        transform_index: u32,
    ) {
        let owned: Vec<OwnedTextPart> = split_hud(text).into_iter().map(OwnedTextPart::from_hud).collect();
        if owned.is_empty() {
            return;
        }
        self.entries.push(TextEntry {
            text: String::new(),
            options,
            transform_index,
            parts: Some(owned),
            stable: None,
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
        text_ctx.begin_prepare_pins();

        text_ctx.viewport.update(
            &gpu.queue,
            crate::glyphon::Resolution {
                width: physical_width,
                height: physical_height,
            },
        );

        /// 文本区域元数据（entry 遍历阶段收集，第二次循环消费）。
        /// `buf` 用 enum 携带 buffer 来源：
        /// - `Slot(u32)`：cache 槽索引（prepare 期间 `text_ctx` 持锁，不会被 evict）
        /// - `Stable(Arc<Buffer>)`：StableText 持有，不在 cache 中；
        ///   由 `metas` 持有 Arc 引用，延寿到第二次循环消费完。
        struct AreaMeta {
            buf: MetaBuf,
            left: f32,
            top: f32,
            color: crate::glyphon::Color,
            bounds: TextBounds,
            transform_index: u32,
        }

        enum MetaBuf {
            Slot(u32),
            Stable(Arc<Buffer>),
        }

        fn phys_transform_index(
            transform_index: u32,
            transform_table: &[f32],
            global_transforms: &mut Vec<f32>,
            scale: f32,
        ) -> u32 {
            let base = transform_index as usize * 12;
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
        }

        let mut metas: Vec<AreaMeta> = Vec::with_capacity(self.entries.len() * 2);
        // 注：StableText 的 Arc<Buffer> 直接放 metas.buf.Stable 里，延寿到第二次循环消费完。
        // 不再需要平行 stable_bufs vec + hi 计数器。

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
            let phys_idx = phys_transform_index(
                entry.transform_index,
                transform_table,
                global_transforms,
                scale,
            );
            let top = entry.options.y * scale;

            if let Some(ref arc) = entry.stable {
                metas.push(AreaMeta {
                    buf: MetaBuf::Stable(arc.clone()),
                    left: entry.options.x * scale,
                    top,
                    color,
                    bounds,
                    transform_index: phys_idx,
                });
            } else if let Some(ref parts) = entry.parts {
                // HUD 多段：逻辑 x 横拼，再 * scale
                let mut cursor_x = entry.options.x;
                let step = text_ctx.ensure_digit_table(&entry.options);
                for part in parts {
                    match part {
                        OwnedTextPart::Static(s) => {
                            if s.is_empty() {
                                continue;
                            }
                            let slot = text_ctx.get_or_shape_text(s, &entry.options);
                            let w = text_ctx.slot_line_width(slot);
                            metas.push(AreaMeta {
                                buf: MetaBuf::Slot(slot),
                                left: cursor_x * scale,
                                top,
                                color,
                                bounds,
                                transform_index: phys_idx,
                            });
                            cursor_x += w;
                        }
                        OwnedTextPart::Dynamic(s) => {
                            if s.is_empty() {
                                continue;
                            }
                            let opts = TextOptions {
                                max_width: None,
                                align: TextAlign::Left,
                                ..entry.options.clone()
                            };
                            let key = ShapeKey::from_text(s, &opts);
                            let (area_meta, w) = match text_ctx.peek_shape_slot(&key) {
                                Some(si) => {
                                    text_ctx.pin_slot(si);
                                    let w = text_ctx.slot_line_width(si);
                                    (AreaMeta {
                                        buf: MetaBuf::Slot(si),
                                        left: cursor_x * scale,
                                        top,
                                        color,
                                        bounds,
                                        transform_index: phys_idx,
                                    }, w)
                                }
                                None => {
                                    let metrics = Metrics::new(entry.options.font_size, entry.options.font_size * 1.2);
                                    let mut buffer = text_ctx.take_buffer(metrics);
                                    let attrs = opts.attrs.as_ref()
                                        .map(|a| a.as_attrs())
                                        .unwrap_or_else(Attrs::new);
                                    buffer.set_size(None, None);
                                    buffer.set_text(s, &attrs, Shaping::Advanced, None);
                                    buffer.shape_until_scroll(&mut text_ctx.font_system, false);
                                    let lw = buffer
                                        .line_layout(&mut text_ctx.font_system, 0)
                                        .map(|layout| layout.iter().map(|run| run.w).sum::<f32>())
                                        .unwrap_or(0.0);
                                    (AreaMeta {
                                        buf: MetaBuf::Stable(Arc::new(buffer)),
                                        left: cursor_x * scale,
                                        top,
                                        color,
                                        bounds,
                                        transform_index: phys_idx,
                                    }, lw)
                                }
                            };
                            metas.push(area_meta);
                            cursor_x += w;
                        }
                        OwnedTextPart::Stable(arc, line_width) => {
                            metas.push(AreaMeta {
                                buf: MetaBuf::Stable(arc.clone()),
                                left: cursor_x * scale,
                                top,
                                color,
                                bounds,
                                transform_index: phys_idx,
                            });
                            cursor_x += line_width;
                        }
                        OwnedTextPart::Digits(s) => {
                            for ch in s.chars() {
                                if ch == ' ' {
                                    cursor_x += step * 0.5;
                                    continue;
                                }
                                if let Some(d) = ch.to_digit(10) {
                                    let slot = text_ctx.digit_slot(d, &entry.options);
                                    metas.push(AreaMeta {
                                        buf: MetaBuf::Slot(slot),
                                        left: cursor_x * scale,
                                        top,
                                        color,
                                        bounds,
                                        transform_index: phys_idx,
                                    });
                                    cursor_x += step;
                                } else if is_hud_digit_char(ch) {
                                    // 数学符号：预 shape 表项，宽度用自身 glyph（非 tabular）
                                    let slot = text_ctx.glyph_slot_char(ch, &entry.options);
                                    let w = text_ctx.slot_line_width(slot);
                                    metas.push(AreaMeta {
                                        buf: MetaBuf::Slot(slot),
                                        left: cursor_x * scale,
                                        top,
                                        color,
                                        bounds,
                                        transform_index: phys_idx,
                                    });
                                    cursor_x += w;
                                }
                                // 其它：跳过
                            }
                        }
                    }
                }
            } else {
                let key = ShapeKey::from_entry(entry);
                let slot = text_ctx.get_or_shape(key, &entry.options);
                metas.push(AreaMeta {
                    buf: MetaBuf::Slot(slot),
                    left: entry.options.x * scale,
                    top,
                    color,
                    bounds,
                    transform_index: phys_idx,
                });
            }
        }

        let vertex_start;
        let vertex_count;
        {
            let TextContext {
                ref mut font_system,
                ref mut swash_cache,
                ref mut text_atlas,
                ref mut text_renderer,
                ref viewport,
                ref shape_slots,
                ..
            } = *text_ctx;

            let mut areas: Vec<TextArea> = Vec::with_capacity(metas.len());
            for meta in &metas {
                let buf: &Buffer = match &meta.buf {
                    MetaBuf::Stable(arc) => arc,
                    MetaBuf::Slot(si) => {
                        debug_assert!(
                            (*si as usize) < shape_slots.len(),
                            "shape slot {} out of bounds (len {})",
                            si,
                            shape_slots.len()
                        );
                        &*shape_slots[*si as usize].buffer
                    }
                };
                areas.push(TextArea {
                    buffer: buf,
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

/// HUD 多段：Static / Dynamic / Digits。单行 LTR，不保证与整段 `draw_text` 像素级一致。
///
/// ```ignore
/// draw_text_parts(&mut batch.texts, &[
///     TextPart::Static("分数: "),
///     TextPart::Digits("123"),       // 数字优化
/// ], TextOptions::default().x(16.0).y(16.0).font_size(20.0));
/// ```
pub fn draw_text_parts(
    list: &mut TextEntryList,
    parts: &[TextPart<'_>],
    options: TextOptions,
) {
    list.push_parts(parts, options);
}

/// HUD 自动切分：`split_hud` → Static + Digits（启发式）。
///
/// 更推荐跨帧 [`HudLine`]：语义上区分 Static / Dynamic，Digits 仅数字槽。
///
/// ```ignore
/// draw_text_hud(&mut batch.texts, "FPS: 60.5", opts);
/// draw_text_hud!(&mut batch.texts, opts; "FPS: {:.1}", fps);
/// ```
pub fn draw_text_hud(list: &mut TextEntryList, text: &str, options: TextOptions) {
    list.push_hud(text, options);
}

/// 绘制一条 [`HudLine`]。
pub fn draw_hud_line(list: &mut TextEntryList, line: &HudLine, options: TextOptions) {
    line.draw(list, options);
}

/// `format!` 拼串后 [`split_hud`]，得到 `Vec<`[`HudPart`]`>`。
///
/// 底层是编译器内建的 `format!`，本宏只做糖：不复刻 format 解析器。
///
/// ```ignore
/// let parts = hud_format!("score={score}");
/// // ≈ split_hud(&format!("score={score}"))
/// ```
#[macro_export]
macro_rules! hud_format {
    ($($arg:tt)*) => {
        $crate::text::split_hud(&::std::format!($($arg)*))
    };
}

/// `format!` + [`draw_text_hud`]。
///
/// 语法：`draw_text_hud!(list, options; "fmt", args...)`
/// （`;` 分隔选项与 format 参数，避免和 `TextOptions` 里的逗号混淆。）
///
/// ```ignore
/// draw_text_hud!(
///     &mut batch.texts,
///     TextOptions::default().x(16.0).y(12.0).font_size(14.0).color(WHITE);
///     "FPS: {:.1}  score={}",
///     fps,
///     score,
/// );
/// // 展开为：
/// // draw_text_hud(list, &format!("FPS: {:.1}  score={}", fps, score), opts)
/// ```
#[macro_export]
macro_rules! draw_text_hud {
    ($list:expr, $opts:expr; $($arg:tt)*) => {
        $crate::text::draw_text_hud($list, &::std::format!($($arg)*), $opts)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};
    use rustc_hash::FxHasher;

    fn plain_entry(text: &str, options: TextOptions) -> TextEntry {
        TextEntry {
            text: text.into(),
            options,
            transform_index: 0,
            parts: None,
            stable: None,
        }
    }

    #[test]
    fn stable_text_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<StableText>();
    }

    #[test]
    fn stable_text_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<StableText>();
    }

    #[test]
    fn shape_key_ignores_position_and_color() {
        let e1 = plain_entry(
            "Hello",
            TextOptions::default()
                .x(10.0)
                .y(20.0)
                .color(Color::new(1.0, 0.0, 0.0, 1.0)),
        );
        let mut e2 = plain_entry(
            "Hello",
            TextOptions::default()
                .x(99.0)
                .y(0.0)
                .color(Color::new(0.0, 1.0, 0.0, 1.0)),
        );
        e2.transform_index = 3;
        assert_eq!(ShapeKey::from_entry(&e1), ShapeKey::from_entry(&e2));
    }

    #[test]
    fn shape_key_differs_on_font_size_and_text() {
        let base = plain_entry("Hello", TextOptions::default().font_size(16.0));
        let sized = plain_entry("Hello", TextOptions::default().font_size(18.0));
        let other = plain_entry("World", TextOptions::default().font_size(16.0));
        assert_ne!(ShapeKey::from_entry(&base), ShapeKey::from_entry(&sized));
        assert_ne!(ShapeKey::from_entry(&base), ShapeKey::from_entry(&other));
    }

    #[test]
    fn shape_key_hash_stable() {
        let e = plain_entry("稳定", TextOptions::default().font_size(14.0));
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

    #[test]
    fn push_parts_stores_owned_parts() {
        let mut list = TextEntryList::new();
        draw_text_parts(
            &mut list,
            &[TextPart::Static("分数: "), TextPart::Digits("42")],
            TextOptions::default().x(10.0).y(20.0),
        );
        assert_eq!(list.entries.len(), 1);
        let e = &list.entries[0];
        assert!(e.text.is_empty());
        let parts = e.parts.as_ref().expect("parts");
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            OwnedTextPart::Static(s) => assert_eq!(s, "分数: "),
            _ => panic!("expected Static"),
        }
        match &parts[1] {
            OwnedTextPart::Digits(s) => assert_eq!(s, "42"),
            _ => panic!("expected Digits"),
        }
    }

    #[test]
    fn split_hud_fps_and_score() {
        // `:` `.` 属 Digits 表 → 与数字并成一段；标签 → Static
        assert_eq!(
            split_hud("FPS: 60.5"),
            vec![
                HudPart::Static("FPS".into()),
                HudPart::Digits(": 60.5".into()),
            ]
        );
        assert_eq!(
            split_hud("分数: 42"),
            vec![
                HudPart::Static("分数".into()),
                HudPart::Digits(": 42".into()),
            ]
        );
        assert_eq!(split_hud(""), Vec::<HudPart>::new());
        assert_eq!(
            split_hud("123"),
            vec![HudPart::Digits("123".into())]
        );
        assert_eq!(
            split_hud("-12.5%"),
            vec![HudPart::Digits("-12.5%".into())]
        );
        assert_eq!(
            split_hud("a+b=3"),
            vec![
                HudPart::Static("a".into()),
                HudPart::Digits("+".into()),
                HudPart::Static("b".into()),
                HudPart::Digits("=3".into()),
            ]
        );
    }

    #[test]
    fn draw_text_hud_uses_parts() {
        let mut list = TextEntryList::new();
        draw_text_hud(
            &mut list,
            "x=9",
            TextOptions::default().x(1.0).y(2.0),
        );
        let parts = list.entries[0].parts.as_ref().unwrap();
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            OwnedTextPart::Static(s) => assert_eq!(s, "x"),
            _ => panic!("expected Static"),
        }
        match &parts[1] {
            OwnedTextPart::Digits(s) => assert_eq!(s, "=9"),
            _ => panic!("expected Digits"),
        }
    }

    #[test]
    fn hud_line_static_dynamic_digits() {
        let mut line = HudLine::new()
            .text("分数: ")
            .digits("0")
            .text("  mode=")
            .dynamic("Both");
        line.write_slot(1, "42");
        line.set_dynamic(3, "Parts");
        assert_eq!(
            line.spans(),
            &[
                HudPart::Static("分数: ".into()),
                HudPart::Digits("42".into()),
                HudPart::Static("  mode=".into()),
                HudPart::Dynamic("Parts".into()),
            ]
        );
        let mut list = TextEntryList::new();
        line.draw(&mut list, TextOptions::default());
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.entries[0].parts.as_ref().unwrap().len(), 4);
    }

    #[test]
    fn is_hud_digit_covers_math_symbols() {
        assert!(is_hud_digit_char('0'));
        assert!(is_hud_digit_char('.'));
        assert!(is_hud_digit_char('-'));
        assert!(is_hud_digit_char('%'));
        assert!(is_hud_digit_char('×'));
        assert!(!is_hud_digit_char('a'));
        assert!(!is_hud_digit_char('分'));
    }

    #[test]
    fn hud_format_macro_splits_like_format_plus_split_hud() {
        let score = 42u32;
        let fps = 60.5f64;
        let via_macro = crate::hud_format!("score={score} fps={fps:.1}");
        let via_fn = split_hud(&format!("score={score} fps={fps:.1}"));
        assert_eq!(via_macro, via_fn);
        assert!(via_macro.iter().any(|p| matches!(p, HudPart::Digits(_))));
    }

    // ---- StableText cache tests (require GPU) ----

    fn make_test_gpu() -> std::sync::Arc<crate::gpu::GpuContext> {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        );
        std::sync::Arc::new(crate::gpu::GpuContext::new(&instance))
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn make_stable_liveness_drop_clears_held() {
        let gpu = make_test_gpu();
        assert_eq!(gpu.shape_cache_held_count(), 0);
        let h = gpu.make_stable_text("hello", &TextOptions::default().font_size(20.0));
        assert_eq!(gpu.shape_cache_held_count(), 1);
        assert_eq!(gpu.shape_cache_len(), 1);
        drop(h);
        // GC scavenge 后 held_count 应归 0
        assert_eq!(gpu.shape_cache_held_count(), 0);
        assert_eq!(gpu.shape_cache_len(), 1); // 槽仍在，只是不再 held
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn make_stable_cache_hit_shares_liveness() {
        let gpu = make_test_gpu();
        let h1 = gpu.make_stable_text("hello", &TextOptions::default().font_size(20.0));
        assert_eq!(gpu.shape_cache_held_count(), 1);
        let h2 = gpu.make_stable_text("hello", &TextOptions::default().font_size(20.0));
        // 同文案 cache hit → count 仍为 1
        assert_eq!(gpu.shape_cache_held_count(), 1);
        assert_eq!(gpu.shape_cache_len(), 1);
        // 共享 liveness arc
        assert!(std::sync::Arc::ptr_eq(&h1.liveness, &h2.liveness));
        drop(h1);
        // 仍有 h2 存活 → count 保持 1
        assert_eq!(gpu.shape_cache_held_count(), 1);
        drop(h2);
        // 全 drop → count 归 0
        assert_eq!(gpu.shape_cache_held_count(), 0);
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn clear_shape_cache_preserves_held_slots() {
        let gpu = make_test_gpu();
        let h1 = gpu.make_stable_text("keep", &TextOptions::default().font_size(20.0));
        let h2 = gpu.make_stable_text("drop", &TextOptions::default().font_size(20.0));
        drop(h2); // "drop" is now dead
        assert_eq!(gpu.shape_cache_held_count(), 1);

        // 加一个非 held 槽（用 draw_text 路径制造）
        let mut list = TextEntryList::new();
        list.push("nothandled", TextOptions::default().font_size(20.0));
        let _ = list.prepare_texts(&gpu, 1, 1, 1.0, &[], &mut Vec::new());
        assert_eq!(gpu.shape_cache_len(), 3);
        assert_eq!(gpu.shape_cache_held_count(), 1);

        // clear → 只清非 held + 死标记
        gpu.clear_shape_cache();
        assert_eq!(gpu.shape_cache_len(), 1);
        assert_eq!(gpu.shape_cache_held_count(), 1);
        drop(h1);
        assert_eq!(gpu.shape_cache_held_count(), 0);
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn max_entries_does_not_evict_held_slots() {
        let gpu = make_test_gpu();
        gpu.set_shape_cache_max_entries(Some(2));
        // 创建 3 个 handle → 总槽 = 3，超过 cap = 2；但 cap 只约束非 held
        let _h1 = gpu.make_stable_text("a", &TextOptions::default().font_size(20.0));
        let _h2 = gpu.make_stable_text("b", &TextOptions::default().font_size(20.0));
        let _h3 = gpu.make_stable_text("c", &TextOptions::default().font_size(20.0));
        // 全部被持有，cap 不应触发 evict
        assert_eq!(gpu.shape_cache_held_count(), 3);
        assert_eq!(gpu.shape_cache_len(), 3);
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn stable_text_clone_shares_arc_and_liveness() {
        let gpu = make_test_gpu();
        let h1 = gpu.make_stable_text("clone_test", &TextOptions::default().font_size(20.0));
        let h2 = h1.clone();
        assert!(std::sync::Arc::ptr_eq(&h1.buffer, &h2.buffer));
        assert!(std::sync::Arc::ptr_eq(&h1.liveness, &h2.liveness));
        // clone 后 count 仍为 1（共享同一 liveness arc）
        assert_eq!(gpu.shape_cache_held_count(), 1);
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn draw_text_then_make_stable_shares_buffer() {
        let gpu = make_test_gpu();
        // draw_text 走 cache
        let mut list = TextEntryList::new();
        list.push("shared", TextOptions::default().font_size(20.0));
        let _ = list.prepare_texts(&gpu, 1, 1, 1.0, &[], &mut Vec::new());
        // 此时 cache 已有 "shared"，无 liveness
        assert_eq!(gpu.shape_cache_held_count(), 0);
        assert_eq!(gpu.shape_cache_len(), 1);

        // make_stable → 命中已有 slot，liveness = Some
        let h1 = gpu.make_stable_text("shared", &TextOptions::default().font_size(20.0));
        assert_eq!(gpu.shape_cache_held_count(), 1);
        let h2 = gpu.make_stable_text("shared", &TextOptions::default().font_size(20.0));
        assert_eq!(gpu.shape_cache_held_count(), 1);
        // 共享 Arc
        assert!(std::sync::Arc::ptr_eq(&h1.buffer, &h2.buffer));
        assert!(std::sync::Arc::ptr_eq(&h1.liveness, &h2.liveness));
        drop(h1);
        drop(h2);
        // 全 drop → held_count 归 0
        assert_eq!(gpu.shape_cache_held_count(), 0);
    }
}
