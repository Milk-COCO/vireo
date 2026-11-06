//! **多通道有符号距离场（MSDF）**：与按尺寸精确 SDF 并存的另一种字形表示。
//!
//! 为什么需要它：精确 SDF 的缓存键含「字号 × 带宽档」，所以改字号或改描边宽度跨档时，
//! 该样式涉及的所有字形都要**重新从轮廓算距离**。MSDF 把距离存成「生成 em 的百分比」，
//! 一个字形一份数据就能服务任意字号；三通道各自持有不同边的距离，片元取中位数，
//! 尖角不会被单通道距离抹圆。
//!
//! 与精确 SDF 的分工（策略见 `text::sdf_policy`）：
//! - MSDF：字号/缩放/宽度变化频繁的场景，`reach_em` 落在 range 阶梯内的样式；
//! - 精确 SDF：大外扩、小字号、像素字体（这些场景单通道 8bit 更准也更省）。
//!
//! 算法要点：
//! 1. 从字体轮廓取线段（复用 `sdf::OutlineGeometry`，与精确路径同一套映射，保证两者对齐）；
//! 2. 每条边分到 R/G/B 之一（沿轮廓 `i % 3`，相邻边必然异色）；
//! 3. 每个 texel 对三个通道分别求「到该色边的最短距离」（`seg_dist2` 自带端点夹紧，
//!    所以角附近的距离天然正确），符号由填充掩码决定；
//! 4. 片元取三通道中位数即得真实距离 —— 这正是 MSDF 比单通道省内存又能保尖角的原因。

use cosmic_text::CacheKey;
use skrifa::outline::pen::PathElement;
use zeno::{Fill, Mask, Origin, Scratch, Vector};

use super::sdf::{self, Seg};

/// 生成 em 阶梯（像素）。按目标字号选「最小且不小于 `0.75 × 目标字号`」的一档。
///
/// 为什么要阶梯而不是单一值：MSDF 的清晰度取决于「目标像素 / 生成像素」比值，
/// 12–16px 的中文正文如果从 48em 降采样会明显发糊；24em 专供小字号。
pub(crate) const MSDF_EM_LADDER: [f32; 3] = [24.0, 48.0, 96.0];

/// 相对 range 阶梯（占 em 的比例，表示距离场能表示的最大绝对距离）。
///
/// 一个样式的「外扩 / 目标字号」决定需要哪一档：
/// - `0.08`：常规 1–2px 描边、小投影；
/// - `0.20`：中等描边/发光；
/// - `0.45`：很粗的描边（44px 字号下约 20px）。
///
/// 超出最大档就回退精确 SDF（单通道 R8 更省内存）。
pub(crate) const MSDF_RANGE_EM: [f32; 3] = [0.08, 0.20, 0.45];

/// 生成好的 MSDF 图（RGBA8，含 padding）。
#[derive(Clone, Debug)]
pub(crate) struct MsdfGlyph {
    pub width: u16,
    pub height: u16,
    /// 相对笔位的左偏移（已含 padding）。
    pub left: i16,
    /// 相对基线的上偏移（已含 padding）。
    pub top: i16,
    /// 距离范围（生成 em 下的像素）；调用方按「range_em × 生成 em」自己算，
    /// 这里留一份供诊断/文档使用。
    #[allow(dead_code)]
    pub range_px: f32,
    pub data: Vec<u8>,
}

/// 选生成 em 档：最小的档 `>= 0.75 * em_target`；超出最大档用最大档。
pub(crate) fn pick_em(em_target: f32) -> f32 {
    let want = em_target * 0.75;
    for em in MSDF_EM_LADDER {
        if em >= want {
            return em;
        }
    }
    MSDF_EM_LADDER[MSDF_EM_LADDER.len() - 1]
}

/// 选 range 档：最小的档 `>= reach_em`；超出最大档返回 `None`（调用方回退精确 SDF）。
pub(crate) fn pick_range_em(reach_em: f32) -> Option<f32> {
    for r in MSDF_RANGE_EM {
        if r >= reach_em {
            return Some(r);
        }
    }
    None
}

/// 一条带颜色的边：`(段, 起点, 两位通道掩码)`。
struct Edge {
    seg: Seg,
    from: [f32; 2],
    /// bit0 = R、bit1 = G、bit2 = B；每条边写两个通道。
    mask: u8,
}

/// MSDF 的三色是**两位掩码**（cyan/magenta/yellow）：每条边把自己的距离写进两个通道。
///
/// 这是中位数还原能成立的关键：靠近单条边时有两个通道都拿到真实距离，
/// 第三个通道被「别的边」占据，中位数 = 真值；尖角处两条边共享一个通道，
/// 中位数等于两条边距离的最小值（正确的伪距离）。按 `i % 3` 只给一个通道是错的
/// （中位数会取到「第二近的边」）。
const COLOR_MASKS: [u8; 3] = [0b110, 0b101, 0b011];

/// 给一条轮廓的每条边分配两位掩码，保证相邻边不同（环上 3 着色总能做到）。
fn color_contour(count: usize) -> Vec<u8> {
    let mut colors = Vec::with_capacity(count);
    for i in 0..count {
        if i == 0 {
            colors.push(COLOR_MASKS[0]);
            continue;
        }
        let prev = colors[i - 1];
        let first = colors[0];
        let is_last = i + 1 == count;
        let mask = COLOR_MASKS
            .iter()
            .copied()
            .find(|m| *m != prev && (!is_last || *m != first))
            .unwrap_or(COLOR_MASKS[0]);
        colors.push(mask);
    }
    colors
}

/// 把路径拆成「带颜色的边」。
///
/// 颜色沿路径 `i % 3`：相邻边必然异色，这样三个通道的中位数在绝大多数位置等于
/// 到最近边的真实距离（第三个通道的最近边通常更远）。
fn collect_edges(elements: &[PathElement], map: &impl Fn(f32, f32) -> (f32, f32)) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut cur = [0.0f32, 0.0];
    let mut start = [0.0f32, 0.0];
    let mut contours: Vec<(usize, usize)> = Vec::new(); // (start, end) 边下标
    let mut contour_start = 0usize;
    let push = |seg: Seg, from: [f32; 2], edges: &mut Vec<Edge>| {
        edges.push(Edge { seg, from, mask: 0 });
    };
    for element in elements {
        match *element {
            PathElement::MoveTo { x, y } => {
                if edges.len() > contour_start {
                    contours.push((contour_start, edges.len()));
                }
                let (mx, my) = map(x, y);
                cur = [mx, my];
                start = cur;
                contour_start = edges.len();
            }
            PathElement::LineTo { x, y } => {
                let (mx, my) = map(x, y);
                push(Seg::Line([cur[0], cur[1], mx, my]), cur, &mut edges);
                cur = [mx, my];
            }
            PathElement::QuadTo { cx0, cy0, x, y } => {
                let (cx, cy) = map(cx0, cy0);
                let (mx, my) = map(x, y);
                push(Seg::Quad([cx, cy, mx, my]), cur, &mut edges);
                cur = [mx, my];
            }
            PathElement::CurveTo {
                cx0,
                cy0,
                cx1,
                cy1,
                x,
                y,
            } => {
                let (c0x, c0y) = map(cx0, cy0);
                let (c1x, c1y) = map(cx1, cy1);
                let (mx, my) = map(x, y);
                push(Seg::Cubic([c0x, c0y, c1x, c1y, mx, my]), cur, &mut edges);
                cur = [mx, my];
            }
            PathElement::Close => {
                if cur != start {
                    push(
                        Seg::Line([cur[0], cur[1], start[0], start[1]]),
                        cur,
                        &mut edges,
                    );
                }
                contours.push((contour_start, edges.len()));
                contour_start = edges.len();
                cur = start;
            }
        }
    }
    if edges.len() > contour_start {
        contours.push((contour_start, edges.len()));
    }
    // 每条轮廓单独着色：相邻边掩码不同（环上 3 着色）。
    for (start_idx, end_idx) in contours {
        let count = end_idx - start_idx;
        let masks = color_contour(count);
        for (i, mask) in masks.into_iter().enumerate() {
            edges[start_idx + i].mask = mask;
        }
    }
    edges
}

/// 三通道距离场。纯函数，便于单测：`inside[i] > 127` 视为内部。
fn field(edges: &[Edge], inside: &[u8], w: usize, h: usize, range: f32) -> Vec<u8> {
    // 每通道一个「到该色边的最短距离平方」
    let mut d2 = [
        vec![f32::MAX; w * h],
        vec![f32::MAX; w * h],
        vec![f32::MAX; w * h],
    ];
    for edge in edges {
        let bbox = edge_bbox(edge, range + 1.0);
        let x0 = (bbox[0].max(0.0)) as usize;
        let y0 = (bbox[1].max(0.0)) as usize;
        let x1 = ((bbox[2] + 1.0).max(0.0) as usize).min(w);
        let y1 = ((bbox[3] + 1.0).max(0.0) as usize).min(h);
        for j in y0..y1 {
            for i in x0..x1 {
                let p = [i as f32 + 0.5, j as f32 + 0.5];
                let d = sdf::seg_dist2(&edge.seg, edge.from, p);
                let idx = j * w + i;
                // 每条边写进它掩码里的**两个**通道（中位数还原的前提）
                for (c, chan) in d2.iter_mut().enumerate() {
                    if edge.mask & (1 << c) != 0 && d < chan[idx] {
                        chan[idx] = d;
                    }
                }
            }
        }
    }

    let inv = 0.5 / range.max(1e-3);
    let mut out = vec![255u8; w * h * 4];
    for idx in 0..w * h {
        let sign = if inside[idx] > 127 { -1.0 } else { 1.0 };
        let mut ch = [0f32; 3];
        for c in 0..3 {
            let d = d2[c][idx].sqrt();
            ch[c] = sign * d.min(range);
        }
        for c in 0..3 {
            let q = (ch[c] * inv + 0.5) * 255.0;
            out[idx * 4 + c] = q.clamp(0.0, 255.0) as u8;
        }
        // alpha 留作未来「覆盖率」，先填不透明
        out[idx * 4 + 3] = 255;
    }
    out
}

/// 边的包围盒（含起点），外扩 `pad`。
fn edge_bbox(edge: &Edge, pad: f32) -> [f32; 4] {
    let (mut min_x, mut min_y, mut max_x, mut max_y) =
        (edge.from[0], edge.from[1], edge.from[0], edge.from[1]);
    match edge.seg {
        Seg::Line([_, _, x, y]) => {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        Seg::Quad([cx, cy, x, y]) => {
            for (px, py) in [(cx, cy), (x, y)] {
                min_x = min_x.min(px);
                max_x = max_x.max(px);
                min_y = min_y.min(py);
                max_y = max_y.max(py);
            }
        }
        Seg::Cubic([c1x, c1y, c2x, c2y, x, y]) => {
            for (px, py) in [(c1x, c1y), (c2x, c2y), (x, y)] {
                min_x = min_x.min(px);
                max_x = max_x.max(px);
                min_y = min_y.min(py);
                max_y = max_y.max(py);
            }
        }
    }
    [min_x - pad, min_y - pad, max_x + pad, max_y + pad]
}

/// 生成一个 glyph 的 MSDF。`em_px` 是生成字号，`range_px` 是距离范围（同单位）。
pub(crate) fn generate(
    font_system: &mut cosmic_text::FontSystem,
    key: CacheKey,
    placement: (i16, i16, u16, u16),
    range_px: f32,
) -> Option<MsdfGlyph> {
    let (swash_left, swash_top, swash_w, swash_h) = placement;
    if swash_w == 0 || swash_h == 0 || !(range_px > 0.0) {
        return None;
    }
    let geo = sdf::outline_geometry(font_system, key)?;
    let pad = range_px.ceil().max(1.0);
    let map = geo.map(swash_left, swash_top, pad);
    let edges = collect_edges(&geo.pen, &map);
    if edges.is_empty() {
        return None;
    }

    let w = swash_w as usize + 2 * pad as usize;
    let h = swash_h as usize + 2 * pad as usize;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }

    // 符号：用 zeno 在同一空间栅格化填充掩码（与精确 SDF 完全一致）
    let cmds = edges_to_commands(&edges);
    let mut inside = vec![0u8; w * h];
    let mut scratch = Scratch::new();
    Mask::with_scratch(cmds.as_slice(), &mut scratch)
        .style(Fill::NonZero)
        .origin(Origin::TopLeft)
        .offset(Vector::ZERO)
        .render_offset(Vector::ZERO)
        .size(w as u32, h as u32)
        .render_into(&mut inside, None);

    let data = field(&edges, &inside, w, h, pad);
    Some(MsdfGlyph {
        width: w as u16,
        height: h as u16,
        left: swash_left.saturating_sub(pad as i16),
        top: swash_top.saturating_add(pad as i16),
        range_px: pad,
        data,
    })
}

/// 把带颜色的边还原成 zeno 命令（只用于填充掩码）。
fn edges_to_commands(edges: &[Edge]) -> Vec<zeno::Command> {
    let mut cmds = Vec::with_capacity(edges.len() + 1);
    let mut last_end: Option<[f32; 2]> = None;
    for edge in edges {
        if last_end != Some(edge.from) {
            cmds.push(zeno::Command::MoveTo(Vector::new(
                edge.from[0],
                edge.from[1],
            )));
        }
        match edge.seg {
            Seg::Line([_, _, x, y]) => cmds.push(zeno::Command::LineTo(Vector::new(x, y))),
            Seg::Quad([cx, cy, x, y]) => cmds.push(zeno::Command::QuadTo(
                Vector::new(cx, cy),
                Vector::new(x, y),
            )),
            Seg::Cubic([c1x, c1y, c2x, c2y, x, y]) => cmds.push(zeno::Command::CurveTo(
                Vector::new(c1x, c1y),
                Vector::new(c2x, c2y),
                Vector::new(x, y),
            )),
        }
        last_end = Some(match edge.seg {
            Seg::Line([_, _, x, y]) | Seg::Quad([_, _, x, y]) => [x, y],
            Seg::Cubic([_, _, _, _, x, y]) => [x, y],
        });
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 量化 → 解码的往返误差应在半个量化步长内。
    fn quantize(v: f32, range: f32) -> u8 {
        (((v.clamp(-range, range)) * (0.5 / range) + 0.5) * 255.0).clamp(0.0, 255.0) as u8
    }
    fn decode(q: u8, range: f32) -> f32 {
        (q as f32 / 255.0 * 2.0 - 1.0) * range
    }

    #[test]
    fn quantization_roundtrip() {
        let range = 6.0;
        for v in [-6.0f32, -3.3, -0.2, 0.0, 0.7, 2.5, 5.9] {
            let d = decode(quantize(v, range), range);
            assert!(
                (d - v).abs() <= range / 127.0 + 1e-3,
                "量化误差过大：{v} -> {d}"
            );
        }
    }

    /// 相邻边必须异色（否则中位数会退化）。
    #[test]
    fn adjacent_edges_have_distinct_colors() {
        let path = [
            PathElement::MoveTo { x: 0.0, y: 0.0 },
            PathElement::LineTo { x: 10.0, y: 0.0 },
            PathElement::LineTo { x: 10.0, y: 10.0 },
            PathElement::LineTo { x: 0.0, y: 10.0 },
            PathElement::Close,
        ];
        let id = |x: f32, y: f32| (x, y);
        let edges = collect_edges(&path, &id);
        assert_eq!(edges.len(), 4, "4 条边（含闭合边）");
        for (i, edge) in edges.iter().enumerate() {
            let next = &edges[(i + 1) % edges.len()];
            assert_ne!(edge.mask, next.mask, "第 {i} 与下一条边掩码相同");
            assert_eq!(edge.mask.count_ones(), 2, "每条边必须占两个通道");
        }
    }

    /// 方形中心：三通道距离应一致（都在 5px 左右），中位数≈真值。
    #[test]
    fn square_center_median_matches_true_distance() {
        let path = [
            PathElement::MoveTo { x: 2.0, y: 2.0 },
            PathElement::LineTo { x: 18.0, y: 2.0 },
            PathElement::LineTo { x: 18.0, y: 18.0 },
            PathElement::LineTo { x: 2.0, y: 18.0 },
            PathElement::Close,
        ];
        let id = |x: f32, y: f32| (x, y);
        let edges = collect_edges(&path, &id);
        let (w, h) = (20usize, 20usize);
        // 内部掩码：方形内部 = 255
        let mut inside = vec![0u8; w * h];
        for y in 2..18 {
            for x in 2..18 {
                inside[y * w + x] = 255;
            }
        }
        let range = 6.0;
        let img = field(&edges, &inside, w, h, range);
        // 中心 texel (10,10)
        let idx = (10 * w + 10) * 4;
        let ch = [
            decode(img[idx], range),
            decode(img[idx + 1], range),
            decode(img[idx + 2], range),
        ];
        let mut sorted = ch;
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[1];
        // 中心到最近边 8px，但被 range 钳到 -6
        assert!(
            (median + range).abs() < 0.2,
            "中心应有符号距离被钳到 -{range}，实际 {median}（三通道 {ch:?}）"
        );
        assert!(inside[idx / 4] > 0, "掩码内部");
        // 外部一点：应在 +range 附近或更大
        let out_idx = 1 * w + 1;
        let outer = [
            decode(img[out_idx * 4], range),
            decode(img[out_idx * 4 + 1], range),
            decode(img[out_idx * 4 + 2], range),
        ];
        assert!(outer.iter().all(|v| *v > 0.0), "外部距离应为正：{outer:?}");
    }

    /// 三通道必须**不总是相同**：若通道全等，min/median 就退化成单通道，
    /// 说明着色（掩码）没写进不同通道。
    #[test]
    fn channels_differ_near_edges() {
        let path = [
            PathElement::MoveTo { x: 2.0, y: 2.0 },
            PathElement::LineTo { x: 18.0, y: 2.0 },
            PathElement::LineTo { x: 18.0, y: 18.0 },
            PathElement::LineTo { x: 2.0, y: 18.0 },
            PathElement::Close,
        ];
        let id = |x: f32, y: f32| (x, y);
        let edges = collect_edges(&path, &id);
        let (w, h) = (20usize, 20usize);
        let mut inside = vec![0u8; w * h];
        for y in 2..18 {
            for x in 2..18 {
                inside[y * w + x] = 255;
            }
        }
        let range = 6.0;
        let img = field(&edges, &inside, w, h, range);
        let mut differ = 0;
        for idx in 0..w * h {
            let c = [img[idx * 4], img[idx * 4 + 1], img[idx * 4 + 2]];
            if !(c[0] == c[1] && c[1] == c[2]) {
                differ += 1;
            }
        }
        assert!(
            differ > (w * h) / 10,
            "至少 10% 的 texel 三通道应互不相同（实际 {differ}/{}）",
            w * h
        );
    }

    /// MSDF 生成耗时（真实字体）：`cargo test --lib msdf_timing -- --ignored --nocapture`
    ///
    /// 与 `sdf::sdf_timing` 的精确 SDF 并列看：MSDF 更贵（三通道 + RGBA8），
    /// 但**一个字形只算一次**（与字号/宽度无关），所以多尺寸/反复调宽度时总量更省。
    #[test]
    #[ignore = "需要系统字体；量化 MSDF 生成成本（debug vs release 差很多）"]
    fn msdf_timing() {
        use cosmic_text::{Attrs, Buffer, Metrics, Shaping, SwashCache};
        let mut fs = cosmic_text::FontSystem::new();
        let mut swash = SwashCache::new();
        let mut buf = Buffer::new(&mut fs, Metrics::new(48.0, 57.6));
        buf.set_text("国永光", &Attrs::new(), Shaping::Advanced, None);
        buf.shape_until_scroll(&mut fs, true);

        let mut keys = Vec::new();
        for run in buf.layout_runs() {
            for glyph in run.glyphs.iter() {
                let phys = glyph.physical((0.0, 0.0), 1.0);
                if let Some(image) = swash.get_image(&mut fs, phys.cache_key) {
                    let p = image.placement;
                    if p.width > 0 && p.height > 0 {
                        keys.push((
                            phys.cache_key,
                            (p.left as i16, p.top as i16, p.width as u16, p.height as u16),
                        ));
                    }
                }
            }
        }
        assert!(!keys.is_empty(), "没有排到字形（系统字体缺失？）");

        for range_em in MSDF_RANGE_EM {
            let range = range_em * MSDF_EM_LADDER[1]; // 48em
            let start = std::time::Instant::now();
            let n = 10;
            for _ in 0..n {
                for (key, placement) in &keys {
                    let _ = generate(&mut fs, *key, *placement, range);
                }
            }
            let per_glyph = start.elapsed().as_secs_f64() * 1000.0 / (n * keys.len()) as f64;
            println!(
                "  MSDF em48 range {:.2}em ({range:.1}px): {per_glyph:.3} ms/字形（{} 个字形 × {n} 轮）",
                range_em,
                keys.len()
            );
        }
    }

    #[test]
    fn pick_em_and_range_ladders() {
        assert_eq!(pick_em(12.0), 24.0, "12px 目标 -> 24em（0.75*12=9 <= 24）");
        assert_eq!(pick_em(40.0), 48.0, "40px -> 48em（30 <= 48）");
        assert_eq!(pick_em(200.0), 96.0, "超大目标 -> 最大档");
        assert_eq!(pick_range_em(0.05), Some(0.08));
        assert_eq!(pick_range_em(0.15), Some(0.20));
        assert_eq!(pick_range_em(0.45), Some(0.45));
        assert_eq!(pick_range_em(0.6), None, "超最大档应回退精确路径");
    }
}
