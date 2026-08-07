//! 纹理子区域示例：set_uv() + draw_rectangle
//!
//! 从一张大纹理中切取子区域展示（sprite sheet 用）

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let tex_id = app.load_texture("logo_bg.png");
    let idx = app.window(WindowDesc::new("Texture Sub-Region", 450, 220), None::<fn()>);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let tex = app.texture(tex_id).unwrap();

        let mut batch = DrawBatch::new();
        batch.set_texture(Some(tex));
        let s = 0.1; // logo 1000x1000，缩小到 100x100

        // 画整张纹理
        draw_rectangle(&mut batch, Pos::new(20.0, 80.0), tex.width as f32 * s, tex.height as f32 * s, Some(WHITE));

        // 左上角 1/2 区域
        let w2 = tex.width / 2;
        let h2 = tex.height / 2;
        let (u0, v0, u1, v1) = tex.uv(0, 0, w2, h2);
        batch.set_uv(u0, v0, u1, v1);
        draw_rectangle(&mut batch, Pos::new(180.0, 80.0), w2 as f32 * s, h2 as f32 * s, Some(WHITE));

        // 右下角 1/2 区域
        let (u0, v0, u1, v1) = tex.uv(w2, h2, w2, h2);
        batch.set_uv(u0, v0, u1, v1);
        draw_rectangle(&mut batch, Pos::new(280.0, 80.0), w2 as f32 * s, h2 as f32 * s, Some(WHITE));

        // 标签
        draw_text(&mut batch.texts, "Full", Pos::new(20.0, 60.0),
                  TextDef::default().font_size(14.0), TextOverride::from_color(WHITE));
        draw_text(&mut batch.texts, "Top-left", Pos::new(180.0, 60.0),
                  TextDef::default().font_size(14.0), TextOverride::from_color(WHITE));
        draw_text(&mut batch.texts, "Bottom-right", Pos::new(280.0, 60.0),
                  TextDef::default().font_size(14.0), TextOverride::from_color(WHITE));

        win.draw(Color::new(0.05, 0.05, 0.08, 1.0), &[&batch]);

        true
    });
}
