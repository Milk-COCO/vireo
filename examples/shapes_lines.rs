//! 线段与折线：`draw_line` / `draw_line_chain`，SDF vs 几何
//!
//! 键 S：切换 sdf_feather（SDF 柔边 / 纯几何）

use std::f32::consts::TAU;
use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Shapes — Lines", 900, 480), None::<fn()>);

    let mut t: f32 = 0.0;
    let mut use_sdf = true;
    let mut s_was = false;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.016;

        let s_down = win.key_down(KeyCode::KeyS);
        if s_down && !s_was {
            use_sdf = !use_sdf;
        }
        s_was = s_down;

        let mut batch = DrawBatch::new();
        batch.sdf_feather = if use_sdf { Some(1.0) } else { None };

        draw_text(
            &mut batch.texts,
            &format!(
                "draw_line / draw_line_chain  |  mode: {}  (S toggle)",
                if use_sdf { "SDF feather=1" } else { "geometry" }
            ),
            TextOptions::default()
                .x(20.0)
                .y(16.0)
                .font_size(16.0)
                .color(Color::new(0.75, 0.78, 0.88, 1.0)),
        );

        // 粗细网格
        let thicknesses = [1.0, 2.0, 4.0, 8.0, 16.0];
        for (i, &th) in thicknesses.iter().enumerate() {
            let y = 70.0 + i as f32 * 36.0;
            draw_line(&mut batch, 40.0, y, 280.0, y, th, Color::new(0.3, 0.7, 1.0, 1.0));
            draw_text(
                &mut batch.texts,
                &format!("{th:.0}px"),
                TextOptions::default()
                    .x(290.0)
                    .y(y - 8.0)
                    .font_size(13.0)
                    .color(Color::new(0.55, 0.55, 0.65, 1.0)),
            );
        }
        draw_text(
            &mut batch.texts,
            "draw_line",
            TextOptions::default()
                .x(40.0)
                .y(50.0)
                .font_size(13.0)
                .color(GOLD),
        );

        // 斜线
        draw_line(&mut batch, 40.0, 280.0, 280.0, 400.0, 6.0, ORANGE);
        draw_line(&mut batch, 40.0, 400.0, 280.0, 280.0, 3.0, PINK);

        // 折线：折线路径
        let mut pts = Vec::new();
        for i in 0..12 {
            let x = 380.0 + i as f32 * 40.0;
            let y = 120.0 + (t * 2.0 + i as f32 * 0.6).sin() * 40.0;
            pts.push((x, y));
        }
        draw_line_chain(&mut batch, &pts, 4.0, GREEN);
        draw_text(
            &mut batch.texts,
            "draw_line_chain (animated)",
            TextOptions::default()
                .x(380.0)
                .y(50.0)
                .font_size(13.0)
                .color(GOLD),
        );

        // 闭合多边形式折线（不自动闭口，手动接回首点）
        let star = [
            (520.0, 280.0),
            (560.0, 360.0),
            (480.0, 320.0),
            (560.0, 320.0),
            (480.0, 360.0),
            (520.0, 280.0),
        ];
        draw_line_chain(&mut batch, &star, 3.0, PURPLE);
        draw_text(
            &mut batch.texts,
            "polyline (closed by hand)",
            TextOptions::default()
                .x(450.0)
                .y(380.0)
                .font_size(12.0)
                .color(Color::new(0.55, 0.55, 0.65, 1.0)),
        );

        // 旋转射线
        let cx = 760.0;
        let cy = 320.0;
        for i in 0..8 {
            let a = t + i as f32 * TAU / 8.0;
            draw_line(
                &mut batch,
                cx,
                cy,
                cx + a.cos() * 90.0,
                cy + a.sin() * 90.0,
                2.0 + (i as f32),
                Color::new(1.0, 0.5 + i as f32 * 0.05, 0.3, 1.0),
            );
        }
        draw_circle(&mut batch, cx, cy, 6.0, WHITE);

        win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        true
    });
}
