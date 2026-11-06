//! 文本样式展示：描边 / 多层描边 / 软投影 / 发光 / 内描边 / 浮雕。
//!
//! 全部来自同一个原语：**对字形的有符号距离场取带软边的阈值**（见
//! `docs/reference/text-style.md`）。所以这里每种效果都只是 `TextStyle` 的一个图层列表。
//!
//! 按键：
//! - `1`..`8` 切换示例组（描边 / 多层 / 投影 / 发光 / 内描边 / 浮雕 / 全部 / 渐变字）
//! - `←→` 描边宽度（逻辑像素，DPI 变化时视觉宽度不变）
//! - `Q` / `ESC` 退出

use std::sync::Arc;

use vireo::prelude::*;

const W: u32 = 900;
const H: u32 = 520;

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
    let mut group = 7u32;

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return true,
        };
        if win.key_down(KeyCode::Escape) || win.key_down(KeyCode::KeyQ) {
            return false;
        }
        if win.key_down(KeyCode::ArrowLeft) {
            width = (width - 0.5).max(0.5);
        }
        if win.key_down(KeyCode::ArrowRight) {
            width = (width + 0.5).min(8.0);
        }
        for (k, code) in [
            (1u32, KeyCode::Digit1),
            (2, KeyCode::Digit2),
            (3, KeyCode::Digit3),
            (4, KeyCode::Digit4),
            (5, KeyCode::Digit5),
            (6, KeyCode::Digit6),
            (7, KeyCode::Digit7),
            (8, KeyCode::Digit8),
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
                "内描边（阈值取负，只在字形内侧）",
                "Inner 内描边",
                Some(
                    TextStyle::inner_outline(width + 1.0, warm)
                        .with_fill(TextPaint::solid(Color::new(0.08, 0.09, 0.12, 1.0))),
                ),
                width,
            );
        }
        if show_all || group == 6 {
            row(
                &mut b,
                next(84.0),
                "浮雕（一亮一暗两层，不同偏移）",
                "Emboss 浮雕",
                Some(TextStyle::emboss(width, WHITE, ink).with_fill(TextPaint::solid(blue))),
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

        draw_text(
            &mut b.texts,
            &format!(
                "1-8 切换示例组（当前 {group}，7 = 全部）    ←→ 描边宽度 {width:.1}     Q 退出"
            ),
            Pos::new(28.0, 24.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.72, 0.77, 0.86, 1.0)),
        );

        win.draw(Color::new(0.07, 0.075, 0.09, 1.0), &[&b]);
        true
    })
    .await
    .unwrap();
}
