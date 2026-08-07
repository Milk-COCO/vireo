//! 增量/叠加渲染：一次 `draw` 内顺序画多个 batch
//!
//! 一个 `win.draw(clear_color, batches)` = 一次 acquire + 一个 render pass +
//! 一次 present = **一帧**。pass 内先按 `clear_color` 清屏，再按顺序画所有 batch。
//!
//! 历史：旧版支持同一帧内两次 `win.draw(None, ...)`（第一次 Clear、第二次 Load 增量）。
//! 该写法在 swapchain 多 buffer + `FLIP_DISCARD`（present 后内容被丢弃，Load 只读到
//! 未定义值）下已不可靠，且会让两帧交替上屏。等价能力请用一次 draw 多 batch 表达。

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("增量渲染", 500, 200).present_mode(PresentMode::Immediate),
        None::<fn()>,
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        // 同一次 draw：先 Clear（底色），再按序叠加 b1、b2
        let mut b1 = DrawBatch::new();
        draw_rectangle(&mut b1, Pos::new(20.0, 40.0), 460.0, 50.0, Some(Color::new(0.15, 0.3, 0.5, 1.0)));
        draw_text(&mut b1.texts, "Batch #1", Pos::new(30.0, 50.0),
                  TextDef::default().font_size(20.0), TextOverride::from_color(WHITE));

        let mut b2 = DrawBatch::new();
        draw_rectangle(&mut b2, Pos::new(20.0, 110.0), 460.0, 50.0, Some(Color::new(0.5, 0.2, 0.15, 1.0)));
        draw_text(&mut b2.texts, "Batch #2（叠加）", Pos::new(30.0, 120.0),
                  TextDef::default().font_size(20.0), TextOverride::from_color(WHITE));

        win.draw(Color::new(0.06, 0.08, 0.12, 1.0), &[&b1, &b2]);

        true
    });
}
