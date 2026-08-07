//! 贴图形状对比：几何路径（上）vs SDF 路径（下）
//!
//! 上半无 SDF feather，下半有 SDF feather=1.0。同一张贴图，两种渲染路径。

use vireo::prelude::*;
use std::f32::consts::PI;

fn draw_shapes(batch: &mut DrawBatch) {
    // 第一行：填充形状
    draw_rectangle(batch, Pos::new(10.0, 10.0), 90.0, 65.0, Some(WHITE));
    draw_rounded_rect(batch, Pos::new(120.0, 10.0), 90.0, 65.0, 14.0, Some(WHITE));
    draw_triangle(batch, 240.0, 75.0, 280.0, 10.0, 320.0, 75.0, Some(WHITE));
    draw_circle(batch, Pos::new(390.0, 42.0), 32.0, Some(WHITE));
    draw_ellipse(batch, Pos::new(480.0, 42.0), 45.0, 28.0, Some(WHITE));
    draw_arc(batch, Pos::new(580.0, 42.0), 32.0, 0.0, PI * 1.3, Some(WHITE));
    draw_polygon(batch, &[(680.0, 10.0), (730.0, 10.0), (730.0, 75.0), (680.0, 75.0)], Some(WHITE));

    // 第二行：折线 + 描边
    draw_line_chain(batch, &[(10.0, 120.0), (80.0, 100.0), (150.0, 110.0)], 5.0, Some(WHITE));
    draw_line(batch, 190.0, 110.0, 310.0, 110.0, 5.0, Some(WHITE));
    draw_rect_outline(batch, Pos::new(350.0, 90.0), 60.0, 45.0, 3.0, Some(WHITE));
    draw_rounded_rect_outline(batch, Pos::new(440.0, 90.0), 60.0, 45.0, 10.0, 3.0, Some(WHITE), 8);
    draw_circle_outline(batch, Pos::new(560.0, 112.0), 22.0, 3.0, Some(WHITE), 32);
    draw_arc_outline(batch, Pos::new(630.0, 112.0), 25.0, 0.0, PI * 1.2, 3.0, Some(WHITE), 24);
    draw_triangle_outline(batch, 700.0, 135.0, 720.0, 90.0, 740.0, 135.0, 2.5, Some(WHITE));
}

fn main() {
    let mut app = App::new();

    let tex = Some(app.load_texture("logo_quad.png"));

    let idx = app.window(
        WindowDesc::new("Texture: Geometry vs SDF", 760, 380),
        None::<fn()>,
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        match tex {
            Some(i) => {
                let t = app.texture(i).unwrap();
                let label = Color::new(0.5, 0.5, 0.7, 1.0);

                // ---- 上半：几何模式 ----
                let mut geo = DrawBatch::new();
                geo.set_texture(Some(t));
                draw_shapes(&mut geo);
                draw_text(&mut geo.texts, "Geometry (sdf_feather = None)",
                          Pos::new(10.0, 155.0), TextDef::default().font_size(14.0), TextOverride::from_color(label));

                // ---- 下半：SDF 模式 ----
                let mut sdf = DrawBatch::new();
                sdf.set_position(0.0, 175.0);
                sdf.set_texture(Some(t));
                sdf.set_sdf_feather(Some(1.0));
                draw_shapes(&mut sdf);
                draw_text(&mut sdf.texts, "SDF (sdf_feather = 1.0)",
                          Pos::new(10.0, 155.0), TextDef::default().font_size(14.0), TextOverride::from_color(label));

                win.draw(Color::new(0.08, 0.08, 0.12, 1.0), &[&geo, &sdf]);
            }
            None => {
                let mut batch = DrawBatch::new();
                draw_text(&mut batch.texts, "Place logo_quad.png in the project root.",
                          Pos::new(150.0, 160.0), TextDef::default().font_size(18.0), TextOverride::from_color(Color::new(0.8, 0.8, 0.8, 1.0)));
                win.draw(Color::new(0.08, 0.08, 0.12, 1.0), &[&batch]);
            }
        }

        true
    });
}
