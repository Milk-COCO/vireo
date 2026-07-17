//! 形状绘制：填充和描边函数。所有函数追加顶点到给定的 `DrawBatch`。

use std::f32::consts::{PI, FRAC_PI_2};

use crate::color::Color;
use crate::color::colors::WHITE;
use crate::context::DrawBatch;
use crate::gpu::Vertex;

/// 纹理绘制选项，链式 setter。
pub struct TextureOptions {
    pub x: f32, pub y: f32, pub w: f32, pub h: f32,
    pub u0: f32, pub v0: f32, pub u1: f32, pub v1: f32,
    pub color: Color,
}

impl Default for TextureOptions {
    fn default() -> Self {
        Self { x:0.0, y:0.0, w:0.0, h:0.0, u0:0.0, v0:0.0, u1:1.0, v1:1.0, color: WHITE }
    }
}

impl TextureOptions {
    pub fn rect(mut self, x: f32, y: f32, w: f32, h: f32) -> Self { self.x=x; self.y=y; self.w=w; self.h=h; self }
    pub fn uv(mut self, u0: f32, v0: f32, u1: f32, v1: f32) -> Self { self.u0=u0; self.v0=v0; self.u1=u1; self.v1=v1; self }
    pub fn color(mut self, c: Color) -> Self { self.color = c; self }
}

/// 可被 draw_texture 使用的纹理源。
pub trait TextureSource {
    fn bind_group(&self) -> &wgpu::BindGroup;
}

impl TextureSource for crate::texture::Texture {
    fn bind_group(&self) -> &wgpu::BindGroup { &self.bind_group }
}

impl TextureSource for &crate::texture::Texture {
    fn bind_group(&self) -> &wgpu::BindGroup { &self.bind_group }
}

/// 在矩形上绘制纹理。支持同一 batch 内多次调用用不同纹理。
///
/// 对于离屏画布，你可以直接拿他的 pub `texture` 参数。
pub fn draw_texture(batch: &mut DrawBatch, tex: &impl TextureSource, opts: TextureOptions) {
    let base = batch.vertices.len() as u32;
    let x2 = opts.x + opts.w;
    let y2 = opts.y + opts.h;
    batch.push_vertex_uv(opts.x, opts.y, opts.u0, opts.v0, opts.color);
    batch.push_vertex_uv(x2,     opts.y, opts.u1, opts.v0, opts.color);
    batch.push_vertex_uv(x2,     y2,     opts.u1, opts.v1, opts.color);
    batch.push_vertex_uv(opts.x, y2,     opts.u0, opts.v1, opts.color);
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    batch.add_texture_segment(tex.bind_group().clone());
}

/// 填充矩形。
pub fn draw_rectangle(batch: &mut DrawBatch, x: f32, y: f32, w: f32, h: f32, color: Color) {
    if w == 0.0 || h == 0.0 || color.a == 0.0 { return; }
    let base = batch.vertices.len() as u32;
    let x2 = x + w;
    let y2 = y + h;
    batch.push_vertex(x,  y,  color);
    batch.push_vertex(x2, y,  color);
    batch.push_vertex(x2, y2, color);
    batch.push_vertex(x,  y2, color);
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 填充圆（shader SDF，完美边缘）。
pub fn draw_circle(batch: &mut DrawBatch, cx: f32, cy: f32, r: f32, color: Color) {
    if r == 0.0 || color.a == 0.0 { return; }
    let f = batch.sdf_feather;
    let base = batch.vertices.len() as u32;
    for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let mut v = Vertex::new_uv(cx + dx * r, cy + dy * r, 0.0, 0.0, color);
        v.circle = [cx, cy, r, r];
        v.uv = [f, 0.0];
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 绘制线段
pub fn draw_line(
    batch: &mut DrawBatch,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    thickness: f32,
    color: Color,
) {
    if thickness == 0.0 || color.a == 0.0 {
        return;
    }

    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len == 0.0 {
        return;
    }

    let nx = -dy / len * thickness * 0.5;
    let ny = dx / len * thickness * 0.5;

    let base = batch.vertices.len() as u32;

    batch.push_vertex(x1 + nx, y1 + ny, color);
    batch.push_vertex(x1 - nx, y1 - ny, color);
    batch.push_vertex(x2 - nx, y2 - ny, color);
    batch.push_vertex(x2 + nx, y2 + ny, color);

    batch
        .indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 填充椭圆（shader SDF）。
pub fn draw_ellipse(
    batch: &mut DrawBatch, cx: f32, cy: f32, rx: f32, ry: f32, color: Color,
) {
    if rx == 0.0 || ry == 0.0 || color.a == 0.0 { return; }
    let f = batch.sdf_feather;
    let base = batch.vertices.len() as u32;
    for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let mut v = Vertex::new_uv(cx + dx * rx, cy + dy * ry, 0.0, 0.0, color);
        v.circle = [cx, cy, rx, ry];
        v.uv = [f, 0.0];
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 绘制圆角矩形
pub fn draw_rounded_rect(
    batch: &mut DrawBatch,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: Color,
    corner_segments: u32,
) {
    if w == 0.0 || h == 0.0 || color.a == 0.0 {
        return;
    }

    let r = radius.min(w * 0.5).min(h * 0.5);
    let cs = corner_segments.max(2);
    let base = batch.vertices.len() as u32;

    // 中心
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    batch.push_vertex(cx, cy, color);

    let corners = [
        (x + r, y + r, 0),           // 左上（从 PI 到 1.5*PI）
        (x + w - r, y + r, 1),       // 右上（1.5*PI 到 2*PI）
        (x + w - r, y + h - r, 2),   // 右下（0 到 0.5*PI）
        (x + r, y + h - r, 3),       // 左下（0.5*PI 到 PI）
    ];

    for (corner_x, corner_y, quadrant) in &corners {
        let start_angle = std::f32::consts::PI * (1.0 + *quadrant as f32 * 0.5);
        for i in 0..cs {
            let t = i as f32 / cs as f32;
            let angle = start_angle + t * std::f32::consts::FRAC_PI_2;
            let vx = corner_x + r * angle.cos();
            let vy = corner_y + r * angle.sin();
            batch.push_vertex(vx, vy, color);
        }
    }

    let total_samples = cs * 4;
    let center_idx = base;
    let perimeter_start = base + 1;

    for i in 0..total_samples {
        let next = (i + 1) % total_samples;
        batch.indices.push(center_idx);
        batch.indices.push(perimeter_start + i);
        batch.indices.push(perimeter_start + next);
    }
}

/// 绘制三角形
pub fn draw_triangle(
    batch: &mut DrawBatch,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    color: Color,
) {
    if color.a == 0.0 {
        return;
    }

    let base = batch.vertices.len() as u32;
    batch.push_vertex(x1, y1, color);
    batch.push_vertex(x2, y2, color);
    batch.push_vertex(x3, y3, color);
    batch.indices.extend_from_slice(&[base, base + 1, base + 2]);
}

/// 绘制任意凸多边形（顶点按逆时针顺序）
pub fn draw_polygon(batch: &mut DrawBatch, points: &[(f32, f32)], color: Color) {
    if points.len() < 3 || color.a == 0.0 {
        return;
    }

    let base = batch.vertices.len() as u32;

    for (px, py) in points {
        batch.push_vertex(*px, *py, color);
    }

    for i in 1..points.len() as u32 - 1 {
        batch.indices.extend_from_slice(&[base, base + i, base + i + 1]);
    }
}

/// 绘制弧线（实心扇形）
pub fn draw_arc(
    batch: &mut DrawBatch,
    cx: f32,
    cy: f32,
    r: f32,
    start_angle: f32,
    end_angle: f32,
    color: Color,
    segments: u32,
) {
    if r == 0.0 || color.a == 0.0 {
        return;
    }

    let segments = segments.max(2);
    let base = batch.vertices.len() as u32;

    batch.push_vertex(cx, cy, color);

    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let angle = start_angle + t * (end_angle - start_angle);
        let vx = cx + r * angle.cos();
        let vy = cy + r * angle.sin();
        batch.push_vertex(vx, vy, color);
    }

    for i in 0..segments {
        batch.indices.push(base);
        batch.indices.push(base + 1 + i);
        batch.indices.push(base + 1 + i + 1);
    }
}

/// 描边矩形
pub fn draw_rect_outline(batch: &mut DrawBatch, x: f32, y: f32, w: f32, h: f32, thickness: f32, color: Color) {
    if w == 0.0 || h == 0.0 || thickness == 0.0 || color.a == 0.0 {
        return;
    }
    let t = thickness;
    draw_rectangle(batch, x, y, w, t, color);           // top
    draw_rectangle(batch, x, y + h - t, w, t, color);   // bottom
    draw_rectangle(batch, x, y, t, h, color);           // left
    draw_rectangle(batch, x + w - t, y, t, h, color);   // right
}

/// 描边圆环
pub fn draw_circle_outline(batch: &mut DrawBatch, cx: f32, cy: f32, r: f32, thickness: f32, color: Color, segments: u32) {
    if r == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let n = segments.max(8) as usize;
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n + 1);
    for i in 0..n {
        let a = (i as f32 / n as f32) * std::f32::consts::TAU;
        pts.push((cx + r * a.cos(), cy + r * a.sin()));
    }
    pts.push(pts[0]);
    draw_line_chain(batch, &pts, thickness, color);
}

/// 描边椭圆环
pub fn draw_ellipse_outline(batch: &mut DrawBatch, cx: f32, cy: f32, rx: f32, ry: f32, thickness: f32, color: Color, segments: u32) {
    if rx == 0.0 || ry == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let n = (segments as usize).max(16);
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n + 1);
    for i in 0..n {
        let a = (i as f32 / n as f32) * std::f32::consts::TAU;
        pts.push((cx + rx * a.cos(), cy + ry * a.sin()));
    }
    pts.push(pts[0]);
    draw_line_chain(batch, &pts, thickness, color);
}

/// 描边圆角矩形
pub fn draw_rounded_rect_outline(
    batch: &mut DrawBatch,
    x: f32, y: f32, w: f32, h: f32,
    radius: f32, thickness: f32,
    color: Color,
    corner_segments: u32,
) {
    if w == 0.0 || h == 0.0 || thickness == 0.0 || color.a == 0.0 {
        return;
    }
    let r = radius.min(w * 0.5).min(h * 0.5);
    if r == 0.0 {
        draw_rect_outline(batch, x, y, w, h, thickness, color);
        return;
    }
    let cs = corner_segments.max(2);
    let inner_r = (r - thickness).max(0.0);

    // Outer perimeter (clockwise, from 12 o'clock)
    let mut outer: Vec<(f32, f32)> = Vec::new();
    // Top-left corner
    let (cx, cy) = (x + r, y + r);
    for i in 0..=cs {
        let angle = PI + (i as f32 / cs as f32) * FRAC_PI_2;
        outer.push((cx + r * angle.cos(), cy + r * angle.sin()));
    }
    // Top-right corner
    let (cx, cy) = (x + w - r, y + r);
    for i in 0..=cs {
        let angle = PI * 1.5 + (i as f32 / cs as f32) * FRAC_PI_2;
        outer.push((cx + r * angle.cos(), cy + r * angle.sin()));
    }
    // Bottom-right corner
    let (cx, cy) = (x + w - r, y + h - r);
    for i in 0..=cs {
        let angle = 0.0 + (i as f32 / cs as f32) * FRAC_PI_2;
        outer.push((cx + r * angle.cos(), cy + r * angle.sin()));
    }
    // Bottom-left corner
    let (cx, cy) = (x + r, y + h - r);
    for i in 0..=cs {
        let angle = FRAC_PI_2 + (i as f32 / cs as f32) * FRAC_PI_2;
        outer.push((cx + r * angle.cos(), cy + r * angle.sin()));
    }

    // Inner perimeter (clockwise, same order and direction as outer)
    let mut inner: Vec<(f32, f32)> = Vec::new();
    // Top-left
    let (cx, cy) = (x + r, y + r);
    for i in 0..=cs {
        let angle = PI + (i as f32 / cs as f32) * FRAC_PI_2;
        inner.push((cx + inner_r * angle.cos(), cy + inner_r * angle.sin()));
    }
    // Top-right
    let (cx, cy) = (x + w - r, y + r);
    for i in 0..=cs {
        let angle = PI * 1.5 + (i as f32 / cs as f32) * FRAC_PI_2;
        inner.push((cx + inner_r * angle.cos(), cy + inner_r * angle.sin()));
    }
    // Bottom-right
    let (cx, cy) = (x + w - r, y + h - r);
    for i in 0..=cs {
        let angle = 0.0 + (i as f32 / cs as f32) * FRAC_PI_2;
        inner.push((cx + inner_r * angle.cos(), cy + inner_r * angle.sin()));
    }
    // Bottom-left
    let (cx, cy) = (x + r, y + h - r);
    for i in 0..=cs {
        let angle = FRAC_PI_2 + (i as f32 / cs as f32) * FRAC_PI_2;
        inner.push((cx + inner_r * angle.cos(), cy + inner_r * angle.sin()));
    }

    // Total N vertices per ring, total 2N vertices
    let total = outer.len();
    let base = batch.vertices.len() as u32;
    for i in 0..total {
        batch.push_vertex(outer[i].0, outer[i].1, color);
    }
    for i in 0..total {
        batch.push_vertex(inner[i].0, inner[i].1, color);
    }
    for i in 0..total {
        let j = (i + 1) % total;
        let o0 = base + i as u32;
        let i0 = base + total as u32 + i as u32;
        let o1 = base + j as u32;
        let i1 = base + total as u32 + j as u32;
        batch.indices.extend_from_slice(&[o0, i0, o1, i0, i1, o1]);
    }
}

/// 连续折线（首尾不自动闭合）。每条 segment 用三角形四边形连接。
pub fn draw_line_chain(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Color) {
    if points.len() < 2 || thickness == 0.0 || color.a == 0.0 {
        return;
    }
    let n = points.len();
    let closed = n > 2
        && (points[0].0 - points[n - 1].0).abs() < 0.001
        && (points[0].1 - points[n - 1].1).abs() < 0.001;
    // 闭合时首尾重复，顶点数减一
    let vcount = if closed { n - 1 } else { n };
    if vcount < 2 { return; }

    let h = thickness * 0.5;

    // 每条线段的方向和法线
    let mut norms: Vec<(f32, f32)> = Vec::new();
    for i in 0..vcount {
        let j = (i + 1) % n; // 闭合时最后一根线段回到 point[0]
        let dx = points[j].0 - points[i].0;
        let dy = points[j].1 - points[i].1;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.001 {
            norms.push((0.0, 1.0));
        } else {
            norms.push((-dy / len, dx / len));
        }
    }

    let base = batch.vertices.len() as u32;

    // 每个顶点生成偏移顶点对（外侧 + 内侧），使用 miter join
    for i in 0..vcount {
        let n_prev = norms[(i + vcount - 1) % vcount];
        let n_next = norms[i];
        let dot = n_prev.0 * n_next.0 + n_prev.1 * n_next.1;

        // miter offset = (n0 + n1) * h / (1 + dot)
        let denom = (1.0 + dot).max(0.05);
        let ox = (n_prev.0 + n_next.0) * h / denom;
        let oy = (n_prev.1 + n_next.1) * h / denom;

        batch.push_vertex(points[i].0 + ox, points[i].1 + oy, color);
        batch.push_vertex(points[i].0 - ox, points[i].1 - oy, color);
    }

    // 三角形条带（闭合时最后一根回到 vertex 0）
    for i in 0..vcount - 1 {
        let o0 = base + (i * 2) as u32;
        let i0 = o0 + 1;
        let o1 = base + ((i + 1) * 2) as u32;
        let i1 = o1 + 1;
        batch.indices.extend_from_slice(&[o0, i0, o1, i0, i1, o1]);
    }
    if closed {
        let o0 = base + ((vcount - 1) * 2) as u32;
        let i0 = o0 + 1;
        let o1 = base;
        let i1 = base + 1;
        batch.indices.extend_from_slice(&[o0, i0, o1, i0, i1, o1]);
    }
}

/// 描边三角形
pub fn draw_triangle_outline(
    batch: &mut DrawBatch,
    x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32,
    thickness: f32,
    color: Color,
) {
    draw_line_chain(batch, &[(x1, y1), (x2, y2), (x3, y3), (x1, y1)], thickness, color);
}

/// 描边多边形
pub fn draw_polygon_outline(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Color) {
    if points.len() < 3 {
        return;
    }
    let mut closed: Vec<(f32, f32)> = points.to_vec();
    closed.push(points[0]);
    draw_line_chain(batch, &closed, thickness, color);
}

/// 描边扇形（弧线 + 圆心到两端的连线）
pub fn draw_arc_outline(
    batch: &mut DrawBatch,
    cx: f32, cy: f32, r: f32,
    start_angle: f32, end_angle: f32,
    thickness: f32,
    color: Color,
    segments: u32,
) {
    if r == 0.0 || thickness == 0.0 || color.a == 0.0 {
        return;
    }
    let segments = segments.max(2);
    let sx = cx + r * start_angle.cos();
    let sy = cy + r * start_angle.sin();
    let ex = cx + r * end_angle.cos();
    let ey = cy + r * end_angle.sin();

    // 连续折线：圆心 → 弧 → 圆心（闭合）
    let mut points = vec![(cx, cy), (sx, sy)];
    for i in 1..segments {
        let t = i as f32 / segments as f32;
        let angle = start_angle + t * (end_angle - start_angle);
        points.push((cx + r * angle.cos(), cy + r * angle.sin()));
    }
    points.push((ex, ey));
    points.push((cx, cy));
    draw_line_chain(batch, &points, thickness, color);
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
        draw_rectangle(&mut batch, 10.0, 20.0, 30.0, 40.0, WHITE);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn rect_zero_size_skipped() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, 0.0, 0.0, 0.0, 100.0, WHITE);
        draw_rectangle(&mut batch, 0.0, 0.0, 100.0, 0.0, WHITE);
        assert!(batch.vertices.is_empty());
        assert!(batch.indices.is_empty());
    }

    #[test]
    fn rect_transparent_skipped() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, 0.0, 0.0, 100.0, 100.0, Color::new(1.0, 0.0, 0.0, 0.0));
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn circle_produces_faces() {
        let mut batch = test_batch();
        draw_circle(&mut batch, 100.0, 100.0, 50.0, RED);
        // 包围 quad: 4 vertices, 6 indices
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn circle_zero_radius_skipped() {
        let mut batch = test_batch();
        draw_circle(&mut batch, 0.0, 0.0, 0.0, RED);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn circle_min_segments() {
        let mut batch = test_batch();
        draw_circle(&mut batch, 0.0, 0.0, 10.0, RED);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn line_produces_quad() {
        let mut batch = test_batch();
        draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 2.0, WHITE);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn line_zero_thickness_skipped() {
        let mut batch = test_batch();
        draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 0.0, WHITE);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn line_zero_length_skipped() {
        let mut batch = test_batch();
        draw_line(&mut batch, 50.0, 50.0, 50.0, 50.0, 2.0, WHITE);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn ellipse_produces_faces() {
        let mut batch = test_batch();
        draw_ellipse(&mut batch, 0.0, 0.0, 30.0, 20.0, BLUE);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn ellipse_zero_radius_skipped() {
        let mut batch = test_batch();
        draw_ellipse(&mut batch, 0.0, 0.0, 0.0, 10.0, BLUE);
        draw_ellipse(&mut batch, 0.0, 0.0, 10.0, 0.0, BLUE);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn rounded_rect_produces_faces() {
        let mut batch = test_batch();
        draw_rounded_rect(&mut batch, 10.0, 10.0, 100.0, 60.0, 10.0, GREEN, 4);
        // 1 center + 4 corners * 4 = 17 vertices, 16 * 3 = 48 indices
        assert_eq!(batch.vertices.len(), 17);
        assert_eq!(batch.indices.len(), 48);
    }

    #[test]
    fn rounded_rect_zero_size_skipped() {
        let mut batch = test_batch();
        draw_rounded_rect(&mut batch, 0.0, 0.0, 0.0, 100.0, 5.0, WHITE, 4);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn triangle_produces_one_face() {
        let mut batch = test_batch();
        draw_triangle(&mut batch, 0.0, 0.0, 100.0, 0.0, 50.0, 100.0, RED);
        assert_eq!(batch.vertices.len(), 3);
        assert_eq!(batch.indices.len(), 3);
    }

    #[test]
    fn polygon_produces_fan() {
        let mut batch = test_batch();
        let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];
        draw_polygon(&mut batch, &pts, WHITE);
        assert_eq!(batch.vertices.len(), 4);
        // fan: 2 triangles = 6 indices
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn polygon_too_few_points_skipped() {
        let mut batch = test_batch();
        draw_polygon(&mut batch, &[(0.0, 0.0), (10.0, 10.0)], WHITE);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn arc_produces_faces() {
        let mut batch = test_batch();
        draw_arc(&mut batch, 0.0, 0.0, 50.0, 0.0, std::f32::consts::PI, RED, 8);
        // 1 center + 9 perimeter = 10 vertices, 8 * 3 = 24 indices
        assert_eq!(batch.vertices.len(), 10);
        assert_eq!(batch.indices.len(), 24);
    }

    #[test]
    fn arc_zero_radius_skipped() {
        let mut batch = test_batch();
        draw_arc(&mut batch, 0.0, 0.0, 0.0, 0.0, 1.0, RED, 8);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn multiple_shapes_in_one_batch() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, 0.0, 0.0, 10.0, 10.0, RED);
        draw_circle(&mut batch, 100.0, 100.0, 5.0, BLUE);
        draw_triangle(&mut batch, 0.0, 0.0, 10.0, 0.0, 5.0, 10.0, GREEN);

        assert_eq!(batch.vertices.len(), 4 + 4 + 3);
        assert_eq!(batch.indices.len(), 6 + 6 + 3);
    }
}
