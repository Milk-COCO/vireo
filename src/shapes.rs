//! 形状绘制：填充和描边函数。所有函数追加顶点到给定的 `DrawBatch`。

use std::f32::consts::{PI, FRAC_PI_2};

use crate::color::Color;
use crate::context::{DrawBatch, UvRect};
use crate::gpu::Vertex;

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

/// 填充矩形。
pub fn draw_rectangle(batch: &mut DrawBatch, x: f32, y: f32, w: f32, h: f32, color: Color) {
    match batch.sdf_feather {
        None => {
            if w == 0.0 || h == 0.0 || color.a == 0.0 { return; }
            let (c0, c1, c2) = batch.current_matrix();
            let base = batch.vertices.len() as u32;
            let x2 = x + w; let y2 = y + h;
            let uv = batch.uv;
            for (px, py, u, v) in &[(x, y, uv.u0, uv.v0), (x2, y, uv.u1, uv.v0), (x2, y2, uv.u1, uv.v1), (x, y2, uv.u0, uv.v1)] {
                batch.vertices.push(Vertex::new_uv(*px, *py, *u, *v, color).with_transform(c0, c1, c2));
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        Some(_) => draw_rounded_rect(batch, x, y, w, h, 0.0, color),
    }
}

/// 填充圆（shader SDF，完美边缘）。
pub fn draw_circle(batch: &mut DrawBatch, cx: f32, cy: f32, r: f32, color: Color) {
    if r == 0.0 || color.a == 0.0 { return; }
    match batch.sdf_feather {
        None => {
            let n = ((r * std::f32::consts::TAU) as u32).clamp(32, 256);
            let (c0, c1, c2) = batch.current_matrix();
            let bounds = (cx - r, cy - r, cx + r, cy + r);
            let uv = &batch.uv;
            let base = batch.vertices.len() as u32;
            let (cu, cv) = shape_uv(uv, cx, cy, bounds);
            batch.vertices.push(Vertex::new_uv(cx, cy, cu, cv, color).with_transform(c0, c1, c2));
            for i in 0..=n {
                let a = (i as f32 / n as f32) * std::f32::consts::TAU;
                let px = cx + r * a.cos();
                let py = cy + r * a.sin();
                let (u, v) = shape_uv(uv, px, py, bounds);
                batch.vertices.push(Vertex::new_uv(px, py, u, v, color).with_transform(c0, c1, c2));
            }
            for i in 0..n {
                batch.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + i + 1]);
            }
        }
        Some(f) => {
            let (c0, c1, c2) = batch.current_matrix();
            let base = batch.vertices.len() as u32;
            let bounds = (cx - r, cy - r, cx + r, cy + r);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = cx + dx * r; let py = cy + dy * r;
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv(px, py, u, v, color)
                    .with_transform(c0, c1, c2);
                vx.sdf_params = [cx, cy, r, r];
                vx.sdf_type = 1; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// 绘制线段（shader SDF）。
pub fn draw_line(
    batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32,
    thickness: f32, color: Color,
) {
    if thickness == 0.0 || color.a == 0.0 { return; }
    if (x2 - x1).abs() + (y2 - y1).abs() < 0.001 { return; }
    let h = thickness * 0.5;
    match batch.sdf_feather {
        None => {
            draw_line_chain(batch, &[(x1, y1), (x2, y2)], thickness, color);
        }
        Some(f) => {
            let pad = h + f;
            let min_x = x1.min(x2) - pad;
            let min_y = y1.min(y2) - pad;
            let max_x = x1.max(x2) + pad;
            let max_y = y1.max(y2) + pad;
            let (c0, c1, c2) = batch.current_matrix();
            let base = batch.vertices.len() as u32;
            let bx0 = x1.min(x2) - h; let by0 = y1.min(y2) - h;
            let bx1 = x1.max(x2) + h; let by1 = y1.max(y2) + h;
            let bounds = (bx0, by0, bx1, by1);
            let uv = &batch.uv;
            for (cx, cy) in &[(min_x, min_y), (max_x, min_y), (max_x, max_y), (min_x, max_y)] {
                let (u, v) = shape_uv(uv, *cx, *cy, bounds);
                let mut vx = Vertex::new_uv(*cx, *cy, u, v, color)
                    .with_transform(c0, c1, c2);
                vx.sdf_params = [x1, y1, x2, y2];
                vx.sdf_extra = [h, 0.0];
                vx.sdf_type = 3; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// 填充椭圆（shader SDF）。
pub fn draw_ellipse(
    batch: &mut DrawBatch, cx: f32, cy: f32, rx: f32, ry: f32, color: Color,
) {
    if rx == 0.0 || ry == 0.0 || color.a == 0.0 { return; }
    match batch.sdf_feather {
        None => {
            let n = ((rx.max(ry) * std::f32::consts::TAU) as u32).clamp(32, 256);
            let (c0, c1, c2) = batch.current_matrix();
            let bounds = (cx - rx, cy - ry, cx + rx, cy + ry);
            let uv = &batch.uv;
            let base = batch.vertices.len() as u32;
            let (cu, cv) = shape_uv(uv, cx, cy, bounds);
            batch.vertices.push(Vertex::new_uv(cx, cy, cu, cv, color).with_transform(c0, c1, c2));
            for i in 0..=n {
                let a = (i as f32 / n as f32) * std::f32::consts::TAU;
                let px = cx + rx * a.cos();
                let py = cy + ry * a.sin();
                let (u, v) = shape_uv(uv, px, py, bounds);
                batch.vertices.push(Vertex::new_uv(px, py, u, v, color).with_transform(c0, c1, c2));
            }
            for i in 0..n {
                batch.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + i + 1]);
            }
        }
        Some(f) => {
            let (c0, c1, c2) = batch.current_matrix();
            let base = batch.vertices.len() as u32;
            let bounds = (cx - rx, cy - ry, cx + rx, cy + ry);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = cx + dx * rx; let py = cy + dy * ry;
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv(px, py, u, v, color)
                    .with_transform(c0, c1, c2);
                vx.sdf_params = [cx, cy, rx, ry];
                vx.sdf_type = 1; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// 填充圆角矩形（shader SDF）。
pub fn draw_rounded_rect(
    batch: &mut DrawBatch, x: f32, y: f32, w: f32, h: f32,
    radius: f32, color: Color,
) {
    if w == 0.0 || h == 0.0 || color.a == 0.0 { return; }
    let r = radius.min(w * 0.5).min(h * 0.5);
    match batch.sdf_feather {
        None if r == 0.0 => {
            draw_rectangle(batch, x, y, w, h, color);
        }
        None => {
            let (c0, c1, c2) = batch.current_matrix();
            let cs = ((r * std::f32::consts::FRAC_PI_2) as u32).clamp(8, 64);
            let bounds = (x, y, x + w, y + h);
            let uv = &batch.uv;
            let xr = x + r;
            let yr = y + r;
            let x2 = x + w - r;
            let y2 = y + h - r;

            if x2 > xr && y2 > yr {
                let base = batch.vertices.len() as u32;
                batch.vertices.push(Vertex::new_uv(xr, yr, shape_uv(uv, xr, yr, bounds).0, shape_uv(uv, xr, yr, bounds).1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(x2, yr, shape_uv(uv, x2, yr, bounds).0, shape_uv(uv, x2, yr, bounds).1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(x2, y2, shape_uv(uv, x2, y2, bounds).0, shape_uv(uv, x2, y2, bounds).1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(xr, y2, shape_uv(uv, xr, y2, bounds).0, shape_uv(uv, xr, y2, bounds).1, color).with_transform(c0, c1, c2));
                batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }

            for &(ex, ey, ew, eh) in &[
                (xr, y,  w - 2.0 * r, r),
                (xr, y2, w - 2.0 * r, r),
                (x,  yr, r, h - 2.0 * r),
                (x2, yr, r, h - 2.0 * r),
            ] {
                if ew <= 0.0 || eh <= 0.0 { continue; }
                let base = batch.vertices.len() as u32;
                let ex2 = ex + ew; let ey2 = ey + eh;
                batch.vertices.push(Vertex::new_uv(ex,  ey,  shape_uv(uv, ex,  ey,  bounds).0, shape_uv(uv, ex,  ey,  bounds).1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(ex2, ey,  shape_uv(uv, ex2, ey,  bounds).0, shape_uv(uv, ex2, ey,  bounds).1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(ex2, ey2, shape_uv(uv, ex2, ey2, bounds).0, shape_uv(uv, ex2, ey2, bounds).1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(ex,  ey2, shape_uv(uv, ex,  ey2, bounds).0, shape_uv(uv, ex,  ey2, bounds).1, color).with_transform(c0, c1, c2));
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
                batch.vertices.push(Vertex::new_uv(cx, cy, cu, cv, color).with_transform(c0, c1, c2));
                for i in 0..=cs {
                    let a = sa + (i as f32 / cs as f32) * (ea - sa);
                    let px = cx + r * a.cos();
                    let py = cy + r * a.sin();
                    let (u, v) = shape_uv(uv, px, py, bounds);
                    batch.vertices.push(Vertex::new_uv(px, py, u, v, color).with_transform(c0, c1, c2));
                }
                for i in 0..cs {
                    batch.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + i + 1]);
                }
            }
        }
        Some(f) => {
            let cx = x + w * 0.5;
            let cy = y + h * 0.5;
            let hw = w * 0.5;
            let hh = h * 0.5;
            let (c0, c1, c2) = batch.current_matrix();
            let base = batch.vertices.len() as u32;
            let bounds = (x, y, x + w, y + h);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = cx + dx * (hw + f); let py = cy + dy * (hh + f);
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv(px, py, u, v, color)
                    .with_transform(c0, c1, c2);
                vx.sdf_params = [cx, cy, hw, hh];
                vx.sdf_extra = [r, 0.0];
                vx.sdf_type = 2; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// 绘制三角形（shader SDF）。
pub fn draw_triangle(
    batch: &mut DrawBatch, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, color: Color,
) {
    if color.a == 0.0 { return; }
    let (c0, c1, c2) = batch.current_matrix();
    match batch.sdf_feather {
        None => {
            let base = batch.vertices.len() as u32;
            let bounds = (x1.min(x2).min(x3), y1.min(y2).min(y3), x1.max(x2).max(x3), y1.max(y2).max(y3));
            let uv = &batch.uv;
            let (u1, v1) = shape_uv(uv, x1, y1, bounds);
            let (u2, v2) = shape_uv(uv, x2, y2, bounds);
            let (u3, v3) = shape_uv(uv, x3, y3, bounds);
            batch.vertices.push(Vertex::new_uv(x1, y1, u1, v1, color).with_transform(c0, c1, c2));
            batch.vertices.push(Vertex::new_uv(x2, y2, u2, v2, color).with_transform(c0, c1, c2));
            batch.vertices.push(Vertex::new_uv(x3, y3, u3, v3, color).with_transform(c0, c1, c2));
            batch.indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
        Some(f) => {
            let min_x = x1.min(x2).min(x3) - f;
            let min_y = y1.min(y2).min(y3) - f;
            let max_x = x1.max(x2).max(x3) + f;
            let max_y = y1.max(y2).max(y3) + f;
            let base = batch.vertices.len() as u32;
            let bounds = (x1.min(x2).min(x3), y1.min(y2).min(y3), x1.max(x2).max(x3), y1.max(y2).max(y3));
            let uv = &batch.uv;
            for (cx, cy) in &[(min_x, min_y), (max_x, min_y), (max_x, max_y), (min_x, max_y)] {
                let (u, v) = shape_uv(uv, *cx, *cy, bounds);
                let mut vx = Vertex::new_uv(*cx, *cy, u, v, color)
                    .with_transform(c0, c1, c2);
                vx.sdf_params = [x1, y1, x2, y2];
                vx.sdf_extra = [x3, y3];
                vx.sdf_type = 4; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// 绘制凸多边形（shader SDF，边数据通过 storage buffer 传递）。
/// 顶点须按逆时针排列。
pub fn draw_polygon(batch: &mut DrawBatch, points: &[(f32, f32)], color: Color) {
    if points.len() < 3 || color.a == 0.0 { return; }

    let n = points.len();
    match batch.sdf_feather {
        None => {
            let (c0, c1, c2) = batch.current_matrix();
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
                batch.vertices.push(Vertex::new_uv(*px, *py, u, v, color).with_transform(c0, c1, c2));
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
            for i in 0..n {
                let j = (i + 1) % n;
                let dx = points[j].0 - points[i].0;
                let dy = points[j].1 - points[i].1;
                let len = (dx * dx + dy * dy).sqrt();
                if len < 0.001 { continue; }
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
            let (c0, c1, c2) = batch.current_matrix();
            let base = batch.vertices.len() as u32;
            let bounds = (min_x, min_y, max_x, max_y);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = if *dx < 0.0 { min_x - f } else { max_x + f };
                let py = if *dy < 0.0 { min_y - f } else { max_y + f };
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv(px, py, u, v, color)
                    .with_transform(c0, c1, c2);
                vx.sdf_params = [start_f, count_f, 0.0, 0.0];
                vx.sdf_type = 6; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// 绘制弧线/扇形（shader SDF）。
pub fn draw_arc(
    batch: &mut DrawBatch, cx: f32, cy: f32, r: f32,
    start_angle: f32, end_angle: f32, color: Color,
) {
    if r == 0.0 || color.a == 0.0 { return; }
    let span = (end_angle - start_angle).abs();
    if span < 0.001 { return; }
    match batch.sdf_feather {
        None => {
            let n = ((r * span) as u32).clamp(16, 256);
            let (c0, c1, c2) = batch.current_matrix();
            let bounds = (cx - r, cy - r, cx + r, cy + r);
            let uv = &batch.uv;
            let base = batch.vertices.len() as u32;
            let (cu, cv) = shape_uv(uv, cx, cy, bounds);
            batch.vertices.push(Vertex::new_uv(cx, cy, cu, cv, color).with_transform(c0, c1, c2));
            for i in 0..=n {
                let a = start_angle + (i as f32 / n as f32) * (end_angle - start_angle);
                let px = cx + r * a.cos();
                let py = cy + r * a.sin();
                let (u, v) = shape_uv(uv, px, py, bounds);
                batch.vertices.push(Vertex::new_uv(px, py, u, v, color).with_transform(c0, c1, c2));
            }
            for i in 0..n {
                batch.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + i + 1]);
            }
        }
        Some(f) => {
            let ext = r + f;
            let (c0, c1, c2) = batch.current_matrix();
            let base = batch.vertices.len() as u32;
            let bounds = (cx - r, cy - r, cx + r, cy + r);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = cx + dx * ext; let py = cy + dy * ext;
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv(px, py, u, v, color)
                    .with_transform(c0, c1, c2);
                vx.sdf_params = [cx, cy, r, 0.0];
                vx.sdf_extra = [start_angle, end_angle];
                vx.sdf_type = 5; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// 描边矩形
pub fn draw_rect_outline(batch: &mut DrawBatch, x: f32, y: f32, w: f32, h: f32, thickness: f32, color: Color) {
    if w == 0.0 || h == 0.0 || thickness == 0.0 || color.a == 0.0 { return; }
    let half = thickness * 0.5;
    let x2 = x + w;
    let y2 = y + h;
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
    let cr = (r - half).max(0.0);
    let cs = corner_segments.max(2);

    let mut pts: Vec<(f32, f32)> = Vec::new();
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

    match batch.sdf_feather {
        None => {
            let (c0, c1, c2) = batch.current_matrix();
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
                batch.vertices.push(Vertex::new_uv(p0.0, p0.1, u0.0, u0.1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(p1.0, p1.1, u1.0, u1.1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(p2.0, p2.1, u2.0, u2.1, color).with_transform(c0, c1, c2));
                batch.vertices.push(Vertex::new_uv(p3.0, p3.1, u3.0, u3.1, color).with_transform(c0, c1, c2));
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
                batch.vertices.push(Vertex::new_uv(cx, cy, cu, cv, color).with_transform(c0, c1, c2));
                let span = ea - sa;
                let abs_span = span.abs();
                let m = ((join_n as f32 * abs_span / std::f32::consts::TAU) as u32).max(2);
                for k in 0..=m {
                    let a = sa + (k as f32 / m as f32) * span;
                    let px = cx + h * a.cos();
                    let py = cy + h * a.sin();
                    let (u, v) = shape_uv(uv, px, py, bounds);
                    batch.vertices.push(Vertex::new_uv(px, py, u, v, color).with_transform(c0, c1, c2));
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

            let pad = h + f;
            let (c0, c1, c2) = batch.current_matrix();
            let base = batch.vertices.len() as u32;
            let start_f = start_idx as f32;
            let count_f = actual_seg_count as f32;
            let bounds = (min_x - h, min_y - h, max_x + h, max_y + h);
            let uv = &batch.uv;
            for (dx, dy) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let px = if *dx < 0.0 { min_x - pad } else { max_x + pad };
                let py = if *dy < 0.0 { min_y - pad } else { max_y + pad };
                let (u, v) = shape_uv(uv, px, py, bounds);
                let mut vx = Vertex::new_uv(px, py, u, v, color)
                    .with_transform(c0, c1, c2);
                vx.sdf_params = [start_f, count_f, h, 0.0];
                vx.sdf_type = 7; vx.sdf_feather = f;
                batch.vertices.push(vx);
            }
            batch.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
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
    fn circle_sdf_produces_quad() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(0.0);
        draw_circle(&mut batch, 100.0, 100.0, 50.0, RED);
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
    fn circle_sdf_min_segments() {
        let mut batch = test_batch();
        batch.sdf_feather = Some(0.0);
        draw_circle(&mut batch, 0.0, 0.0, 10.0, RED);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn line_geometry_produces_caps() {
        let mut batch = test_batch();
        draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 2.0, WHITE);
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
    fn ellipse_geometry_produces_fan() {
        let mut batch = test_batch();
        draw_ellipse(&mut batch, 0.0, 0.0, 30.0, 20.0, BLUE);
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
    fn rounded_rect_geometry_produces_triangles() {
        let mut batch = test_batch();
        draw_rounded_rect(&mut batch, 10.0, 10.0, 100.0, 60.0, 10.0, GREEN);
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
    fn triangle_geometry_produces_one_triangle() {
        let mut batch = test_batch();
        draw_triangle(&mut batch, 0.0, 0.0, 100.0, 0.0, 50.0, 100.0, RED);
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
        draw_triangle(&mut batch, 0.0, 0.0, 100.0, 0.0, 50.0, 100.0, RED);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }

    #[test]
    fn polygon_geometry_produces_fan() {
        let mut batch = test_batch();
        let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];
        draw_polygon(&mut batch, &pts, WHITE);
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
    fn arc_geometry_produces_fan() {
        let mut batch = test_batch();
        draw_arc(&mut batch, 0.0, 0.0, 50.0, 0.0, std::f32::consts::PI, RED);
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
        batch.sdf_feather = Some(0.0);
        draw_rectangle(&mut batch, 0.0, 0.0, 10.0, 10.0, RED);
        draw_circle(&mut batch, 100.0, 100.0, 5.0, BLUE);
        draw_triangle(&mut batch, 0.0, 0.0, 10.0, 0.0, 5.0, 10.0, GREEN);

        assert_eq!(batch.vertices.len(), 4 + 4 + 4);
        assert_eq!(batch.indices.len(), 6 + 6 + 6);
    }

    #[test]
    fn rect_default_mode_is_geometry() {
        let mut batch = test_batch();
        draw_rectangle(&mut batch, 10.0, 10.0, 100.0, 60.0, GREEN);
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
        draw_rectangle(&mut batch, 10.0, 20.0, 30.0, 40.0, WHITE);
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
        draw_circle(&mut batch, 100.0, 100.0, 50.0, RED);
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
        draw_line_chain(&mut batch, &pts, 2.0, WHITE);
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
        draw_line_chain(&mut batch, &pts, 2.0, WHITE);
        assert_eq!(batch.vertices.len(), 4);
        assert_eq!(batch.indices.len(), 6);
    }
}
