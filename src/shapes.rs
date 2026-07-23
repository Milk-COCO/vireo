//! 形状绘制：`Shape` 描述几何，`draw_shape` / `draw_*` 追加顶点到 `DrawBatch`。

use std::f32::consts::{PI, FRAC_PI_2};

use crate::color::Color;
use crate::context::{DrawBatch, Pos, Transform, UvRect};
use crate::gpu::Vertex;

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
        batch.bind_group = bg;
    }

    shape.append(batch, batch.color);

    if tex_overridden {
        batch.add_texture_segment(batch.bind_group.clone());
        batch.bind_group = saved_bg;
    }
    batch.transform = saved_xform;
    batch.cached_transform_index = saved_xform_cache;
    batch.uv = saved_uv;
    batch.sdf_feather = saved_feather;
    batch.color = saved_color;
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

/// 将顶点在形状包围盒中的位置映射为最终 UV（考虑 batch.uv 子区域）。
fn shape_uv(uv: &UvRect, px: f32, py: f32, bounds: (f32, f32, f32, f32)) -> (f32, f32) {
    let (min_x, min_y, max_x, max_y) = bounds;
    let u = if max_x > min_x { (px - min_x) / (max_x - min_x) } else { 0.5 };
    let v = if max_y > min_y { (py - min_y) / (max_y - min_y) } else { 0.5 };
    (uv.u0 + u * (uv.u1 - uv.u0), uv.v0 + v * (uv.v1 - uv.v0))
}

/// 折线转角弧：prev→at→next，返回 (start_angle, end_angle)。直线返回 (0,0)。
fn join_arc(prev: (f32, f32), at: (f32, f32), next: (f32, f32)) -> (f32, f32) {
    let dx1 = at.0 - prev.0; let dy1 = at.1 - prev.1;
    let dx2 = next.0 - at.0; let dy2 = next.1 - at.1;
    let l1 = (dx1*dx1 + dy1*dy1).sqrt();
    let l2 = (dx2*dx2 + dy2*dy2).sqrt();
    if l1 < 0.001 || l2 < 0.001 { return (0.0, 0.0); }
    let d1x = dx1/l1; let d1y = dy1/l1;
    let d2x = dx2/l2; let d2y = dy2/l2;
    let cross = d1x * d2y - d1y * d2x;
    if cross.abs() < 0.001 { return (0.0, 0.0); } // 直线，无转角
    let (sa, ea) = if cross > 0.0 {
        // 左转：外侧 = 两个左法线 n1_l → n2_l
        ((-d1x).atan2(d1y), (-d2x).atan2(d2y))
    } else {
        // 右转：外侧 = 两个右法线 n1_r → n2_r
        (d1x.atan2(-d1y), d2x.atan2(-d2y))
    };
    let mut span = ea - sa;
    if span > std::f32::consts::PI { span -= std::f32::consts::TAU; }
    else if span < -std::f32::consts::PI { span += std::f32::consts::TAU; }
    (sa, sa + span)
}

fn emit_rectangle(batch: &mut DrawBatch, w: f32, h: f32, color: Color) {
    match batch.sdf_feather {
        None => {
            if w == 0.0 || h == 0.0 || color.a == 0.0 { return; }
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let uv = batch.uv;
            for (px, py, u, v) in &[(0.0, 0.0, uv.u0, uv.v0), (w, 0.0, uv.u1, uv.v0), (w, h, uv.u1, uv.v1), (0.0, h, uv.u0, uv.v1)] {
                batch.vertices.push(Vertex::new_uv_xform(*px, *py, *u, *v, color, idx));
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        Some(_) => emit_rounded_rect(batch, w, h, 0.0, color),
    }
}

fn emit_circle(batch: &mut DrawBatch, r: f32, color: Color) {
    if r == 0.0 || color.a == 0.0 { return; }
    match batch.sdf_feather {
        None => {
            let n = ((r * std::f32::consts::TAU) as u32).clamp(32, 256);
            let idx = batch.current_transform_index();
            batch.vertices.reserve(n as usize + 2);
            batch.indices.reserve(n as usize * 3);
            let bounds = (-r, -r, r, r);
            let uv = &batch.uv;
            let base = batch.vertices.len() as u32;
            let (cu, cv) = shape_uv(uv, 0.0, 0.0, bounds);
            batch.vertices.push(Vertex::new_uv_xform(0.0, 0.0, cu, cv, color, idx));
            for i in 0..=n {
                let a = (i as f32 / n as f32) * std::f32::consts::TAU;
                let px = r * a.cos();
                let py = r * a.sin();
                let (u, v) = shape_uv(uv, px, py, bounds);
                batch.vertices.push(Vertex::new_uv_xform(px, py, u, v, color, idx));
            }
            for i in 0..n {
                batch.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + i + 1]);
            }
        }
        Some(f) => {
            batch.note_sdf();
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let bounds = (-r, -r, r, r);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = dx * r; let py = dy * r;
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv_xform(px, py, u, v, color, idx);
                vx.sdf_params = [0.0, 0.0, r, r];
                vx.sdf_type = 1; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn emit_line(
    batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32,
    thickness: f32, color: Color,
) {
    if thickness == 0.0 || color.a == 0.0 { return; }
    if (x2 - x1).abs() + (y2 - y1).abs() < 0.001 {
        // 零长线：没有线段部分，只有端点圆弧帽。
        // 当前线实现是“线 + 圆弧端点”，端点半径 = thickness/2。
        // 画一个圆（点），中心在 (x1,y1)（当前 batch 空间）。
        let r = thickness * 0.5;
        if r > 0.0 {
            let saved_xform = batch.transform.take();
            let saved_cache = batch.cached_transform_index;
            let dot = Transform::translation(x1, y1);
            batch.transform = Some(match saved_xform {
                Some(cur) => cur.then(&dot),
                None => dot,
            });
            batch.cached_transform_index = None;
            emit_circle(batch, r, color);
            batch.transform = saved_xform;
            batch.cached_transform_index = saved_cache;
        }
        return;
    }
    let h = thickness * 0.5;
    match batch.sdf_feather {
        None => {
            emit_line_chain(batch, &[(x1, y1), (x2, y2)], thickness, color);
        }
        Some(f) => {
            batch.note_sdf();
            let pad = h + f;
            let min_x = x1.min(x2) - pad;
            let min_y = y1.min(y2) - pad;
            let max_x = x1.max(x2) + pad;
            let max_y = y1.max(y2) + pad;
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let bx0 = x1.min(x2) - h; let by0 = y1.min(y2) - h;
            let bx1 = x1.max(x2) + h; let by1 = y1.max(y2) + h;
            let bounds = (bx0, by0, bx1, by1);
            let uv = &batch.uv;
            for (cx, cy) in &[(min_x, min_y), (max_x, min_y), (max_x, max_y), (min_x, max_y)] {
                let (u, v) = shape_uv(uv, *cx, *cy, bounds);
                let mut vx = Vertex::new_uv_xform(*cx, *cy, u, v, color, idx);
                vx.sdf_params = [x1, y1, x2, y2];
                vx.sdf_extra = [h, 0.0];
                vx.sdf_type = 3; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn emit_ellipse(
    batch: &mut DrawBatch, rx: f32, ry: f32, color: Color,
) {
    if rx == 0.0 || ry == 0.0 || color.a == 0.0 { return; }
    match batch.sdf_feather {
        None => {
            let n = ((rx.max(ry) * std::f32::consts::TAU) as u32).clamp(32, 256);
            let idx = batch.current_transform_index();
            batch.vertices.reserve(n as usize + 2);
            batch.indices.reserve(n as usize * 3);
            let bounds = (-rx, -ry, rx, ry);
            let uv = &batch.uv;
            let base = batch.vertices.len() as u32;
            let (cu, cv) = shape_uv(uv, 0.0, 0.0, bounds);
            batch.vertices.push(Vertex::new_uv_xform(0.0, 0.0, cu, cv, color, idx));
            for i in 0..=n {
                let a = (i as f32 / n as f32) * std::f32::consts::TAU;
                let px = rx * a.cos();
                let py = ry * a.sin();
                let (u, v) = shape_uv(uv, px, py, bounds);
                batch.vertices.push(Vertex::new_uv_xform(px, py, u, v, color, idx));
            }
            for i in 0..n {
                batch.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + i + 1]);
            }
        }
        Some(f) => {
            batch.note_sdf();
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let bounds = (-rx, -ry, rx, ry);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = dx * rx; let py = dy * ry;
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv_xform(px, py, u, v, color, idx);
                vx.sdf_params = [0.0, 0.0, rx, ry];
                vx.sdf_type = 1; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn emit_rounded_rect(
    batch: &mut DrawBatch, w: f32, h: f32,
    radius: f32, color: Color,
) {
    if w == 0.0 || h == 0.0 || color.a == 0.0 { return; }
    let r = radius.min(w * 0.5).min(h * 0.5);
    match batch.sdf_feather {
        None if r == 0.0 => {
            emit_rectangle(batch, w, h, color);
        }
        None => {
            let idx = batch.current_transform_index();
            let cs = ((r * std::f32::consts::FRAC_PI_2) as u32).clamp(8, 64);
            let est_v = 4 + 4 * 4 + 4 * (cs as usize + 2);
            batch.vertices.reserve(est_v);
            batch.indices.reserve(est_v * 3);
            let bounds = (0.0, 0.0, w, h);
            let uv = &batch.uv;
            let xr = r;
            let yr = r;
            let x2 = w - r;
            let y2 = h - r;

            if x2 > xr && y2 > yr {
                let base = batch.vertices.len() as u32;
                let (u0, v0) = shape_uv(uv, xr, yr, bounds);
                let (u1, v1) = shape_uv(uv, x2, yr, bounds);
                let (u2, v2) = shape_uv(uv, x2, y2, bounds);
                let (u3, v3) = shape_uv(uv, xr, y2, bounds);
                batch.vertices.push(Vertex::new_uv_xform(xr, yr, u0, v0, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(x2, yr, u1, v1, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(x2, y2, u2, v2, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(xr, y2, u3, v3, color, idx));
                batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }

            for &(ex, ey, ew, eh) in &[
                (xr, 0.0,  w - 2.0 * r, r),
                (xr, y2, w - 2.0 * r, r),
                (0.0,  yr, r, h - 2.0 * r),
                (x2, yr, r, h - 2.0 * r),
            ] {
                if ew <= 0.0 || eh <= 0.0 { continue; }
                let base = batch.vertices.len() as u32;
                let ex2 = ex + ew; let ey2 = ey + eh;
                let (u0, v0) = shape_uv(uv, ex, ey, bounds);
                let (u1, v1) = shape_uv(uv, ex2, ey, bounds);
                let (u2, v2) = shape_uv(uv, ex2, ey2, bounds);
                let (u3, v3) = shape_uv(uv, ex, ey2, bounds);
                batch.vertices.push(Vertex::new_uv_xform(ex, ey, u0, v0, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(ex2, ey, u1, v1, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(ex2, ey2, u2, v2, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(ex, ey2, u3, v3, color, idx));
                batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }

            let two_pi = 2.0 * std::f32::consts::PI;
            let frac_pi_2 = std::f32::consts::FRAC_PI_2;
            let pi = std::f32::consts::PI;
            for &(cx, cy, sa, ea) in &[
                (xr, yr, pi,       pi * 1.5),
                (x2, yr, pi * 1.5, two_pi),
                (x2, y2, 0.0,      frac_pi_2),
                (xr, y2, frac_pi_2, pi),
            ] {
                let base = batch.vertices.len() as u32;
                let (cu, cv) = shape_uv(uv, cx, cy, bounds);
                batch.vertices.push(Vertex::new_uv_xform(cx, cy, cu, cv, color, idx));
                for i in 0..=cs {
                    let a = sa + (i as f32 / cs as f32) * (ea - sa);
                    let px = cx + r * a.cos();
                    let py = cy + r * a.sin();
                    let (u, v) = shape_uv(uv, px, py, bounds);
                    batch.vertices.push(Vertex::new_uv_xform(px, py, u, v, color, idx));
                }
                for i in 0..cs {
                    batch.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + i + 1]);
                }
            }
        }
        Some(f) => {
            batch.note_sdf();
            let cx = w * 0.5;
            let cy = h * 0.5;
            let hw = w * 0.5;
            let hh = h * 0.5;
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let bounds = (0.0, 0.0, w, h);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = cx + dx * (hw + f); let py = cy + dy * (hh + f);
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv_xform(px, py, u, v, color, idx);
                vx.sdf_params = [cx, cy, hw, hh];
                vx.sdf_extra = [r, 0.0];
                vx.sdf_type = 2; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn emit_triangle(
    batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, color: Color,
) {
    if color.a == 0.0 { return; }
    // 退化：任一边长度接近 0 或面积接近 0 → 不画
    let abx = x2 - x1; let aby = y2 - y1;
    let bcx = x3 - x2; let bcy = y3 - y2;
    let cax = x1 - x3; let cay = y1 - y3;
    let l2ab = abx*abx + aby*aby;
    let l2bc = bcx*bcx + bcy*bcy;
    let l2ca = cax*cax + cay*cay;
    if l2ab < 0.000001 || l2bc < 0.000001 || l2ca < 0.000001 { return; }
    let area2 = (abx * bcy - aby * bcx).abs();
    if area2 < 0.0001 { return; }

    let idx = batch.current_transform_index();
    match batch.sdf_feather {
        None => {
            let base = batch.vertices.len() as u32;
            let bounds = (x1.min(x2).min(x3), y1.min(y2).min(y3), x1.max(x2).max(x3), y1.max(y2).max(y3));
            let uv = &batch.uv;
            let (u1, v1) = shape_uv(uv, x1, y1, bounds);
            let (u2, v2) = shape_uv(uv, x2, y2, bounds);
            let (u3, v3) = shape_uv(uv, x3, y3, bounds);
            batch.vertices.push(Vertex::new_uv_xform(x1, y1, u1, v1, color, idx));
            batch.vertices.push(Vertex::new_uv_xform(x2, y2, u2, v2, color, idx));
            batch.vertices.push(Vertex::new_uv_xform(x3, y3, u3, v3, color, idx));
            batch.indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
        Some(f) => {
            batch.note_sdf();
            let min_x = x1.min(x2).min(x3) - f;
            let min_y = y1.min(y2).min(y3) - f;
            let max_x = x1.max(x2).max(x3) + f;
            let max_y = y1.max(y2).max(y3) + f;
            let base = batch.vertices.len() as u32;
            let bounds = (x1.min(x2).min(x3), y1.min(y2).min(y3), x1.max(x2).max(x3), y1.max(y2).max(y3));
            let uv = &batch.uv;
            for (cx, cy) in &[(min_x, min_y), (max_x, min_y), (max_x, max_y), (min_x, max_y)] {
                let (u, v) = shape_uv(uv, *cx, *cy, bounds);
                let mut vx = Vertex::new_uv_xform(*cx, *cy, u, v, color, idx);
                vx.sdf_params = [x1, y1, x2, y2];
                vx.sdf_extra = [x3, y3];
                vx.sdf_type = 4; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn emit_polygon(batch: &mut DrawBatch, points: &[(f32, f32)], color: Color) {
    if points.len() < 3 || color.a == 0.0 { return; }

    let n = points.len();
    match batch.sdf_feather {
        None => {
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let mut min_x = f32::MAX; let mut min_y = f32::MAX;
            let mut max_x = f32::MIN; let mut max_y = f32::MIN;
            for (px, py) in points {
                min_x = min_x.min(*px); min_y = min_y.min(*py);
                max_x = max_x.max(*px); max_y = max_y.max(*py);
            }
            let bounds = (min_x, min_y, max_x, max_y);
            let uv = &batch.uv;
            for (px, py) in points {
                let (u, v) = shape_uv(uv, *px, *py, bounds);
                batch.vertices.push(Vertex::new_uv_xform(*px, *py, u, v, color, idx));
            }
            for i in 1..(n as u32 - 1) {
                batch.indices.extend_from_slice(&[base, base + i, base + i + 1]);
            }
        }
        Some(f) => {
            let mut min_x = f32::MAX; let mut min_y = f32::MAX;
            let mut max_x = f32::MIN; let mut max_y = f32::MIN;
            for (px, py) in points {
                min_x = min_x.min(*px); min_y = min_y.min(*py);
                max_x = max_x.max(*px); max_y = max_y.max(*py);
            }

            let start_idx = (batch.polygon_edges.len() / 4) as u32;
            let mut edge_count: u32 = 0;
            let mut edge_tmp: Vec<f32> = Vec::with_capacity(n * 4);
            for i in 0..n {
                let j = (i + 1) % n;
                let dx = points[j].0 - points[i].0;
                let dy = points[j].1 - points[i].1;
                let len = (dx * dx + dy * dy).sqrt();
                if len < 0.001 { continue; }
                let nx = -dy / len;
                let ny = dx / len;
                let offset = nx * points[i].0 + ny * points[i].1;
                edge_tmp.push(nx);
                edge_tmp.push(ny);
                edge_tmp.push(offset);
                edge_tmp.push(0.0);
                edge_count += 1;
            }
            if edge_count < 3 {
                return;
            }
            batch.note_sdf();
            batch.polygon_edges.extend_from_slice(&edge_tmp);

            let start_f = start_idx as f32;
            let count_f = edge_count as f32;
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let bounds = (min_x, min_y, max_x, max_y);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = if *dx < 0.0 { min_x - f } else { max_x + f };
                let py = if *dy < 0.0 { min_y - f } else { max_y + f };
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv_xform(px, py, u, v, color, idx);
                vx.sdf_params = [start_f, count_f, 0.0, 0.0];
                vx.sdf_type = 6; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn emit_arc(
    batch: &mut DrawBatch, r: f32,
    start_angle: f32, end_angle: f32, color: Color,
) {
    if r == 0.0 || color.a == 0.0 { return; }
    let span = (end_angle - start_angle).abs();
    if span < 0.001 { return; }
    match batch.sdf_feather {
        None => {
            let n = ((r * span) as u32).clamp(16, 256);
            let idx = batch.current_transform_index();
            let bounds = (-r, -r, r, r);
            let uv = &batch.uv;
            let base = batch.vertices.len() as u32;
            let (cu, cv) = shape_uv(uv, 0.0, 0.0, bounds);
            batch.vertices.push(Vertex::new_uv_xform(0.0, 0.0, cu, cv, color, idx));
            for i in 0..=n {
                let a = start_angle + (i as f32 / n as f32) * (end_angle - start_angle);
                let px = r * a.cos();
                let py = r * a.sin();
                let (u, v) = shape_uv(uv, px, py, bounds);
                batch.vertices.push(Vertex::new_uv_xform(px, py, u, v, color, idx));
            }
            for i in 0..n {
                batch.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + i + 1]);
            }
        }
        Some(f) => {
            batch.note_sdf();
            let ext = r + f;
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let bounds = (-r, -r, r, r);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = dx * ext; let py = dy * ext;
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv_xform(px, py, u, v, color, idx);
                vx.sdf_params = [0.0, 0.0, r, 0.0];
                vx.sdf_extra = [start_angle, end_angle];
                vx.sdf_type = 5; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn emit_rect_outline(batch: &mut DrawBatch, w: f32, h: f32, thickness: f32, color: Color) {
    if w == 0.0 || h == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let half = thickness * 0.5;
    let x2 = w;
    let y2 = h;
    emit_line_chain(batch, &[
        (half, half),
        (x2 - half, half),
        (x2 - half, y2 - half),
        (half, y2 - half),
        (half, half),
    ], thickness, color);
}

fn emit_circle_outline(batch: &mut DrawBatch, r: f32, thickness: f32, color: Color, segments: u32) {
    if r == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let n = segments.max(8) as usize;
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n + 1);
    for i in 0..n {
        let a = (i as f32 / n as f32) * std::f32::consts::TAU;
        pts.push((r * a.cos(), r * a.sin()));
    }
    pts.push(pts[0]);
    emit_line_chain(batch, &pts, thickness, color);
}

fn emit_ellipse_outline(batch: &mut DrawBatch, rx: f32, ry: f32, thickness: f32, color: Color, segments: u32) {
    if rx == 0.0 || ry == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let n = (segments as usize).max(16);
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n + 1);
    for i in 0..n {
        let a = (i as f32 / n as f32) * std::f32::consts::TAU;
        pts.push((rx * a.cos(), ry * a.sin()));
    }
    pts.push(pts[0]);
    emit_line_chain(batch, &pts, thickness, color);
}

fn emit_rounded_rect_outline(
    batch: &mut DrawBatch,
    w: f32, h: f32,
    radius: f32, thickness: f32,
    color: Color,
    corner_segments: u32,
) {
    if w == 0.0 || h == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let r = radius.min(w * 0.5).min(h * 0.5);
    if r == 0.0 {
        emit_rect_outline(batch, w, h, thickness, color);
        return;
    }

    let half = thickness * 0.5;
    let cr = (r - half).max(0.0);
    let cs = corner_segments.max(2);

    let cs_usize = cs as usize;
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(4 * (cs_usize + 1) + 1);
    let two_pi = 2.0 * PI;
    let corners: [(f32, f32, f32, f32); 4] = [
        (r,         r,         PI,        PI * 1.5),
        (w - r,     r,         PI * 1.5,  two_pi),
        (w - r,     h - r,     0.0,       FRAC_PI_2),
        (r,         h - r,     FRAC_PI_2, PI),
    ];
    for (cx, cy, sa, ea) in corners {
        if cr > 0.0 {
            for i in 0..=cs {
                let a = sa + (i as f32 / cs as f32) * (ea - sa);
                pts.push((cx + cr * a.cos(), cy + cr * a.sin()));
            }
        } else {
            pts.push((cx, cy));
        }
    }
    pts.push(pts[0]);
    emit_line_chain(batch, &pts, thickness, color);
}

fn emit_line_chain(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Color) {
    if points.len() < 2 || thickness == 0.0 || color.a == 0.0 { return; }

    let n = points.len();
    let closed = n > 2
        && (points[0].0 - points[n - 1].0).abs() < 0.001
        && (points[0].1 - points[n - 1].1).abs() < 0.001;
    let vcount = if closed { n - 1 } else { n };
    if vcount < 2 { return; }
    let seg_count = if closed { vcount } else { vcount - 1 };

    let h = thickness * 0.5;

    match batch.sdf_feather {
        None => {
            let idx = batch.current_transform_index();
            let join_n = ((h * 4.0) as u32).clamp(4, 16);
            let uv = &batch.uv;

            // 整体包围盒（含半线宽）
            let mut bx0 = f32::MAX; let mut by0 = f32::MAX;
            let mut bx1 = f32::MIN; let mut by1 = f32::MIN;
            for (px, py) in points {
                bx0 = bx0.min(*px); by0 = by0.min(*py);
                bx1 = bx1.max(*px); by1 = by1.max(*py);
            }
            let bounds = (bx0 - h, by0 - h, bx1 + h, by1 + h);

            for i in 0..seg_count {
                let j = if i + 1 < n { i + 1 } else { 0 };
                let (x1, y1) = points[i];
                let (x2, y2) = points[j];
                let dx = x2 - x1;
                let dy = y2 - y1;
                let len = (dx * dx + dy * dy).sqrt();
                if len < 0.001 { continue; }
                let nx = -dy / len * h;
                let ny = dx / len * h;
                let base = batch.vertices.len() as u32;
                let p0 = (x1 + nx, y1 + ny);
                let p1 = (x2 + nx, y2 + ny);
                let p2 = (x2 - nx, y2 - ny);
                let p3 = (x1 - nx, y1 - ny);
                let u0 = shape_uv(uv, p0.0, p0.1, bounds);
                let u1 = shape_uv(uv, p1.0, p1.1, bounds);
                let u2 = shape_uv(uv, p2.0, p2.1, bounds);
                let u3 = shape_uv(uv, p3.0, p3.1, bounds);
                batch.vertices.push(Vertex::new_uv_xform(p0.0, p0.1, u0.0, u0.1, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(p1.0, p1.1, u1.0, u1.1, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(p2.0, p2.1, u2.0, u2.1, color, idx));
                batch.vertices.push(Vertex::new_uv_xform(p3.0, p3.1, u3.0, u3.1, color, idx));
                batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }

            for i in 0..vcount {
                let (cx, cy) = points[i];
                let (sa, ea) = if closed {
                    let prev = if i == 0 { vcount - 1 } else { i - 1 };
                    let next = if i + 1 < n { i + 1 } else { 0 };
                    join_arc(points[prev], (cx, cy), points[next])
                } else if i == 0 {
                    // 起点半圆帽
                    let dir = (points[1].1 - points[0].1).atan2(points[1].0 - points[0].0);
                    let cap = dir + std::f32::consts::PI;
                    (cap - std::f32::consts::FRAC_PI_2, cap + std::f32::consts::FRAC_PI_2)
                } else if i == vcount - 1 {
                    // 终点半圆帽
                    let dir = (points[i].1 - points[i - 1].1).atan2(points[i].0 - points[i - 1].0);
                    (dir - std::f32::consts::FRAC_PI_2, dir + std::f32::consts::FRAC_PI_2)
                } else {
                    // 内顶点：转角扇区
                    join_arc(points[i-1], (cx, cy), points[i+1])
                };
                let base = batch.vertices.len() as u32;
                let (cu, cv) = shape_uv(uv, cx, cy, bounds);
                batch.vertices.push(Vertex::new_uv_xform(cx, cy, cu, cv, color, idx));
                let span = ea - sa;
                let abs_span = span.abs();
                let m = ((join_n as f32 * abs_span / std::f32::consts::TAU) as u32).max(2);
                for k in 0..=m {
                    let a = sa + (k as f32 / m as f32) * span;
                    let px = cx + h * a.cos();
                    let py = cy + h * a.sin();
                    let (u, v) = shape_uv(uv, px, py, bounds);
                    batch.vertices.push(Vertex::new_uv_xform(px, py, u, v, color, idx));
                }
                for k in 0..m {
                    batch.indices.extend_from_slice(&[base, base + 1 + k, base + 1 + k + 1]);
                }
            }
        }
        Some(f) => {
            let mut min_x = f32::MAX; let mut min_y = f32::MAX;
            let mut max_x = f32::MIN; let mut max_y = f32::MIN;
            for (px, py) in points {
                min_x = min_x.min(*px); min_y = min_y.min(*py);
                max_x = max_x.max(*px); max_y = max_y.max(*py);
            }

            let start_idx = (batch.polygon_edges.len() / 4) as u32;
            for i in 0..seg_count {
                let j = if i + 1 < n { i + 1 } else { 0 };
                let (x1, y1) = points[i];
                let (x2, y2) = points[j];
                if (x2 - x1).abs() + (y2 - y1).abs() < 0.001 { continue; }
                batch.polygon_edges.push(x1);
                batch.polygon_edges.push(y1);
                batch.polygon_edges.push(x2);
                batch.polygon_edges.push(y2);
            }
            let actual_seg_count = (batch.polygon_edges.len() / 4) as u32 - start_idx;
            if actual_seg_count == 0 { return; }
            batch.note_sdf();

            let pad = h + f;
            let idx = batch.current_transform_index();
            let base = batch.vertices.len() as u32;
            let start_f = start_idx as f32;
            let count_f = actual_seg_count as f32;
            let bounds = (min_x - h, min_y - h, max_x + h, max_y + h);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = if *dx < 0.0 { min_x - pad } else { max_x + pad };
                let py = if *dy < 0.0 { min_y - pad } else { max_y + pad };
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv_xform(px, py, u, v, color, idx);
                vx.sdf_params = [start_f, count_f, h, 0.0];
                vx.sdf_type = 7; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn emit_triangle_outline(
    batch: &mut DrawBatch,
    x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32,
    thickness: f32,
    color: Color,
) {
    emit_line_chain(batch, &[(x1, y1), (x2, y2), (x3, y3), (x1, y1)], thickness, color);
}

fn emit_polygon_outline(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Color) {
    if points.len() < 3 {
        return;
    }
    let mut closed: Vec<(f32, f32)> = Vec::with_capacity(points.len() + 1);
    closed.extend_from_slice(points);
    closed.push(points[0]);
    emit_line_chain(batch, &closed, thickness, color);
}

fn emit_arc_outline(
    batch: &mut DrawBatch,
    r: f32,
    start_angle: f32, end_angle: f32,
    thickness: f32,
    color: Color,
    segments: u32,
) {
    if r == 0.0 || thickness == 0.0 || color.a == 0.0 {
        return;
    }
    let segments = segments.max(2);
    let sx = r * start_angle.cos();
    let sy = r * start_angle.sin();
    let ex = r * end_angle.cos();
    let ey = r * end_angle.sin();

    let mut points: Vec<(f32, f32)> = Vec::with_capacity(segments as usize + 3);
    points.push((0.0, 0.0));
    points.push((sx, sy));
    for i in 1..segments {
        let t = i as f32 / segments as f32;
        let angle = start_angle + t * (end_angle - start_angle);
        points.push((r * angle.cos(), r * angle.sin()));
    }
    points.push((ex, ey));
    points.push((0.0, 0.0));
    emit_line_chain(batch, &points, thickness, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::colors::{WHITE, RED, BLUE, GREEN};

    fn test_batch() -> DrawBatch {
        DrawBatch::new()
    }

    #[test]
    fn rect_produces_vertices() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, Pos::new(10.0, 20.0), 30.0, 40.0, Some(WHITE));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn none_color_uses_batch_color() {
        let mut batch = test_batch();
        batch.set_color(GREEN);
        draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 10.0, 10.0, None);
        assert_eq!(batch.vertices[0].color, [GREEN.r, GREEN.g, GREEN.b, GREEN.a]);
        assert_eq!(batch.color, GREEN);
    }

    #[test]
    fn some_color_does_not_write_batch_color() {
        let mut batch = test_batch();
        batch.set_color(WHITE);
        draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        assert_eq!(batch.vertices[0].color, [RED.r, RED.g, RED.b, RED.a]);
        assert_eq!(batch.color, WHITE);
    }

    #[test]
    fn opts_sdf_and_transform_restore_batch_state() {
        let mut batch = test_batch();
        batch.sdf_feather = None;
        batch.set_color(WHITE);
        batch.set_position(10.0, 20.0);
        draw_shape(
            &mut batch,
            &Shape::Circle { pos: Pos::new(100.0, 200.0), r: 5.0 },
            ShapeOverride::new()
                .color(RED)
                .sdf(1.0),
        );
        assert_eq!(batch.sdf_feather, None);
        assert_eq!(batch.color, WHITE);
        assert_eq!(batch.vertices[0].sdf_type, 1);
        assert_eq!(batch.vertices[0].color, [RED.r, RED.g, RED.b, RED.a]);
        // 覆盖后的 shape 与 batch 后续 draw 应使用不同 transform_index
        draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 1.0, 1.0, Some(BLUE));
        let idx_override = batch.vertices[0].transform_index;
        let idx_restored = batch.vertices.last().unwrap().transform_index;
        assert_ne!(idx_override, idx_restored);
    }

    #[test]
    fn draw_shape_matches_draw_circle_sdf() {
        let mut a = test_batch();
        a.sdf_feather = Some(1.0);
        draw_circle(&mut a, Pos::new(50.0, 60.0), 20.0, Some(RED));
        let mut b = test_batch();
        b.sdf_feather = Some(1.0);
        draw_shape(
            &mut b,
            &Shape::Circle { pos: Pos::new(50.0, 60.0), r: 20.0 },
            ShapeOverride::from_color(Some(RED)),
        );
        assert_eq!(a.vertices.len(), b.vertices.len());
        assert_eq!(a.indices, b.indices);
        for (va, vb) in a.vertices.iter().zip(b.vertices.iter()) {
            assert_eq!(va.sdf_type, vb.sdf_type);
            assert_eq!(va.sdf_type, 1);
            assert_eq!(va.sdf_params, vb.sdf_params);
        }
    }

    #[test]
    fn rect_zero_size_skipped() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 0.0, 100.0, Some(WHITE));
        draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 100.0, 0.0, Some(WHITE));
        assert!(batch.vertices.is_empty());
        assert!(batch.indices.is_empty());
    }

    #[test]
    fn rect_transparent_skipped() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 100.0, 100.0, Some(Color::new(1.0, 0.0, 0.0, 0.0)));
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn circle_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(0.0);
        draw_circle(&mut batch, Pos::new(100.0, 100.0), 50.0, Some(RED));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn circle_zero_radius_skipped() {
        let mut batch = test_batch();
        draw_circle(&mut batch, Pos::new(0.0, 0.0), 0.0, Some(RED));
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn circle_sdf_min_segments() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(0.0);
        draw_circle(&mut batch, Pos::new(0.0, 0.0), 10.0, Some(RED));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn line_geometry_produces_caps() {
        let mut batch = test_batch();
        draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 2.0, Some(WHITE));
        assert!(batch.vertices.len() > 4);
        assert!(batch.indices.len() > 6);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0);
        }
    }

    #[test]
    fn line_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(1.0);
        draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 2.0, Some(WHITE));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn line_zero_thickness_skipped() {
        let mut batch = test_batch();
        draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 0.0, Some(WHITE));
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn line_zero_length_becomes_dot() {
        let mut batch = test_batch();
        draw_line(&mut batch, 50.0, 50.0, 50.0, 50.0, 2.0, Some(WHITE));
        // 零长线：无线段，只有端点圆弧 → 画半径 = thickness/2 的圆（点）
        assert!(!batch.vertices.is_empty());
    }

    #[test]
    fn ellipse_geometry_produces_fan() {
        let mut batch = test_batch();
        draw_ellipse(&mut batch, Pos::new(0.0, 0.0), 30.0, 20.0, Some(BLUE));
        assert!(batch.vertices.len() > 4);
        assert!(batch.indices.len() > 6);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0);
        }
    }

    #[test]
    fn ellipse_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(1.0);
        draw_ellipse(&mut batch, Pos::new(0.0, 0.0), 30.0, 20.0, Some(BLUE));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn ellipse_zero_radius_skipped() {
        let mut batch = test_batch();
        draw_ellipse(&mut batch, Pos::new(0.0, 0.0), 0.0, 10.0, Some(BLUE));
        draw_ellipse(&mut batch, Pos::new(0.0, 0.0), 10.0, 0.0, Some(BLUE));
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn rounded_rect_geometry_produces_triangles() {
        let mut batch = test_batch();
        draw_rounded_rect(&mut batch, Pos::new(10.0, 10.0), 100.0, 60.0, 10.0, Some(GREEN));
        assert!(batch.vertices.len() > 4);
        assert!(batch.indices.len() > 6);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0);
        }
    }

    #[test]
    fn rounded_rect_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(1.0);
        draw_rounded_rect(&mut batch, Pos::new(10.0, 10.0), 100.0, 60.0, 10.0, Some(GREEN));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn rounded_rect_zero_size_skipped() {
        let mut batch = test_batch();
        draw_rounded_rect(&mut batch, Pos::new(0.0, 0.0), 0.0, 100.0, 5.0, Some(WHITE));
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn triangle_geometry_produces_one_triangle() {
        let mut batch = test_batch();
        draw_triangle(&mut batch, 0.0, 0.0, 100.0, 0.0, 50.0, 100.0, Some(RED));
        assert_eq!(batch.vertices.len(), 3);
        assert_eq!(batch.indices.len(), 3);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0);
        }
    }

    #[test]
    fn triangle_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(1.0);
        draw_triangle(&mut batch, 0.0, 0.0, 100.0, 0.0, 50.0, 100.0, Some(RED));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn polygon_geometry_produces_fan() {
        let mut batch = test_batch();
        let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];
        draw_polygon(&mut batch, &pts, Some(WHITE));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0);
        }
    }

    #[test]
    fn polygon_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(1.0);
        let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];
        draw_polygon(&mut batch, &pts, Some(WHITE));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn polygon_too_few_points_skipped() {
        let mut batch = test_batch();
        draw_polygon(&mut batch, &[(0.0, 0.0), (10.0, 10.0)], Some(WHITE));
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn arc_geometry_produces_fan() {
        let mut batch = test_batch();
        draw_arc(&mut batch, Pos::new(0.0, 0.0), 50.0, 0.0, std::f32::consts::PI, Some(RED));
        assert!(batch.vertices.len() > 4);
        assert!(batch.indices.len() > 6);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0);
        }
    }

    #[test]
    fn arc_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(1.0);
        draw_arc(&mut batch, Pos::new(0.0, 0.0), 50.0, 0.0, std::f32::consts::PI, Some(RED));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn arc_zero_radius_skipped() {
        let mut batch = test_batch();
        draw_arc(&mut batch, Pos::new(0.0, 0.0), 0.0, 0.0, 1.0, Some(RED));
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn multiple_shapes_in_one_batch() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(0.0);
        draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        draw_circle(&mut batch, Pos::new(100.0, 100.0), 5.0, Some(BLUE));
        draw_triangle(&mut batch, 0.0, 0.0, 10.0, 0.0, 5.0, 10.0, Some(GREEN));

        assert_eq!(batch.vertices.len(), 4 + 4 + 4);
        assert_eq!(batch.indices.len(), 6 + 6 + 6);
    }

    #[test]
    fn rect_default_mode_is_geometry() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, Pos::new(10.0, 10.0), 100.0, 60.0, Some(GREEN));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0);
        }
    }

    #[test]
    fn rect_geometry_mode_produces_triangles() {
        let mut batch = test_batch();
        batch.sdf_feather = None;
        draw_rectangle(&mut batch, Pos::new(10.0, 20.0), 30.0, 40.0, Some(WHITE));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0, "geometry mode should produce sdf_type=0");
        }
    }

    #[test]
    fn circle_geometry_mode_produces_triangle_fan() {
        let mut batch = test_batch();
        batch.sdf_feather = None;
        draw_circle(&mut batch, Pos::new(100.0, 100.0), 50.0, Some(RED));
        let n = 256u32;
        assert_eq!(batch.vertices.len() as u32, 1 + n + 1);
        assert_eq!(batch.indices.len() as u32, n * 3);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0, "geometry mode should produce sdf_type=0");
        }
    }

    #[test]
    fn line_chain_geometry_produces_segments() {
        let mut batch = test_batch();
        let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
        draw_line_chain(&mut batch, &pts, 2.0, Some(WHITE));
        assert!(batch.vertices.len() >= 8);
        assert!(batch.indices.len() >= 12);
        for v in &batch.vertices {
            assert_eq!(v.sdf_type, 0);
        }
    }

    #[test]
    fn line_chain_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(1.0);
        let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
        draw_line_chain(&mut batch, &pts, 2.0, Some(WHITE));
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }
}
