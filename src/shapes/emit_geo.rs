// ---- 几何模板实例化（geo-instance path） ----
//
// 这些 `geo_emit_*` 由 [`crate::render::DrawBatch::geo_instance_shape`] 调用，
// 是 `draw_shape` 几何模式（`sdf_feather = None`）的热路径。思路：
// 1. 用「形状参数 + UV」算一个 `key`；
// 2. `geo_emit_template` 命中缓存 → 跳过网格生成，只推实例（CPU 节省）；
// 3. 未命中 → 临时接管 `vertices`/`indices` 跑原 mesh 发射器捕获几何，
//    剥掉 color/transform 登记为模板，再推实例。
// 因此模板与 mesh 路径几何逐位一致，无渲染分叉。

use crate::color::Color;
use crate::render::{DrawBatch, UvRect};

use super::emit_mesh::*;

#[inline]
fn mix_key(mut h: u64, x: f32) -> u64 {
    h = h.wrapping_mul(0x9E3779B97F4A7C15);
    h ^= (x.to_bits() as u64).rotate_left(23);
    h
}

#[inline]
fn uv_key(uv: &UvRect) -> u64 {
    let mut h = 0xCBF29CE484222325u64;
    h = mix_key(h, uv.u0);
    h = mix_key(h, uv.v0);
    h = mix_key(h, uv.u1);
    h = mix_key(h, uv.v1);
    h
}

pub(crate) fn geo_emit_rectangle(batch: &mut DrawBatch, w: f32, h: f32, color: Color) {
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 || color.a == 0.0 { return; }
    let mut k = 0x51E774B9u64;
    k = mix_key(k, w);
    k = mix_key(k, h);
    k ^= uv_key(&batch.uv);
    batch.geo_emit_template(k, |b| emit_rectangle(b, w, h, color), color);
}

pub(crate) fn geo_emit_rounded_rect(batch: &mut DrawBatch, w: f32, h: f32, radius: f32, color: Color) {
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 || color.a == 0.0 { return; }
    let r = radius.min(w * 0.5).min(h * 0.5).max(0.0);
    if r <= 0.0 {
        return geo_emit_rectangle(batch, w, h, color);
    }
    let mut k = 0x52E884BAu64;
    k = mix_key(k, w);
    k = mix_key(k, h);
    k = mix_key(k, radius);
    k ^= uv_key(&batch.uv);
    batch.geo_emit_template(k, |b| emit_rounded_rect(b, w, h, radius, color), color);
}

pub(crate) fn geo_emit_circle(batch: &mut DrawBatch, r: f32, color: Color) {
    if !r.is_finite() || r <= 0.0 || color.a == 0.0 { return; }
    let mut k = 0x53E994BBu64;
    k = mix_key(k, r);
    k ^= uv_key(&batch.uv);
    batch.geo_emit_template(k, |b| emit_circle(b, r, color), color);
}

pub(crate) fn geo_emit_ellipse(batch: &mut DrawBatch, rx: f32, ry: f32, color: Color) {
    if !rx.is_finite() || !ry.is_finite() || rx <= 0.0 || ry <= 0.0 || color.a == 0.0 { return; }
    let mut k = 0x54EA94BCu64;
    k = mix_key(k, rx);
    k = mix_key(k, ry);
    k ^= uv_key(&batch.uv);
    batch.geo_emit_template(k, |b| emit_ellipse(b, rx, ry, color), color);
}

pub(crate) fn geo_emit_line(
    batch: &mut DrawBatch,
    x1: f32, y1: f32, x2: f32, y2: f32,
    thickness: f32,
    color: Color,
) {
    if !thickness.is_finite() || thickness <= 0.0 || color.a == 0.0 { return; }
    if (x2 - x1).abs() + (y2 - y1).abs() < 0.001 {
        // 零长线 = 端点圆弧帽 → 画圆（点）。与 mesh 路径一致：位置走内部平移。
        let r = thickness * 0.5;
        if r <= 0.0 { return; }
        let saved_xform = batch.transform.take();
        let saved_cache = batch.cached_transform_index;
        let dot = crate::render::Transform::translation(x1, y1);
        batch.transform = Some(match saved_xform {
            Some(cur) => cur.then(&dot),
            None => dot,
        });
        batch.cached_transform_index = None;
        geo_emit_circle(batch, r, color);
        batch.transform = saved_xform;
        batch.cached_transform_index = saved_cache;
        return;
    }
    geo_emit_line_chain(batch, &[(x1, y1), (x2, y2)], thickness, color);
}

pub(crate) fn geo_emit_line_chain(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Color) {
    if points.len() < 2 || !thickness.is_finite() || thickness <= 0.0 || color.a == 0.0 { return; }
    let mut k = 0x55EB95BDu64;
    k = mix_key(k, thickness);
    for (px, py) in points {
        k = mix_key(k, *px);
        k = mix_key(k, *py);
    }
    k ^= uv_key(&batch.uv);
    let pts = points.to_vec();
    batch.geo_emit_template(k, |b| emit_line_chain(b, &pts, thickness, color), color);
}

pub(crate) fn geo_emit_triangle(
    batch: &mut DrawBatch,
    x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32,
    color: Color,
) {
    if color.a == 0.0 { return; }
    let abx = x2 - x1; let aby = y2 - y1;
    let bcx = x3 - x2; let bcy = y3 - y2;
    let cax = x1 - x3; let cay = y1 - y3;
    let l2ab = abx*abx + aby*aby;
    let l2bc = bcx*bcx + bcy*bcy;
    let l2ca = cax*cax + cay*cay;
    if l2ab < 0.000001 || l2bc < 0.000001 || l2ca < 0.000001 { return; }
    if (abx * bcy - aby * bcx).abs() < 0.0001 { return; }
    let mut k = 0x56EC96BEu64;
    k = mix_key(k, x1); k = mix_key(k, y1);
    k = mix_key(k, x2); k = mix_key(k, y2);
    k = mix_key(k, x3); k = mix_key(k, y3);
    k ^= uv_key(&batch.uv);
    batch.geo_emit_template(k, |b| emit_triangle(b, x1, y1, x2, y2, x3, y3, color), color);
}

pub(crate) fn geo_emit_polygon(batch: &mut DrawBatch, points: &[(f32, f32)], color: Color) {
    if points.len() < 3 || color.a == 0.0 || points.iter().any(|(x,y)| !x.is_finite() || !y.is_finite()) { return; }
    let mut k = 0x57ED97BFu64;
    for (px, py) in points {
        k = mix_key(k, *px);
        k = mix_key(k, *py);
    }
    k ^= uv_key(&batch.uv);
    let pts = points.to_vec();
    batch.geo_emit_template(k, |b| emit_polygon(b, &pts, color), color);
}

pub(crate) fn geo_emit_arc(
    batch: &mut DrawBatch,
    r: f32, start_angle: f32, end_angle: f32,
    color: Color,
) {
    if !r.is_finite() || r <= 0.0 || color.a == 0.0 { return; }
    if !end_angle.is_finite() || !start_angle.is_finite() || (end_angle - start_angle).abs() < 0.001 { return; }
    let mut k = 0x58EE98C0u64;
    k = mix_key(k, r);
    k = mix_key(k, start_angle);
    k = mix_key(k, end_angle);
    k ^= uv_key(&batch.uv);
    batch.geo_emit_template(k, |b| emit_arc(b, r, start_angle, end_angle, color), color);
}

pub(crate) fn geo_emit_rect_outline(batch: &mut DrawBatch, w: f32, h: f32, thickness: f32, color: Color) {
    if !w.is_finite() || !h.is_finite() || !thickness.is_finite() || w <= 0.0 || h <= 0.0 || thickness <= 0.0 || color.a == 0.0 { return; }
    let half = thickness * 0.5;
    let x2 = w;
    let y2 = h;
    geo_emit_line_chain(batch, &[
        (half, half),
        (x2 - half, half),
        (x2 - half, y2 - half),
        (half, y2 - half),
        (half, half),
    ], thickness, color);
}

pub(crate) fn geo_emit_circle_outline(batch: &mut DrawBatch, r: f32, thickness: f32, color: Color, segments: u32) {
    if !r.is_finite() || !thickness.is_finite() || r <= 0.0 || thickness <= 0.0 || color.a == 0.0 { return; }
    let n = segments.max(8) as usize;
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n + 1);
    for i in 0..n {
        let a = (i as f32 / n as f32) * std::f32::consts::TAU;
        pts.push((r * a.cos(), r * a.sin()));
    }
    pts.push(pts[0]);
    geo_emit_line_chain(batch, &pts, thickness, color);
}

pub(crate) fn geo_emit_ellipse_outline(batch: &mut DrawBatch, rx: f32, ry: f32, thickness: f32, color: Color, segments: u32) {
    if !rx.is_finite() || !ry.is_finite() || !thickness.is_finite() || rx <= 0.0 || ry <= 0.0 || thickness <= 0.0 || color.a == 0.0 { return; }
    let n = (segments as usize).max(16);
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n + 1);
    for i in 0..n {
        let a = (i as f32 / n as f32) * std::f32::consts::TAU;
        pts.push((rx * a.cos(), ry * a.sin()));
    }
    pts.push(pts[0]);
    geo_emit_line_chain(batch, &pts, thickness, color);
}

pub(crate) fn geo_emit_rounded_rect_outline(
    batch: &mut DrawBatch,
    w: f32, h: f32, radius: f32, thickness: f32,
    color: Color,
    corner_segments: u32,
) {
    if !w.is_finite() || !h.is_finite() || !thickness.is_finite() || w <= 0.0 || h <= 0.0 || thickness <= 0.0 || color.a == 0.0 { return; }
    let r = radius.min(w * 0.5).min(h * 0.5);
    if r == 0.0 {
        return geo_emit_rect_outline(batch, w, h, thickness, color);
    }
    let half = thickness * 0.5;
    let cr = (r - half).max(0.0);
    let cs = corner_segments.max(2);
    let cs_usize = cs as usize;
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(4 * (cs_usize + 1) + 1);
    let two_pi = 2.0 * std::f32::consts::PI;
    let corners: [(f32, f32, f32, f32); 4] = [
        (r,         r,         std::f32::consts::PI,        std::f32::consts::PI * 1.5),
        (w - r,     r,         std::f32::consts::PI * 1.5,  two_pi),
        (w - r,     h - r,     0.0,       std::f32::consts::FRAC_PI_2),
        (r,         h - r,     std::f32::consts::FRAC_PI_2, std::f32::consts::PI),
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
    geo_emit_line_chain(batch, &pts, thickness, color);
}

pub(crate) fn geo_emit_triangle_outline(
    batch: &mut DrawBatch,
    x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32,
    thickness: f32,
    color: Color,
) {
    geo_emit_line_chain(batch, &[(x1, y1), (x2, y2), (x3, y3), (x1, y1)], thickness, color);
}

pub(crate) fn geo_emit_polygon_outline(batch: &mut DrawBatch, points: &[(f32, f32)], thickness: f32, color: Color) {
    if points.len() < 3 { return; }
    let mut closed: Vec<(f32, f32)> = Vec::with_capacity(points.len() + 1);
    closed.extend_from_slice(points);
    closed.push(points[0]);
    geo_emit_line_chain(batch, &closed, thickness, color);
}

pub(crate) fn geo_emit_arc_outline(
    batch: &mut DrawBatch,
    r: f32, start_angle: f32, end_angle: f32, thickness: f32,
    color: Color,
    segments: u32,
) {
    if !r.is_finite() || !thickness.is_finite() || r <= 0.0 || thickness <= 0.0 || color.a == 0.0 { return; }
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
    geo_emit_line_chain(batch, &points, thickness, color);
}
