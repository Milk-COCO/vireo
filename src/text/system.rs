//! 文字系统的公开门面。
//!
//! [`TextSystem`] 拥有整个文字运行时状态（字体库、typeset 缓存、glyph atlas、
//! 渲染器），并对其加锁。`GpuContext` 只持有 `TextSystem`：
//!
//! - 用户侧走 [`GpuContext::text`](crate::gpu::GpuContext::text) 拿到 `&TextSystem`，
//!   调用 `measure` / `typeset_*` / `stable` 这些**不需要 device**的方法；
//! - crate 内部（prepare / draw / pipeline）走 `GpuContext::text_ctx()` 拿互斥锁守卫，
//!   直接操作 [`TextContext`] 的字段。
//!
//! 这一层存在的理由：文字此前和图形（Shape）共用 `GpuContext` 的一堆方法，
//! 语义混在一起；同时"用户能管住 glyph 缓存"需要一个稳定的入口对象。

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::glyphon::{Attrs, Buffer, Metrics, Shaping, TextAtlasStats};
use wgpu::{Device, Queue};

use super::{GlyphCache, StableText, TextContext, TextDef, TypesetCacheStats};

/// 文字系统（字体、排版缓存、字形图集、渲染器的统一入口）。
///
/// 由 `GpuContext` 创建并持有；通过
/// [`GpuContext::text`](crate::gpu::GpuContext::text) 访问。
pub struct TextSystem {
    ctx: Mutex<TextContext>,
}

impl TextSystem {
    pub(crate) fn new(ctx: TextContext) -> Self {
        Self {
            ctx: Mutex::new(ctx),
        }
    }

    /// 内部使用：拿互斥锁守卫（prepare / draw / pipeline 直接操作 [`TextContext`]）。
    pub(crate) fn ctx(&self) -> MutexGuard<'_, TextContext> {
        self.ctx.lock().unwrap()
    }

    // ---- 度量 ----

    /// 测量文本尺寸（逻辑像素）。参数与 `draw_text` 一致。
    pub fn measure(&self, text: &str, options: &TextDef) -> (f32, f32) {
        let mut ctx = self.ctx();
        let line_height = options.font_size * 1.2;
        let metrics = Metrics::new(options.font_size, line_height);
        let mut buffer = Buffer::new(&mut ctx.font_system, metrics);
        buffer.set_size(options.max_width, None);

        let attrs = options
            .attrs
            .as_ref()
            .map(|a| a.as_attrs())
            .unwrap_or_else(Attrs::new);

        buffer.set_text(text, &attrs, Shaping::Advanced, Some(options.align.into()));
        buffer.shape_until_scroll(&mut ctx.font_system, false);

        let num_lines = buffer.lines.len() as f32;
        let max_w = (0..buffer.lines.len()).fold(0.0f32, |max, i| {
            let line_w = buffer
                .line_layout(&mut ctx.font_system, i)
                .map(|layout| layout.iter().map(|run| run.w).sum())
                .unwrap_or(0.0);
            max.max(line_w)
        });

        (max_w, line_height * num_lines)
    }

    /// 文本的**视觉**包围盒（逻辑像素）：在 [`Self::measure`] 之上加上样式的溢出
    /// （描边宽度 + 3σ 软边 + 偏移）。
    ///
    /// 排版仍然用 [`Self::measure`] —— 这个只用于裁剪/居中这类"视觉外框"需求。
    /// 注意：`Px` 意图在这里按 1:1 处理（没有 dpi 上下文），需要精确设备像素时自己换算。
    pub fn measure_visual(&self, text: &str, options: &TextDef) -> (f32, f32) {
        let (w, h) = self.measure(text, options);
        let pad = options
            .style
            .as_ref()
            .map(|s| s.reach_px(1.0))
            .unwrap_or(0.0);
        (w + pad * 2.0, h + pad * 2.0)
    }

    /// 文本图集的当前占用（覆盖度 / 距离场共用 R8 图 + 彩色图）。
    pub fn atlas_stats(&self) -> TextAtlasStats {
        self.ctx().text_atlas.stats()
    }

    /// 字形缓存视图：预算 / 预光栅化 / 钉子 / 回收 / 观测。
    ///
    /// ```text
    /// let glyphs = gpu.text().glyphs();
    /// glyphs.set_retention_budget(Some(4 << 20));        // 保留 4 MiB（永不掉帧）
    /// glyphs.set_hard_budget(Some(2 << 20));             // 或：硬上限（可能掉帧）
    /// glyphs.set_auto_compact(true);                     // 默认就是 true
    /// let hold = glyphs.hold(dev, queue, "标题", &def, scale); // 钉住常用字形
    /// let stats = glyphs.stats();
    /// println!("{} 条 / {} 字节 / 峰值 {} 字节",
    ///     stats.entries(), stats.used_bytes(), stats.peak_used_bytes);
    /// drop(hold);                                         // 解钉
    /// ```
    pub fn glyphs(&self) -> GlyphCache<'_> {
        GlyphCache::new(self)
    }

    // ---- 排版（typeset）缓存 ----

    /// 设置排版缓存 TTL（真实时间，与 FPS 无关）。
    /// - `Some(d)`：超过 d 未使用则过期
    /// - `None`：永不按时间自动回收
    pub fn set_typeset_ttl(&self, ttl: Option<Duration>) {
        self.ctx().set_typeset_ttl(ttl);
    }

    /// 当前排版缓存 TTL（`None` = 不自动按时间回收）。
    pub fn typeset_ttl(&self) -> Option<Duration> {
        self.ctx().typeset_ttl()
    }

    /// 设置排版缓存最大条数。
    /// - `Some(n)`：最多 n 条不同文案键，满则 LRU
    /// - `None`：不限制条数
    pub fn set_typeset_max_entries(&self, max: Option<usize>) {
        self.ctx().set_typeset_max_entries(max);
    }

    /// 当前排版缓存最大条数（`None` = 不限制）。
    pub fn typeset_max_entries(&self) -> Option<usize> {
        self.ctx().typeset_max_entries()
    }

    /// 立即清空排版缓存（不影响已排除的 `StableText` 句柄）。
    pub fn clear_typeset_cache(&self) {
        self.ctx().clear_typeset_cache();
    }

    /// 当前排版缓存条目数。
    pub fn typeset_len(&self) -> usize {
        self.ctx().typeset_len()
    }

    /// 缓存中由 [`StableText`] 活跃持有的条目数。
    /// 这些槽不会被 TTL/LRU/`clear_typeset_cache` 回收。
    pub fn typeset_held_count(&self) -> usize {
        self.ctx().typeset_held_count()
    }

    /// 排版缓存命中统计。
    pub fn typeset_stats(&self) -> TypesetCacheStats {
        self.ctx().typeset_stats()
    }

    /// 重置排版缓存命中统计。
    pub fn reset_typeset_stats(&self) {
        self.ctx().reset_typeset_stats();
    }

    // ---- resolved glyph 元数据（不是 atlas 位图）----

    /// 清空 `TextPart::Glyphs` 的 resolved glyph 元数据缓存。
    /// 不清空 glyph atlas 中已经光栅化的位图。
    pub fn clear_resolved_glyph_cache(&self) {
        self.ctx().clear_glyph_cache();
    }

    /// 当前 resolved glyph 元数据缓存条目数。
    pub fn resolved_glyph_cache_len(&self) -> usize {
        self.ctx().glyph_cache_len()
    }

    // ---- 句柄 / 预热 / 字体 ----

    /// 从文本创建 [`StableText`]（预 shape，跨帧复用）。
    /// 只要返回的 `StableText` 存活，对应的 Buffer 不会被释放。
    pub fn stable(&self, text: &str, options: &TextDef) -> StableText {
        self.ctx().make_stable(text, options)
    }

    /// 预烘焙一个 `StableText` 的实例块（[`StableText::bake`] 的实现）。
    pub(crate) fn bake_stable(
        &self,
        gpu: &crate::gpu::GpuContext,
        stable: &StableText,
        scale: f32,
    ) -> usize {
        self.ctx().bake_stable(gpu, stable, scale)
    }

    /// 预热文字管线：用单字符 "A" 跑一次 prepare，触发首帧字体/atlas lazy 初始化。
    pub fn preheat(
        &self,
        device: &Device,
        queue: &Queue,
        physical_width: u32,
        physical_height: u32,
    ) {
        self.ctx()
            .preheat(device, queue, physical_width, physical_height);
    }

    /// 加载自定义字体（TTF/OTF 字节数据）。
    pub fn load_font(&self, data: &[u8]) {
        let mut ctx = self.ctx();
        ctx.font_system.db_mut().load_font_data(data.to_vec());
        // Font fallback 可能变化，排版缓存与 resolved glyph 必须重建。
        // 活跃 `StableText` 句柄故意保留：它们已经 shape 过并自己持有 Buffer。
        ctx.clear_typeset_cache();
    }

    /// 从文件加载字体（TTF/OTF），等价于读文件后调用 [`Self::load_font`]。
    pub fn load_font_file(&self, path: impl AsRef<std::path::Path>) -> Result<(), String> {
        let data =
            std::fs::read(path.as_ref()).map_err(|e| format!("failed to read font file: {}", e))?;
        self.load_font(&data);
        Ok(())
    }
}
