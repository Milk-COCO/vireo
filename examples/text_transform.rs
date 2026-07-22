//! 文字 Transform 演示：对比旧 API（无变换）和新 API（跟随变换）

use vireo::prelude::*;

fn main() {
    let mut app = App::new();

    let idx = app.window(
        WindowDesc::new("Text Transform Demo", 600, 320),
        None::<fn()>,
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let bg = Color::new(0.08, 0.08, 0.14, 1.0);
        let gray = Color::new(0.4, 0.4, 0.5, 1.0);
        let yl = Color::new(1.0, 0.85, 0.3, 1.0);

        // === 标题 ===
        let mut title = DrawBatch::new();
        draw_text(&mut title.texts, "batch.text() vs draw_text(): transform follows shapes",
            TextOptions::default().x(10.0).y(10.0).font_size(13.0).color(Color::new(0.6, 0.6, 0.8, 1.0)));

        // === 1. 无 transform（旧 API） ===
        let mut b0 = DrawBatch::new();
        draw_rectangle(&mut b0, Pos::new(10.0, 50.0), 6.0, 24.0, Some(yl));
        draw_text(&mut b0.texts, "old: no transform",
            TextOptions::default().x(22.0).y(50.0).font_size(13.0).color(gray));

        // === 2. 平移 ===
        let mut b1 = DrawBatch::new();
        b1.set_position(200.0, 40.0);
        draw_rectangle(&mut b1, Pos::new(10.0, 50.0), 6.0, 24.0, Some(yl));
        b1.text("translate (200, 40)", TextOptions::default().x(22.0).y(50.0).font_size(13.0).color(yl));
        b1.text("geometry + text move together", TextOptions::default().x(22.0).y(67.0).font_size(11.0).color(gray));

        // === 3. 旋转（正坐标，绕自身 pivot） ===
        let mut b2 = DrawBatch::new();
        b2.set_position(120.0, 180.0);
        b2.set_pivot(50.0, 10.0);
        b2.set_rad(0.35);
        // 半透明背景框
        draw_rectangle(&mut b2, Pos::new(0.0, 0.0), 100.0, 20.0, Some(Color::new(0.15, 0.5, 0.3, 0.2)));
        b2.text("rotate 20°", TextOptions::default().x(5.0).y(2.0).font_size(14.0).color(yl));
        b2.text("around pivot", TextOptions::default().x(5.0).y(20.0).font_size(11.0).color(gray));

        // === 4. 缩放 ===
        let mut b3 = DrawBatch::new();
        b3.set_position(380.0, 40.0);
        b3.set_scale(2.0, 1.3);
        draw_rectangle(&mut b3, Pos::new(10.0, 50.0), 3.0, 18.0, Some(yl));
        b3.text("scale 2x", TextOptions::default().x(17.0).y(50.0).font_size(14.0).color(yl));
        b3.text("stretched", TextOptions::default().x(17.0).y(67.0).font_size(11.0).color(gray));

        win.draw(Some(bg), &[&title, &b0, &b1, &b2, &b3]);
        true
    });
}
