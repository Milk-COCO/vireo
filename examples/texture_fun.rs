//! 神圣 logo

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let logo_idx = app.load_texture("logo.png");
    let logo_bg_idx = app.load_texture("logo_bg.png");
    let idx = app.window(
        WindowDesc::new("可是我觉得这很神圣啊", 600, 400),
        None::<fn()>,
    );

    let mut angle: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let logo = app.texture(logo_idx).unwrap();
        let logo_bg = app.texture(logo_bg_idx).unwrap();
        angle += 0.012;

        let cx = 300.0;
        let cy = 200.0;
        let float = (angle * 2.0).sin() * 40.0;

        // Batch 1: 圣光背景（发光圆圈）
        let mut glow = DrawBatch::new();
        let glow_r = 100.0 + (angle * 1.5).sin() * 20.0;
        for j in 0..6 {
            let a = j as f32 / 6.0;
            draw_circle(&mut glow, Pos::new(cx, cy + float), glow_r * (1.0 - a * 0.5), Some(Color::new(1.0, 0.9, 0.5, 0.08 - a * 0.01)));
        }

        // Batch 2: 四张部分围绕中心旋转 + 一起上下浮动
        let mut batch = DrawBatch::new();
        // texture set via set_texture

        for i in 0..4 {
            let a = angle + i as f32 * std::f32::consts::FRAC_PI_2;
            let r = 130.0 + (angle * 3.0).sin() * 30.0;
            let x = cx + a.cos() * r - 40.0;
            let y = cy + a.sin() * r - 40.0 + float;

            let (u0, v0) = match i {
                0 => (0.0, 0.0),
                1 => (0.5, 0.0),
                2 => (0.0, 0.5),
                _ => (0.5, 0.5),
            };
            batch.set_texture(Some(&logo_bg));
            batch.set_uv(u0, v0, u0 + 0.5, v0 + 0.5);
            draw_rectangle(&mut batch, Pos::new(x, y), 80.0, 80.0, Some(WHITE));
            batch.clear_uv();
        }

        // 中间原图
        batch.set_texture(Some(&logo));
        draw_rectangle(&mut batch, Pos::new(cx - 50.0, cy + float - 50.0), 100.0, 100.0, Some(WHITE));

        win.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&glow, &batch]);

        true
    });
}
