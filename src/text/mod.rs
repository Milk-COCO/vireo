//! 文本渲染：glyphon + cosmic-text，使用系统字体。
//!
//! `prepare_texts` 对「内容相同」的条目复用已 shape 的 `Buffer`（跳过 harfrust），
//! 位置/颜色/transform 不参与缓存键。
//!
//! batch 基础贴图：`DrawBatch::set_texture` / `set_uv` 更新画笔，`push*` 时冻结到
//! [`TextEntry`] 的 [`TextTextureState`]；prepare 按 generation 输出
//! [`PreparedTextSegment`] 供 Renderer 分段绑定。
//!
//! 缓存策略（均可配置，经 `GpuContext`）：
//! - TTL：`set_shape_cache_ttl(Some(d) | None)`，`None` = 不按时间回收
//! - 条数：`set_shape_cache_max_entries(Some(n) | None)`，`None` = 不限制
//! - 立即清空：`clear_shape_cache`

pub use cosmic_text::{AttrsOwned, Family, FamilyOwned, FeatureTag, Style, Weight};

use crate::color::Color;
use crate::render::Pos;


mod cache;
pub(crate) use cache::{GlyphKey, ShapeCacheSlot, ShapeKey};
pub use cache::ShapeCacheStats;
mod context;
mod entry;
mod hud;
mod prepare;
mod stencil;
pub use entry::{TextEntry, TextEntryList};
pub use hud::{HudLine, StableText, TextPart, split_hud, draw_hud_line};
pub(crate) use context::ResolvedTextGlyph;
pub use context::{Attrs, ColorMode, TextContext};
pub(crate) use stencil::{TextStencilMode, stencil_text_ds_pass, stencil_text_ds_test};

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

/// 文本绘制覆盖（OVERRIDE）——覆盖 batch 文本状态机。
/// 与 `ShapeOverride` 语义对称。`None` = 保持 batch 状态。
#[derive(Clone, Debug, Default)]
pub struct TextOverride {
    /// `Some` = 覆盖 batch.text_color；`None` = 保持
    pub color: Option<Color>,
    /// `None` = 保持 batch.text_clip；`Some(None)` = 清除裁剪；`Some(Some(b))` = 设置
    pub clip: Option<Option<crate::glyphon::TextBounds>>,
    /// `Some` = 在 batch 局部空间上叠加变换（右乘 batch 变换），不污染 batch 状态。
    /// `None` = 保持 batch 当前变换。
    pub transform: Option<crate::render::Transform>,
    /// `Some` = 覆盖 batch.uv（文字 base_uv）；`None` = 保持
    pub uv: Option<crate::render::UvRect>,
    /// `Some(None)` = 白贴图；`Some(Some(bg))` = 指定文字 base 纹理；`None` = 保持
    /// 与 `ShapeOverride::bind_group` 共享语义（`BatchOverride` 去重后同一字段）。
    pub bind_group: Option<Option<wgpu::BindGroup>>,
}

impl TextOverride {
    pub fn new() -> Self {
        Self::default()
    }

    /// 仅覆盖 color 的快捷构造。
    pub fn from_color(c: Color) -> Self {
        Self { color: Some(c), clip: None, transform: None, uv: None, bind_group: None }
    }

    pub fn color(mut self, c: Color) -> Self {
        self.color = Some(c);
        self
    }

    /// 裁切矩形，**逻辑像素**（prepare 时 × scale → 物理，与 `pos` 一致）。
    pub fn clip(mut self, l: i32, t: i32, r: i32, b: i32) -> Self {
        self.clip = Some(Some(crate::glyphon::TextBounds { left: l, top: t, right: r, bottom: b }));
        self
    }

    pub fn clear_clip(mut self) -> Self {
        self.clip = Some(None);
        self
    }

    /// 在 batch 局部空间上叠加变换（不覆盖整个 batch transform，仅对本次绘制生效）。
    pub fn transform(mut self, t: crate::render::Transform) -> Self {
        self.transform = Some(t);
        self
    }

    pub fn uv(mut self, uv: crate::render::UvRect) -> Self {
        self.uv = Some(uv);
        self
    }

    pub fn uv_rect(mut self, u0: f32, v0: f32, u1: f32, v1: f32) -> Self {
        self.uv = Some(crate::render::UvRect { u0, v0, u1, v1 });
        self
    }

    pub fn texture(mut self, tex: &crate::texture::Texture) -> Self {
        self.bind_group = Some(Some(tex.bind_group.clone()));
        self
    }

    pub fn clear_texture(mut self) -> Self {
        self.bind_group = Some(None);
        self
    }

    pub fn bind_group(mut self, bg: Option<wgpu::BindGroup>) -> Self {
        self.bind_group = Some(bg);
        self
    }
}

/// 文本渲染选项——决定文字长什么样。
#[derive(Clone, Debug)]
pub struct TextDef {
    pub font_size: f32,
    /// 最大宽度，超过则换行。None 表示不换行。
    pub max_width: Option<f32>,
    /// 水平对齐。需要配合 max_width 使用才有效果。
    pub align: TextAlign,
    /// 字体属性（family、weight、style 等）。None 使用默认 Attrs。
    pub attrs: Option<AttrsOwned>,
}

impl Default for TextDef {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            max_width: None,
            align: TextAlign::Left,
            attrs: None,
        }
    }
}

impl TextDef {
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
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

/// 文字入队时捕获的 batch 贴图状态。
///
/// 由 [`DrawBatch::set_texture`] / [`DrawBatch::set_uv`] 更新画笔，并在
/// `text` / `push*` 时 **clone 冻结** 到对应 [`TextEntry`]。连续相同
/// [`generation`](Self::generation) 的条目在 prepare 时合并为同一渲染段。
///
/// 请用访问器读取；不要手改字段或自行构造后塞回引擎（引擎只认入队时快照，
/// `generation` 仅由 `set_texture` / `set_uv` 递增）。
#[derive(Clone, Debug)]
pub struct TextTextureState {
    pub(crate) generation: u64,
    pub(crate) view: Option<wgpu::TextureView>,
    pub(crate) uv: crate::render::UvRect,
    pub(crate) bind_group: Option<wgpu::BindGroup>,
}

/// 一次 `prepare_texts` 产出的连续文字渲染段（内部用）。
///
/// 同一段内 `texture_view` 相同；`None` = 白贴图 / 默认 base。
/// `vertex_start` + `vertex_count` 对应 glyphon 实例缓冲中的 glyph 范围。
pub(crate) struct PreparedTextSegment {
    pub vertex_start: u32,
    pub vertex_count: u32,
    pub texture_view: Option<wgpu::TextureView>,
    pub bind_group: Option<wgpu::BindGroup>,
}

impl Default for TextTextureState {
    fn default() -> Self {
        Self {
            generation: 0,
            view: None,
            uv: crate::render::UvRect::default(),
            bind_group: None,
        }
    }
}

impl TextTextureState {
    /// 状态代数：`set_texture` / `set_uv` 各递增一次；同 generation 的条目合并渲染。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 是否绑定了 batch 基础贴图（`None` = 白贴图路径）。
    pub fn has_texture(&self) -> bool {
        self.view.is_some() || self.bind_group.is_some()
    }

    /// 捕获时的 batch UV 子区域。
    pub fn uv(&self) -> crate::render::UvRect {
        self.uv
    }
}


/// 往 `batch.texts` 添加一条文本（`transform_index = 0` = 单位阵，见 `DrawBatch::transform_table`）。
/// `pos` 为逻辑世界坐标。随 batch 变换请用 `DrawBatch::text`。
pub fn draw_text(list: &mut TextEntryList, text: &str, pos: Pos, def: TextDef, ov: TextOverride) {
    list.push(text, pos, def, ov);
}

/// HUD 多段：Normal / Dynamic / Glyphs / Stable。单行 LTR，不保证与整段 `draw_text` 像素级一致。
///
/// `parts` 为切片引用；引擎 clone 进 [`TextEntry::Parts`]。
///
/// ```ignore
/// draw_text_parts(&mut batch.texts, &[
///     TextPart::normal("分数: "),
///     TextPart::glyphs("123"),
///     // 本段更大字号：
///     // TextPart::glyphs_def("99", TextDef::default().font_size(28.0)),
/// ], Pos::new(16.0, 16.0), TextDef::default().font_size(20.0), TextOverride::default());
/// ```
pub fn draw_text_parts(
    list: &mut TextEntryList,
    parts: &[TextPart],
    pos: Pos,
    def: TextDef,
    ov: TextOverride,
) {
    list.push_parts(parts, pos, def, ov);
}

/// HUD 自动切分：`split_hud` → Normal + Glyphs（启发式）。
///
/// 更推荐跨帧 [`HudLine`]：语义上区分 Normal / Dynamic，Glyphs 走按字 direct path。
///
/// ```ignore
/// draw_text_hud(&mut batch.texts, "FPS: 60.5", Pos::new(16.0, 12.0), def, ov);
/// draw_text_hud!(&mut batch.texts, pos, def, ov; "FPS: {:.1}", fps);
/// ```
pub fn draw_text_hud(
    list: &mut TextEntryList,
    text: &str,
    pos: Pos,
    def: TextDef,
    ov: TextOverride,
) {
    list.push_hud(text, pos, def, ov);
}

/// `format!` 拼串后 [`split_hud`]，得到 `Vec<`[`TextPart`]`>`。
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
/// 语法：`draw_text_hud!(list, pos, def, ov; "fmt", args...)`
///
/// ```ignore
/// draw_text_hud!(
///     &mut batch.texts,
///     Pos::new(16.0, 12.0),
///     TextDef::default().font_size(14.0),
///     TextOverride::from_color(WHITE);
///     "FPS: {:.1}  score={}",
///     fps,
///     score,
/// );
/// // 展开为：
/// // draw_text_hud(list, &format!("FPS: {:.1}  score={}", fps, score), pos, def, ov)
/// ```
#[macro_export]
macro_rules! draw_text_hud {
    ($list:expr, $pos:expr, $def:expr, $ov:expr; $($arg:tt)*) => {
        $crate::text::draw_text_hud($list, &::std::format!($($arg)*), $pos, $def, $ov)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::hud::is_hud_digit_char;

    fn plain_entry(text: &str, def: TextDef) -> TextEntry {
        TextEntry::Normal {
            text: text.into(),
            pos: Pos::new(0.0, 0.0),
            def,
            override_: TextOverride::default(),
            transform_index: 0,
            texture_state: TextTextureState::default(),
        }
    }

    fn shape_key_from_entry(entry: &TextEntry) -> ShapeKey {
        match entry {
            TextEntry::Normal { text, def, .. } => ShapeKey::from_text(text, def),
            _ => panic!("from_entry called on non-Normal TextEntry"),
        }
    }

    #[test]
    fn stable_text_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<StableText>();
    }

    #[test]
    fn shape_key_ignores_position_and_color() {
        let e1 = plain_entry(
            "Hello",
            TextDef::default(),
        );
        let e2 = plain_entry(
            "Hello",
            TextDef::default(),
        );
        assert_eq!(shape_key_from_entry(&e1), shape_key_from_entry(&e2));
    }

    #[test]
    fn shape_key_differs_on_font_size_and_text() {
        let base = plain_entry("Hello", TextDef::default().font_size(16.0));
        let sized = plain_entry("Hello", TextDef::default().font_size(18.0));
        let other = plain_entry("World", TextDef::default().font_size(16.0));
        assert_ne!(shape_key_from_entry(&base), shape_key_from_entry(&sized));
        assert_ne!(shape_key_from_entry(&base), shape_key_from_entry(&other));
    }

    #[test]
    fn parts_culling_font_size_uses_largest_part() {
let entry = TextEntry::Parts {
            pos: Pos::new(0.0, 0.0),
            def: TextDef::default().font_size(16.0),
            parts: vec![
                TextPart::normal("small"),
                TextPart::glyphs_def("99", TextDef::default().font_size(48.0)),
            ],
            override_: TextOverride::default(),
            transform_index: 0,
            texture_state: TextTextureState::default(),
        };
        assert_eq!(entry.approx_font_size(), 48.0);
    }

    fn part_kind_str(p: &TextPart) -> (&'static str, &str) {
        match p {
            TextPart::Normal(s, _) => ("normal", s.as_str()),
            TextPart::Dynamic(s, _) => ("dynamic", s.as_str()),
            TextPart::Glyphs(s, _) => ("glyphs", s.as_str()),
            TextPart::Stable(h) => ("stable", h.text()),
        }
    }

    #[test]
    fn text_entry_captures_texture_state_on_push() {
        let mut list = TextEntryList::new();
        list.set_texture_state(None);
        list.push(
            "a",
            Pos::new(0.0, 0.0),
            TextDef::default(),
            TextOverride::default(),
        );
        assert_eq!(list.entries[0].texture_state().generation(), 1);
        assert!(!list.entries[0].texture_state().has_texture());

        list.set_uv_state(crate::render::UvRect {
            u0: 0.1,
            v0: 0.2,
            u1: 0.9,
            v1: 0.8,
        });
        list.push(
            "b",
            Pos::new(0.0, 16.0),
            TextDef::default(),
            TextOverride::default(),
        );
        assert_eq!(list.entries[1].texture_state().generation(), 2);
        assert_eq!(list.entries[1].texture_state().uv().u0, 0.1);
        assert_ne!(
            list.entries[0].texture_state().generation(),
            list.entries[1].texture_state().generation()
        );

        list.clear();
        assert!(list.entries.is_empty());
        list.push(
            "c",
            Pos::new(0.0, 0.0),
            TextDef::default(),
            TextOverride::default(),
        );
        assert_eq!(list.entries[0].texture_state().generation(), 0);
    }

    #[test]
    fn push_parts_stores_parts() {
        let mut list = TextEntryList::new();
        draw_text_parts(
            &mut list,
            &[
                TextPart::normal("分数: "),
                TextPart::glyphs("42"),
                TextPart::glyphs_def("99", TextDef::default().font_size(28.0)),
            ],
            Pos::new(0.0, 0.0),
            TextDef::default().font_size(16.0),
            TextOverride::default(),
        );
        assert_eq!(list.entries.len(), 1);
        match &list.entries[0] {
            TextEntry::Parts { parts, def, .. } => {
                assert_eq!(parts.len(), 3);
                assert!((def.font_size - 16.0).abs() < 1e-5);
                match &parts[0] {
                    TextPart::Normal(s, None) => assert_eq!(s, "分数: "),
                    _ => panic!("expected Normal(None)"),
                }
                match &parts[1] {
                    TextPart::Glyphs(s, None) => assert_eq!(s, "42"),
                    _ => panic!("expected Glyphs(None)"),
                }
                match &parts[2] {
                    TextPart::Glyphs(s, Some(d)) => {
                        assert_eq!(s, "99");
                        assert!((d.font_size - 28.0).abs() < 1e-5);
                    }
                    _ => panic!("expected Glyphs(Some)"),
                }
            }
            _ => panic!("expected Parts"),
        }
    }

    #[test]
    fn split_hud_fps_and_score() {
        // `:` `.` 属自动数值段字符 → 与数字并成一段；标签 → Normal
        let p = split_hud("FPS: 60.5");
        assert_eq!(p.len(), 2);
        assert_eq!(part_kind_str(&p[0]), ("normal", "FPS"));
        assert_eq!(part_kind_str(&p[1]), ("glyphs", ": 60.5"));

        let p = split_hud("分数: 42");
        assert_eq!(p.len(), 2);
        assert_eq!(part_kind_str(&p[0]), ("normal", "分数"));
        assert_eq!(part_kind_str(&p[1]), ("glyphs", ": 42"));

        assert!(split_hud("").is_empty());

        let p = split_hud("123");
        assert_eq!(p.len(), 1);
        assert_eq!(part_kind_str(&p[0]), ("glyphs", "123"));

        let p = split_hud("-12.5%");
        assert_eq!(p.len(), 1);
        assert_eq!(part_kind_str(&p[0]), ("glyphs", "-12.5%"));

        let p = split_hud("a+b=3");
        assert_eq!(p.len(), 4);
        assert_eq!(part_kind_str(&p[0]), ("normal", "a"));
        assert_eq!(part_kind_str(&p[1]), ("glyphs", "+"));
        assert_eq!(part_kind_str(&p[2]), ("normal", "b"));
        assert_eq!(part_kind_str(&p[3]), ("glyphs", "=3"));
    }

    #[test]
    fn hud_line_normal_dynamic_glyphs() {
        let mut line = HudLine::new()
            .text("分数: ")
            .glyphs("0")
            .text("  mode=")
            .dynamic("Both");
        line.write_slot(1, "42");
        line.set_dynamic(3, "Parts");
        let parts = line.parts();
        assert_eq!(parts.len(), 4);
        assert_eq!(part_kind_str(&parts[0]), ("normal", "分数: "));
        assert_eq!(part_kind_str(&parts[1]), ("glyphs", "42"));
        assert_eq!(part_kind_str(&parts[2]), ("normal", "  mode="));
        assert_eq!(part_kind_str(&parts[3]), ("dynamic", "Parts"));
        let mut list = TextEntryList::new();
        line.draw(&mut list, Pos::new(0.0, 0.0), TextDef::default(), TextOverride::default());
        assert_eq!(list.entries.len(), 1);
        let parts = match &list.entries[0] {
            TextEntry::Parts { parts, .. } => parts,
            _ => panic!("expected Parts"),
        };
        assert_eq!(parts.len(), 4);
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
        assert_eq!(via_macro.len(), via_fn.len());
        for (a, b) in via_macro.iter().zip(via_fn.iter()) {
            assert_eq!(part_kind_str(a), part_kind_str(b));
        }
        assert!(via_macro.iter().any(|p| matches!(p, TextPart::Glyphs(_, _))));
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
        let h = gpu.make_stable_text("hello", &TextDef::default().font_size(20.0));
        assert_eq!(gpu.shape_cache_held_count(), 1);
        assert_eq!(gpu.shape_cache_len(), 1);
        drop(h);
        // GC scavenge 后 held_count 应归 0
        assert_eq!(gpu.shape_cache_held_count(), 0);
        assert_eq!(gpu.shape_cache_len(), 1); // 槽仍在，只是不再 held
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn glyph_cache_resolves_arbitrary_chars_on_demand() {
        let gpu = make_test_gpu();
        let mut ctx = gpu.text_ctx.lock().unwrap();
        let def = TextDef::default().font_size(20.0);
        for ch in ['7', '中', '±', 'A', ' '] {
            let cluster = ctx.resolve_glyph(ch, &def);
            assert!(cluster.advance >= 0.0);
        }
        assert_eq!(ctx.glyph_cache.len(), 5);

        let slots_before = ctx.shape_slots.len();
        let cached = ctx.resolve_glyph('中', &def);
        assert!(cached.advance >= 0.0);
        assert_eq!(ctx.glyph_cache.len(), 5);
        assert_eq!(ctx.shape_slots.len(), slots_before);

        ctx.resolve_glyph('中', &def.clone().font_size(24.0));
        assert_eq!(ctx.glyph_cache.len(), 6);
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn stable_text_direct_prepare_emits_instances() {
        let gpu = make_test_gpu();
        let stable = gpu.make_stable_text(
            "Stable 123 中文",
            &TextDef::default().font_size(20.0),
        );
        let mut list = TextEntryList::new();
        list.push_stable(
            &stable,
            Pos::new(0.0, 0.0),
            TextOverride::default(),
        );
        let segments = list.prepare_texts(
            &gpu,
            640,
            480,
            1.0,
            &[],
            &mut vec![
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
            ],
            None,
            Color::new(1.0, 1.0, 1.0, 1.0),
        );
        assert_eq!(segments.len(), 1);
        assert!(segments[0].vertex_count > 0);
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn stable_text_part_direct_prepare_emits_instances() {
        let gpu = make_test_gpu();
        let stable = gpu.make_stable_text(
            "Parts 中文 العربية",
            &TextDef::default().font_size(20.0),
        );
        let mut list = TextEntryList::new();
        list.push_parts(
            &[TextPart::normal("prefix "), TextPart::stable(&stable)],
            Pos::new(0.0, 0.0),
            TextDef::default().font_size(20.0),
            TextOverride::default(),
        );
        let segments = list.prepare_texts(
            &gpu,
            640,
            480,
            1.0,
            &[],
            &mut Vec::new(),
            None,
            Color::new(1.0, 1.0, 1.0, 1.0),
        );
        assert!(!segments.is_empty());
        assert!(segments.iter().map(|s| s.vertex_count).sum::<u32>() > 0);
    }

    #[test]
    #[ignore = "requires GPU; run with --ignored"]
    fn make_stable_cache_hit_shares_liveness() {
        let gpu = make_test_gpu();
        let h1 = gpu.make_stable_text("hello", &TextDef::default().font_size(20.0));
        assert_eq!(gpu.shape_cache_held_count(), 1);
        let h2 = gpu.make_stable_text("hello", &TextDef::default().font_size(20.0));
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
        let h1 = gpu.make_stable_text("keep", &TextDef::default().font_size(20.0));
        let h2 = gpu.make_stable_text("drop", &TextDef::default().font_size(20.0));
        drop(h2); // "drop" is now dead
        assert_eq!(gpu.shape_cache_held_count(), 1);

        // 加一个非 held 槽（用 draw_text 路径制造）
        let mut list = TextEntryList::new();
        list.push("nothandled", Pos::new(0.0, 0.0), TextDef::default().font_size(20.0), TextOverride::default());
        let _ = list.prepare_texts(&gpu, 1, 1, 1.0, &[], &mut Vec::new(), None, Color::new(1.0, 1.0, 1.0, 1.0));
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
    fn draw_text_then_make_stable_shares_buffer() {
        let gpu = make_test_gpu();
        // draw_text 走 cache
        let mut list = TextEntryList::new();
        list.push("shared", Pos::new(0.0, 0.0), TextDef::default().font_size(20.0), TextOverride::default());
        let _ = list.prepare_texts(&gpu, 1, 1, 1.0, &[], &mut Vec::new(), None, Color::new(1.0, 1.0, 1.0, 1.0));
        // 此时 cache 已有 "shared"，无 liveness
        assert_eq!(gpu.shape_cache_held_count(), 0);
        assert_eq!(gpu.shape_cache_len(), 1);

        // make_stable → 命中已有 slot，liveness = Some
        let h1 = gpu.make_stable_text("shared", &TextDef::default().font_size(20.0));
        assert_eq!(gpu.shape_cache_held_count(), 1);
        let h2 = gpu.make_stable_text("shared", &TextDef::default().font_size(20.0));
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
