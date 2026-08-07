//! 离屏渲染示例：与窗口对称的 OffscreenCanvas

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let off_idx = app.offscreen(256, 256, AntiAliasing::None);
    let idx = app.window(WindowDesc::new("Offscreen Render", 800, 600), None::<fn()>);

    app.run(move |app| {
        let canvas = app.offscreen_ref(&off_idx).unwrap();
        let win = app.window_ref(&idx).unwrap();

        // 离屏渲染
        let mut off_batch = DrawBatch::new();
        draw_rectangle(&mut off_batch, Pos::new(0.0, 0.0), 64.0, 64.0, Some(RED));
        draw_rectangle(&mut off_batch, Pos::new(192.0, 0.0), 64.0, 64.0, Some(BLUE));
        draw_rectangle(&mut off_batch, Pos::new(0.0, 192.0), 64.0, 64.0, Some(GREEN));
        draw_rectangle(&mut off_batch, Pos::new(192.0, 192.0), 64.0, 64.0, Some(YELLOW));
        draw_rectangle(&mut off_batch, Pos::new(96.0, 96.0), 64.0, 64.0, Some(WHITE));
        canvas.draw(Some(DARKGRAY), &[&off_batch]);

        // 贴到窗口
        let mut win_batch = DrawBatch::new();
        win_batch.set_texture(Some(&canvas.texture));
        draw_rectangle(&mut win_batch, Pos::new(50.0, 50.0), 256.0, 256.0, Some(WHITE));
        draw_rectangle(&mut win_batch, Pos::new(350.0, 200.0), 256.0, 256.0, Some(WHITE));
        win.draw(BLACK, &[&win_batch]);

        true
    });
}
