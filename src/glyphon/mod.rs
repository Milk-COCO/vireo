//! Glyphon provides a simple way to render 2D text with [wgpu], [cosmic-text] and [etagere].
//!
//! [wpgu]: https://github.com/gfx-rs/wgpu
//! [cosmic-text]: https://github.com/pop-os/cosmic-text
//! [etagere]: https://github.com/nical/etagere

mod blend;
mod cache;
mod colr;
mod custom_glyph;
mod error;
mod sdf;
mod text_atlas;
mod text_render;
mod viewport;

pub use cache::Cache;
pub use custom_glyph::{
    ContentType, CustomGlyph, CustomGlyphCacheKey, CustomGlyphId, RasterizeCustomGlyphRequest,
    RasterizedCustomGlyph,
};
pub use error::{PrepareError, RenderError};
pub use text_atlas::{ColorMode, TextAtlas, TextAtlasStats};
pub(crate) use text_atlas::{InnerAtlas, PinRegistry};
pub(crate) use text_render::GlyphonCacheKey;
pub use text_render::TextRenderer;
pub use viewport::Viewport;

// Re-export all top-level types from `cosmic-text` for convenience.
#[doc(no_inline)]
pub use cosmic_text::{
    self, Action, Affinity, Attrs, AttrsList, AttrsOwned, Buffer, BufferLine, CacheKey, Color,
    Command, Cursor, Edit, Editor, Family, FamilyOwned, Font, FontSystem, LayoutCursor,
    LayoutGlyph, LayoutLine, LayoutRun, LayoutRunIter, Metrics, ShapeGlyph, ShapeLine, ShapeSpan,
    ShapeWord, Shaping, Stretch, Style, SubpixelBin, SwashCache, SwashContent, SwashImage, Weight,
    Wrap, fontdb,
};

use etagere::AllocId;
use wgpu::{Device, Queue};

pub(crate) enum GpuCacheStatus {
    InAtlas {
        x: u16,
        y: u16,
        content_type: ContentType,
    },
    SkipRasterization,
}

pub(crate) struct GlyphDetails {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) gpu_cache: GpuCacheStatus,
    pub(crate) atlas_id: Option<AllocId>,
    pub(crate) top: i16,
    pub(crate) left: i16,
    /// The atlas `generation` in which this glyph was last touched by `prepare`. A glyph is
    /// considered in use for the current frame (and not evictable) while this equals the atlas's
    /// current `generation`.
    pub(crate) last_used: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct GlyphToRender {
    pub(crate) pos: [i32; 2],
    pub(crate) dim: [u16; 2],
    pub(crate) uv: [u16; 2],
    pub(crate) color: u32,
    pub(crate) content_type_with_srgb: [u16; 2],
    pub(crate) depth: f32,
    pub(crate) transform_index: u32,
    pub(crate) base_uv_rect: [f32; 4],
    /// 样式表下标 + 1（`0` = 无样式）。
    pub(crate) style_index: u32,
    /// 该字形的距离场带宽（**设备或生成像素**，按 content_type 解释）。
    pub(crate) sdf_band: f32,
    /// 图集里的 texel 尺寸 `(w<<16)|h`：尺寸阶梯的 quad 与 texel 不是 1:1 时用它算 UV 矩形。
    pub(crate) entry_dim: u32,
}

/// The screen resolution to use when rendering text.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resolution {
    /// The width of the screen in pixels.
    pub width: u32,
    /// The height of the screen in pixels.
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Params {
    screen_resolution: Resolution,
    /// 逻辑像素 → 设备像素的换算因子（描边/偏移的 `Dp` 意图靠它折算）。
    /// 文本管线没有相机 uniform，所以这个量放在这里。
    dpi_scale: f32,
    /// 本帧样式数量（0 = 本帧没有任何带样式的文本）。
    style_count: u32,
}

/// Controls the visible area of the text. Any text outside of the visible area will be clipped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextBounds {
    /// The position of the left edge of the visible area.
    pub left: i32,
    /// The position of the top edge of the visible area.
    pub top: i32,
    /// The position of the right edge of the visible area.
    pub right: i32,
    /// The position of the bottom edge of the visible area.
    pub bottom: i32,
}

/// The default visible area doesn't clip any text.
impl Default for TextBounds {
    fn default() -> Self {
        Self {
            left: i32::MIN,
            top: i32::MIN,
            right: i32::MAX,
            bottom: i32::MAX,
        }
    }
}

/// A text area containing text to be rendered along with its overflow behavior.
#[derive(Clone)]
pub struct TextArea<'a> {
    /// The buffer containing the text to be rendered.
    pub buffer: &'a Buffer,
    /// The left edge of the buffer.
    pub left: f32,
    /// The top edge of the buffer.
    pub top: f32,
    /// The scaling to apply to the buffer.
    pub scale: f32,
    /// The visible bounds of the text area. This is used to clip the text and doesn't have to
    /// match the `left` and `top` values.
    pub bounds: TextBounds,
    /// The default color of the text area.
    pub default_color: Color,
    /// Additional custom glyphs to render.
    pub custom_glyphs: &'a [CustomGlyph],
    /// Index into the `transforms` storage buffer (group 2 binding 0).
    /// 0 = identity (default).
    pub transform_index: u32,
    /// 当前 batch 基础贴图 UV 区域 `(u0, v0, u1, v1)`。
    /// VS 按字形 corner 插值到 `VertexOutput.base_uv`（每字形独立映射，可重复）。
    pub base_uv_rect: [f32; 4],
    /// 样式表下标 + 1（`0` = 无样式 ⇒ 走普通覆盖度图集，零额外开销）。
    pub style_index: u32,
    /// 只画图层（不画 fill）的样式下标 + 1 —— 彩色字形用它把描边/投影垫在底下，
    /// emoji 自己的颜色保持不变。`0` = 没有。
    pub style_layers_only: u32,
    /// 距离场带宽（设备像素）；`style_index == 0` 时忽略。
    pub sdf_band: f32,
    /// 该样式里「画在填充之上」的层数；`> 0` 才会额外发一趟 above quad。
    pub style_layers: u32,
    /// 位图：第 i 位 = 第 i 层画在填充之上。
    pub style_above_mask: u32,
    /// 该样式向字形外侧的最大外扩（设备像素）——决定距离场带宽档。
    pub style_reach: f32,
    /// 几何描边（真 miter/bevel），画在填充与图层之下。`None` = 没有。
    pub(crate) vector_stroke: Option<crate::text::TextStrokePx>,
}

/// A pre-resolved glyph that bypasses `Buffer::layout_runs()` while retaining
/// cosmic-text physical positioning and the normal atlas/clip path.
pub(crate) struct ResolvedGlyphArea<'a> {
    pub glyph: &'a LayoutGlyph,
    pub line_y: f32,
    pub line_top: f32,
    pub line_height: f32,
    pub left: f32,
    pub top: f32,
    pub scale: f32,
    pub bounds: TextBounds,
    pub default_color: Color,
    pub transform_index: u32,
    pub base_uv_rect: [f32; 4],
    /// 样式表下标 + 1（`0` = 无样式）。
    pub style_index: u32,
    /// 距离场带宽（设备像素）。
    pub sdf_band: f32,
    /// 样式层数（0 = 无样式）。
    pub style_layers: u32,
    /// 第 i 层画在填充之上的位图。
    pub style_above_mask: u32,
    /// 样式外扩（设备像素）。0 = 拿不到样式，按尺寸生成。
    pub style_reach: f32,
    /// 几何描边（真 miter/bevel）。`None` = 没有。
    pub vector_stroke: Option<crate::text::TextStrokePx>,
}

pub(crate) struct State<'a> {
    pub(crate) device: &'a Device,
    pub(crate) queue: &'a Queue,
}
