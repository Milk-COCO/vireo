//! 同一份 Material 自动作用于 batch 中的 shape 与 text。

use std::cell::RefCell;
use vireo::prelude::*;

const WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    if in.target_type == 1u {
        return vec4<f32>(0.25, 0.95, 0.85, 1.0);
    }
    let wave = 0.5 + 0.5 * sin(in.local_pos.x * 0.08);
    return vec4<f32>(0.25 + wave * 0.65, 0.35, 0.95 - wave * 0.45, 1.0);
}
"#;

fn main() {
    let mut app = App::new();
    let window = app.window(
        WindowDesc::new("One Material: Shape + Text", 640, 400),
        None::<fn()>,
    );
    let material: RefCell<Option<std::sync::Arc<Material>>> = RefCell::new(None);

    app.run(move |app| {
        let win = app.window_ref(&window).unwrap();
        let mat = if let Some(mat) = material.borrow().as_ref() {
            mat.clone()
        } else {
            let mat = win.gpu().create_material(WGSL).expect("material WGSL");
            *material.borrow_mut() = Some(mat.clone());
            mat
        };

        let mut batch = DrawBatch::new();
        batch.custom_material = Some(mat);
        batch.sdf_feather = Some(1.5);
        draw_rounded_rect(
            &mut batch,
            Pos::new(100.0, 110.0),
            440.0,
            180.0,
            36.0,
            Some(WHITE),
        );
        draw_text(
            &mut batch.texts,
            "ONE SHADER / SHAPE + TEXT",
            Pos::new(150.0, 182.0),
            TextDef::default().font_size(24.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(Some(Color::new(0.035, 0.045, 0.075, 1.0)), &[&batch]);
        true
    });
}
