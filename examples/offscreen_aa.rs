//! 离屏抗锯齿对比：MSAA vs SDF vs None

use vireo::prelude::*;

fn draw_off(batch: &mut DrawBatch, sdf: f32) {
    batch.sdf_feather = sdf;
    draw_circle(batch, 150.0, 150.0, 90.0, Color::new(0.2, 0.6, 1.0, 1.0));
    draw_triangle(batch, 150.0, 260.0, 60.0, 70.0, 240.0, 70.0, ORANGE);
    draw_line(batch, 30.0, 150.0, 270.0, 150.0, 3.0, Color::new(0.4, 0.7, 0.4, 1.0));
}

fn main() {
    let mut app = App::new();
    let off_msaa = app.offscreen(300, 300, AntiAliasing::Msaa { samples: 4, alpha_to_coverage: true });
    let off_sdf = app.offscreen(300, 300, AntiAliasing::None);
    let off_none = app.offscreen(300, 300, AntiAliasing::None);
    let win = app.window(
        WindowDesc::new("Offscreen AA: MSAA vs SDF vs None", 1000, 380).high_dpi(true),
        None::<fn()>,
    );

    app.run(move |app| {
        if let Some(c) = app.offscreen_ref(&off_msaa) { let mut b = DrawBatch::new(); draw_off(&mut b, 0.0); c.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(c) = app.offscreen_ref(&off_sdf) { let mut b = DrawBatch::new(); draw_off(&mut b, 2.0); c.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(c) = app.offscreen_ref(&off_none) { let mut b = DrawBatch::new(); draw_off(&mut b, 0.0); c.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let (Some(w), Some(c1), Some(c2), Some(c3)) = (
            app.window_ref(&win),
            app.offscreen_ref(&off_msaa),
            app.offscreen_ref(&off_sdf),
            app.offscreen_ref(&off_none),
        ) {
            let mut b = DrawBatch::new();
            draw_texture(&mut b, &c1.texture, TextureOptions::default().rect(15.0, 30.0, 300.0, 300.0));
            draw_texture(&mut b, &c2.texture, TextureOptions::default().rect(350.0, 30.0, 300.0, 300.0));
            draw_texture(&mut b, &c3.texture, TextureOptions::default().rect(685.0, 30.0, 300.0, 300.0));
            draw_text(&mut b.texts, "MSAA x4",
                TextOptions::default().x(100.0).y(345.0).font_size(14.0).color(WHITE));
            draw_text(&mut b.texts, "SDF 2px",
                TextOptions::default().x(445.0).y(345.0).font_size(14.0).color(WHITE));
            draw_text(&mut b.texts, "No AA",
                TextOptions::default().x(795.0).y(345.0).font_size(14.0).color(WHITE));
            w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]);
        }
        true
    });
}
