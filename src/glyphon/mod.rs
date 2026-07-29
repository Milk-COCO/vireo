//! Glyphon provides a simple way to render 2D text with [wgpu], [cosmic-text] and [etagere].
//!
//! [wpgu]: https://github.com/gfx-rs/wgpu
//! [cosmic-text]: https://github.com/pop-os/cosmic-text
//! [etagere]: https://github.com/nical/etagere

mod cache;
mod custom_glyph;
mod error;
mod text_atlas;
mod text_render;
mod viewport;

pub use cache::Cache;
pub use custom_glyph::{
    ContentType, CustomGlyph, CustomGlyphId, RasterizeCustomGlyphRequest, RasterizedCustomGlyph,
};
pub use error::{PrepareError, RenderError};
pub use text_atlas::{ColorMode, TextAtlas};
pub use text_render::TextRenderer;
pub use viewport::Viewport;

// Re-export all top-level types from `cosmic-text` for convenience.
#[doc(no_inline)]
pub use cosmic_text::{
    self, fontdb, Action, Affinity, Attrs, AttrsList, AttrsOwned, Buffer, BufferLine, CacheKey,
    Color, Command, Cursor, Edit, Editor, Family, FamilyOwned, Font, FontSystem, LayoutCursor,
    LayoutGlyph, LayoutLine, LayoutRun, LayoutRunIter, Metrics, ShapeGlyph, ShapeLine, ShapeSpan,
    ShapeWord, Shaping, Stretch, Style, SubpixelBin, SwashCache, SwashContent, SwashImage, Weight,
    Wrap,
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
    width: u16,
    height: u16,
    gpu_cache: GpuCacheStatus,
    atlas_id: Option<AllocId>,
    top: i16,
    left: i16,
    /// The atlas `generation` in which this glyph was last touched by `prepare`. A glyph is
    /// considered in use for the current frame (and not evictable) while this equals the atlas's
    /// current `generation`.
    last_used: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct GlyphToRender {
    pos: [i32; 2],
    dim: [u16; 2],
    uv: [u16; 2],
    color: u32,
    content_type_with_srgb: [u16; 2],
    depth: f32,
    transform_index: u32,
    base_uv_rect: [f32; 4],
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Params {
    screen_resolution: Resolution,
    _pad: [u32; 2],
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
}

pub(crate) struct State<'a> {
    pub(crate) device: &'a Device,
    pub(crate) queue: &'a Queue,
}
