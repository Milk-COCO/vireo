//! 纹理加载示例：App::load_texture() 在 run 前加载，run 内直接使用

use vireo::prelude::*;

fn main() {
    let mut app = App::new();

    let tex = Some(app.load_texture("logo.png"));

    let idx = app.window(WindowDesc::new("Vireo Texture Demo", 800, 600).high_dpi(true), None::<fn()>);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mut batch = DrawBatch::new();

        match tex {
            Some(i) => {
                let t = app.texture(i).unwrap();
                let s = 0.2; // logo 是 1000x1000，缩小到可看
                batch.set_texture(Some(t));
                draw_rectangle(&mut batch, Pos::new(20.0, 80.0), t.width as f32 * s, t.height as f32 * s, Some(WHITE));
                draw_rectangle(&mut batch, Pos::new(250.0, 200.0), t.width as f32 * s * 2.0, t.height as f32 * s * 2.0, Some(WHITE));

                draw_text(
                    &mut batch.texts,
                    "From file (logo.png)",
                    Pos::new(20.0, 60.0), TextDef::default().font_size(14.0), TextOverride::from_color(Color::new(0.6, 0.6, 0.7, 1.0)),
                );
                draw_text(
                    &mut batch.texts,
                    "2x scaled",
                    Pos::new(250.0, 480.0), TextDef::default().font_size(12.0), TextOverride::from_color(Color::new(0.5, 0.5, 0.6, 1.0)),
                );
            }
            None => {
                draw_text(
                    &mut batch.texts,
                    "Texture not found.\nPlace a logo.png in examples/",
                    Pos::new(200.0, 250.0), TextDef::default().font_size(20.0), TextOverride::from_color(Color::new(0.8, 0.8, 0.8, 1.0)),
                );
            }
        }

        win.draw(Color::new(0.08, 0.08, 0.12, 1.0), &[&batch]);
        true
    });
}
