//! 形状绘制：`Shape` 描述几何，`draw_shape` / `draw_*` 追加顶点到 `DrawBatch`。

mod emit_mesh;
mod emit_geo;
#[cfg(test)]
mod tests;

use emit_mesh::*;
pub(crate) use emit_geo::*;

use crate::color::Color;
use crate::render::{DrawBatch, Pos, Transform, UvRect};

/// 可绘制形状（填充 + 描边）。具体光栅化（SDF / 几何）由 [`Shape::append`] 决定。
/// 位置（WHERE）通过 `draw_shape(batch, pos, shape, opts)` 的 `Pos` 参数传入。
/// 以下变体只包含几何定义（WHAT），不包含坐标：
/// - `Rect`/`RoundedRect`/`Circle`/`Ellipse`/`Arc` 以原点为锚点
/// - `Line`/`Triangle`/`LineChain`/`Polygon` 的点本身就是纯几何，保留坐标
#[derive(Clone, Debug)]
pub enum Shape<'a> {
    Rect { pos: Pos, w: f32, h: f32 },
    RoundedRect { pos: Pos, w: f32, h: f32, radius: f32 },
    Circle { pos: Pos, r: f32 },
    Ellipse { pos: Pos, rx: f32, ry: f32 },
    Line { x1: f32, y1: f32, x2: f32, y2: f32, thickness: f32 },
    LineChain { points: &'a [(f32, f32)], thickness: f32 },
    Triangle { x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32 },
    Polygon { points: &'a [(f32, f32)] },
    Arc { pos: Pos, r: f32, start: f32, end: f32 },
    RectOutline { pos: Pos, w: f32, h: f32, thickness: f32 },
    CircleOutline { pos: Pos, r: f32, thickness: f32, segments: u32 },
    EllipseOutline {
        pos: Pos,
        rx: f32,
        ry: f32,
        thickness: f32,
        segments: u32,
    },
    RoundedRectOutline {
        pos: Pos,
        w: f32,
        h: f32,
        radius: f32,
        thickness: f32,
        corner_segments: u32,
    },
    TriangleOutline {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        x3: f32,
        y3: f32,
        thickness: f32,
    },
    PolygonOutline { points: &'a [(f32, f32)], thickness: f32 },
    ArcOutline {
        pos: Pos,
        r: f32,
        start: f32,
        end: f32,
        thickness: f32,
        segments: u32,
    },
}

impl<'a> Shape<'a> {
    /// 有锚点坐标的形状返回 `Some(pos)`；坐标即位置的形状（Line/Triangle/…）返回 `None`。
    pub fn position(&self) -> Option<Pos> {
        match *self {
            Shape::Rect { pos, .. }
            | Shape::RoundedRect { pos, .. }
            | Shape::Circle { pos, .. }
            | Shape::Ellipse { pos, .. }
            | Shape::Arc { pos, .. }
            | Shape::RectOutline { pos, .. }
            | Shape::CircleOutline { pos, .. }
            | Shape::EllipseOutline { pos, .. }
            | Shape::RoundedRectOutline { pos, .. }
            | Shape::ArcOutline { pos, .. } => Some(pos),
            Shape::Line { .. }
            | Shape::LineChain { .. }
            | Shape::Triangle { .. }
            | Shape::Polygon { .. }
            | Shape::TriangleOutline { .. }
            | Shape::PolygonOutline { .. } => None,
        }
    }
}

/// 单次绘制的可选覆盖（外层 `None` = 保持 batch 状态，**不写回**）。
///
/// | 字段 | 保持 | 覆盖 |
/// |------|------|------|
/// | `color` | `None` | `Some(c)` |
/// | `sdf_feather` | `None` | `Some(None)` 几何 / `Some(Some(f))` SDF |
/// | `uv` | `None` | `Some(UvRect)` |
/// | `transform` | `None` | `Some(Transform)` 绝对替换 |
/// | `bind_group` | `None` | `Some(None)` 白纹理 / `Some(Some(bg))` |
#[derive(Clone, Debug, Default)]
pub struct ShapeOverride {
    /// `Some` = 仅本次颜色；`None` = `batch.color`
    pub color: Option<Color>,
    /// 与 `DrawBatch::sdf_feather` 同形：`None` 保持；`Some(None)` 几何；`Some(Some(f))` SDF
    pub sdf_feather: Option<Option<f32>>,
    /// `Some` = 仅本次 UV 子区域
    pub uv: Option<UvRect>,
    /// `Some` = 仅本次绝对变换
    pub transform: Option<Transform>,
    /// 与 `DrawBatch::bind_group` 同形：`None` 保持；`Some(None)` 清贴图；`Some(Some(bg))` 绑定
    pub bind_group: Option<Option<wgpu::BindGroup>>,
}

impl ShapeOverride {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 `Option<Color>` 构造（供 `draw_*(…, color)` 使用）。
    #[inline]
    pub fn from_color(color: Option<Color>) -> Self {
        Self {
            color,
            ..Self::default()
        }
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    pub fn geometry(mut self) -> Self {
        self.sdf_feather = Some(None);
        self
    }

    pub fn sdf(mut self, feather: f32) -> Self {
        self.sdf_feather = Some(Some(feather));
        self
    }

    pub fn uv(mut self, uv: UvRect) -> Self {
        self.uv = Some(uv);
        self
    }

    pub fn uv_rect(mut self, u0: f32, v0: f32, u1: f32, v1: f32) -> Self {
        self.uv = Some(UvRect { u0, v0, u1, v1 });
        self
    }

    pub fn transform(mut self, t: Transform) -> Self {
        self.transform = Some(t);
        self
    }

    pub fn position(mut self, x: f32, y: f32) -> Self {
        self.transform = Some(Transform::translation(x, y));
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

impl<'a> Shape<'a> {
    /// 将形状写入 batch。`color` 为解析后的有效色（已合并状态与覆盖）。
    /// 位置由 `draw_shape` 根据 `self.position()` 在 batch transform 中设置。
    pub fn append(&self, batch: &mut DrawBatch, color: Color) {
        match *self {
            Shape::Rect { w, h, .. } => emit_rectangle(batch, w, h, color),
            Shape::RoundedRect { w, h, radius, .. } => {
                emit_rounded_rect(batch, w, h, radius, color)
            }
            Shape::Circle { r, .. } => emit_circle(batch, r, color),
            Shape::Ellipse { rx, ry, .. } => emit_ellipse(batch, rx, ry, color),
            Shape::Line {
                x1, y1, x2, y2, thickness,
            } => emit_line(batch, x1, y1, x2, y2, thickness, color),
            Shape::LineChain { points, thickness } => {
                emit_line_chain(batch, points, thickness, color)
            }
            Shape::Triangle {
                x1, y1, x2, y2, x3, y3,
            } => emit_triangle(batch, x1, y1, x2, y2, x3, y3, color),
            Shape::Polygon { points } => emit_polygon(batch, points, color),
            Shape::Arc { r, start, end, .. } => emit_arc(batch, r, start, end, color),
            Shape::RectOutline { w, h, thickness, .. } => {
                emit_rect_outline(batch, w, h, thickness, color)
            }
            Shape::CircleOutline { r, thickness, segments, .. } => {
                emit_circle_outline(batch, r, thickness, color, segments)
            }
            Shape::EllipseOutline { rx, ry, thickness, segments, .. } => {
                emit_ellipse_outline(batch, rx, ry, thickness, color, segments)
            }
            Shape::RoundedRectOutline { w, h, radius, thickness, corner_segments, .. } => {
                emit_rounded_rect_outline(batch, w, h, radius, thickness, color, corner_segments)
            }
            Shape::TriangleOutline {
                x1, y1, x2, y2, x3, y3, thickness,
            } => emit_triangle_outline(batch, x1, y1, x2, y2, x3, y3, thickness, color),
            Shape::PolygonOutline { points, thickness } => {
                emit_polygon_outline(batch, points, thickness, color)
            }
            Shape::ArcOutline { r, start, end, thickness, segments, .. } => {
                emit_arc_outline(batch, r, start, end, thickness, color, segments)
            }
        }
    }
}

/// 通过 [`Shape`]（含 `Pos`）+ [`ShapeOverride`] 绘制。
/// 覆盖项仅作用于本次，结束后恢复 batch 状态。
/// 有 `position()` 的形状在 batch transform 中设置平移，其余保留 batch 当前变换。
pub fn draw_shape(batch: &mut DrawBatch, shape: &Shape<'_>, opts: ShapeOverride) {
    batch.record_pending_mesh_command();
    let effective_feather = opts.sdf_feather.unwrap_or(batch.sdf_feather);
    // fragment-only custom material（无 custom VS）允许走 instance/geo instance 路径，
    // 性能与无 material 相同（1 dc 跨参数合并）。custom VS 仍走 mesh。
    let fragment_only = batch
        .custom_material
        .as_ref()
        .map(|m| !m.has_custom_vertex_shader())
        .unwrap_or(true);
    if effective_feather.is_some() && fragment_only {
        batch.instance_shape(shape, opts);
        return;
    }
    if effective_feather.is_none() && fragment_only {
        batch.geo_instance_shape(shape, opts);
        return;
    }

    let index_start = batch.indices.len() as u32;
    let saved_color = batch.color;
    let saved_feather = batch.sdf_feather;
    let saved_uv = batch.uv;
    let saved_xform = batch.transform;
    let saved_xform_cache = batch.cached_transform_index;
    let saved_bg = batch.bind_group.clone();
    let tex_overridden = opts.bind_group.is_some();

    if let Some(c) = opts.color {
        batch.color = c;
    }
    if let Some(f) = opts.sdf_feather {
        batch.sdf_feather = f;
    }
    if let Some(uv) = opts.uv {
        batch.uv = uv;
    }

    let xform_set = shape.position().is_some() || opts.transform.is_some();
    if xform_set {
        let base = match shape.position() {
            Some(p) => Transform::translation(p.x, p.y),
            None => Transform::IDENTITY,
        };
        let cur = batch.transform.take();
        batch.transform = Some(match (cur, opts.transform) {
            (Some(existing), Some(t)) => existing.then(&base).then(&t),
            (Some(existing), None) => existing.then(&base),
            (None, Some(t)) => base.then(&t),
            (None, None) => base,
        });
        batch.cached_transform_index = None;
    }

    if let Some(bg) = opts.bind_group {
        batch.add_texture_segment(batch.bind_group.clone());
        batch.advance_shape_texture_generation();
        batch.bind_group = bg;
    }

    shape.append(batch, batch.color);
    batch.record_mesh_command(index_start, batch.sdf_feather.is_none());

    if tex_overridden {
        batch.add_texture_segment(batch.bind_group.clone());
        batch.advance_shape_texture_generation();
        batch.bind_group = saved_bg;
    }
    batch.transform = saved_xform;
    batch.cached_transform_index = saved_xform_cache;
    batch.uv = saved_uv;
    batch.sdf_feather = saved_feather;
    batch.color = saved_color;
}

/// 通过共享 unit quad + instance buffer 绘制 [`Shape`]。
///
/// 默认 SDF 模式下，所有填充和描边变体使用实例路径；几何模式与自定义材质
/// 自动回退到普通 mesh 路径。普通 [`draw_shape`] 会自动选择相同路径。
pub fn draw_instance_shape(batch: &mut DrawBatch, shape: &Shape<'_>, opts: ShapeOverride) {
    batch.instance_shape(shape, opts);
}

/// 填充矩形。`color`: `None` = `batch.color`，`Some` = 仅本次。
pub fn draw_rectangle(batch: &mut DrawBatch, pos: Pos, w: f32, h: f32, color: Option<Color>) {
    draw_shape(
        batch,
        &Shape::Rect { pos, w, h },
        ShapeOverride::from_color(color),
    );
}

/// 填充圆（shader SDF，完美边缘）。
pub fn draw_circle(batch: &mut DrawBatch, pos: Pos, r: f32, color: Option<Color>) {
    draw_shape(batch, &Shape::Circle { pos, r }, ShapeOverride::from_color(color));
}

/// 绘制线段（shader SDF）。坐标即位置，不走 Pos 解耦。
pub fn draw_line(batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32, thickness: f32, color: Option<Color>) {
    draw_shape(batch, &Shape::Line { x1, y1, x2, y2, thickness }, ShapeOverride::from_color(color));
}

/// 填充椭圆（shader SDF）。
pub fn draw_ellipse(batch: &mut DrawBatch, pos: Pos, rx: f32, ry: f32, color: Option<Color>) {
    draw_shape(
        batch,
        &Shape::Ellipse { pos, rx, ry },
        ShapeOverride::from_color(color),
    );
}

/// 填充圆角矩形（shader SDF）。
pub fn draw_rounded_rect(batch: &mut DrawBatch, pos: Pos, w: f32, h: f32, radius: f32, color: Option<Color>) {
    draw_shape(
        batch,
        &Shape::RoundedRect { pos, w, h, radius },
        ShapeOverride::from_color(color),
    );
}

/// 绘制三角形（shader SDF）。坐标即位置，不走 Pos 解耦。
pub fn draw_triangle(batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, color: Option<Color>) {
    draw_shape(batch, &Shape::Triangle { x1, y1, x2, y2, x3, y3 }, ShapeOverride::from_color(color));
}

/// 绘制多边形（shader SDF / geo 扇形三角化）。
/// 顶点须按逆时针排列。坐标即位置，不走 Pos 解耦。
///
/// **限制**：仅支持**凸**多边形。SDF 用固定半平面（`shader.wgsl:112-131`），
/// 凹多边形会填充错误；geo 路径用 fan 三角化，凹多边形同样不正确。
/// 自交多边形（bowtie）亦不支持。如需凹多边形，请自行用 stencil 或外部 mesh 工具。
pub fn draw_polygon(batch: &mut DrawBatch, points: &[(f32, f32)], color: Option<Color>) {
    draw_shape(batch, &Shape::Polygon { points }, ShapeOverride::from_color(color));
}

/// 绘制弧线/扇形（shader SDF）。
pub fn draw_arc(batch: &mut DrawBatch, pos: Pos, r: f32, start_angle: f32, end_angle: f32, color: Option<Color>) {
    draw_shape(
        batch,
        &Shape::Arc { pos, r, start: start_angle, end: end_angle },
        ShapeOverride::from_color(color),
    );
}

/// 描边矩形
pub fn draw_rect_outline(batch: &mut DrawBatch, pos: Pos, w: f32, h: f32, thickness: f32, color: Option<Color>) {
    draw_shape(
        batch,
        &Shape::RectOutline { pos, w, h, thickness },
        ShapeOverride::from_color(color),
    );
}

/// 描边圆环
pub fn draw_circle_outline(batch: &mut DrawBatch, pos: Pos, r: f32, thickness: f32, color: Option<Color>, segments: u32) {
    draw_shape(
        batch,
        &Shape::CircleOutline { pos, r, thickness, segments },
        ShapeOverride::from_color(color),
    );
}

/// 描边椭圆环
pub fn draw_ellipse_outline(batch: &mut DrawBatch, pos: Pos, rx: f32, ry: f32, thickness: f32, color: Option<Color>, segments: u32) {
    draw_shape(
        batch,
        &Shape::EllipseOutline { pos, rx, ry, thickness, segments },
        ShapeOverride::from_color(color),
    );
}

/// 描边圆角矩形（line_chain SDF 沿中心线采样）。
pub fn draw_rounded_rect_outline(batch: &mut DrawBatch, pos: Pos, w: f32, h: f32, radius: f32, thickness: f32, color: Option<Color>, corner_segments: u32) {
    draw_shape(
        batch,
        &Shape::RoundedRectOutline { pos, w, h, radius, thickness, corner_segments },
        ShapeOverride::from_color(color),
    );
}

/// 连续折线（shader SDF，segment 数据通过 storage buffer 传递）。
/// 首尾坐标相近时自动闭合。
pub fn draw_line_chain(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Option<Color>) {
    draw_shape(batch, &Shape::LineChain { points, thickness }, ShapeOverride::from_color(color));
}

/// 描边三角形
pub fn draw_triangle_outline(batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, thickness: f32, color: Option<Color>) {
    draw_shape(batch, &Shape::TriangleOutline { x1, y1, x2, y2, x3, y3, thickness }, ShapeOverride::from_color(color));
}

/// 描边多边形
pub fn draw_polygon_outline(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Option<Color>) {
    draw_shape(batch, &Shape::PolygonOutline { points, thickness }, ShapeOverride::from_color(color));
}

/// 描边扇形（弧线 + 圆心到两端的连线）
pub fn draw_arc_outline(batch: &mut DrawBatch, pos: Pos, r: f32, start_angle: f32, end_angle: f32, thickness: f32, color: Option<Color>, segments: u32) {
    draw_shape(
        batch,
        &Shape::ArcOutline { pos, r, start: start_angle, end: end_angle, thickness, segments },
        ShapeOverride::from_color(color),
    );
}
