use std::sync::Arc;

use crate::glyphon::Buffer;
use crate::render::Pos;

use super::{ResolvedTextGlyph, TextDef, TextEntryList, TextOverride};

#[inline]
pub(crate) fn is_hud_digit_char(ch: char) -> bool {
    ch.is_ascii_digit()
        || matches!(
            ch,
            '.' | ','
                | '+'
                | '-'
                | '*'
                | '/'
                | '%'
                | '='
                | ':'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '±'
                | '×'
                | '÷'
                | '°'
                | '−'
                | 'e'
                | 'E'
        )
}

/// 已 shape 的文字句柄，跨帧复用，与 `draw_text` 共享同一 cache。
///
/// **线程安全**：`StableText: Send + Sync`（内部使用共享的 shape buffer 与 glyph 模板），
/// 可跨线程传递（但 [`crate::gpu::GpuContext`] 本身不是 `Send`，创建与使用应在同一线程）。
/// ## 生命周期
/// - **Buffer 内存**：由 `StableText` 内的 `Arc<Buffer>` 保活；`drop` 后回收。
/// - **Glyph 模板**：创建时从已 shaping 的 Buffer 提取，绘制时直接 physicalize，
///   保留连字、fallback、复杂脚本和多行布局，同时跳过重复的 layout-run 遍历。
/// - **cache 槽**：[`crate::gpu::GpuContext::make_stable_text`] 把对应槽标为「live」(`liveness: Arc<()>`)，
///   **直至所有 `StableText` clone 均 drop** 才会变为可淘汰。
///   同一文案的多次 `make_stable_text` 共享同一 liveness 标记（0→1 创建一次）。
///   GC/evict/clear 使用 `Arc::strong_count` 探测 liveness 标记是否还活着：
///   `strong_count > 1` = 仍有 `StableText` 持有；`== 1` = 已死（`slot` 自身唯一持有），可淘汰。
/// - **`Clone`**：复制 `Arc<Buffer>` 与 `Arc<()>`（liveness 标记），增加 `strong_count`。
///   所有 clone 共享同一 cache 槽，只有**所有** clone 均 drop 后槽才变为可淘汰。
///
/// ## 与 [`TextPart::Normal`]/[`TextPart::Dynamic`] 的差异
/// | 维度 | `Normal`/`Dynamic` | `Stable` |
/// |------|-------------------|----------|
/// | 缓存条目 | `draw_text` 自动管 | 用户 `make_stable_text` 显式创建 |
/// | 跨帧复用 | TTL/LRU 可能 evict | **所有 handle 均 drop 前**永不 evict |
///
/// ## `max_width` / `align` 支持
/// - 当 `max_width: None`（默认）：**单行左对齐**，同旧版行为。
/// - 当 `max_width: Some(w)`：文本在 w 逻辑像素处换行，**`align` 生效**（`Left` / `Center` / `Right`）。
///
/// ## 限制
/// - **不支持 `Glyphs` 切分**：整段是单 buffer；但其已 shaping glyph 模板会走
///   direct prepare。
///
/// ## 绘制 API
/// - [`crate::render::DrawBatch::text_stable`]：每帧传 `pos + TextOverride`；其余已在 `make_stable_text` 时定型。
/// - [`TextEntryList::push_stable`]：添加到 `TextEntryList`（与 `text_parts` 配合使用）。
///
/// ```ignore
/// let h = gpu.make_stable_text("Score: {}", TextOptions::default().font_size(20.0));
/// batch.text_stable(&h, Pos::new(16.0, 16.0), TextOverride::from_color(WHITE));
/// ```
#[derive(Clone)]
pub struct StableText {
    pub(crate) buffer: Arc<Buffer>,
    pub(crate) resolved_glyphs: Arc<[Arc<ResolvedTextGlyph>]>,
    pub(crate) line_width: f32,
    /// 创建时 `TextDef.font_size`（culling 近似高度用）。
    pub(crate) font_size: f32,
    pub(crate) liveness: Arc<()>,
    /// 原文案（创建时传入的字符串），用于调试和用户侧去重。
    pub(crate) text: String,
    /// 实际 layout 行数（包含 `max_width` 自动折行后产生的多行）。culling 高度用。
    pub(crate) line_count: u32,
    /// 创建时的完整定义（含样式）：实例固化要靠它复算样式参数。
    pub(crate) def: TextDef,
    /// 烘焙实例块（跨帧复用；失效时自动重烘）。
    pub(crate) baked: Arc<BakedHandle>,
}

/// 烘焙实例的失效键。任一字段变化都要重烘。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct BakeKey {
    /// 图集布局代数（缩图集 / 重建 / 淘汰 / 清空 / 装载）。
    pub(crate) mask_epoch: u64,
    pub(crate) color_epoch: u64,
    /// DPI（按位比较）。
    pub(crate) scale_bits: u32,
    /// 物理裁剪框。
    pub(crate) bounds: [i32; 4],
    /// 该条目是否走"带物理变换"的路径（clip 的处理方式随之不同）。
    pub(crate) transform_is_zero: bool,
}

/// 烘焙实例里"跟随绘制颜色"的占位色。
///
/// 实例的 `color` 不是都该跟着 `TextOverride` 变：几何描边用它自己的颜色、逐字覆盖
/// （`glyph.color_opt`）也各是各的。烘焙时把**等于条目颜色**的那些实例改写成这个哨兵，
/// 复用打补丁时只把哨兵换成本帧颜色，其余原样保留。
pub(crate) const BAKED_FILL_SENTINEL: u32 = 0x00AB_CDEF;

/// 烘焙好的实例块：`StableText` 每帧只把它拷进帧实例数组并打几个补丁。
pub(crate) struct BakedBlock {
    /// 烘焙时的整数物理原点（`floor(pos × scale)`）。实例是**本地坐标**，
    /// 绘制位置 = 本地 + 整数位移 + 变换槽里的平移量。
    pub(crate) origin: (i32, i32),
    pub(crate) instances: Vec<crate::glyphon::GlyphToRender>,
    pub(crate) key: BakeKey,
}

/// 烘焙状态 + 计数。`stable` 句柄与它的每个 `TextEntry::Stable` 共享同一个实例。
///
/// 类型本身公开只是因为 `TextEntry::Stable` 的字段是公开枚举字段；字段与构造都是
/// crate 内部使用，外部没有可用的方法。
#[doc(hidden)]
pub struct BakedHandle {
    pub(crate) block: std::sync::Mutex<Option<BakedBlock>>,
    /// 累计烘焙次数（观测 + 用例用）。
    pub(crate) bakes: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for BakedHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BakedHandle")
            .field(
                "instances",
                &self.with_block(|b| b.map(|b| b.instances.len())),
            )
            .field("bakes", &self.bakes())
            .finish()
    }
}

impl Default for BakedHandle {
    fn default() -> Self {
        Self {
            block: std::sync::Mutex::new(None),
            bakes: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl BakedHandle {
    /// 在持锁状态下读烘焙块（拷贝/判定都在锁内完成，不跨 prepare 调用）。
    pub(crate) fn with_block<R>(&self, f: impl FnOnce(Option<&BakedBlock>) -> R) -> R {
        let guard = self.block.lock().unwrap_or_else(|e| e.into_inner());
        f(guard.as_ref())
    }

    /// 写入新的烘焙块并计数。
    pub(crate) fn store(&self, block: Option<BakedBlock>) {
        let mut guard = self.block.lock().unwrap_or_else(|e| e.into_inner());
        *guard = block;
        self.bakes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn bakes(&self) -> u64 {
        self.bakes.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl StableText {
    /// 原文案（`make_stable_text` 时传入的字符串）。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 创建时字号。
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// 首行逻辑宽度（shape 后）。
    pub fn line_width(&self) -> f32 {
        self.line_width
    }

    /// 实际行数（≥ 1；`max_width` 折行后可能 > 1）。
    pub fn line_count(&self) -> u32 {
        self.line_count
    }

    /// 预烘焙实例（像 `prewarm` 一样，把"首次绘制"的成本提前付掉）。
    ///
    /// 实例固化是**自动**的：首次 `text_stable` 绘制就会烘焙；这个入口只是让加载界面
    /// 承担那一次成本。返回本次烘焙的实例数（0 = 没有可画的字形）。
    ///
    /// 烘焙用的裁剪是"全屏"（`TextBounds::default()`）、变换取恒等、颜色取白 ——
    /// 与不带 clip 的绘制一致；带 clip / 带变换的首帧会再烘焙一次。
    pub fn bake(&self, gpu: &crate::gpu::GpuContext, scale: f32) -> usize {
        gpu.text().bake_stable(gpu, self, scale)
    }

    /// 累计烘焙次数（诊断用：稳态下不该增长）。
    pub fn bakes(&self) -> u64 {
        self.baked.bakes()
    }

    /// 当前烘焙块里的实例数（`None` = 还没烘焙）。
    pub fn baked_instances(&self) -> Option<usize> {
        self.baked.with_block(|b| b.map(|b| b.instances.len()))
    }

    /// 丢掉烘焙块（强制下次绘制重烘）。一般不需要：失效条件会自动处理。
    pub fn invalidate_bake(&self) {
        let mut guard = self.baked.block.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
}

impl std::fmt::Debug for StableText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StableText")
            .field("text", &self.text)
            .field("font_size", &self.font_size)
            .field("line_width", &self.line_width)
            .field("line_count", &self.line_count)
            .field("resolved_glyph_count", &self.resolved_glyphs.len())
            .field("buffer_strong_count", &Arc::strong_count(&self.buffer))
            .field("live_handle_count", &Arc::strong_count(&self.liveness))
            .finish()
    }
}

/// HUD 文本段（拥有权；单行 LTR；不保证与整段 `draw_text` 像素级一致）。
///
/// 四种类型的差异：
/// - [`TextPart::Normal`]：内容稳定，走整段 shape 缓存，同内容可命中。
/// - [`TextPart::Dynamic`]：内容会变但仍需整段 shape，适合需要整段 shaping 的短句。
/// - [`TextPart::Glyphs`]：任意字符的按字 direct path。每个字符首次遇到时单独 shape，
///   后续复用 resolved glyph 元数据；间距使用字体自身 advance。
/// - [`TextPart::Stable`]：预 shape 句柄，不走 cache 查询。详见 [`StableText`]。
///
/// **`TextDef`**：`Normal` / `Dynamic` / `Glyphs` 的第二参数 `None` = 使用
/// `draw_text_parts` / `push_parts` 的行级 `def`；`Some(def)` = 仅本段覆盖。
/// [`TextPart::Stable`] 无此项（字号等已在 `make_stable_text` 时定型）。
///
/// 提交时传 [`&[TextPart]`](TextPart)（例如 `&vec[..]` / [`HudLine::parts`]）；
/// 引擎 clone 进 [`crate::text::TextEntry::Parts`]。
#[derive(Clone, Debug)]
pub enum TextPart {
    /// 内容稳定（标签、说明）。走整段 shape 缓存，同内容可 hit。
    /// `def: None` → 行级 `TextDef`；`Some` → 本段专用。
    Normal(String, Option<TextDef>),
    /// 内容会变的任意文案。仍走整段 shape；字符串一变就 miss。
    Dynamic(String, Option<TextDef>),
    /// 任意字符的按字 direct path；按需 shape，使用字体自身 advance。
    Glyphs(String, Option<TextDef>),
    /// 预 shape 稳定文本；`TextDef` 已在创建时定型，不可在此覆盖。
    Stable(StableText),
}

impl TextPart {
    /// 稳定文案，用行级 `TextDef`。
    #[inline]
    pub fn normal(text: impl Into<String>) -> Self {
        Self::Normal(text.into(), None)
    }
    /// 稳定文案 + 本段 `TextDef`。
    #[inline]
    pub fn normal_def(text: impl Into<String>, def: TextDef) -> Self {
        Self::Normal(text.into(), Some(def))
    }
    /// 动态文案，用行级 `TextDef`。
    #[inline]
    pub fn dynamic(text: impl Into<String>) -> Self {
        Self::Dynamic(text.into(), None)
    }
    /// 动态文案 + 本段 `TextDef`。
    #[inline]
    pub fn dynamic_def(text: impl Into<String>, def: TextDef) -> Self {
        Self::Dynamic(text.into(), Some(def))
    }
    /// 按字 direct path，用行级 `TextDef`。
    #[inline]
    pub fn glyphs(text: impl Into<String>) -> Self {
        Self::Glyphs(text.into(), None)
    }
    /// 按字 direct path + 本段 `TextDef`。
    #[inline]
    pub fn glyphs_def(text: impl Into<String>, def: TextDef) -> Self {
        Self::Glyphs(text.into(), Some(def))
    }
    /// 预 shape 句柄（clone `StableText`）。
    #[inline]
    pub fn stable(s: &StableText) -> Self {
        Self::Stable(s.clone())
    }

    #[inline]
    pub(crate) fn resolve_def<'a>(&'a self, row: &'a TextDef) -> &'a TextDef {
        match self {
            Self::Normal(_, Some(d)) | Self::Dynamic(_, Some(d)) | Self::Glyphs(_, Some(d)) => d,
            Self::Normal(_, None) | Self::Dynamic(_, None) | Self::Glyphs(_, None) => row,
            Self::Stable(_) => row,
        }
    }

    /// 段内字符串（Stable 返回原文案）。
    pub fn as_str(&self) -> &str {
        match self {
            Self::Normal(s, _) | Self::Dynamic(s, _) | Self::Glyphs(s, _) => s.as_str(),
            Self::Stable(h) => h.text(),
        }
    }
}

/// 薄 wrapper：跨帧持有 [`Vec<TextPart>`]，只改 Dynamic/Glyphs 槽。
///
/// ```ignore
/// let mut line = HudLine::new()
///     .text("分数: ")
///     .glyphs("0")
///     .text("  模式: ")
///     .dynamic("Both");
/// // 每帧
/// line.set_glyphs(1, score.to_string());
/// line.draw(&mut batch.texts, pos, def, ov);
/// ```
#[derive(Clone, Debug, Default)]
pub struct HudLine {
    parts: Vec<TextPart>,
}

impl HudLine {
    pub fn new() -> Self {
        Self { parts: Vec::new() }
    }

    pub fn text(mut self, s: impl Into<String>) -> Self {
        self.parts.push(TextPart::normal(s));
        self
    }

    pub fn dynamic(mut self, s: impl Into<String>) -> Self {
        self.parts.push(TextPart::dynamic(s));
        self
    }

    /// 按字 direct path 槽（见 [`TextPart::Glyphs`]）。
    pub fn glyphs(mut self, s: impl Into<String>) -> Self {
        self.parts.push(TextPart::glyphs(s));
        self
    }

    pub fn parts(&self) -> &[TextPart] {
        &self.parts
    }

    pub fn set_text(&mut self, index: usize, s: impl Into<String>) {
        if index >= self.parts.len() {
            self.parts
                .resize_with(index + 1, || TextPart::normal(String::new()));
        }
        self.parts[index] = TextPart::normal(s);
    }

    pub fn set_dynamic(&mut self, index: usize, s: impl Into<String>) {
        if index >= self.parts.len() {
            self.parts
                .resize_with(index + 1, || TextPart::normal(String::new()));
        }
        self.parts[index] = TextPart::dynamic(s);
    }

    pub fn set_glyphs(&mut self, index: usize, s: impl Into<String>) {
        if index >= self.parts.len() {
            self.parts
                .resize_with(index + 1, || TextPart::normal(String::new()));
        }
        self.parts[index] = TextPart::glyphs(s);
    }

    /// 原地改 Normal/Dynamic/Glyphs 槽的字符串。
    pub fn write_slot(&mut self, index: usize, s: &str) {
        if index >= self.parts.len() {
            self.parts
                .resize_with(index + 1, || TextPart::normal(String::new()));
        }
        match &mut self.parts[index] {
            TextPart::Normal(buf, _) | TextPart::Dynamic(buf, _) | TextPart::Glyphs(buf, _) => {
                buf.clear();
                buf.push_str(s);
            }
            TextPart::Stable(_) => {}
        }
    }

    pub fn draw(&self, list: &mut TextEntryList, pos: Pos, def: TextDef, ov: TextOverride) {
        list.push_parts(&self.parts, pos, def, ov);
    }

    pub(crate) fn draw_indexed(
        &self,
        list: &mut TextEntryList,
        pos: Pos,
        def: TextDef,
        ov: TextOverride,
        transform_index: u32,
    ) {
        list.push_parts_indexed(&self.parts, pos, def, ov, transform_index);
    }
}

/// 将 HUD 字符串切成 Normal / Glyphs 段（启发式）。
pub fn split_hud(s: &str) -> Vec<TextPart> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<TextPart> = Vec::new();
    let mut cur = String::new();
    let mut cur_digits: Option<bool> = None;

    let flush = |out: &mut Vec<TextPart>, cur: &mut String, cur_digits: &mut Option<bool>| {
        if cur.is_empty() {
            *cur_digits = None;
            return;
        }
        let part = match *cur_digits {
            Some(true) => TextPart::glyphs(std::mem::take(cur)),
            _ => TextPart::normal(std::mem::take(cur)),
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

/// 绘制一条 [`HudLine`]。
pub fn draw_hud_line(
    list: &mut TextEntryList,
    line: &HudLine,
    pos: Pos,
    def: TextDef,
    ov: TextOverride,
) {
    line.draw(list, pos, def, ov);
}
