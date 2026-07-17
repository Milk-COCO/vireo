/// 文本属性（Attrs）示例 —— 展示用 with_family/with_weight/with_style 链式 builder
///
/// 预期效果：
/// 1. 标题 "Font Attributes Demo"  — BOLD + SansSerif, 36px, 金色
/// 2. "Normal weight"             — SansSerif, 默认粗细, 白色
/// 3. "Light weight (300)"        — SansSerif, LIGHT, 灰白色
/// 4. "Bold weight (700)"         — SansSerif, BOLD, 白色
/// 5. "Extra Bold (800)"          — SansSerif, EXTRA_BOLD, 亮蓝色
/// 6. "Normal style"              — 常规, 白色
/// 7. "Italic style"              — ITALIC 斜体, 粉红色
/// 8. "Oblique style"             — OBLIQUE 倾斜, 绿色
/// 9. "Monospace"                 — Monospace 等宽, 红色
/// 10. "Cursive"                  — Cursive 手写体, 金色
/// 11. "Serif"                    — Serif 衬线字体, 白色
/// 12. "中文 粗细 测试"          — BOLD + 中文, 白色
/// 13. 链式组合                    — with_family + with_weight + with_style, 青色
///
/// 所有属性全用 TextOptions 的 with_xxx builder 设置，无需手动构建 AttrsOwned。
use vireo::prelude::*;

fn main() {
    let mut app = App::new();

    let idx = app.window(
        WindowDesc::new("Text Attributes", 700, 600).high_dpi(true),
        None::<fn()>,
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let w = win.logical_width as f32;

        let mut batch = DrawBatch::new();

        // 标题: BOLD + SansSerif
        draw_text(
            &mut batch.texts,
            "Font Attributes Demo",
            TextOptions::default()
                .with_family(Family::SansSerif)
                .with_weight(Weight::BOLD)
                .x(20.0)
                .y(30.0)
                .font_size(36.0)
                .color(GOLD),
        );

        let mut y = 90.0_f32;

        // Weight 系列
        draw_text(
            &mut batch.texts,
            "Normal weight — default (400)",
            TextOptions::default()
                .with_family(Family::SansSerif)
                .x(20.0).y(y).font_size(18.0).color(WHITE),
        );
        y += 28.0;

        draw_text(
            &mut batch.texts,
            "Light weight (300)",
            TextOptions::default()
                .with_family(Family::SansSerif)
                .with_weight(Weight::LIGHT)
                .x(30.0).y(y).font_size(18.0)
                .color(Color::new(0.7, 0.7, 0.7, 1.0)),
        );
        y += 28.0;

        draw_text(
            &mut batch.texts,
            "Bold weight (700) — 加粗",
            TextOptions::default()
                .with_family(Family::SansSerif)
                .with_weight(Weight::BOLD)
                .x(30.0).y(y).font_size(18.0).color(WHITE),
        );
        y += 28.0;

        draw_text(
            &mut batch.texts,
            "Extra Bold (800)",
            TextOptions::default()
                .with_family(Family::SansSerif)
                .with_weight(Weight::EXTRA_BOLD)
                .x(30.0).y(y).font_size(18.0).color(SKYBLUE),
        );
        y += 35.0;

        // Style 系列
        draw_text(
            &mut batch.texts,
            "Normal style — regular",
            TextOptions::default()
                .x(20.0).y(y).font_size(18.0).color(WHITE),
        );
        y += 28.0;

        draw_text(
            &mut batch.texts,
            "Italic style — 意大利斜体",
            TextOptions::default()
                .with_style(Style::Italic)
                .x(30.0).y(y).font_size(18.0).color(PINK),
        );
        y += 28.0;

        draw_text(
            &mut batch.texts,
            "Oblique style — 机械倾斜",
            TextOptions::default()
                .with_style(Style::Oblique)
                .x(30.0).y(y).font_size(18.0).color(GREEN),
        );
        y += 35.0;

        // Family 系列
        draw_text(
            &mut batch.texts,
            "Monospace — 等宽字体 0123 abc",
            TextOptions::default()
                .with_family(Family::Monospace)
                .x(20.0).y(y).font_size(18.0).color(RED),
        );
        y += 28.0;

        draw_text(
            &mut batch.texts,
            "Cursive — 手写体风格",
            TextOptions::default()
                .with_family(Family::Cursive)
                .x(20.0).y(y).font_size(18.0).color(GOLD),
        );
        y += 28.0;

        draw_text(
            &mut batch.texts,
            "Serif — 衬线字体",
            TextOptions::default()
                .with_family(Family::Serif)
                .x(20.0).y(y).font_size(18.0).color(WHITE),
        );
        y += 35.0;

        // 中文 + BOLD
        draw_text(
            &mut batch.texts,
            "中文 粗细 测试 — 属性也影响中文渲染",
            TextOptions::default()
                .with_family(Family::SansSerif)
                .with_weight(Weight::BOLD)
                .x(20.0).y(y).font_size(18.0).color(WHITE),
        );
        y += 35.0;

        // 全部组合
        draw_text(
            &mut batch.texts,
            "Chained: Italic + Bold + Monospace",
            TextOptions::default()
                .with_family(Family::Monospace)
                .with_weight(Weight::BOLD)
                .with_style(Style::Italic)
                .x(20.0).y(y).font_size(18.0)
                .color(Color::new(0.4, 1.0, 1.0, 1.0)),
        );

        // 底部提示
        draw_text(
            &mut batch.texts,
            "窗口: 700 x 600  ·  内嵌字体: Fira Sans + Noto Sans SC",
            TextOptions::default()
                .x(w * 0.5 - 230.0).y(570.0).font_size(12.0)
                .color(Color::new(0.4, 0.4, 0.5, 1.0)),
        );

        // 背景参考线
        draw_line(&mut batch, 0.0, 0.0, w, 0.0, 1.0, Color::new(0.2, 0.2, 0.25, 1.0));
        draw_line(&mut batch, 0.0, 0.0, 0.0, 600.0, 1.0, Color::new(0.2, 0.2, 0.25, 1.0));
        draw_line(&mut batch, 0.0, 600.0, w, 600.0, 1.0, Color::new(0.2, 0.2, 0.25, 1.0));
        draw_line(&mut batch, w, 0.0, w, 600.0, 1.0, Color::new(0.2, 0.2, 0.25, 1.0));

        win.draw(Some(Color::new(0.08, 0.1, 0.14, 1.0)), &[&batch]);

        true
    });
}
