//! 抗锯齿对比：SSAA / MSAA / SDF / None
//!
//! - SSAA x4：per-sample 着色（几何光栅化，每个采样点独立执行片段着色器）
//! - MSAA x4：几何光栅化 + 硬件多重采样
//! - SDF 1px：纯 SDF feather 分析性抗锯齿
//! - No AA：几何光栅化，无抗锯齿
//!
//! 注意：这个示例按四种方式渲染了相同的画面到四个窗口！

use vireo::prelude::*;

fn draw_shapes(batch: &mut DrawBatch, sdf: f32, label: &str) {
    if sdf > 0.0 {
        batch.sdf_feather = Some(sdf);
    }
    draw_rounded_rect(batch, 160.0, 10.0, 140.0, 80.0, 20.0, Color::new(0.15, 0.5, 0.3, 1.0));
    draw_triangle(batch, 80.0, 180.0, 30.0, 130.0, 150.0, 130.0, YELLOW);
    draw_triangle(batch, 170.0, 100.0, 210.0, 160.0, 130.0, 180.0, ORANGE);
    draw_circle(batch, 260.0, 130.0, 50.0, Color::new(0.2, 0.6, 1.0, 1.0));
    draw_line(batch, 10.0, 110.0, 310.0, 110.0, 3.0, Color::new(0.4, 0.7, 0.4, 1.0));
    draw_line_chain(batch, &[(10.0, 30.0), (80.0, 20.0), (150.0, 50.0)], 3.0, WHITE);
    draw_text(&mut batch.texts, label,
        TextOptions::default().x(10.0).y(215.0).font_size(14.0).color(Color::new(0.5, 0.5, 0.6, 1.0)));
}

fn main() {
    let mut app = App::new();

    let ssaa = app.window(
        WindowDesc::new("SSAA x4", 320, 240)
            .anti_aliasing(AntiAliasing::Ssaa { samples: 4, alpha_to_coverage: true }),
        None::<fn()>,
    );
    let msaa = app.window(
        WindowDesc::new("MSAA x4", 320, 240)
            .anti_aliasing(AntiAliasing::Msaa { samples: 4, alpha_to_coverage: true }),
        None::<fn()>,
    );
    let sdf = app.window(
        WindowDesc::new("SDF 1px", 320, 240)
            .anti_aliasing(AntiAliasing::None),
        None::<fn()>,
    );
    let raw = app.window(WindowDesc::new("No AA", 320, 240), None::<fn()>);

    app.run(move |app| {
        if let Some(w) = app.window_ref(&ssaa) { let mut b = DrawBatch::new(); draw_shapes(&mut b, 0.0, "SSAA x4"); w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(w) = app.window_ref(&msaa) { let mut b = DrawBatch::new(); draw_shapes(&mut b, 0.0, "MSAA x4"); w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(w) = app.window_ref(&sdf)  { let mut b = DrawBatch::new(); draw_shapes(&mut b, 1.0, "SDF 1px");      w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        if let Some(w) = app.window_ref(&raw)  { let mut b = DrawBatch::new(); draw_shapes(&mut b, 0.0, "No AA");             w.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&b]); }
        true
    });
}
