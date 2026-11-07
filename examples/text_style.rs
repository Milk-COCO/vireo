//! 文本样式展示：描边 / 多层描边 / 软投影 / 发光 / 内描边 / 内阴影 / 渐变字 / emoji / 几何描边。
//!
//! 全部来自同一个原语：**对字形的有符号距离场取带软边的阈值**（见
//! `docs/reference/text-style.md`）。所以这里每种效果都只是 `TextStyle` 的一个图层列表。
//!
//! 按键：
//! - `1`..`9` / `0` 切换示例组（描边 / 多层 / 投影 / 发光 / 内描边 / 内阴影 / 全部 / 渐变字 /
//!   emoji / 几何描边）；默认第 1 组，按 `7` 看全部
//! - `←→` 描边宽度（逻辑像素，DPI 变化时视觉宽度不变；按住会加速 0.5→1→2 px/步）
//! - `Q` / `ESC` 退出
//!
//! **性能提示**：带样式的字形要现场算距离场（约 0.25–1.8 ms/字形，见
//! `cargo test --lib sdf_timing -- --ignored --nocapture`）。debug 构建比 release 慢 5–7 倍，
//! 所以调宽度请在 `--release` 下玩：`cargo run --release --example text_style`。

use std::sync::Arc;

use vireo::prelude::*;

const W: u32 = 900;
// 10 组 × 84px 行距 ≈ 840，窗口要放得下（之前 520 高，后几组直接被裁掉）
const H: u32 = 900;

/// 一行演示文字 + 说明。
fn row(b: &mut DrawBatch, y: f32, label: &str, text: &str, style: Option<TextStyle>, width: f32) {
    draw_text(
        &mut b.texts,
        label,
        Pos::new(28.0, y + 10.0),
        TextDef::default().font_size(12.0),
        TextOverride::from_color(Color::new(0.62, 0.67, 0.76, 1.0)),
    );
    let def = TextDef {
        style: style.map(Arc::new),
        ..TextDef::default().font_size(44.0)
    };
    let _ = width;
    draw_text(
        &mut b.texts,
        text,
        Pos::new(230.0, y),
        def,
        TextOverride::from_color(Color::new(0.96, 0.97, 1.0, 1.0)),
    );
}

#[vireo::main]
async fn main(app: App) {
    let idx = app.window(WindowDesc::new("Vireo Text Styles", W, H));

    let mut width = 2.0f32;
    // 默认只画一组：一次上 100+ 个带样式字形在 debug 下要几百毫秒（每个字形都要算距离场）。
    // 想看全部按 7。
    let mut group = std::env::var("VIREO_DEMO_GROUP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1u32);
    // 描边宽度按"重复延迟"步进，而不是每帧步进：
    // 每帧步进 = 60 次/秒，会把 band 档位一路撞过去，每次都触发重新栅格化（看着就像卡死）。
    let mut repeat_at = std::time::Instant::now();
    let mut held = (false, false);
    let mut hold_ms = 0.0f64;
    // 帧耗时统计（HUD 显示；按键后"要等多久"看这个）
    let mut frame_ms = 0.0f64;
    let mut hud_text = String::from("帧 -- ms");
    let mut last_log = std::time::Instant::now();

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return true,
        };
        if win.key_down(KeyCode::Escape) || win.key_down(KeyCode::KeyQ) {
            return false;
        }
        let (left, right) = (
            win.key_down(KeyCode::ArrowLeft),
            win.key_down(KeyCode::ArrowRight),
        );
        let dir = match (left, right) {
            (true, false) => -1,
            (false, true) => 1,
            _ => 0,
        };
        // 刚按下立刻响应一次；按住之后按**加速度**推进（越按越快）：
        //   0~0.6s：每 120ms 走 0.5；0.6~1.4s：每 60ms 走 1.0；之后每 30ms 走 2.0。
        // 为什么要限速而不是每帧走：每帧改宽度会让样式表每帧重登记、跨 band 档（4/8/16/32/64）
        // 时还要把全屏带样式字形重新栅格化一次，看起来像卡死。加速后从 2 拉到 30 约 1 秒。
        if dir != 0 {
            hold_ms += ctx.frame_time() * 1000.0;
        } else {
            hold_ms = 0.0;
        }
        let (interval_ms, step) = if hold_ms < 600.0 {
            (120.0, 0.5)
        } else if hold_ms < 1400.0 {
            (60.0, 1.0)
        } else {
            (30.0, 2.0)
        };
        let fresh = dir != 0 && (held.0, held.1) != (left, right);
        if dir != 0 && (fresh || repeat_at.elapsed().as_secs_f64() * 1000.0 >= interval_ms) {
            width = (width + step * dir as f32).max(0.0);
            repeat_at = std::time::Instant::now();
        }
        held = (left, right);
        for (k, code) in [
            (1u32, KeyCode::Digit1),
            (2, KeyCode::Digit2),
            (3, KeyCode::Digit3),
            (4, KeyCode::Digit4),
            (5, KeyCode::Digit5),
            (6, KeyCode::Digit6),
            (7, KeyCode::Digit7),
            (8, KeyCode::Digit8),
            (9, KeyCode::Digit9),
            (10, KeyCode::Digit0),
        ] {
            if win.key_down(code) {
                group = k;
            }
        }

        let mut b = DrawBatch::new();
        let ink = Color::new(0.10, 0.11, 0.14, 0.92);
        let blue = Color::new(0.25, 0.55, 0.95, 1.0);
        let warm = Color::new(1.0, 0.66, 0.25, 1.0);

        let mut y = 70.0f32;
        let mut next = |n: f32| {
            let v = y;
            y += n;
            v
        };

        let show_all = group == 7;
        if show_all || group == 1 {
            row(
                &mut b,
                next(84.0),
                "外描边（硬边，宽度按设备像素）",
                "Outline 描边",
                Some(TextStyle::outline(width, ink)),
                width,
            );
        }
        if show_all || group == 2 {
            // 多层：由外到内（先画宽的，再画窄的）
            let multi = TextStyle::new()
                .with_layer(TextLayer::hard(width + 3.0, TextPaint::solid(ink)))
                .with_layer(TextLayer::hard(width, TextPaint::solid(blue)));
            row(
                &mut b,
                next(84.0),
                "多层描边（3px 黑 + 2px 蓝）",
                "多层 描边",
                Some(multi),
                width,
            );
        }
        if show_all || group == 3 {
            row(
                &mut b,
                next(84.0),
                "软投影（距离场 → erf 边缘，偏移 4,4）",
                "Shadow 投影",
                Some(
                    TextStyle::shadow((4.0, 4.0), 3.0, ink)
                        .with_layer(TextLayer::hard(width, TextPaint::solid(ink))),
                ),
                width,
            );
        }
        if show_all || group == 4 {
            row(
                &mut b,
                next(84.0),
                "发光（无偏移的软层，加性混合）",
                "Glow 发光",
                Some(
                    TextStyle::glow(6.0, Color::new(0.35, 0.75, 1.0, 0.55)).with_layer(
                        TextLayer::hard(width, TextPaint::solid(ink)).blend(TextBlend::Over),
                    ),
                ),
                width,
            );
        }
        if show_all || group == 5 {
            row(
                &mut b,
                next(84.0),
                "内描边（字形内的环 (−w, 0]，画在填充之上）",
                "Inner 内描边",
                Some(
                    TextStyle::inner_outline(width + 1.0, warm)
                        .with_fill(TextPaint::solid(Color::new(0.08, 0.09, 0.12, 1.0))),
                ),
                width,
            );
        }
        if show_all || group == 6 {
            // 内阴影：字形内的 `(−∞, 0]` 带 + 偏移 + 软边，画在填充之上（否则会被填充盖住）。
            let y_row = next(84.0);
            let inner_shadow = TextStyle::inner_shadow((4.0, 4.0), width.max(1.5), ink)
                .with_fill(TextPaint::solid(Color::new(0.55, 0.68, 0.95, 1.0)));
            row(
                &mut b,
                y_row,
                "内阴影（字形内暗部，不越界）",
                "Inner shadow 内阴影",
                Some(inner_shadow),
                width,
            );
        }
        if show_all || group == 8 {
            // 渐变字：`in_rect` 让渐变跨满整行（不给则逐字形）
            let y_row = next(84.0);
            let grad = TextGradient::vertical(
                Color::new(1.0, 0.72, 0.28, 1.0),
                Color::new(0.92, 0.25, 0.55, 1.0),
            )
            .in_rect(230.0, y_row, 420.0, 56.0);
            row(
                &mut b,
                y_row,
                "渐变填充（页面矩形参照系 + 描边）",
                "Gradient 渐变字",
                Some(
                    TextStyle::new()
                        .with_layer(TextLayer::hard(width, TextPaint::solid(ink)))
                        .with_fill(TextPaint::Gradient(grad)),
                ),
                width,
            );
        }
        if show_all || group == 9 {
            // 彩色字形（emoji）：描边垫在底下，emoji 保留自己的颜色
            let y_row = next(84.0);
            draw_text(
                &mut b.texts,
                "彩色字形加描边（COLRv1 走 alpha 距离场）",
                Pos::new(28.0, y_row + 10.0),
                TextDef::default().font_size(12.0),
                TextOverride::from_color(Color::new(0.62, 0.67, 0.76, 1.0)),
            );
            let def = TextDef {
                style: Some(Arc::new(TextStyle::outline(
                    width + 1.0,
                    Color::new(0.05, 0.06, 0.09, 0.9),
                ))),
                ..TextDef::default()
                    .font_size(44.0)
                    .with_family(Family::Name("Segoe UI Emoji"))
            };
            draw_text(
                &mut b.texts,
                "😀🍑🌈🦄",
                Pos::new(230.0, y_row),
                def,
                TextOverride::from_color(WHITE),
            );
        }

        if show_all || group == 10 {
            // 几何描边：真 miter 尖角（距离场描边是圆角连接）
            let y_row = next(84.0);
            draw_text(
                &mut b.texts,
                "几何描边（miter 尖角，SVG 观感）",
                Pos::new(28.0, y_row + 10.0),
                TextDef::default().font_size(12.0),
                TextOverride::from_color(Color::new(0.62, 0.67, 0.76, 1.0)),
            );
            let def = TextDef {
                style: Some(Arc::new(TextStyle::vector_outline(
                    TextStroke::new(width * 2.0, ink)
                        .join(TextJoin::Miter)
                        .miter_limit(6.0),
                ))),
                ..TextDef::default().font_size(44.0)
            };
            draw_text(
                &mut b.texts,
                "Vector 尖角 AMW",
                Pos::new(230.0, y_row),
                def,
                TextOverride::from_color(Color::new(0.96, 0.97, 1.0, 1.0)),
            );
        }

        // 帧耗时（含"这一帧栅格化了多少字形"）——按键后要等多久，看这个数最直接
        frame_ms = frame_ms * 0.85 + ctx.frame_time() * 1000.0 * 0.15; // 指数平滑
        let fps = ctx.fps();
        // HUD 字符串只在数值**变化到可读粒度**时更新：每帧换字符串会让排版缓存每帧失效，
        // 既费 CPU 又可能让整批文字的 slot 抖动。
        let want = format!("帧 {:.0} ms（{:.0} fps）", frame_ms, fps);
        if want != hud_text {
            hud_text = want;
        }
        // 需要命令行看帧率时：`VIREO_FPS_LOG=1 cargo run --release --example text_style`
        if std::env::var("VIREO_FPS_LOG").is_ok() {
            let now = std::time::Instant::now();
            if (now - last_log).as_secs_f64() >= 1.0 {
                last_log = now;
                eprintln!("[fps] group={group} {fps:.1} fps  {frame_ms:.1} ms/帧");
            }
        }

        draw_text(
            &mut b.texts,
            &format!("1-9 / 0 切组（当前 {group}，7 = 全部）  ←→ 描边宽度 {width:.1}  Q 退出"),
            Pos::new(28.0, 24.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.72, 0.77, 0.86, 1.0)),
        );
        draw_text(
            &mut b.texts,
            &hud_text,
            Pos::new(28.0, 42.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(Color::new(0.5, 0.56, 0.66, 1.0)),
        );

        win.draw(Color::new(0.07, 0.075, 0.09, 1.0), &[&b]);
        true
    })
    .await
    .unwrap();
}
