//! 多 batch 合并到同一个 render pass

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Multi Batch", 500, 300), None::<fn()>);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        // Batch 1: 红色边框
        let mut batch1 = DrawBatch::new();
        draw_rect_outline(&mut batch1, Pos::new(10.0, 10.0), 100.0, 80.0, 2.0, Some(RED));

        // Batch 2: 文字
        let mut batch2 = DrawBatch::new();
        draw_text(&mut batch2.texts, "Batch 1: 红色边框", Pos::new(20.0, 100.0),
                  TextDef::default().font_size(14.0), TextOverride::from_color(WHITE));

        // Batch 3: 绿色圆圈 + 文字
        let mut batch3 = DrawBatch::new();
        draw_circle(&mut batch3, Pos::new(40.0, 180.0), 20.0, Some(GREEN));
        draw_text(&mut batch3.texts, "Batch 3: 绿色圆圈", Pos::new(20.0, 210.0),
                  TextDef::default().font_size(14.0), TextOverride::from_color(WHITE));

        win.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&batch1, &batch2, &batch3]);

        true
    });
}
