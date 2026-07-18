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
    draw_rounded_rect(batch, x, y, w, h, 0.0, color);
}

/// 填充圆（shader SDF，完美边缘）。
pub fn draw_circle(batch: &mut DrawBatch, cx: f32, cy: f32, r: f32, color: Color) {
    if r == 0.0 || color.a == 0.0 { return; }
    let f = batch.sdf_feather;
    let (c0, c1, c2) = batch.current_matrix();
    let base = batch.vertices.len() as u32;
    for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let mut v = Vertex::new_uv(cx + dx * r, cy + dy * r, 0.0, 0.0, color)
            .with_transform(c0, c1, c2);
        v.sdf_params = [cx, cy, r, r];
        v.uv = [f, 0.0];
        v.sdf_type = 1; v.sdf_feather = batch.sdf_feather;
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 绘制线段（shader SDF）。
pub fn draw_line(
    batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32,
    thickness: f32, color: Color,
) {
    if thickness == 0.0 || color.a == 0.0 { return; }
    if (x2 - x1).abs() + (y2 - y1).abs() < 0.001 { return; }
    let h = thickness * 0.5;
    let f = batch.sdf_feather;
    let pad = h + f;
    let min_x = x1.min(x2) - pad;
    let min_y = y1.min(y2) - pad;
    let max_x = x1.max(x2) + pad;
    let max_y = y1.max(y2) + pad;
    let (c0, c1, c2) = batch.current_matrix();
    let base = batch.vertices.len() as u32;
    for (cx, cy) in &[(min_x, min_y), (max_x, min_y), (max_x, max_y), (min_x, max_y)] {
        let mut v = Vertex::new_uv(*cx, *cy, h, 0.0, color)
            .with_transform(c0, c1, c2);
        v.sdf_params = [x1, y1, x2, y2];
        v.sdf_type = 3; v.sdf_feather = batch.sdf_feather;
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 填充椭圆（shader SDF）。
pub fn draw_ellipse(
    batch: &mut DrawBatch, cx: f32, cy: f32, rx: f32, ry: f32, color: Color,
) {
    if rx == 0.0 || ry == 0.0 || color.a == 0.0 { return; }
    let f = batch.sdf_feather;
    let (c0, c1, c2) = batch.current_matrix();
    let base = batch.vertices.len() as u32;
    for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let mut v = Vertex::new_uv(cx + dx * rx, cy + dy * ry, 0.0, 0.0, color)
            .with_transform(c0, c1, c2);
        v.sdf_params = [cx, cy, rx, ry];
        v.uv = [f, 0.0];
        v.sdf_type = 1; v.sdf_feather = batch.sdf_feather;
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 填充圆角矩形（shader SDF）。
pub fn draw_rounded_rect(
    batch: &mut DrawBatch, x: f32, y: f32, w: f32, h: f32,
    radius: f32, color: Color,
) {
    if w == 0.0 || h == 0.0 || color.a == 0.0 { return; }
    let r = radius.min(w * 0.5).min(h * 0.5);
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    let hw = w * 0.5;
    let hh = h * 0.5;
    let f = batch.sdf_feather;
    let (c0, c1, c2) = batch.current_matrix();
    let base = batch.vertices.len() as u32;
    for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let mut v = Vertex::new_uv(cx + dx * (hw + f), cy + dy * (hh + f), r, f, color)
            .with_transform(c0, c1, c2);
        v.sdf_params = [cx, cy, hw, hh];
        v.sdf_type = 2; v.sdf_feather = batch.sdf_feather;
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 绘制三角形（shader SDF）。
pub fn draw_triangle(
    batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, color: Color,
) {
    if color.a == 0.0 { return; }
    let f = batch.sdf_feather;
    let min_x = x1.min(x2).min(x3) - f;
    let min_y = y1.min(y2).min(y3) - f;
    let max_x = x1.max(x2).max(x3) + f;
    let max_y = y1.max(y2).max(y3) + f;
    let (c0, c1, c2) = batch.current_matrix();
    let base = batch.vertices.len() as u32;
    for (cx, cy) in &[(min_x, min_y), (max_x, min_y), (max_x, max_y), (min_x, max_y)] {
        let mut v = Vertex::new_uv(*cx, *cy, x3, y3, color)
            .with_transform(c0, c1, c2);
        v.sdf_params = [x1, y1, x2, y2];
        v.sdf_type = 4; v.sdf_feather = batch.sdf_feather;
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 绘制凸多边形（shader SDF，边数据通过 storage buffer 传递）。
/// 顶点须按逆时针排列。
pub fn draw_polygon(batch: &mut DrawBatch, points: &[(f32, f32)], color: Color) {
    if points.len() < 3 || color.a == 0.0 { return; }

    let n = points.len();
    let f = batch.sdf_feather;

    // 计算包围盒
    let mut min_x = f32::MAX; let mut min_y = f32::MAX;
    let mut max_x = f32::MIN; let mut max_y = f32::MIN;
    for (px, py) in points {
        min_x = min_x.min(*px); min_y = min_y.min(*py);
        max_x = max_x.max(*px); max_y = max_y.max(*py);
    }

    // 计算边的内法线，追加到 batch 的 polygon_edges
    let start_idx = (batch.polygon_edges.len() / 4) as u32; // vec4 索引
    let mut edge_count: u32 = 0;
    for i in 0..n {
        let j = (i + 1) % n;
        let dx = points[j].0 - points[i].0;
        let dy = points[j].1 - points[i].1;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.001 { continue; }
        // 内法线（CCW 逆时针，Y 轴向下）：(-dy, dx)/len
        let nx = -dy / len;
        let ny = dx / len;
        let offset = nx * points[i].0 + ny * points[i].1;
        batch.polygon_edges.push(nx);
        batch.polygon_edges.push(ny);
        batch.polygon_edges.push(offset);
        batch.polygon_edges.push(0.0);
        edge_count += 1;
    }
    if edge_count < 3 { return; }

    let start_f = start_idx as f32;
    let count_f = edge_count as f32;

    // 包围 quad + feather 余量
    let (c0, c1, c2) = batch.current_matrix();
    let base = batch.vertices.len() as u32;
    for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let mut v = Vertex::new_uv(
            if *dx < 0.0 { min_x - f } else { max_x + f },
            if *dy < 0.0 { min_y - f } else { max_y + f },
            start_f, count_f, color,
        ).with_transform(c0, c1, c2);
        v.sdf_params = [start_f, count_f, 0.0, 0.0];
        v.sdf_type = 6; v.sdf_feather = batch.sdf_feather;
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 绘制弧线/扇形（shader SDF）。
pub fn draw_arc(
    batch: &mut DrawBatch, cx: f32, cy: f32, r: f32,
    start_angle: f32, end_angle: f32, color: Color,
) {
    if r == 0.0 || color.a == 0.0 { return; }
    let f = batch.sdf_feather;
    let ext = r + f;
    let (c0, c1, c2) = batch.current_matrix();
    let base = batch.vertices.len() as u32;
    for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let mut v = Vertex::new_uv(cx + dx * ext, cy + dy * ext, start_angle, end_angle, color)
            .with_transform(c0, c1, c2);
        v.sdf_params = [cx, cy, r, 0.0];
        v.sdf_type = 5; v.sdf_feather = f;
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 描边矩形
pub fn draw_rect_outline(batch: &mut DrawBatch, x: f32, y: f32, w: f32, h: f32, thickness: f32, color: Color) {
    if w == 0.0 || h == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let half = thickness * 0.5;
    let x2 = x + w;
    let y2 = y + h;
    // 中心线闭合矩形，line_chain SDF 自动处理线段转 thick line
    draw_line_chain(batch, &[
        (x + half, y + half),
        (x2 - half, y + half),
        (x2 - half, y2 - half),
        (x + half, y2 - half),
        (x + half, y + half),
    ], thickness, color);
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

/// 描边圆角矩形（line_chain SDF 沿中心线采样）。
pub fn draw_rounded_rect_outline(
    batch: &mut DrawBatch,
    x: f32, y: f32, w: f32, h: f32,
    radius: f32, thickness: f32,
    color: Color,
    corner_segments: u32,
) {
    if w == 0.0 || h == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let r = radius.min(w * 0.5).min(h * 0.5);
    if r == 0.0 {
        draw_rect_outline(batch, x, y, w, h, thickness, color);
        return;
    }

    let half = thickness * 0.5;
    let cr = (r - half).max(0.0); // 中心线圆角半径
    let cs = corner_segments.max(2);

    let mut pts: Vec<(f32, f32)> = Vec::new();
    // TL(π→3π/2), TR(3π/2→2π), BR(0→π/2), BL(π/2→π)
    let two_pi = 2.0 * PI;
    let corners: [(f32, f32, f32, f32); 4] = [
        (x + r,     y + r,     PI,        PI * 1.5),
        (x + w - r, y + r,     PI * 1.5,  two_pi),
        (x + w - r, y + h - r, 0.0,       FRAC_PI_2),
        (x + r,     y + h - r, FRAC_PI_2, PI),
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
    draw_line_chain(batch, &pts, thickness, color);
}

/// 连续折线（shader SDF，segment 数据通过 storage buffer 传递）。
/// 首尾坐标相近时自动闭合。
pub fn draw_line_chain(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Color) {
    if points.len() < 2 || thickness == 0.0 || color.a == 0.0 { return; }

    let n = points.len();
    let closed = n > 2
        && (points[0].0 - points[n - 1].0).abs() < 0.001
        && (points[0].1 - points[n - 1].1).abs() < 0.001;
    let vcount = if closed { n - 1 } else { n };
    if vcount < 2 { return; }
    let seg_count = if closed { vcount } else { vcount - 1 };

    let h = thickness * 0.5;
    let f = batch.sdf_feather;

    // 包围盒
    let mut min_x = f32::MAX; let mut min_y = f32::MAX;
    let mut max_x = f32::MIN; let mut max_y = f32::MIN;
    for (px, py) in points {
        min_x = min_x.min(*px); min_y = min_y.min(*py);
        max_x = max_x.max(*px); max_y = max_y.max(*py);
    }

    // 追加 segment 数据 (x1,y1,x2,y2) 到 storage buffer
    let start_idx = (batch.polygon_edges.len() / 4) as u32; // vec4 索引
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

    // 包围 quad（加 feather + thickness 余量）
    let pad = h + f;
    let (c0, c1, c2) = batch.current_matrix();
    let base = batch.vertices.len() as u32;
    let start_f = start_idx as f32;
    let count_f = actual_seg_count as f32;
    for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let mut v = Vertex::new_uv(
            if *dx < 0.0 { min_x - pad } else { max_x + pad },
            if *dy < 0.0 { min_y - pad } else { max_y + pad },
            start_f, count_f, color,
        ).with_transform(c0, c1, c2);
        v.sdf_params = [start_f, count_f, h, 0.0];
        v.sdf_type = 7; v.sdf_feather = batch.sdf_feather;
        batch.vertices.push(v);
    }
    batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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
        draw_rounded_rect(&mut batch, 10.0, 10.0, 100.0, 60.0, 10.0, GREEN);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn rounded_rect_zero_size_skipped() {
        let mut batch = test_batch();
        draw_rounded_rect(&mut batch, 0.0, 0.0, 0.0, 100.0, 5.0, WHITE);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn triangle_produces_one_face() {
        let mut batch = test_batch();
        draw_triangle(&mut batch, 0.0, 0.0, 100.0, 0.0, 50.0, 100.0, RED);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn polygon_produces_fan() {
        let mut batch = test_batch();
        let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];
        draw_polygon(&mut batch, &pts, WHITE);
        assert_eq!(batch.vertices.len(), 4);
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
        draw_arc(&mut batch, 0.0, 0.0, 50.0, 0.0, std::f32::consts::PI, RED);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn arc_zero_radius_skipped() {
        let mut batch = test_batch();
        draw_arc(&mut batch, 0.0, 0.0, 0.0, 0.0, 1.0, RED);
        assert!(batch.vertices.is_empty());
    }

    #[test]
    fn multiple_shapes_in_one_batch() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, 0.0, 0.0, 10.0, 10.0, RED);
        draw_circle(&mut batch, 100.0, 100.0, 5.0, BLUE);
        draw_triangle(&mut batch, 0.0, 0.0, 10.0, 0.0, 5.0, 10.0, GREEN);

        assert_eq!(batch.vertices.len(), 4 + 4 + 4);
        assert_eq!(batch.indices.len(), 6 + 6 + 6);
    }
}
