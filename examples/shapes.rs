//! 形状展示示例：填充 + 描边（Fill + Outline）
//!
//! 上半区域：填充形状
//! 下半区域：描边形状

use vireo::prelude::*;
use std::f32::consts::PI;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Vireo Shapes Demo", 1000, 450).anti_aliasing(AntiAliasing::Msaa { samples: 4, alpha_to_coverage: true }),
        None::<fn()>,
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mut batch = DrawBatch::new();

        // ====== 填充形状 ======
        draw_rectangle(&mut batch, 20.0, 20.0, 100.0, 80.0, RED);
        draw_circle(&mut batch, 200.0, 60.0, 40.0, GREEN);
        draw_ellipse(&mut batch, 340.0, 60.0, 60.0, 40.0, Color::new(0.2, 0.6, 1.0, 1.0));
        draw_rounded_rect(&mut batch, 460.0, 20.0, 100.0, 80.0, 20.0, ORANGE, 16);
        draw_triangle(&mut batch, 620.0, 100.0, 580.0, 20.0, 660.0, 20.0, YELLOW);
        draw_polygon(&mut batch, &[(720.0, 20.0), (780.0, 20.0), (810.0, 70.0), (780.0, 100.0), (720.0, 80.0)], PURPLE);
        draw_arc(&mut batch, 930.0, 60.0, 50.0, 0.0, PI * 0.75, PINK, 24);

        let fill_labels = ["Rect", "Circle", "Ellipse", "RndRect", "Triangle", "Polygon", "Arc"];
        let fill_xs =   [20.0, 190.0, 330.0, 470.0, 610.0, 735.0, 920.0];
        for i in 0..fill_labels.len() {
            draw_text(&mut batch.texts, fill_labels[i],
                TextOptions::default().x(fill_xs[i]).y(115.0).font_size(12.0).color(Color::new(0.6, 0.6, 0.7, 1.0)));
        }

        // ====== 描边形状 ======
        let y = 170.0;
        draw_rect_outline(&mut batch, 20.0, y, 100.0, 80.0, 4.0, RED);
        draw_circle_outline(&mut batch, 200.0, y + 40.0, 40.0, 4.0, GREEN, 48);
        draw_ellipse_outline(&mut batch, 340.0, y + 40.0, 60.0, 40.0, 4.0, Color::new(0.2, 0.6, 1.0, 1.0), 48);
        draw_rounded_rect_outline(&mut batch, 460.0, y, 100.0, 80.0, 20.0, 4.0, ORANGE, 16);
        draw_triangle_outline(&mut batch, 620.0, y + 80.0, 580.0, y, 660.0, y, 4.0, YELLOW);
        draw_polygon_outline(&mut batch, &[(720.0, y), (780.0, y), (810.0, y + 50.0), (780.0, y + 80.0), (720.0, y + 60.0)], 4.0, PURPLE);
        draw_arc_outline(&mut batch, 930.0, y + 40.0, 50.0, 0.0, PI * 0.75, 4.0, PINK, 24);

        let outline_labels = ["Rect", "Circle", "Ellipse", "RndRect", "Triangle", "Polygon", "Arc"];
        for i in 0..outline_labels.len() {
            draw_text(&mut batch.texts, outline_labels[i],
                TextOptions::default().x(fill_xs[i]).y(y + 95.0).font_size(12.0).color(Color::new(0.6, 0.6, 0.7, 1.0)));
        }

        win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        true
    });
}
