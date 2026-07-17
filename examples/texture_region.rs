//! 纹理子区域示例：Texture::uv() + add_quad_uv()
//!
//! 从一张大纹理中切取子区域展示（sprite sheet 用）

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let tex_id = app.load_texture("logo.png").unwrap();
    let idx = app.window(WindowDesc::new("Texture Sub-Region", 800, 400), None::<fn()>);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let tex = app.texture(tex_id).unwrap();

        let mut batch = DrawBatch::new();
        batch.set_texture(tex);

        // 画整张纹理（缩小 20%）
        batch.add_quad_uv(20.0, 20.0, tex.width as f32 * 0.5, tex.height as f32 * 0.5,
            0.0, 0.0, 1.0, 1.0, WHITE);

        // 左上角 1/2 区域
        let w2 = tex.width / 2;
        let h2 = tex.height / 2;
        let (u0, v0, u1, v1) = tex.uv(0, 0, w2, h2);
        batch.add_quad_uv(320.0, 20.0, w2 as f32, h2 as f32, u0, v0, u1, v1, WHITE);

        // 右下角 1/2 区域
        let (u0, v0, u1, v1) = tex.uv(w2, h2, w2, h2);
        batch.add_quad_uv(320.0, 220.0, w2 as f32, h2 as f32, u0, v0, u1, v1, WHITE);

        // 标签
        draw_text(&mut batch.texts, "Full texture (50%)",
            TextOptions::default().x(20.0).y(tex.height as f32 * 0.5 + 30.0).font_size(12.0).color(WHITE));
        draw_text(&mut batch.texts, "Top-left half",
            TextOptions::default().x(320.0).y(20.0 + h2 as f32 + 5.0).font_size(12.0).color(WHITE));
        draw_text(&mut batch.texts, "Bottom-right half",
            TextOptions::default().x(320.0).y(220.0 + h2 as f32 + 5.0).font_size(12.0).color(WHITE));

        win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);

        true
    });
}
