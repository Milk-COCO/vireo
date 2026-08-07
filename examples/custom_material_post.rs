//! “后处理”示例：先画到 OffscreenCanvas，再把它当普通纹理贴回窗口，
//! 并在这次贴图绘制上挂 Material。

use vireo::prelude::*;

const WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    // from DrawBatch::set_texture
    let src = vireo_base_sample(in.base_uv);
    let center = in.base_uv - vec2<f32>(0.5);
    let vignette = clamp(1.15 - dot(center, center) * 1.8, 0.35, 1.0);
    let split = vec3<f32>(src.r * 1.08, src.g, src.b * 1.12);
    return vec4<f32>(split * vignette, src.a);
}
"#;

fn main() {
    let mut app = App::new();
    let window = app.window(
        WindowDesc::new("Material on Offscreen Texture", 640, 400),
        None::<fn()>,
    );
    let scene = app.offscreen(640, 400, AntiAliasing::None);
    let material = app.material(WGSL).expect("material WGSL");

    app.run(move |app| {
        let win = app.window_ref(&window).unwrap();
        let scene_canvas = app.offscreen_ref(&scene).unwrap();

        let mut scene_batch = DrawBatch::new();
        draw_circle(
            &mut scene_batch,
            Pos::new(210.0, 205.0),
            115.0,
            Some(Color::new(0.95, 0.2, 0.35, 1.0)),
        );
        draw_rounded_rect(
            &mut scene_batch,
            Pos::new(325.0, 105.0),
            220.0,
            200.0,
            30.0,
            Some(Color::new(0.15, 0.55, 1.0, 0.9)),
        );
        draw_text(
            &mut scene_batch.texts,
            "CANVAS -> TEXTURE -> MATERIAL",
            Pos::new(128.0, 342.0),
            TextDef::default().font_size(18.0),
            TextOverride::from_color(WHITE),
        );
        scene_canvas.draw(Some(Color::new(0.055, 0.07, 0.11, 1.0)), &[&scene_batch]);

        let mut present = DrawBatch::new();
        present.set_texture(Some(&scene_canvas.texture));
        present.custom_material = Some(material.clone());
        draw_rectangle(&mut present, Pos::new(0.0, 0.0), 640.0, 400.0, Some(WHITE));

        win.draw(BLACK, &[&present]);
        true
    });
}
