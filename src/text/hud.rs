use std::sync::Arc;

use crate::glyphon::Buffer;
use crate::render::Pos;

use super::{TextDef, TextEntryList, TextOverride, ResolvedTextGlyph};

#[inline]
pub(crate) fn is_hud_digit_char(ch: char) -> bool {
    ch.is_ascii_digit()
        || matches!(
            ch,
            '.' | ',' | '+' | '-' | '*' | '/' | '%' | '=' | ':' | '(' | ')' | '[' | ']' | '{'
                | '}' | '±' | '×' | '÷' | '°' | '−' | 'e' | 'E'
        )
}

/// 已 shape 的文字句柄，跨帧复用，与 `draw_text` 共享同一 cache。
///
/// **线程安全**：`StableText: Send + Sync`（内部使用共享的 shape buffer 与 glyph 模板），
/// 可跨线程传递（但 [`GpuContext`] 本身不是 `Send`，创建与使用应在同一线程）。
/// ## 生命周期
/// - **Buffer 内存**：由 `StableText` 内的 `Arc<Buffer>` 保活；`drop` 后回收。
/// - **Glyph 模板**：创建时从已 shaping 的 Buffer 提取，绘制时直接 physicalize，
///   保留连字、fallback、复杂脚本和多行布局，同时跳过重复的 layout-run 遍历。
/// - **cache 槽**：[`GpuContext::make_stable_text`] 把对应槽标为「live」(`liveness: Arc<()>`)，
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
/// - [`DrawBatch::text_stable`]：每帧传 `pos + TextOverride`；其余已在 `make_stable_text` 时定型。
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
/// 引擎 clone 进 [`TextEntry::Parts`]。
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
            self.parts.resize_with(index + 1, || TextPart::normal(String::new()));
        }
        self.parts[index] = TextPart::normal(s);
    }

    pub fn set_dynamic(&mut self, index: usize, s: impl Into<String>) {
        if index >= self.parts.len() {
            self.parts.resize_with(index + 1, || TextPart::normal(String::new()));
        }
        self.parts[index] = TextPart::dynamic(s);
    }

    pub fn set_glyphs(&mut self, index: usize, s: impl Into<String>) {
        if index >= self.parts.len() {
            self.parts.resize_with(index + 1, || TextPart::normal(String::new()));
        }
        self.parts[index] = TextPart::glyphs(s);
    }

    /// 原地改 Normal/Dynamic/Glyphs 槽的字符串。
    pub fn write_slot(&mut self, index: usize, s: &str) {
        if index >= self.parts.len() {
            self.parts.resize_with(index + 1, || TextPart::normal(String::new()));
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


