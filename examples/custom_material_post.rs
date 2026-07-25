//! 同一 Material API 的全屏后处理：OffscreenCanvas -> window。

use std::cell::RefCell;
use vireo::prelude::*;

const WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    if in.target_type != 2u {
        return in.color;
    }
    let center = in.uv - vec2<f32>(0.5);
    let vignette = clamp(1.15 - dot(center, center) * 1.8, 0.35, 1.0);
    let split = vec3<f32>(in.color.r * 1.08, in.color.g, in.color.b * 1.12);
    return vec4<f32>(split * vignette, 1.0);
}
"#;

fn main() {
    let mut app = App::new();
    let window = app.window(
        WindowDesc::new("Unified Material: Post", 640, 400),
        None::<fn()>,
    );
    let scene = app.offscreen(640, 400, AntiAliasing::None);
    let material: RefCell<Option<std::sync::Arc<Material>>> = RefCell::new(None);

    app.run(move |app| {
        let win = app.window_ref(&window).unwrap();
        let scene_canvas = app.offscreen_ref(&scene).unwrap();
        let mat = if let Some(mat) = material.borrow().as_ref() {
            mat.clone()
        } else {
            let mat = win.gpu().create_material(WGSL).expect("material WGSL");
            mat.set_texture(
                &win.gpu().device,
                scene_canvas.view(),
                &win.gpu().default_sampler,
            );
            *material.borrow_mut() = Some(mat.clone());
            mat
        };

        let mut batch = DrawBatch::new();
        draw_circle(&mut batch, Pos::new(210.0, 205.0), 115.0, Some(Color::new(0.95, 0.2, 0.35, 1.0)));
        draw_rounded_rect(
            &mut batch,
            Pos::new(325.0, 105.0),
            220.0,
            200.0,
            30.0,
            Some(Color::new(0.15, 0.55, 1.0, 0.9)),
        );
        draw_text(
            &mut batch.texts,
            "OFFSCREEN -> MATERIAL -> WINDOW",
            Pos::new(128.0, 342.0),
            TextDef::default().font_size(18.0),
            TextOverride::from_color(WHITE),
        );
        scene_canvas.draw(Some(Color::new(0.055, 0.07, 0.11, 1.0)), &[&batch]);
        win.draw_post(&mat);
        true
    });
}
