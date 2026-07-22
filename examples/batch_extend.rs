//! 增量渲染测试：同一帧内 Clear → Load 追加
//!
//! 第 0 帧：draw(Some(bg)) 清屏 + 画背景，然后 draw(None) 追加一行
//! 第 1 帧起保持窗口打开

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("增量渲染", 500, 200).present_mode(PresentMode::Immediate),
        None::<fn()>,
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        // 第一次 draw：Clear
        let mut b1 = DrawBatch::new();
        draw_rectangle(&mut b1, Pos::new(20.0, 40.0), 460.0, 50.0, Some(Color::new(0.15, 0.3, 0.5, 1.0)));
        draw_text(&mut b1.texts, "Draw #1 — Clear",
            TextOptions::default().x(30.0).y(50.0).font_size(20.0).color(WHITE));
        win.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&b1]);

        // 第二次 draw：Load（同帧同纹理，不清屏）
        let mut b2 = DrawBatch::new();
        draw_rectangle(&mut b2, Pos::new(20.0, 110.0), 460.0, 50.0, Some(Color::new(0.5, 0.2, 0.15, 1.0)));
        draw_text(&mut b2.texts, "Draw #2 — Load（增量追加）",
            TextOptions::default().x(30.0).y(120.0).font_size(20.0).color(WHITE));
        win.draw(None, &[&b2]);

        true
    });
}
