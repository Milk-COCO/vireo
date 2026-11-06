//! 字形**有符号距离场**生成：从字体轮廓（不是从覆盖度位图）算精确欧氏距离。
//!
//! 为什么不是"覆盖度做 EDT"：覆盖度只有 0/1 与抗锯齿边缘，距离是从像素格量起的，
//! 1px 描边会抖半像素。从轮廓算距离能到 `band/255` 的精度（band=4 时约 0.016px）。
//!
//! 流程：
//! 1. `skrifa` 取字形轮廓（font units，应用 variations）；
//! 2. 映射到"padding 后的图像空间"（缩放 + 子像素偏移 + y 翻转 + 可选斜体切变）；
//! 3. `zeno` 在同一空间栅格化填充掩码 —— 只用来定**符号**；
//! 4. 每个 texel 求到所有段的最近距离（按段 bbox 剪枝），按掩码取正负，量化成 8bit。
//!
//! 语义约定（与 `text_render::glyph_quad` 的放置一致）：
//! - 距离单位是**设备像素**，字形轮廓上为 `0`，内部为负、外部为正；
//! - 存值 `q = (clamp(d, -band, band) / band * 0.5 + 0.5) * 255`，片元解码
//!   `d = (q / 255 * 2 - 1) * band`。

use cosmic_text::CacheKey;
use skrifa::instance::{NormalizedCoord, Size};
use skrifa::outline::DrawSettings;
use skrifa::outline::pen::PathElement;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use zeno::{Fill, Mask, Origin, Scratch, Vector};

/// `tan(14°)`：swash 合成斜体用的切变系数（x += tan14 * y，y 轴向上）。
const TAN_ITALIC: f32 = 0.249_328_0;

/// 生成好的距离场图（含 padding）。
#[derive(Clone, Debug)]
pub(crate) struct GlyphSdf {
    /// 图像宽（= 字形宽 + 2*band）。
    pub width: u16,
    /// 图像高（= 字形高 + 2*band）。
    pub height: u16,
    /// 相对笔位的左偏移（已含 padding：`swash_left - band`）。
    pub left: i16,
    /// 相对基线的上偏移（已含 padding：`swash_top + band`）。
    pub top: i16,
    /// R8 距离图，行优先。
    pub data: Vec<u8>,
}

/// 一段轮廓（三次以 `[p0, p1, p2, p3]` 计，二次把 `p1` 重复成三次形式）。
#[derive(Clone, Copy, Debug)]
pub(crate) enum Seg {
    /// `[x0, y0, x1, y1]`
    Line([f32; 4]),
    /// 二次贝塞尔（控制点、终点）：`[cx, cy, x1, y1]`，起点由前一段给出
    Quad([f32; 4]),
    /// 三次贝塞尔：`[c1x, c1y, c2x, c2y, x1, y1]`
    Cubic([f32; 6]),
}

/// 把 `skrifa` 的路径元素收集成段列表（`map` 负责空间变换）。
fn collect_segments(
    elements: &[PathElement],
    map: &impl Fn(f32, f32) -> (f32, f32),
) -> (Vec<Seg>, Vec<zeno::Command>) {
    let mut segs = Vec::new();
    let mut cmds = Vec::with_capacity(elements.len());
    let mut cur = (0.0f32, 0.0f32);
    let mut start = (0.0f32, 0.0f32);
    for element in elements {
        match *element {
            PathElement::MoveTo { x, y } => {
                let p = map(x, y);
                cur = p;
                start = p;
                cmds.push(zeno::Command::MoveTo(Vector::new(p.0, p.1)));
            }
            PathElement::LineTo { x, y } => {
                let p = map(x, y);
                segs.push(Seg::Line([cur.0, cur.1, p.0, p.1]));
                cur = p;
                cmds.push(zeno::Command::LineTo(Vector::new(p.0, p.1)));
            }
            PathElement::QuadTo { cx0, cy0, x, y } => {
                let c = map(cx0, cy0);
                let p = map(x, y);
                segs.push(Seg::Quad([c.0, c.1, p.0, p.1]));
                cur = p;
                cmds.push(zeno::Command::QuadTo(
                    Vector::new(c.0, c.1),
                    Vector::new(p.0, p.1),
                ));
            }
            PathElement::CurveTo {
                cx0,
                cy0,
                cx1,
                cy1,
                x,
                y,
            } => {
                let c0 = map(cx0, cy0);
                let c1 = map(cx1, cy1);
                let p = map(x, y);
                segs.push(Seg::Cubic([c0.0, c0.1, c1.0, c1.1, p.0, p.1]));
                cur = p;
                cmds.push(zeno::Command::CurveTo(
                    Vector::new(c0.0, c0.1),
                    Vector::new(c1.0, c1.1),
                    Vector::new(p.0, p.1),
                ));
            }
            PathElement::Close => {
                if cur != start {
                    // `Close` 隐式连回起点：zeno 自己会闭合，但距离场必须显式建段。
                    segs.push(Seg::Line([cur.0, cur.1, start.0, start.1]));
                }
                cur = start;
                cmds.push(zeno::Command::Close);
            }
        }
    }
    (segs, cmds)
}

/// 段的控制点包围盒（含起点，用于剪枝），外扩 `pad`。
fn seg_bbox(seg: &Seg, from: [f32; 2], pad: f32) -> [f32; 4] {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut add = |x: f32, y: f32| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    add(from[0], from[1]);
    match *seg {
        Seg::Line([_, _, x1, y1]) => {
            add(x1, y1);
        }
        Seg::Quad([cx, cy, x1, y1]) => {
            add(cx, cy);
            add(x1, y1);
        }
        Seg::Cubic([c1x, c1y, c2x, c2y, x1, y1]) => {
            add(c1x, c1y);
            add(c2x, c2y);
            add(x1, y1);
        }
    }
    [min_x - pad, min_y - pad, max_x + pad, max_y + pad]
}

/// 段到点的**平方**距离；`from` 是段的起点（Line/Quad/Cubic 共用）。
#[inline]
fn seg_dist2(seg: &Seg, from: [f32; 2], p: [f32; 2]) -> f32 {
    match *seg {
        Seg::Line([_, _, x1, y1]) => {
            let a = from;
            let b = [x1, y1];
            let ab = [b[0] - a[0], b[1] - a[1]];
            let ap = [p[0] - a[0], p[1] - a[1]];
            let len2 = ab[0] * ab[0] + ab[1] * ab[1];
            let t = if len2 > 1e-12 {
                ((ap[0] * ab[0] + ap[1] * ab[1]) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = [ap[0] - ab[0] * t, ap[1] - ab[1] * t];
            d[0] * d[0] + d[1] * d[1]
        }
        Seg::Quad([cx, cy, x1, y1]) => {
            let p0 = from;
            let p1 = [cx, cy];
            let p2 = [x1, y1];
            // 先用几个种子挑最近的 t，再做 Newton（quadratic 的距离方程是三次的）
            let mut best_t = 0.0f32;
            let mut best = f32::MAX;
            for k in 0..=8 {
                let t = k as f32 / 8.0;
                let q = quad_at(p0, p1, p2, t);
                let d = sq(q, p);
                if d < best {
                    best = d;
                    best_t = t;
                }
            }
            let t = refine_quad(p0, p1, p2, p, best_t);
            sq(quad_at(p0, p1, p2, t), p)
        }
        Seg::Cubic([c1x, c1y, c2x, c2y, x1, y1]) => {
            let p0 = from;
            let p1 = [c1x, c1y];
            let p2 = [c2x, c2y];
            let p3 = [x1, y1];
            let mut best_t = 0.0f32;
            let mut best = f32::MAX;
            for k in 0..=12 {
                let t = k as f32 / 12.0;
                let d = sq(cubic_at(p0, p1, p2, p3, t), p);
                if d < best {
                    best = d;
                    best_t = t;
                }
            }
            let t = refine_cubic(p0, p1, p2, p3, p, best_t);
            sq(cubic_at(p0, p1, p2, p3, t), p)
        }
    }
}

#[inline]
fn sq(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

#[inline]
fn quad_at(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    [
        u * u * p0[0] + 2.0 * u * t * p1[0] + t * t * p2[0],
        u * u * p0[1] + 2.0 * u * t * p1[1] + t * t * p2[1],
    ]
}

#[inline]
fn cubic_at(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let (uu, tt) = (u * u, t * t);
    [
        uu * u * p0[0] + 3.0 * uu * t * p1[0] + 3.0 * u * tt * p2[0] + tt * t * p3[0],
        uu * u * p0[1] + 3.0 * uu * t * p1[1] + 3.0 * u * tt * p2[1] + tt * t * p3[1],
    ]
}

/// Newton 迭代求二次贝塞尔的最近点参数（`d/dt |P(t)-p|² = 0`）。
fn refine_quad(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p: [f32; 2], t0: f32) -> f32 {
    let mut t = t0;
    for _ in 0..8 {
        // P(t) 与 P'(t)
        let q = quad_at(p0, p1, p2, t);
        let dq = [
            2.0 * (1.0 - t) * (p1[0] - p0[0]) + 2.0 * t * (p2[0] - p1[0]),
            2.0 * (1.0 - t) * (p1[1] - p0[1]) + 2.0 * t * (p2[1] - p1[1]),
        ];
        let ddq = [
            2.0 * (p2[0] - 2.0 * p1[0] + p0[0]),
            2.0 * (p2[1] - 2.0 * p1[1] + p0[1]),
        ];
        let diff = [q[0] - p[0], q[1] - p[1]];
        // f = (q-p)·q', f' = q'·q' + (q-p)·q''
        let f = diff[0] * dq[0] + diff[1] * dq[1];
        let fp = dq[0] * dq[0] + dq[1] * dq[1] + diff[0] * ddq[0] + diff[1] * ddq[1];
        if fp.abs() < 1e-9 {
            break;
        }
        let step = f / fp;
        let next = (t - step).clamp(0.0, 1.0);
        if (next - t).abs() < 1e-6 {
            t = next;
            break;
        }
        t = next;
    }
    t
}

/// 三次贝塞尔的最近点参数。
fn refine_cubic(
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    p3: [f32; 2],
    p: [f32; 2],
    t0: f32,
) -> f32 {
    let mut t = t0;
    for _ in 0..10 {
        let q = cubic_at(p0, p1, p2, p3, t);
        let u = 1.0 - t;
        let dq = [
            3.0 * u * u * (p1[0] - p0[0])
                + 6.0 * u * t * (p2[0] - p1[0])
                + 3.0 * t * t * (p3[0] - p2[0]),
            3.0 * u * u * (p1[1] - p0[1])
                + 6.0 * u * t * (p2[1] - p1[1])
                + 3.0 * t * t * (p3[1] - p2[1]),
        ];
        let ddq = [
            6.0 * u * (p2[0] - 2.0 * p1[0] + p0[0]) + 6.0 * t * (p3[0] - 2.0 * p2[0] + p1[0]),
            6.0 * u * (p2[1] - 2.0 * p1[1] + p0[1]) + 6.0 * t * (p3[1] - 2.0 * p2[1] + p1[1]),
        ];
        let diff = [q[0] - p[0], q[1] - p[1]];
        let f = diff[0] * dq[0] + diff[1] * dq[1];
        let fp = dq[0] * dq[0] + dq[1] * dq[1] + diff[0] * ddq[0] + diff[1] * ddq[1];
        if fp.abs() < 1e-9 {
            break;
        }
        let next = (t - f / fp).clamp(0.0, 1.0);
        if (next - t).abs() < 1e-6 {
            t = next;
            break;
        }
        t = next;
    }
    t
}

/// 由「段列表 + 填充掩码」算出 8bit 距离场（纯函数，便于单测）。
///
/// `inside[j * w + i] > 127` 视为字形内部。
fn field_from_segments(
    segs: &[Seg],
    starts: &[[f32; 2]],
    inside: &[u8],
    w: usize,
    h: usize,
    band: f32,
) -> Vec<u8> {
    let mut d2 = vec![f32::MAX; w * h];
    for (seg, from) in segs.iter().zip(starts) {
        let bbox = seg_bbox(seg, *from, band + 1.0);
        let x0 = (bbox[0].floor().max(0.0)) as usize;
        let y0 = (bbox[1].floor().max(0.0)) as usize;
        let x1 = ((bbox[2].ceil() + 1.0).max(0.0) as usize).min(w);
        let y1 = ((bbox[3].ceil() + 1.0).max(0.0) as usize).min(h);
        for j in y0..y1 {
            for i in x0..x1 {
                let p = [i as f32 + 0.5, j as f32 + 0.5];
                let d = seg_dist2(seg, *from, p);
                let idx = j * w + i;
                if d < d2[idx] {
                    d2[idx] = d;
                }
            }
        }
    }
    let inv_band = 1.0 / band.max(1e-3);
    let mut out = vec![0u8; w * h];
    for idx in 0..w * h {
        let d = d2[idx].sqrt();
        // 内部为负（掩码 > 127），外部为正
        let signed = if inside[idx] > 127 { -d } else { d };
        let q = (signed.clamp(-band, band) * inv_band * 0.5 + 0.5) * 255.0;
        out[idx] = q.clamp(0.0, 255.0) as u8;
    }
    out
}

/// 为一个字形生成距离场。
///
/// `placement` 是 swash 给出的 `(left, top, width, height)`（未含 padding），
/// 用它作为几何基准可以让 SDF 字形与覆盖度字形**落在同一像素格上**。
pub(crate) fn generate(
    font_system: &mut cosmic_text::FontSystem,
    key: CacheKey,
    placement: (i16, i16, u16, u16),
    band: f32,
) -> Option<GlyphSdf> {
    let (swash_left, swash_top, swash_w, swash_h) = placement;
    if swash_w == 0 || swash_h == 0 {
        return None;
    }
    let font = font_system.get_font(key.font_id, key.font_weight)?;
    let face_index = font_system
        .db()
        .face(font.id())
        .map_or(0, |face| face.index);
    let font_ref = FontRef::from_index(font.data(), face_index).ok()?;
    let glyph_id = GlyphId::new(u32::from(key.glyph_id));

    // 与 `colr_v1_fallback` 完全相同的 variations（只有 wght），否则缓存键会分叉。
    let wght = skrifa::Tag::from_be_bytes(*b"wght");
    let mut variations: Vec<NormalizedCoord> = Vec::new();
    if let Some(axis) = font_ref.axes().get_by_tag(wght) {
        let weight = f32::from(key.font_weight.0).clamp(axis.min_value(), axis.max_value());
        variations.push(axis.normalize(weight));
    }

    let upem = f32::from(
        font_ref
            .metrics(Size::unscaled(), variations.as_slice())
            .units_per_em,
    );
    let size_px = f32::from_bits(key.font_size_bits);
    if upem <= 0.0 || !size_px.is_finite() || size_px <= 0.0 {
        return None;
    }

    // skrifa 取样点（font units，y 轴向上）
    let outline = font_ref.outline_glyphs().get(glyph_id)?;
    let mut pen: Vec<PathElement> = Vec::new();
    outline
        .draw(
            DrawSettings::unhinted(Size::unscaled(), variations.as_slice()),
            &mut pen,
        )
        .ok()?;
    if pen.is_empty() {
        return None;
    }

    // 子像素偏移与 y 翻转：与 swash 的放置一致
    let (mut bx, mut by) = (key.x_bin.as_float(), key.y_bin.as_float());
    if key.flags.contains(cosmic_text::CacheKeyFlags::PIXEL_FONT) {
        bx = bx.round();
        by = by.round();
    }
    let italic = key.flags.contains(cosmic_text::CacheKeyFlags::FAKE_ITALIC);
    let scale = size_px / upem;
    let band_i = band.ceil().max(1.0);
    let pad = band_i;

    // 设备像素（y 向上）→ padding 后的图像空间（y 向下）
    let map = |x: f32, y: f32| -> (f32, f32) {
        let mut dx = x * scale + bx;
        let dy = y * scale + by;
        if italic {
            // swash 的合成斜体：`Transform::skew(14°, 0)`，作用在设备坐标上
            dx += TAN_ITALIC * dy;
        }
        let img_x = dx - swash_left as f32 + pad;
        let img_y = swash_top as f32 - dy + pad;
        (img_x, img_y)
    };
    let (segs, cmds) = collect_segments(&pen, &map);

    // 段的起点（Line/Quad/Cubic 共用）：MoveTo 之后的当前点。
    // `collect_segments` 已经保证每段都从前一段终点接上，这里重建一次。
    let starts = segment_starts(&pen, &map);

    let w = swash_w as usize + 2 * pad as usize;
    let h = swash_h as usize + 2 * pad as usize;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }

    // 符号：zeno 在同一空间栅格化填充掩码
    let mut inside = vec![0u8; w * h];
    let mut scratch = Scratch::new();
    Mask::with_scratch(&cmds, &mut scratch)
        .style(Fill::NonZero)
        .origin(Origin::TopLeft)
        .offset(Vector::ZERO)
        .render_offset(Vector::ZERO)
        .size(w as u32, h as u32)
        .render_into(&mut inside, None);

    let data = field_from_segments(&segs, &starts, &inside, w, h, band_i);

    Some(GlyphSdf {
        width: w as u16,
        height: h as u16,
        left: swash_left.saturating_sub(pad as i16),
        top: swash_top.saturating_add(pad as i16),
        data,
    })
}

/// 与 `collect_segments` 同序的段起点列表。
fn segment_starts(
    elements: &[PathElement],
    map: &impl Fn(f32, f32) -> (f32, f32),
) -> Vec<[f32; 2]> {
    let mut starts = Vec::new();
    let mut cur = [0.0f32, 0.0];
    let mut start = [0.0f32, 0.0];
    for element in elements {
        match *element {
            PathElement::MoveTo { x, y } => {
                let (mx, my) = map(x, y);
                cur = [mx, my];
                start = cur;
            }
            PathElement::LineTo { x, y } => {
                starts.push(cur);
                let (mx, my) = map(x, y);
                cur = [mx, my];
            }
            PathElement::QuadTo { x, y, .. } => {
                starts.push(cur);
                let (mx, my) = map(x, y);
                cur = [mx, my];
            }
            PathElement::CurveTo { x, y, .. } => {
                starts.push(cur);
                let (mx, my) = map(x, y);
                cur = [mx, my];
            }
            PathElement::Close => {
                if cur != start {
                    starts.push(cur);
                }
                cur = start;
            }
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用「段 + 起点」直接跑场（不做字体解析）。
    fn field(segs: &[(Seg, [f32; 2])], w: usize, h: usize, band: f32, inside: &[u8]) -> Vec<u8> {
        let seg_list: Vec<Seg> = segs.iter().map(|(s, _)| *s).collect();
        let starts: Vec<[f32; 2]> = segs.iter().map(|(_, p)| *p).collect();
        field_from_segments(&seg_list, &starts, inside, w, h, band)
    }

    fn decode(q: u8, band: f32) -> f32 {
        (q as f32 / 255.0 * 2.0 - 1.0) * band
    }

    /// 一条水平线段的距离：线上为 0，垂直偏移处等于该偏移。
    #[test]
    fn line_distance_is_euclidean() {
        let segs = [(Seg::Line([0.0, 5.0, 20.0, 5.0]), [0.0, 5.0])];
        let w = 24;
        let h = 12;
        let f = field(&segs, w, h, 4.0, &[0u8; 24 * 12]);
        // texel (10,5) 中心 (10.5,5.5) 距线 0.5
        let d = decode(f[5 * w + 10], 4.0);
        assert!((d - 0.5).abs() < 0.05, "距线 0.5px，实际 {d}");
        // texel (10,9) 距线 4.5 → 钳到 band=4
        let d = decode(f[9 * w + 10], 4.0);
        assert!(d > 3.9, "超出 band 应钳制，实际 {d}");
    }

    /// 内部为负、外部为正（符号由掩码决定）。
    #[test]
    fn sign_follows_inside_mask() {
        let segs = [(Seg::Line([2.0, 5.0, 20.0, 5.0]), [2.0, 5.0])];
        let w = 24;
        let h = 12;
        let mut inside = vec![0u8; w * h];
        for j in 0..6 {
            for i in 0..w {
                inside[j * w + i] = 255;
            }
        }
        let f = field(&segs, w, h, 4.0, &inside);
        let far_inside = decode(f[w + 10], 4.0);
        let far_outside = decode(f[10 * w + 10], 4.0);
        assert!(far_inside < 0.0, "内部应为负，实际 {far_inside}");
        assert!(far_outside > 0.0, "外部应为正，实际 {far_outside}");
    }

    /// 没有任何段时，全图都是"远"（255）。
    #[test]
    fn empty_path_is_all_far() {
        let f = field(&[], 4, 4, 4.0, &[0u8; 16]);
        assert!(f.iter().all(|q| *q == 255));
    }

    /// 二次贝塞尔两端点距离为 0。
    #[test]
    fn quad_distance_at_endpoints_is_zero() {
        let seg = Seg::Quad([10.0, 0.0, 10.0, 10.0]);
        let from = [0.0, 0.0];
        assert!(seg_dist2(&seg, from, from).sqrt() < 1e-3);
        assert!(seg_dist2(&seg, from, [10.0, 10.0]).sqrt() < 1e-2);
    }

    /// 三次贝塞尔中点距离应等于到曲线中点的距离（而非到弦的距离）。
    #[test]
    fn cubic_distance_beats_chord() {
        // 控制点把曲线拱起来：P(0.5) = (5, 7.5)
        let seg = Seg::Cubic([0.0, 10.0, 10.0, 10.0, 10.0, 0.0]);
        let from = [0.0, 0.0];
        let d = seg_dist2(&seg, from, [5.0, 7.5]).sqrt();
        assert!(d < 0.1, "曲线中点距离应≈0，实际 {d}");
    }
}
