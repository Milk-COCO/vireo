//! ciallo — 炸裂开场

use vireo::prelude::*;
use std::f32::consts::TAU;

fn hue(h: f32) -> Color {
    // h in [0, 1] → rainbow RGB
    let h = (h % 1.0) * 6.0;
    let c = 1.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color::new(r, g, b, 0.65)
}

fn main() {
    let mut app = App::new();
    let logo_idx = app.load_texture("logo.png").ok();
    let idx = app.window(
        WindowDesc::new("Ciallo, Vireo!", 640, 420).icon_from_path("logo.png"),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.016;

        let cx = 320.0;
        let cy = 210.0;

        // 彩虹旋转环
        let mut ring = DrawBatch::new();
        for i in 0..16 {
            let a = t + i as f32 * TAU / 16.0;
            let r = 150.0 + (t * 2.5 + i as f32).sin() * 45.0;
            draw_circle(&mut ring,
                cx + a.cos() * r, cy + a.sin() * r,
                10.0, hue(i as f32 / 16.0 + t * 0.05), 20);
        }

        // Logo 贴图
        let mut logo_batch = DrawBatch::new();
        if let Some(logo) = logo_idx.and_then(|i| app.texture(i)) {
            let s = 250.0 + (t * 1.5).sin() * 20.0;
            draw_texture(&mut logo_batch, &logo,
                TextureOptions::default().rect(cx - s / 2.0, cy - s / 2.0, s, s));
        }

        // 文字
        let mut text = DrawBatch::new();
        let wb = (t * 4.0).sin() * 4.0;
        draw_text(&mut text.texts, "Ciallo, Vireo!",
            TextOptions::default()
                .x(cx - 115.0 + wb).y(cy - 55.0)
                .font_size(42.0)
                .color(Color::new(1.0, 0.95, 0.65, 1.0)));
        draw_text(&mut text.texts, "\u{2014} Just a genus of bird!",
            TextOptions::default()
                .x(cx - 50.).y(cy + 0.0)
                .font_size(16.0)
                .color(Color::new(0.5, 0.65, 0.85, 1.0)));

        win.draw(Some(Color::new(0.04, 0.06, 0.1, 1.0)), &[&ring, &logo_batch, &text]);

        true
    });
}
