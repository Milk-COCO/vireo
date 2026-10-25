//! 折线沿线渐变（`draw_line_chain_colors`）：逐点色、按弧长连续过渡。
//!
//! 跑法：`cargo run --example line_gradient`（需显示器）。
//!
//! 三块内容：
//!
//! 1. **vault 那种「预览框内保持、框外淡出」**：一条 200 点缓动曲线，框内 alpha 0.8、
//!    框外 26px 内线性淡到 0（再往外不画）。按 `D` 在**旧做法（切 4 档单色）**和
//!    **逐点渐变**之间切换 —— 旧做法的档位边界肉眼可见（26px / 4 ≈ 6.5px 一级）。
//! 2. **两端异色直线**：白 → 青，验证同一份几何 + 不同色带不串色。
//! 3. **闭合彩环**：逐点色 + 首尾闭合（末点颜色被忽略，wrap 段回到首色）。
//!
//! 关键点：颜色是按**弧长**插值的（不是按点序号），所以点距不均匀时过渡速度也均匀；
//! 色带经过 Douglas–Peucker 简化（误差 ≤ 1/255），线性渐变只占 2 个 stop。

use vireo::prelude::*;

/// 预览框（逻辑像素）。框外每多 1px，alpha 线性下降；超过 `FADE` px 直接为 0。
const BOX: (f32, f32, f32, f32) = (80.0, 70.0, 480.0, 220.0); // x, y, w, h
const FADE: f32 = 26.0;
const INSIDE_ALPHA: f32 = 0.8;

/// 曲线采样点数（vault 的等效密度：点距 ≈ 1px）。
const SAMPLES: usize = 200;

fn curve_points() -> Vec<(f32, f32)> {
    let (bx, by, bw, bh) = BOX;
    (0..SAMPLES)
        .map(|i| {
            let t = i as f32 / (SAMPLES - 1) as f32;
            let x = bx + 8.0 + t * (bw - 16.0);
            // 一条上下起伏的缓动曲线（故意冲出框的上下边）
            let y = by + bh * 0.5 - (t * std::f32::consts::TAU * 1.6).sin() * (bh * 0.75);
            (x, y)
        })
        .collect()
}

/// 逐点 alpha：框内 0.8，框外按超出距离线性淡到 0（超出 > FADE 即 0）。
fn fade_alpha(y: f32) -> f32 {
    let (_, by, _, bh) = BOX;
    let over = if y < by {
        by - y
    } else if y > by + bh {
        y - (by + bh)
    } else {
        0.0
    };
    if over >= FADE {
        0.0
    } else {
        INSIDE_ALPHA * (1.0 - over / FADE)
    }
}

#[vireo::main]
async fn main() {
    let idx = app.window(WindowDesc::new("vireo line gradient", 640, 520));

    // 旧做法：整条曲线按序号切成 4 段，每段一个常量 alpha。
    let banded = curve_points();
    let band_count = 4usize;
    let band_alphas: Vec<f32> = (0..band_count)
        .map(|k| {
            let mid = banded.len() * (2 * k + 1) / (2 * band_count);
            fade_alpha(banded[mid].1)
        })
        .collect();

    let mut smooth = true;
    let mut prev_d = false;

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };
        if win.key_down(KeyCode::Escape) || win.key_down(KeyCode::KeyQ) {
            return false;
        }

        // D：切换「旧分档 / 逐点渐变」
        let d_down = win.key_down(KeyCode::KeyD);
        if d_down && !prev_d {
            smooth = !smooth;
        }
        prev_d = d_down;

        let mut b = DrawBatch::new();

        // ---- 预览框 + 标题 ----
        draw_rectangle(
            &mut b,
            Pos::new(BOX.0, BOX.1),
            BOX.2,
            BOX.3,
            Some(Color::new(0.10, 0.11, 0.15, 1.0)),
        );
        draw_rect_outline(
            &mut b,
            Pos::new(BOX.0, BOX.1),
            BOX.2,
            BOX.3,
            1.5,
            Some(Color::new(0.35, 0.38, 0.45, 1.0)),
        );

        // ---- 1) 曲线：逐点渐变 vs 旧分档 ----
        let curve = curve_points();
        if smooth {
            let colors: Vec<Color> = curve
                .iter()
                .map(|&(_, y)| Color::new(0.55, 0.80, 1.00, fade_alpha(y)))
                .collect();
            draw_line_chain_colors(&mut b, &curve, 2.0, &colors);
        } else {
            // 4 段，每段一个常量色 —— 档位边界就是肉眼看得到的台阶。
            for k in 0..band_count {
                let start = curve.len() * k / band_count;
                let end = curve.len() * (k + 1) / band_count;
                let alpha = band_alphas[k];
                if alpha <= 0.0 {
                    continue;
                }
                let seg = &curve[start..end.min(curve.len())];
                if seg.len() < 2 {
                    continue;
                }
                draw_line_chain(&mut b, seg, 2.0, Some(Color::new(0.55, 0.80, 1.00, alpha)));
            }
        }

        // ---- 2) 两端异色直线 ----
        draw_line_chain_colors(
            &mut b,
            &[(80.0, 330.0), (560.0, 330.0)],
            8.0,
            &[
                Color::new(1.0, 0.85, 0.30, 1.0),
                Color::new(0.20, 0.85, 0.95, 1.0),
            ],
        );

        // ---- 3) 闭合彩环：逐点色，末点即首点（末色被忽略） ----
        let (cx, cy, r) = (320.0f32, 425.0f32, 55.0f32);
        let ring: Vec<(f32, f32)> = (0..=96)
            .map(|i| {
                let a = i as f32 / 96.0 * std::f32::consts::TAU;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        let ring_colors: Vec<Color> = ring
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let t = i as f32 / 96.0;
                Color::new(
                    0.5 + 0.5 * (t * std::f32::consts::TAU).cos(),
                    0.5 + 0.5 * (t * std::f32::consts::TAU + 2.09).cos(),
                    0.5 + 0.5 * (t * std::f32::consts::TAU + 4.19).cos(),
                    1.0,
                )
            })
            .collect();
        draw_line_chain_colors(&mut b, &ring, 6.0, &ring_colors);

        // ---- HUD ----
        let hud = format!(
            "D = 切换  {}",
            if smooth {
                "逐点渐变（弧长连续）"
            } else {
                "旧做法：4 段常量 alpha（看得见台阶）"
            }
        );
        draw_text(
            &mut b.texts,
            &hud,
            Pos::new(16.0, 16.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(0.9, 0.92, 0.96, 1.0)),
        );
        draw_text(
            &mut b.texts,
            "Q / ESC = 退出",
            Pos::new(16.0, 500.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.6, 0.64, 0.72, 1.0)),
        );

        win.draw(Color::new(0.04, 0.04, 0.06, 1.0), &[&b]);
        true
    })
    .await
    .unwrap();
}
