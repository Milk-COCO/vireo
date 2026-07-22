//! 颜色工具：`color_u8!`、`from_hex` / `to_hex`、`lerp`、`with_alpha`

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Color Palette", 720, 400), None::<fn()>);

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.02;
        let mut batch = DrawBatch::new();

        draw_text(
            &mut batch.texts,
            "color_u8! / from_hex / lerp / with_alpha",
            Pos::new(20.0, 16.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(0.75, 0.78, 0.88, 1.0)),
        );

        // color_u8! 色板
        let swatches = [
            color_u8!(255, 80, 80, 255),
            color_u8!(80, 200, 120, 255),
            color_u8!(80, 140, 255, 255),
            color_u8!(255, 200, 60, 255),
            color_u8!(200, 100, 255, 255),
        ];
        for (i, c) in swatches.iter().enumerate() {
            let x = 30.0 + i as f32 * 70.0;
            draw_rounded_rect(&mut batch, Pos::new(x, 60.0), 56.0, 56.0, 8.0, Some(*c));
            draw_text(
                &mut batch.texts,
                &c.to_hex(),
                Pos::new(x, 124.0),
                TextDef::default().font_size(11.0),
                TextOverride::from_color(Color::new(0.55, 0.55, 0.65, 1.0)),
            );
        }
        draw_text(
            &mut batch.texts,
            "color_u8! + to_hex",
            Pos::new(30.0, 48.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(GOLD),
        );

        // from_hex
        let hexes = ["#FF6B6B", "#4ECDC4", "#45B7D1", "#F7DC6F", "#BB8FCE"];
        for (i, h) in hexes.iter().enumerate() {
            let c = Color::from_hex(h).unwrap_or(WHITE);
            let x = 30.0 + i as f32 * 70.0;
            draw_rounded_rect(&mut batch, Pos::new(x, 170.0), 56.0, 56.0, 8.0, Some(c));
            draw_text(
                &mut batch.texts,
                *h,
                Pos::new(x, 234.0),
                TextDef::default().font_size(11.0),
                TextOverride::from_color(Color::new(0.55, 0.55, 0.65, 1.0)),
            );
        }
        draw_text(
            &mut batch.texts,
            "from_hex",
            Pos::new(30.0, 158.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(GOLD),
        );

        // lerp 渐变条
        let a = Color::from_hex("#FF0080").unwrap_or(RED);
        let b = Color::from_hex("#00E5FF").unwrap_or(BLUE);
        let steps = 24;
        for i in 0..steps {
            let u = i as f32 / (steps - 1) as f32;
            let c = a.lerp(&b, u);
            draw_rectangle(&mut batch, Pos::new(30.0 + i as f32 * 18.0, 280.0), 16.0, 40.0, Some(c));
        }
        // 动画 alpha
        let pulse = a.with_alpha(0.3 + 0.7 * (0.5 + 0.5 * t.sin()));
        draw_circle(&mut batch, Pos::new(560.0, 300.0), 36.0, Some(pulse));
        draw_text(
            &mut batch.texts,
            "lerp  |  with_alpha pulse",
            Pos::new(30.0, 268.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(GOLD),
        );

        win.draw(Some(Color::new(0.06, 0.07, 0.1, 1.0)), &[&batch]);
        true
    });
}
