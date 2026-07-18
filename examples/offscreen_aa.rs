//! 离屏抗锯齿对比：SSAA / MSAA / SDF
//!
//! - SSAA：per-sample SDF 着色
//! - MSAA：几何光栅化 + 硬件多重采样
//! - SDF：SDF feather 分析性抗锯齿

use vireo::prelude::*;

fn draw_off(batch: &mut DrawBatch, sdf: f32) {
    if sdf > 0.0 {
        batch.sdf_feather = Some(sdf);
    }
    draw_circle(batch, 150.0, 150.0, 90.0, Color::new(0.2, 0.6, 1.0, 1.0));
    draw_triangle(batch, 150.0, 260.0, 60.0, 70.0, 240.0, 70.0, ORANGE);
    draw_line(batch, 30.0, 150.0, 270.0, 150.0, 3.0, Color::new(0.4, 0.7, 0.4, 1.0));
}

fn main() {
    let mut app = App::new();
    let off_ssaa = app.offscreen(300, 300, AntiAliasing::Ssaa { samples: 4, alpha_to_coverage: true });
    let off_msaa = app.offscreen(300, 300, AntiAliasing::Msaa { samples: 4, alpha_to_coverage: true });
    let off_sdf  = app.offscreen(300, 300, AntiAliasing::None);
    let win = app.window(
        WindowDesc::new("Offscreen AA: SSAA vs MSAA vs SDF", 1000, 380).high_dpi(true),
        None::<fn()>,
    );

    app.run(move |app| {
        if let Some(c) = app.offscreen_ref(&off_ssaa) { let mut b = DrawBatch::new(); draw_off(&mut b, 1.0); c.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(c) = app.offscreen_ref(&off_msaa) { let mut b = DrawBatch::new(); draw_off(&mut b, 0.0); c.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(c) = app.offscreen_ref(&off_sdf)  { let mut b = DrawBatch::new(); draw_off(&mut b, 1.0); c.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let (Some(w), Some(c1), Some(c2), Some(c3)) = (
            app.window_ref(&win),
            app.offscreen_ref(&off_ssaa),
            app.offscreen_ref(&off_msaa),
            app.offscreen_ref(&off_sdf),
        ) {
            let mut b = DrawBatch::new();
            b.set_texture(&c1.texture); draw_rectangle(&mut b, 15.0, 30.0, 300.0, 300.0, WHITE);
            b.set_texture(&c2.texture); draw_rectangle(&mut b, 350.0, 30.0, 300.0, 300.0, WHITE);
            b.set_texture(&c3.texture); draw_rectangle(&mut b, 685.0, 30.0, 300.0, 300.0, WHITE);
            draw_text(&mut b.texts, "SSAA x4 + SDF 1px",
                TextOptions::default().x(75.0).y(345.0).font_size(14.0).color(WHITE));
            draw_text(&mut b.texts, "MSAA x4 (geometry)",
                TextOptions::default().x(415.0).y(345.0).font_size(14.0).color(WHITE));
            draw_text(&mut b.texts, "SDF 1px only",
                TextOptions::default().x(785.0).y(345.0).font_size(14.0).color(WHITE));
            w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]);
        }
        true
    });
}
