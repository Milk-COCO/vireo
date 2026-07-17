//! 形状展示示例：填充 + 描边（Fill + Outline）
//!
//! 上半区域：填充形状
//! 下半区域：描边形状

use vireo::prelude::*;
use std::f32::consts::PI;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Vireo Shapes Demo", 900, 600), None::<fn()>);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mut batch = DrawBatch::new();

        // ====== 填充形状（上半区域）======

        draw_rectangle(&mut batch, 20.0, 20.0, 80.0, 60.0, RED);
        draw_circle(&mut batch, 200.0, 50.0, 30.0, GREEN, 48);
        draw_ellipse(&mut batch, 320.0, 50.0, 50.0, 30.0, Color::new(0.2, 0.6, 1.0, 1.0), 48);
        draw_rounded_rect(&mut batch, 420.0, 20.0, 80.0, 60.0, 15.0, ORANGE, 16);
        draw_triangle(&mut batch, 550.0, 80.0, 520.0, 20.0, 580.0, 20.0, YELLOW);
        draw_polygon(&mut batch, &[(650.0, 20.0), (700.0, 20.0), (720.0, 60.0), (700.0, 80.0), (650.0, 60.0)], PURPLE);
        draw_arc(&mut batch, 820.0, 50.0, 40.0, 0.0, PI * 0.75, PINK, 24);
        draw_line(&mut batch, 20.0, 110.0, 120.0, 110.0, 2.0, WHITE);

        let fill_labels = ["Rect", "Circle", "Ellipse", "RoundedRect", "Triangle", "Polygon", "Arc", "Line"];
        let fill_xs =   [20.0, 185.0, 310.0, 430.0, 545.0, 660.0, 805.0, 130.0];
        for i in 0..fill_labels.len() {
            draw_text(&mut batch.texts, fill_labels[i],
                TextOptions::default().x(fill_xs[i]).y(92.0).font_size(10.0).color(Color::new(0.6, 0.6, 0.7, 1.0)));
        }

        // ====== 描边形状（下半区域）======

        draw_rect_outline(&mut batch, 20.0, 150.0, 80.0, 60.0, 3.0, RED);
        draw_circle_outline(&mut batch, 200.0, 180.0, 30.0, 3.0, GREEN, 48);
        draw_ellipse_outline(&mut batch, 320.0, 180.0, 50.0, 30.0, 3.0, Color::new(0.2, 0.6, 1.0, 1.0), 48);
        draw_rounded_rect_outline(&mut batch, 420.0, 150.0, 80.0, 60.0, 15.0, 3.0, ORANGE, 16);
        draw_triangle_outline(&mut batch, 550.0, 210.0, 520.0, 150.0, 580.0, 150.0, 2.0, YELLOW);
        draw_polygon_outline(&mut batch, &[(650.0, 150.0), (700.0, 150.0), (720.0, 190.0), (700.0, 210.0), (650.0, 190.0)], 2.0, PURPLE);
        draw_arc_outline(&mut batch, 820.0, 180.0, 40.0, 0.0, PI * 0.75, 3.0, PINK, 24);
        draw_line_chain(&mut batch, &[(20.0, 240.0), (50.0, 220.0), (80.0, 250.0), (120.0, 230.0)], 2.0, WHITE);

        let outline_labels = ["Rect", "Circle", "Ellipse", "RoundedRect", "Triangle", "Polygon", "Arc", "LineChain"];
        for i in 0..outline_labels.len() {
            draw_text(&mut batch.texts, outline_labels[i],
                TextOptions::default().x(fill_xs[i]).y(225.0).font_size(10.0).color(Color::new(0.6, 0.6, 0.7, 1.0)));
        }

        win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        true
    });
}
