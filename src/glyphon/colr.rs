//! COLRv1 rasterization.
//!
//! `swash` 0.2.10 — the only glyph scaler behind `cosmic-text` 0.19 — reads the `COLR`
//! table as **version 0 only**: it takes the record count from the v0 layout and
//! binary-searches it. For a COLRv1 font that field is `numBaseGlyphRecords`, which
//! nanoemoji-built fonts leave at 0, so the lookup misses and the scaler falls through to
//! the `glyf` outline. Android's emoji have no outline either, so the glyph rasterizes to
//! `Some(Image { width: 0, height: 0 })` and is dropped as `SkipRasterization` with no
//! warning. That is why emoji are blank on Android 12+ (`NotoColorEmoji.ttf` is COLRv1)
//! while regional-indicator flags still render: they live in a different font,
//! `NotoColorEmojiFlags.ttf`, which is CBDT bitmaps.
//!
//! Windows hits the same gap but escapes it by luck — `seguiemj.ttf` is also COLRv1 but
//! happens to have its v0 `BaseGlyphRecords` populated, which the v0 reader can use.
//!
//! So this module implements the missing piece: walk the COLRv1 paint graph with
//! [`skrifa`], rasterize with [`zeno`], composite into an RGBA buffer. Both crates are
//! already in the dependency tree (swash uses them), so this adds no new dependency.
//!
//! # Relationship to the rest of the text stack
//!
//! [`crate::glyphon::rasterize_glyph`] calls this only after swash has already declined
//! the glyph, so plain text and COLRv0/CBDT glyphs keep going through swash untouched. The
//! result comes back in the same shape swash produces — straight-alpha RGBA8 with a
//! `top`/`left` bearing — so the atlas, the placement math in `prepare_glyph`, and the
//! texture upload need no changes.
//!
//! # Bearing convention
//!
//! Matches swash's `Source::ColorOutline` path, because two glyphs in the same string
//! must land on the same baseline even when one comes from each code path. That path
//! sizes the bitmap from `outline.bounds()` — the union of the *path* bounding boxes in
//! y-up device coordinates — not from `ColorGlyph::bounding_box()`, which for COLRv1
//! returns the clip box (the em square for essentially every emoji) and would shift every
//! glyph horizontally. Hence the separate bounds pass below.
//!
//! Clip boxes are excluded from that union for the same reason. The cost is that a paint
//! graph filling a region bounded only by a clip box, with no glyph outline anywhere, gets
//! clipped to the collected bounds. No emoji font does that; every fill is driven by
//! `PaintGlyph` or `PaintColrGlyph`.
//!
//! # Colour
//!
//! No sRGB conversion happens here, matching swash and every other rasterizer: the values
//! are sRGB-encoded, and the atlas samples through an `Rgba8UnormSrgb` texture so the
//! conversion happens on the GPU (see [`crate::glyphon::ColorMode::Accurate`]).
//!
//! # Removal
//!
//! Delete this file when swash gains COLRv1 support
//! (<https://github.com/dfrg/swash/issues/21>). It is a permanent fork in the meantime:
//! swash is at its last release and that issue has had no activity since 2021.

use skrifa::color::{
    Brush, ColorGlyphFormat, ColorPainter, CompositeMode, Extend, PaintCachedColorGlyph,
    PaintError, Transform as ColrTransform,
};
use skrifa::instance::NormalizedCoord;
use skrifa::metrics::BoundingBox;
use skrifa::outline::pen::PathElement;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use zeno::{Fill, Mask, Origin, Scratch, Vector};

/// Straight-alpha RGBA8, top-down: what swash hands back and what the glyph atlas stores.
pub(crate) struct ColrGlyph {
    /// Baseline-to-top-edge distance in pixels, positive upwards. Same convention as
    /// `swash::scale::image::Placement`.
    pub top: i16,
    /// Left edge relative to the pen position.
    pub left: i16,
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

/// Everything needed to rasterize one COLRv1 glyph.
///
/// Mirrors the inputs `swash_image` derives from a `cosmic_text::CacheKey`, because the two
/// have to agree: one cache key must never produce two different images depending on which
/// rasterizer answered it.
pub(crate) struct ColrRequest<'a> {
    /// Whole font file, as `cosmic_text::Font::data()` returns it.
    pub font_data: &'a [u8],
    /// Face index within `font_data`, for collections (`.ttc`).
    pub face_index: u32,
    pub glyph_id: u16,
    /// Font size in pixels.
    pub size_px: f32,
    /// Subpixel offset from the cache key's `x_bin` / `y_bin`, in pixels.
    pub offset: (f32, f32),
    /// `CacheKeyFlags::FAKE_ITALIC`.
    pub fake_italic: bool,
    /// Normalized axis coordinates in the font's axis order, matching what cosmic-text
    /// hands swash.
    pub variations: &'a [NormalizedCoord],
}

/// Guards against a malformed font asking for an absurd bitmap.
const MAX_DIMENSION: f32 = 4096.0;

/// Forces this module to answer even when swash could have.
///
/// Windows would otherwise give the COLRv1 path zero coverage: `seguiemj.ttf` has v0
/// records, so every emoji there resolves through swash and desktop testing never
/// exercises the code Android depends on. Set it to run the new path on a platform that has
/// a COLRv1 font to hand.
fn force_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VIREO_COLR1_FORCE").is_some())
}

/// Whether `glyph_id` has a COLRv1 graph in `font`.
///
/// Checked before rasterizing so the common case — a glyph that is merely blank, like a
/// space — does not walk the paint graph twice.
pub(crate) fn has_colr_v1(font: &FontRef<'_>, glyph_id: GlyphId) -> bool {
    force_enabled()
        || font
            .color_glyphs()
            .get_with_format(glyph_id, ColorGlyphFormat::ColrV1)
            .is_some()
}

/// Rasterize a COLRv1 glyph, or `None` if the font has no COLRv1 graph for it.
///
/// Must stay a pure function of `request`: `InnerAtlas::grow` re-runs it for every cached
/// glyph and compares the result against the recorded size, so hidden state would let the
/// two calls disagree and leave glyphs pointing at blank atlas regions — silently, with no
/// panic and no log.
pub(crate) fn rasterize(request: &ColrRequest<'_>) -> Option<ColrGlyph> {
    let font = FontRef::from_index(request.font_data, request.face_index).ok()?;
    let glyph_id = GlyphId::new(u32::from(request.glyph_id));
    let color_glyph = font
        .color_glyphs()
        .get_with_format(glyph_id, ColorGlyphFormat::ColrV1)?;

    let location = skrifa::instance::LocationRef::new(request.variations);
    let upem = f32::from(
        font.metrics(skrifa::instance::Size::unscaled(), request.variations)
            .units_per_em,
    );
    if upem <= 0.0 || !request.size_px.is_finite() {
        return None;
    }

    let mut base = Mat::scale_translate(
        request.size_px / upem,
        Vector::new(request.offset.0, request.offset.1),
    );
    if request.fake_italic {
        // The same skew swash applies via `Transform::skew(Angle::from_degrees(14.0), ..)`,
        // composed after scaling so it acts on device coordinates.
        base = Mat::mul(&Mat::skew_x_deg(14.0), &base);
    }

    // Pass 1: where is the ink? A separate traversal rather than reusing the clip box,
    // because the clip box is the em square for essentially every emoji.
    let mut collector = BoundsCollector {
        font: &font,
        variations: request.variations,
        matrix: base,
        scratch: Scratch::new(),
        bounds: None,
        stack: Vec::new(),
    };
    color_glyph.paint(location, &mut collector).ok()?;
    let ink = collector.bounds?;
    if !ink.min.x.is_finite() || !ink.min.y.is_finite() {
        return None;
    }

    // swash's `Source::ColorOutline` sizing, kept identical so glyphs from the two
    // rasterizers share a baseline.
    let origin_x = (ink.min.x + request.offset.0).floor();
    let origin_y = (ink.min.y + request.offset.1).ceil();
    let width = ink.width().ceil();
    let height = ink.height().ceil();
    if width < 1.0 || height < 1.0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return None;
    }
    let width = width as u32;
    let height = height as u32;

    // Pass 2: paint.
    let mut painter = Painter {
        font: &font,
        location: request.variations,
        matrix: base,
        scratch: Scratch::new(),
        origin_x,
        origin_y,
        width,
        height,
        palette: PaletteCache { font: &font },
        canvas: Canvas::new(width, height),
        clips: Vec::new(),
        transforms: Vec::new(),
        layers: Vec::new(),
        scratch_mask: Vec::new(),
    };
    color_glyph.paint(location, &mut painter).ok()?;

    Some(ColrGlyph {
        top: (height as f32 + origin_y) as i16,
        left: origin_x as i16,
        width: width as u16,
        height: height as u16,
        data: painter.canvas.into_straight_alpha(),
    })
}

// ---------------------------------------------------------------------------
// Affine matrices
// ---------------------------------------------------------------------------

/// An affine matrix laid out to match `read_fonts::types::Matrix`, so a skrifa
/// [`ColrTransform`] converts field for field: `x' = xx*x + yx*y + dx`.
#[derive(Clone, Copy, Debug)]
struct Mat {
    xx: f32,
    xy: f32,
    yx: f32,
    yy: f32,
    dx: f32,
    dy: f32,
}

impl Mat {
    fn scale_translate(scale: f32, offset: Vector) -> Self {
        Self {
            xx: scale,
            xy: 0.0,
            yx: 0.0,
            yy: scale,
            dx: offset.x,
            dy: offset.y,
        }
    }

    fn skew_x_deg(degrees: f32) -> Self {
        Self {
            xx: 1.0,
            xy: 0.0,
            yx: degrees.to_radians().tan(),
            yy: 1.0,
            dx: 0.0,
            dy: 0.0,
        }
    }

    fn mul(a: &Self, b: &Self) -> Self {
        Self {
            xx: a.xx * b.xx + a.yx * b.xy,
            xy: a.xy * b.xx + a.yy * b.xy,
            yx: a.xx * b.yx + a.yx * b.yy,
            yy: a.xy * b.yx + a.yy * b.yy,
            dx: a.xx * b.dx + a.yx * b.dy + a.dx,
            dy: a.xy * b.dx + a.yy * b.dy + a.dy,
        }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.xx * x + self.yx * y + self.dx,
            self.xy * x + self.yy * y + self.dy,
        )
    }

    /// Mean absolute scale, for turning a radius in font units into device units. Only an
    /// approximation under non-uniform transforms; COLRv1 does not produce those for
    /// gradient geometry in practice.
    fn scale(&self) -> f32 {
        ((self.xx * self.xx + self.yx * self.yx) + (self.xy * self.xy + self.yy * self.yy)).sqrt()
            * 0.5
    }

    fn from_colr(transform: ColrTransform) -> Self {
        Self {
            xx: transform.xx,
            xy: transform.xy,
            yx: transform.yx,
            yy: transform.yy,
            dx: transform.dx,
            dy: transform.dy,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Pt {
    x: f32,
    y: f32,
}

impl From<Pt> for Vector {
    fn from(point: Pt) -> Self {
        Vector::new(point.x, point.y)
    }
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    min: Pt,
    max: Pt,
}

impl Rect {
    fn from_points(points: impl IntoIterator<Item = Pt>) -> Option<Self> {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut rect = Self {
            min: first,
            max: first,
        };
        for point in iter {
            rect.min.x = rect.min.x.min(point.x);
            rect.min.y = rect.min.y.min(point.y);
            rect.max.x = rect.max.x.max(point.x);
            rect.max.y = rect.max.y.max(point.y);
        }
        Some(rect)
    }

    fn width(&self) -> f32 {
        self.max.x - self.min.x
    }

    fn height(&self) -> f32 {
        self.max.y - self.min.y
    }

    fn union(&self, other: &Self) -> Self {
        Self {
            min: Pt {
                x: self.min.x.min(other.min.x),
                y: self.min.y.min(other.min.y),
            },
            max: Pt {
                x: self.max.x.max(other.max.x),
                y: self.max.y.max(other.max.y),
            },
        }
    }

    fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }
}

// ---------------------------------------------------------------------------
// Glyph outlines
// ---------------------------------------------------------------------------

/// Collect `glyph_id`'s path in **font units**, with variations applied.
///
/// `Size::unscaled()` keeps the output unscaled: COLRv1 paints everything through the
/// transform stack, so scaling here would apply twice.
fn glyph_path(
    font: &FontRef<'_>,
    glyph_id: GlyphId,
    variations: &[NormalizedCoord],
) -> Option<Vec<zeno::Command>> {
    let outline = font.outline_glyphs().get(glyph_id)?;
    let mut pen = Vec::<PathElement>::new();
    outline
        .draw(
            skrifa::outline::DrawSettings::unhinted(skrifa::instance::Size::unscaled(), variations),
            &mut pen,
        )
        .ok()?;
    Some(to_zeno(&pen))
}

fn to_zeno(elements: &[PathElement]) -> Vec<zeno::Command> {
    elements
        .iter()
        .map(|element| match *element {
            PathElement::MoveTo { x, y } => zeno::Command::MoveTo(Vector::new(x, y)),
            PathElement::LineTo { x, y } => zeno::Command::LineTo(Vector::new(x, y)),
            PathElement::QuadTo { cx0, cy0, x, y } => {
                zeno::Command::QuadTo(Vector::new(cx0, cy0), Vector::new(x, y))
            }
            PathElement::CurveTo {
                cx0,
                cy0,
                cx1,
                cy1,
                x,
                y,
            } => zeno::Command::CurveTo(
                Vector::new(cx0, cy0),
                Vector::new(cx1, cy1),
                Vector::new(x, y),
            ),
            PathElement::Close => zeno::Command::Close,
        })
        .collect()
}

fn map_path(path: &mut [zeno::Command], map: &impl Fn(f32, f32) -> (f32, f32)) {
    for command in path {
        *command = match *command {
            zeno::Command::MoveTo(point) => {
                let (x, y) = map(point.x, point.y);
                zeno::Command::MoveTo(Vector::new(x, y))
            }
            zeno::Command::LineTo(point) => {
                let (x, y) = map(point.x, point.y);
                zeno::Command::LineTo(Vector::new(x, y))
            }
            zeno::Command::QuadTo(control, end) => {
                let (cx, cy) = map(control.x, control.y);
                let (x, y) = map(end.x, end.y);
                zeno::Command::QuadTo(Vector::new(cx, cy), Vector::new(x, y))
            }
            zeno::Command::CurveTo(c0, c1, end) => {
                let (c0x, c0y) = map(c0.x, c0.y);
                let (c1x, c1y) = map(c1.x, c1.y);
                let (x, y) = map(end.x, end.y);
                zeno::Command::CurveTo(
                    Vector::new(c0x, c0y),
                    Vector::new(c1x, c1y),
                    Vector::new(x, y),
                )
            }
            zeno::Command::Close => zeno::Command::Close,
        };
    }
}

/// Transform a font-unit path into y-down canvas-local pixel coordinates.
fn path_to_canvas(
    path: &[zeno::Command],
    matrix: Mat,
    origin_x: f32,
    top: f32,
) -> Vec<zeno::Command> {
    let mut out = path.to_vec();
    map_path(&mut out, &|x, y| {
        let (device_x, device_y) = matrix.apply(x, y);
        (device_x - origin_x, top - device_y)
    });
    out
}

/// Rasterize a canvas-local path into a full-canvas coverage mask.
///
/// `TopLeft` plus an explicit size plus a zero offset makes the buffer line up with the
/// canvas one-to-one: buffer row 0 is canvas row 0.
fn rasterize_coverage(
    path: &[zeno::Command],
    width: u32,
    height: u32,
    scratch: &mut Scratch,
    out: &mut Vec<u8>,
) {
    out.clear();
    out.resize((width * height) as usize, 0);
    Mask::with_scratch(path, scratch)
        .style(Fill::NonZero)
        .origin(Origin::TopLeft)
        .offset(Vector::ZERO)
        .render_offset(Vector::ZERO)
        .size(width, height)
        .render_into(out, None);
}

/// Intersect two coverage masks.
fn intersect(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter()
        .zip(b)
        .map(|(x, y)| ((*x as u16 * *y as u16) / 255) as u8)
        .collect()
}

// ---------------------------------------------------------------------------
// Canvas
// ---------------------------------------------------------------------------

/// Premultiplied RGBA8, top-down.
///
/// Premultiplied because that is the only space the Porter-Duff and separable blend
/// formulas are defined in; the conversion to the straight alpha the atlas stores happens
/// once, in [`Canvas::into_straight_alpha`].
#[derive(Clone)]
struct Canvas {
    data: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            data: vec![0; (width * height * 4) as usize],
        }
    }

    /// Demultiply to the straight alpha the atlas stores.
    ///
    /// `a == 0` has to be special-cased: the colour of a fully transparent pixel is
    /// undefined, and `rgb * 255 / 0` would be a division by zero.
    fn into_straight_alpha(mut self) -> Vec<u8> {
        for pixel in self.data.chunks_exact_mut(4) {
            let alpha = pixel[3];
            if alpha == 0 {
                pixel[0] = 0;
                pixel[1] = 0;
                pixel[2] = 0;
            } else if alpha < 255 {
                pixel[0] = ((pixel[0] as u32 * 255) / alpha as u32) as u8;
                pixel[1] = ((pixel[1] as u32 * 255) / alpha as u32) as u8;
                pixel[2] = ((pixel[2] as u32 * 255) / alpha as u32) as u8;
            }
        }
        self.data
    }
}

// ---------------------------------------------------------------------------
// Pass 1: bounds
// ---------------------------------------------------------------------------

/// First traversal: unions the bounds of every glyph outline the paint graph fills.
///
/// Only the transform stack and `push_clip_glyph` matter. Clip boxes are deliberately
/// ignored, because a clip box is allowed to be larger than the ink and using it would pad
/// every glyph out to the em square.
struct BoundsCollector<'a> {
    font: &'a FontRef<'a>,
    variations: &'a [NormalizedCoord],
    matrix: Mat,
    scratch: Scratch,
    bounds: Option<Rect>,
    stack: Vec<Mat>,
}

impl ColorPainter for BoundsCollector<'_> {
    fn push_transform(&mut self, transform: ColrTransform) {
        self.stack.push(self.matrix);
        self.matrix = Mat::mul(&self.matrix, &Mat::from_colr(transform));
    }

    fn pop_transform(&mut self) {
        if let Some(previous) = self.stack.pop() {
            self.matrix = previous;
        }
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        let Some(mut path) = glyph_path(self.font, glyph_id, self.variations) else {
            return;
        };
        map_path(&mut path, &|x, y| self.matrix.apply(x, y));
        let bounds = self.scratch.bounds(&path, Fill::NonZero, None);
        let Some(rect) = Rect::from_points([
            Pt {
                x: bounds.min.x,
                y: bounds.min.y,
            },
            Pt {
                x: bounds.max.x,
                y: bounds.max.y,
            },
        ]) else {
            return;
        };
        if rect.is_empty() {
            return;
        }
        self.bounds = Some(match self.bounds {
            Some(existing) => existing.union(&rect),
            None => rect,
        });
    }

    fn push_clip_box(&mut self, _clip_box: BoundingBox) {}

    fn pop_clip(&mut self) {}

    fn fill(&mut self, _brush: Brush<'_>) {}

    fn push_layer(&mut self, _composite_mode: CompositeMode) {}

    fn pop_layer(&mut self) {}
}

// ---------------------------------------------------------------------------
// Pass 2: paint
// ---------------------------------------------------------------------------

struct Painter<'a> {
    font: &'a FontRef<'a>,
    location: &'a [NormalizedCoord],
    matrix: Mat,
    scratch: Scratch,
    /// Canvas placement in device coordinates (y-up, origin at the pen): `origin_x` is the
    /// left edge and `origin_y` the bottom edge.
    origin_x: f32,
    origin_y: f32,
    width: u32,
    height: u32,
    palette: PaletteCache<'a>,
    canvas: Canvas,
    /// One accumulated clip per pushed clip; `None` means "no clip", which is different
    /// from "clipped to nothing".
    clips: Vec<Option<Vec<u8>>>,
    /// Previous `matrix` per pushed transform. Required, not optional: COLRv1 uses
    /// `PaintTransform` for the highlight and shading layers of most emoji, and leaving the
    /// pushes un-popped skews every later layer.
    transforms: Vec<Mat>,
    layers: Vec<Canvas>,
    /// Reused between fills so a paint graph with many shapes does not reallocate per
    /// shape.
    scratch_mask: Vec<u8>,
}

impl<'a> Painter<'a> {
    /// Device y of the canvas's top edge.
    fn top(&self) -> f32 {
        self.origin_y + self.height as f32
    }

    /// Device (y-up) coordinate of a pixel centre.
    fn device_point(&self, column: u32, row: u32) -> (f32, f32) {
        (
            self.origin_x + column as f32 + 0.5,
            self.top() - row as f32 - 0.5,
        )
    }

    fn glyph_commands(&mut self, glyph_id: GlyphId) -> Option<Vec<zeno::Command>> {
        glyph_path(self.font, glyph_id, self.location)
    }

    /// Intersect a freshly rasterized region into the accumulated clip.
    fn push_region(&mut self, region: Vec<u8>) {
        let region = match self.clips.last().and_then(|clip| clip.clone()) {
            Some(existing) => intersect(&existing, &region),
            None => region,
        };
        self.clips.push(Some(region));
    }

    /// Rasterize `path` (canvas-local) into a full-canvas coverage mask, reusing
    /// `scratch_mask` so a paint graph with many shapes does not reallocate per shape.
    fn coverage_of(&mut self, path: &[zeno::Command]) -> Vec<u8> {
        let mut region = std::mem::take(&mut self.scratch_mask);
        rasterize_coverage(
            path,
            self.width,
            self.height,
            &mut self.scratch,
            &mut region,
        );
        self.scratch_mask.clear();
        region
    }

    /// Composite `brush` over the region described by `coverage` (or the whole canvas when
    /// `coverage` is `None` and there is no clip either).
    fn fill_coverage(&mut self, brush: &Brush<'_>, coverage: Option<Vec<u8>>) {
        let clip = self.clips.last().and_then(|clip| clip.clone());
        let coverage = match (coverage, clip) {
            (Some(coverage), Some(clip)) => Some(intersect(&coverage, &clip)),
            (Some(coverage), None) => Some(coverage),
            (None, Some(clip)) => Some(clip),
            (None, None) => None,
        };
        let Some(coverage) = coverage else {
            return;
        };

        let raster = Raster::resolve(brush, &self.matrix, &self.palette);
        for row in 0..self.height {
            for column in 0..self.width {
                let index = (row * self.width + column) as usize;
                let coverage = coverage[index];
                if coverage == 0 {
                    continue;
                }
                let (x, y) = self.device_point(column, row);
                let [r, g, b, a] = raster.color_at(x, y);
                let alpha = (a as u32 * coverage as u32 / 255) as u8;
                if alpha == 0 {
                    continue;
                }
                let pixel = index * 4;
                source_over(
                    &mut self.canvas.data[pixel..pixel + 4],
                    [
                        (r as u16 * alpha as u16 / 255) as u8,
                        (g as u16 * alpha as u16 / 255) as u8,
                        (b as u16 * alpha as u16 / 255) as u8,
                        alpha,
                    ],
                );
            }
        }
    }
}

impl ColorPainter for Painter<'_> {
    fn push_transform(&mut self, transform: ColrTransform) {
        self.transforms.push(self.matrix);
        self.matrix = Mat::mul(&self.matrix, &Mat::from_colr(transform));
    }

    fn pop_transform(&mut self) {
        if let Some(previous) = self.transforms.pop() {
            self.matrix = previous;
        }
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        let Some(path) = self.glyph_commands(glyph_id) else {
            // A clip glyph that cannot be resolved clips everything away, which is what the
            // spec says happens when a referenced glyph is missing.
            self.clips
                .push(Some(vec![0; (self.width * self.height) as usize]));
            return;
        };
        let path = path_to_canvas(&path, self.matrix, self.origin_x, self.top());
        let region = self.coverage_of(&path);
        self.push_region(region);
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox) {
        // A reversed box means "no clip at all" per the spec, not "clip to nothing".
        let corners = [
            Pt {
                x: clip_box.x_min,
                y: clip_box.y_min,
            },
            Pt {
                x: clip_box.x_max,
                y: clip_box.y_min,
            },
            Pt {
                x: clip_box.x_max,
                y: clip_box.y_max,
            },
            Pt {
                x: clip_box.x_min,
                y: clip_box.y_max,
            },
        ];
        if Rect::from_points(corners).is_none_or(|rect| rect.is_empty()) {
            self.clips.push(None);
            return;
        }
        let corners = corners.map(|corner| {
            let (x, y) = self.matrix.apply(corner.x, corner.y);
            Pt { x, y }
        });
        let top = self.top();
        let origin_x = self.origin_x;
        let path = vec![
            zeno::Command::MoveTo(Vector::new(corners[0].x - origin_x, top - corners[0].y)),
            zeno::Command::LineTo(Vector::new(corners[1].x - origin_x, top - corners[1].y)),
            zeno::Command::LineTo(Vector::new(corners[2].x - origin_x, top - corners[2].y)),
            zeno::Command::LineTo(Vector::new(corners[3].x - origin_x, top - corners[3].y)),
            zeno::Command::Close,
        ];
        let region = self.coverage_of(&path);
        self.push_region(region);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn fill(&mut self, brush: Brush<'_>) {
        self.fill_coverage(&brush, None);
    }

    fn fill_glyph(
        &mut self,
        glyph_id: GlyphId,
        brush_transform: Option<ColrTransform>,
        brush: Brush<'_>,
    ) {
        // Clip and fill in one step. Going through `push_clip_glyph` would rasterize a
        // full-canvas mask, read it back per pixel, and intersect a second time.
        let Some(path) = self.glyph_commands(glyph_id) else {
            return;
        };
        let matrix = match brush_transform {
            Some(transform) => Mat::mul(&self.matrix, &Mat::from_colr(transform)),
            None => self.matrix,
        };
        let path = path_to_canvas(&path, matrix, self.origin_x, self.top());
        let region = self.coverage_of(&path);
        self.fill_coverage(&brush, Some(region));
    }

    fn paint_cached_color_glyph(
        &mut self,
        _glyph: GlyphId,
    ) -> Result<PaintCachedColorGlyph, PaintError> {
        Ok(PaintCachedColorGlyph::Unimplemented)
    }

    fn push_layer(&mut self, _composite_mode: CompositeMode) {
        self.layers.push(self.canvas.clone());
    }

    fn pop_layer_with_mode(&mut self, composite_mode: CompositeMode) {
        let Some(backdrop) = self.layers.pop() else {
            return;
        };
        // `push_layer` already snapshotted the backdrop, so the copy here is the price of
        // getting the layer contents out from behind the borrow.
        let layer = std::mem::replace(&mut self.canvas, backdrop.clone());
        composite(&mut self.canvas, &layer, composite_mode);
    }
}

// ---------------------------------------------------------------------------
// Palettes and gradients
// ---------------------------------------------------------------------------

/// Resolves `CPAL` palette indices to RGBA.
struct PaletteCache<'a> {
    font: &'a FontRef<'a>,
}

impl PaletteCache<'_> {
    /// COLRv1 paints always reference palette 0 — the v1 table has no palette index of its
    /// own.
    fn color(&self, index: u16) -> [u8; 4] {
        let palettes = self.font.color_palettes();
        if let Some(palette) = palettes.get(0)
            && let Some(color) = palette.colors().get(index as usize)
        {
            return [color.red, color.green, color.blue, color.alpha];
        }
        [0, 0, 0, 255]
    }
}

/// A brush reduced to something cheap to sample per pixel.
enum Raster {
    Solid([u8; 4]),
    Linear {
        p0: Pt,
        p1: Pt,
        ramp: Ramp,
    },
    Radial {
        c0: Pt,
        r0: f32,
        c1: Pt,
        r1: f32,
        ramp: Ramp,
    },
    Sweep {
        c0: Pt,
        start: f32,
        end: f32,
        ramp: Ramp,
    },
}

impl Raster {
    fn resolve(brush: &Brush<'_>, matrix: &Mat, palette: &PaletteCache<'_>) -> Self {
        match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => {
                let mut color = palette.color(*palette_index);
                color[3] = (f32::from(color[3]) * alpha).round().clamp(0.0, 255.0) as u8;
                Raster::Solid(color)
            }
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => {
                let (x0, y0) = matrix.apply(p0.x, p0.y);
                let (x1, y1) = matrix.apply(p1.x, p1.y);
                Raster::Linear {
                    p0: Pt { x: x0, y: y0 },
                    p1: Pt { x: x1, y: y1 },
                    ramp: Ramp::new(resolve_stops(color_stops, palette), *extend),
                }
            }
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => {
                let scale = matrix.scale();
                let (x0, y0) = matrix.apply(c0.x, c0.y);
                let (x1, y1) = matrix.apply(c1.x, c1.y);
                Raster::Radial {
                    c0: Pt { x: x0, y: y0 },
                    r0: r0 * scale,
                    c1: Pt { x: x1, y: y1 },
                    r1: r1 * scale,
                    ramp: Ramp::new(resolve_stops(color_stops, palette), *extend),
                }
            }
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => {
                let (x0, y0) = matrix.apply(c0.x, c0.y);
                Raster::Sweep {
                    c0: Pt { x: x0, y: y0 },
                    start: *start_angle,
                    end: *end_angle,
                    ramp: Ramp::new(resolve_stops(color_stops, palette), *extend),
                }
            }
        }
    }

    fn color_at(&self, x: f32, y: f32) -> [u8; 4] {
        match self {
            Raster::Solid(color) => *color,
            Raster::Linear { p0, p1, ramp } => {
                let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
                let length_squared = dx * dx + dy * dy;
                if length_squared <= f32::EPSILON {
                    // Degenerate axis: the spec says treat it as a solid fill.
                    return ramp.first();
                }
                let t = ((x - p0.x) * dx + (y - p0.y) * dy) / length_squared;
                ramp.sample(t)
            }
            Raster::Radial {
                c0,
                r0,
                c1,
                r1,
                ramp,
            } => ramp.sample(two_point_conical(*c0, *r0, *c1, *r1, Pt { x, y })),
            Raster::Sweep {
                c0,
                start,
                end,
                ramp,
            } => {
                let total = end - start;
                if total.abs() <= f32::EPSILON {
                    return ramp.first();
                }
                // skrifa hands the angles back already swapped to clockwise, so negate the
                // pixel's counter-clockwise angle to land in the same convention.
                let angle = (y - c0.y).atan2(x - c0.x).to_degrees();
                ramp.sample((-angle - start) / total)
            }
        }
    }
}

/// Position along the colour line of a two-point conical gradient.
///
/// The colour line is parameterized by the cone surface through `(c0, r0)` and
/// `(c1, r1)`: the value `t` is whichever of the two cone parameters puts the surface
/// through the pixel, nearest end first. Note this is *not* the projection onto the axis —
/// for a cylinder (`r0 == r1`) a pixel on the axis lands at `t = 0.3` rather than `0.5`,
/// because the surface reaches it twice along its length. That is the geometry the
/// specification describes and what Skia implements, not an artifact here.
///
/// Negative radii are not resolved properly: the spec wants the colour line truncated at
/// zero radius and re-interpolated across it. No emoji font uses them.
fn two_point_conical(c0: Pt, r0: f32, c1: Pt, r1: f32, p: Pt) -> f32 {
    let dx = c1.x - c0.x;
    let dy = c1.y - c0.y;
    let dr = r1 - r0;
    let px = p.x - c0.x;
    let py = p.y - c0.y;

    // Concentric circles degenerate to a plain radial ramp.
    if dx.abs() <= f32::EPSILON && dy.abs() <= f32::EPSILON {
        let distance = (px * px + py * py).sqrt();
        if dr.abs() <= f32::EPSILON {
            return 0.0;
        }
        return (distance - r0) / dr;
    }

    // |P - (c0 + t*d)| == r0 + t*dr, expanded: a*t^2 - 2*b*t + c == 0.
    let a = dx * dx + dy * dy - dr * dr;
    let b = px * dx + py * dy + r0 * dr;
    let c = px * px + py * py - r0 * r0;

    if a.abs() <= 1e-6 {
        // A 45-degree cone: the quadratic term vanishes and the rest is linear.
        if b.abs() <= 1e-6 {
            return 0.0;
        }
        return c / (2.0 * b);
    }

    let discriminant = b * b - a * c;
    if discriminant < 0.0 {
        // Outside the cone.
        return 0.0;
    }
    let root = discriminant.sqrt();
    let t0 = (b - root) / a;
    let t1 = (b + root) / a;
    t0.min(t1)
}

/// A colour line plus its `Extend` behaviour.
struct Ramp {
    stops: Vec<(f32, [u8; 4])>,
    extend: Extend,
}

impl Ramp {
    fn new(stops: Vec<(f32, [u8; 4])>, extend: Extend) -> Self {
        Self { stops, extend }
    }

    fn first(&self) -> [u8; 4] {
        self.stops.first().map_or([0, 0, 0, 0], |stop| stop.1)
    }

    fn sample(&self, t: f32) -> [u8; 4] {
        let stops = &self.stops;
        if stops.is_empty() {
            return [0, 0, 0, 0];
        }
        if stops.len() == 1 {
            return stops[0].1;
        }

        let t = match self.extend {
            Extend::Pad => t.clamp(0.0, 1.0),
            // `Repeat` is the mathematical definition. `Unknown` is treated the same way,
            // which is the safe reading for a malformed font.
            Extend::Repeat | Extend::Unknown => {
                let value = t - t.floor();
                if value < 0.0 { value + 1.0 } else { value }
            }
            Extend::Reflect => {
                let value = t.abs() % 2.0;
                if value > 1.0 { 2.0 - value } else { value }
            }
        };

        let first = stops[0];
        let last = stops[stops.len() - 1];
        if t <= first.0 {
            return first.1;
        }
        if t >= last.0 {
            return last.1;
        }
        for window in stops.windows(2) {
            let (a_offset, a_color) = window[0];
            let (b_offset, b_color) = window[1];
            if t >= a_offset && t <= b_offset {
                let span = b_offset - a_offset;
                let f = if span <= f32::EPSILON {
                    0.0
                } else {
                    (t - a_offset) / span
                };
                return [
                    lerp(a_color[0], b_color[0], f),
                    lerp(a_color[1], b_color[1], f),
                    lerp(a_color[2], b_color[2], f),
                    lerp(a_color[3], b_color[3], f),
                ];
            }
        }
        last.1
    }
}

fn resolve_stops(
    stops: &[skrifa::color::ColorStop],
    palette: &PaletteCache<'_>,
) -> Vec<(f32, [u8; 4])> {
    stops
        .iter()
        .map(|stop| {
            let mut color = palette.color(stop.palette_index);
            color[3] = (f32::from(color[3]) * stop.alpha).round().clamp(0.0, 255.0) as u8;
            (stop.offset, color)
        })
        .collect()
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (f32::from(a) + (f32::from(b) - f32::from(a)) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

// ---------------------------------------------------------------------------
// Compositing
// ---------------------------------------------------------------------------

/// Premultiplied straight-`source over` onto one canvas pixel.
fn source_over(pixel: &mut [u8], source: [u8; 4]) {
    let alpha = u32::from(source[3]);
    if alpha == 255 {
        pixel.copy_from_slice(&source);
        return;
    }
    let inverse = 255 - alpha;
    for channel in 0..4 {
        pixel[channel] =
            ((source[channel] as u32 * alpha + pixel[channel] as u32 * inverse) / 255) as u8;
    }
}

/// Merge a layer into its backdrop with `mode`.
///
/// The twelve Porter-Duff operators are exact. The separable blend and HSL families fall
/// back to `SrcOver`, which is what a font effectively gets if it uses them; no shipped
/// emoji font does.
fn composite(backdrop: &mut Canvas, source: &Canvas, mode: CompositeMode) {
    for index in 0..backdrop.data.len() / 4 {
        let pixel = index * 4;
        let src: [u8; 4] = source.data[pixel..pixel + 4]
            .try_into()
            .expect("4 bytes per pixel");
        let dst = &mut backdrop.data[pixel..pixel + 4];
        let dst_alpha = f32::from(dst[3]);

        match mode {
            CompositeMode::SrcOver | CompositeMode::Unknown => {
                source_over(dst, src);
            }
            CompositeMode::Src => dst.copy_from_slice(&src),
            CompositeMode::Dest => {}
            CompositeMode::Clear => dst.fill(0),
            CompositeMode::DestOver => {
                // Backdrop over source, with the backdrop keeping its own alpha.
                for channel in 0..3 {
                    let backdrop_part = f32::from(dst[channel]) * dst_alpha;
                    let source_part =
                        f32::from(src[channel]) * f32::from(src[3]) * (255.0 - dst_alpha) / 255.0;
                    dst[channel] = ((backdrop_part + source_part) / 255.0).round() as u8;
                }
                dst[3] = (dst_alpha + f32::from(src[3]) * (255.0 - dst_alpha) / 255.0) as u8;
            }
            CompositeMode::DestIn => scale(dst, f32::from(src[3]) / 255.0),
            CompositeMode::SrcIn => {
                dst.copy_from_slice(&src);
                scale(dst, dst_alpha / 255.0);
            }
            CompositeMode::DestOut => scale(dst, 1.0 - f32::from(src[3]) / 255.0),
            CompositeMode::SrcOut => {
                dst.copy_from_slice(&src);
                scale(dst, 1.0 - dst_alpha / 255.0);
            }
            CompositeMode::DestAtop => {
                for channel in 0..3 {
                    dst[channel] = (f32::from(src[channel]) * f32::from(src[3]) / 255.0) as u8;
                }
            }
            CompositeMode::SrcAtop => {
                let source_alpha = f32::from(src[3]);
                for channel in 0..3 {
                    dst[channel] = ((f32::from(src[channel]) * dst_alpha / 255.0)
                        + f32::from(dst[channel]) * (255.0 - source_alpha) / 255.0)
                        .round() as u8;
                }
                dst[3] = src[3];
            }
            CompositeMode::Xor => {
                let source_alpha = f32::from(src[3]);
                for channel in 0..3 {
                    dst[channel] = ((f32::from(src[channel]) * source_alpha * (255.0 - dst_alpha)
                        + f32::from(dst[channel]) * dst_alpha * (255.0 - source_alpha))
                        / (255.0 * 255.0)) as u8;
                }
                dst[3] = ((source_alpha * (255.0 - dst_alpha) + dst_alpha * (255.0 - source_alpha))
                    / 255.0) as u8;
            }
            CompositeMode::Plus => {
                for channel in 0..4 {
                    dst[channel] = dst[channel].saturating_add(src[channel]);
                }
            }
            _ => source_over(dst, src),
        }
    }
}

fn scale(pixel: &mut [u8], factor: f32) {
    for channel in pixel.iter_mut() {
        *channel = (f32::from(*channel) * factor).round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A COLRv1 font that is present on the machine running the tests.
    ///
    /// `seguiemj.ttf` is COLRv1, and — unlike Android's emoji font — it also has its v0
    /// `BaseGlyphRecords` populated, which is exactly why desktop emoji render today and
    /// why this path would otherwise never be exercised outside Android.
    fn colr_v1_font() -> Option<&'static [u8]> {
        const CANDIDATES: &[&str] = &[
            r"C:\Windows\Fonts\seguiemj.ttf",
            "/System/Library/Fonts/Apple Color Emoji.ttc",
            "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        ];
        CANDIDATES
            .iter()
            .find_map(|path| std::fs::read(path).ok())
            .map(|data| Box::leak(data.into_boxed_slice()) as &'static [u8])
    }

    struct Rasterized {
        glyph: ColrGlyph,
    }

    fn rasterize_codepoint(data: &[u8], codepoint: char, size_px: f32) -> Option<Rasterized> {
        let font = FontRef::new(data).ok()?;
        let glyph_id = font.charmap().map(codepoint)?;
        let glyph = rasterize(&ColrRequest {
            font_data: data,
            face_index: 0,
            glyph_id: glyph_id.to_u32() as u16,
            size_px,
            offset: (0.0, 0.0),
            fake_italic: false,
            variations: &[],
        })?;
        Some(Rasterized { glyph })
    }

    #[test]
    fn rasterizes_a_colr_v1_glyph_to_coloured_pixels() {
        let Some(data) = colr_v1_font() else {
            eprintln!("skipping: no COLRv1 font on this machine");
            return;
        };

        // U+1F600, a multi-layer colour emoji.
        let Some(Rasterized { glyph }) = rasterize_codepoint(data, '\u{1F600}', 64.0) else {
            eprintln!("skipping: font has no COLRv1 graph for U+1F600");
            return;
        };

        assert!(glyph.width > 0 && glyph.height > 0, "bitmap is empty");
        assert_eq!(
            glyph.data.len(),
            glyph.width as usize * glyph.height as usize * 4,
            "data length does not match the reported size"
        );

        let opaque: Vec<&[u8]> = glyph
            .data
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 200)
            .collect();
        assert!(
            opaque.len() > 64,
            "only {} fully opaque pixels — nothing was drawn",
            opaque.len()
        );

        // A black silhouette would mean the palette never resolved. This emoji is yellow,
        // black and white, so several distinct hues must show up.
        let colours: HashSet<[u8; 3]> = opaque
            .iter()
            .map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        assert!(
            colours.len() >= 3,
            "only {} distinct colours — the glyph came out flat",
            colours.len()
        );
    }

    /// Writes one glyph magnified over a checkerboard, so a half-pixel offset, a vertical
    /// flip or a soft coverage edge is obvious.
    #[test]
    fn writes_a_magnified_glyph() {
        let Some(data) = colr_v1_font() else {
            return;
        };
        let Some(Rasterized { glyph }) = rasterize_codepoint(data, '\u{1F600}', 64.0) else {
            return;
        };

        let scale = 8u32;
        let width = u32::from(glyph.width) * scale;
        let height = u32::from(glyph.height) * scale;
        let mut out = vec![0u8; (width * height * 4) as usize];
        for row in 0..height {
            for column in 0..width {
                let source_x = (column / scale) as usize;
                let source_y = (row / scale) as usize;
                let source = &glyph.data[(source_y * glyph.width as usize + source_x) * 4..][..4];
                let checker = ((column / scale / 4) + (row / scale / 4)).is_multiple_of(2);
                let background = if checker { 70u8 } else { 40u8 };
                let alpha = f32::from(source[3]) / 255.0;
                let target = ((row * width + column) as usize) * 4;
                for channel in 0..3 {
                    out[target + channel] = (f32::from(source[channel]) * alpha
                        + f32::from(background) * (1.0 - alpha))
                        as u8;
                }
                out[target + 3] = 255;
            }
        }

        let image = image::RgbaImage::from_raw(width, height, out).expect("sized");
        image
            .save("target/colr_magnified.png")
            .expect("write magnified glyph");
    }

    /// Writes a contact sheet of emoji to `target/colr_probe.png` so the output can be
    /// inspected by eye rather than only asserted on.
    ///
    /// A hand-written rasterizer fails in ways no assertion catches well: half a pixel
    /// offset, an upside-down glyph, fringes from a wrong blend, banding in a gradient. All
    /// of those are obvious in a picture and invisible in a count.
    #[test]
    fn writes_a_contact_sheet_for_visual_inspection() {
        let Some(data) = colr_v1_font() else {
            return;
        };

        // Only single codepoints: multi-codepoint sequences (ZWJ, VS16, keycaps) are a
        // *shaping* concern handled by cosmic-text before a glyph ever reaches here, and each
        // resulting glyph goes through exactly this path.
        const SAMPLES: &[(char, &str)] = &[
            ('\u{1F600}', "grinning: layered solids"),
            ('\u{1F351}', "peach: radial gradient"),
            ('\u{1F308}', "rainbow: linear gradients"),
            ('\u{1F984}', "unicorn: many layers"),
            ('\u{1F3B5}', "note: solid"),
            ('\u{1F36D}', "lollipop: radial"),
            ('\u{1F440}', "eyes: solids"),
            ('\u{1F41B}', "lady beetle: solids"),
        ];

        let cell = 72u32;
        let sheet_width = SAMPLES.len() as u32 * cell;
        let mut sheet = vec![32u8; (sheet_width * cell * 4) as usize];
        let mut drawn = 0;

        for (index, (codepoint, _name)) in SAMPLES.iter().enumerate() {
            let Some(Rasterized { glyph }) = rasterize_codepoint(data, *codepoint, 64.0) else {
                continue;
            };
            drawn += 1;
            let origin_x = index as u32 * cell;

            // Composite onto the sheet so the shape is visible against a dark background,
            // matching what the atlas will do at draw time.
            let glyph_height = u32::from(glyph.height).min(cell);
            let glyph_width = u32::from(glyph.width).min(cell);
            for row in 0..glyph_height {
                for column in 0..glyph_width {
                    let source =
                        &glyph.data[((row * u32::from(glyph.width) + column) as usize) * 4..][..4];
                    let alpha = source[3];
                    if alpha == 0 {
                        continue;
                    }
                    let target = ((row * sheet_width + origin_x + column) as usize) * 4;
                    for channel in 0..3 {
                        let src = source[channel];
                        let dst = sheet[target + channel];
                        sheet[target + channel] = ((u32::from(src) * u32::from(alpha)
                            + u32::from(dst) * (255 - u32::from(alpha)))
                            / 255) as u8;
                    }
                    sheet[target + 3] = 255;
                }
            }
        }

        assert!(drawn > 0, "no emoji could be rasterized at all");

        let image = image::RgbaImage::from_raw(sheet_width, cell, sheet).expect("sheet is sized");
        let path = std::path::Path::new("target/colr_probe.png");
        image.save(path).expect("write contact sheet");
        eprintln!("wrote {} with {drawn} glyphs", path.display());
    }

    #[test]
    fn bitmap_sits_above_the_baseline() {
        let Some(data) = colr_v1_font() else {
            return;
        };
        let Some(Rasterized { glyph }) = rasterize_codepoint(data, '\u{1F600}', 64.0) else {
            return;
        };

        // Emoji draw above the baseline, so `top` — the baseline-to-top-edge distance —
        // must be positive, and the bitmap must not be taller than a couple of ems.
        assert!(glyph.top > 0, "top = {} should be positive", glyph.top);
        assert!(
            glyph.height < 256,
            "height {} is far past one em at 64px",
            glyph.height
        );
    }

    #[test]
    fn size_scales_the_bitmap() {
        let Some(data) = colr_v1_font() else {
            return;
        };
        let small = rasterize_codepoint(data, '\u{1F600}', 32.0);
        let large = rasterize_codepoint(data, '\u{1F600}', 64.0);
        let (Some(small), Some(large)) = (small, large) else {
            return;
        };

        let ratio = large.glyph.height as f32 / small.glyph.height as f32;
        assert!(
            (1.8..2.2).contains(&ratio),
            "doubling the size only changed the height by {ratio}x"
        );
    }

    #[test]
    fn rasterization_is_deterministic() {
        let Some(data) = colr_v1_font() else {
            return;
        };
        let Some(first) = rasterize_codepoint(data, '\u{1F600}', 48.0) else {
            return;
        };
        let Some(second) = rasterize_codepoint(data, '\u{1F600}', 48.0) else {
            return;
        };

        // `InnerAtlas::grow` re-runs this and compares against the recorded size, so two
        // calls disagreeing would blank the glyph after the atlas outgrows its initial
        // size — with no panic and no log.
        assert_eq!(first.glyph.width, second.glyph.width);
        assert_eq!(first.glyph.height, second.glyph.height);
        assert_eq!(first.glyph.top, second.glyph.top);
        assert_eq!(first.glyph.left, second.glyph.left);
        assert_eq!(first.glyph.data, second.glyph.data);
    }

    #[test]
    fn a_font_without_colr_v1_produces_nothing() {
        let Some(data) = colr_v1_font() else {
            return;
        };
        let font = FontRef::new(data).unwrap();
        // U+0041 is a plain letter: no paint graph, so this must decline rather than
        // fabricate a bitmap.
        let Some(glyph_id) = font.charmap().map('A') else {
            return;
        };
        assert!(
            rasterize(&ColrRequest {
                font_data: data,
                face_index: 0,
                glyph_id: glyph_id.to_u32() as u16,
                size_px: 32.0,
                offset: (0.0, 0.0),
                fake_italic: false,
                variations: &[],
            })
            .is_none()
        );
    }

    #[test]
    fn two_point_conical_finds_the_nearer_cone_surface() {
        let c0 = Pt { x: 0.0, y: 0.0 };
        let c1 = Pt { x: 100.0, y: 0.0 };

        // A point on the first circle's surface is at t = 0.
        let t = two_point_conical(c0, 20.0, c1, 20.0, Pt { x: 20.0, y: 0.0 });
        assert!(t.abs() < 1e-3, "on the first circle gave {t}, expected 0");

        // A cylinder's surface reaches every point on it twice along its length, and the
        // nearer crossing always wins. This is the geometry the specification describes, not
        // a projection onto the axis — so a point on the *second* circle still comes out at
        // 0.6, not 1.0, and a point on the axis comes out at 0.3, not 0.5.
        let t = two_point_conical(c0, 20.0, c1, 20.0, Pt { x: 80.0, y: 0.0 });
        assert!(
            (t - 0.6).abs() < 1e-3,
            "on the second circle gave {t}, expected the nearer crossing 0.6"
        );
        let t = two_point_conical(c0, 20.0, c1, 20.0, Pt { x: 50.0, y: 0.0 });
        assert!((t - 0.3).abs() < 1e-3, "midway gave {t}, expected 0.3");

        // Concentric circles are a plain radial ramp.
        let t = two_point_conical(c0, 10.0, c0, 40.0, Pt { x: 25.0, y: 0.0 });
        assert!(
            (t - 0.5).abs() < 1e-3,
            "concentric midpoint gave {t}, expected 0.5"
        );

        // A growing cone: the surface through a point between the circles.
        let t = two_point_conical(c0, 10.0, c1, 30.0, Pt { x: 50.0, y: 20.0 });
        assert!((0.0..=1.0).contains(&t), "cone gave {t}, outside 0..1");
    }

    #[test]
    fn ramp_honours_extend_modes() {
        let stops = vec![(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])];

        let pad = Ramp::new(stops.clone(), Extend::Pad);
        assert_eq!(pad.sample(-1.0)[0], 0);
        assert_eq!(pad.sample(2.0)[0], 255);

        let repeat = Ramp::new(stops.clone(), Extend::Repeat);
        assert_eq!(repeat.sample(1.25)[0], 64, "1.25 should wrap to 0.25");
        assert_eq!(repeat.sample(-0.75)[0], 64, "-0.75 should wrap to 0.25");

        let reflect = Ramp::new(stops, Extend::Reflect);
        assert_eq!(reflect.sample(1.25)[0], 191, "1.25 should mirror to 0.75");
        assert_eq!(reflect.sample(2.25)[0], 64, "2.25 should mirror to 0.25");
    }

    #[test]
    fn straight_alpha_conversion_handles_zero_alpha() {
        let mut canvas = Canvas::new(1, 1);
        canvas.data.copy_from_slice(&[7, 9, 11, 0]);
        let out = canvas.into_straight_alpha();
        // Fully transparent pixels have no defined colour; they must not divide by zero.
        assert_eq!(out, vec![0, 0, 0, 0]);

        let mut canvas = Canvas::new(1, 1);
        canvas.data.copy_from_slice(&[51, 102, 153, 255]);
        assert_eq!(canvas.into_straight_alpha(), vec![51, 102, 153, 255]);

        let mut canvas = Canvas::new(1, 1);
        // Premultiplied half-alpha red: straight alpha is 255.
        canvas.data.copy_from_slice(&[128, 0, 0, 128]);
        let out = canvas.into_straight_alpha();
        assert_eq!(out[3], 128);
        assert_eq!(out[0], 255);
    }
}
