//! 图形覆盖文本测试：验证 batch1.shapes → batch1.texts → batch2.shapes → batch2.texts
//!
//! 三个 Layer 在同一行，后一个 Layer 的矩形覆盖前一个 Layer 的文字：
//!   可见文字:  "Layer 1"左半 → "Layer 2"左半 → "Layer 3"
//!   覆盖关系:  Layer2矩形覆盖 Layer1文字右半 → Layer3矩形覆盖 Layer2文字右半

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("图形覆盖文本", 520, 180), None::<fn()>);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        // 所有文字和矩形在同一 y 行(80)，x 逐渐右移
        // Layer 1: 红色矩形 + 白色文字（右半被 Layer 2 矩形遮盖）
        let mut b1 = DrawBatch::new();
        draw_rectangle(&mut b1, Pos::new(20.0, 60.0), 140.0, 50.0, Some(Color::new(0.8, 0.15, 0.15, 0.9)));
        draw_text(&mut b1.texts, "Layer 1 — RED",
            TextOptions::default().x(30.0).y(68.0).font_size(24.0).color(WHITE));

        // Layer 2: 绿色矩形 + 白色文字（矩形盖 Layer 1 文字右半，自身文字右半被 Layer 3 盖）
        let mut b2 = DrawBatch::new();
        draw_rectangle(&mut b2, Pos::new(130.0, 60.0), 170.0, 50.0, Some(Color::new(0.15, 0.7, 0.15, 0.95)));
        draw_text(&mut b2.texts, "Layer 2 — GREEN",
            TextOptions::default().x(140.0).y(68.0).font_size(24.0).color(WHITE));

        // Layer 3: 蓝色矩形 + 白色文字（矩形盖 Layer 2 文字右半）
        let mut b3 = DrawBatch::new();
        draw_rectangle(&mut b3, Pos::new(270.0, 60.0), 180.0, 50.0, Some(Color::new(0.15, 0.25, 0.8, 0.95)));
        draw_text(&mut b3.texts, "Layer 3 — BLUE",
            TextOptions::default().x(280.0).y(68.0).font_size(24.0).color(WHITE));

        win.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&b1, &b2, &b3]);

        true
    });
}
