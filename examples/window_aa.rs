//! 抗锯齿对比：MSAA / SDF / None

use vireo::prelude::*;

fn draw_shapes(batch: &mut DrawBatch, sdf: f32) {
    batch.sdf_feather = sdf;
    draw_triangle(batch, 150.0, 280.0, 60.0, 40.0, 350.0, 60.0, YELLOW);
    draw_triangle(batch, 250.0, 120.0, 380.0, 250.0, 180.0, 280.0, ORANGE);
    draw_circle(batch, 420.0, 190.0, 70.0, Color::new(0.2, 0.6, 1.0, 1.0));
    draw_line(batch, 30.0, 160.0, 470.0, 160.0, 3.0, Color::new(0.4, 0.7, 0.4, 1.0));
    draw_line_chain(batch, &[(30.0, 40.0), (150.0, 20.0), (250.0, 60.0)], 3.0, WHITE);
    let label = if sdf > 0.0 { format!("SDF {:.0}px", sdf) } else { "SDF off".into() };
    draw_text(&mut batch.texts, &label,
        TextOptions::default().x(30.0).y(310.0).font_size(16.0).color(Color::new(0.5, 0.5, 0.6, 1.0)));
}

fn main() {
    let mut app = App::new();

    let msaa = app.window(
        WindowDesc::new("MSAA x4", 500, 350)
            .anti_aliasing(AntiAliasing::Msaa { samples: 4, alpha_to_coverage: true }),
        None::<fn()>,
    );
    let sdf = app.window(WindowDesc::new("SDF 2px", 500, 350), None::<fn()>);
    let raw = app.window(WindowDesc::new("No AA", 500, 350), None::<fn()>);

    app.run(move |app| {
        if let Some(w) = app.window_ref(&msaa) { let mut b = DrawBatch::new(); draw_shapes(&mut b, 0.0); w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(w) = app.window_ref(&sdf) { let mut b = DrawBatch::new(); draw_shapes(&mut b, 2.0); w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(w) = app.window_ref(&raw) { let mut b = DrawBatch::new(); draw_shapes(&mut b, 0.0); w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        true
    });
}
