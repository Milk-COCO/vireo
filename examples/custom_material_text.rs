//! 同一份 Material 自动作用于 batch 中的 shape 与 text，且 text 也能读取 batch 贴图。

use vireo::prelude::*;

const WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let base = vireo_base_sample(in.base_uv);
    if in.target_type == VIREO_TARGET_TEXT {
        return vec4<f32>(base.rgb, in.color.a);
    }
    // shape: local_pos is valid (vireo_has_local_pos() == true)
    let wave = 0.5 + 0.5 * sin(in.local_pos.x * 0.08);
    let tint = vec3<f32>(0.25 + wave * 0.65, 0.35, 0.95 - wave * 0.45);
    return vec4<f32>(base.rgb * tint, in.color.a);
}
"#;

fn checker_rgba(w: u32, h: u32, c0: [u8; 3], c1: [u8; 3], cell: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let on = ((x / cell) + (y / cell)) % 2 == 0;
            let c = if on { c0 } else { c1 };
            v.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    v
}

fn main() {
    let mut app = App::new();
    let window = app.window(
        WindowDesc::new("One Material: Shape + Text", 640, 400),
        None::<fn()>,
    );
    let material = app.material(WGSL).expect("material WGSL");
    let texture = Texture::from_rgba(
        96,
        96,
        &checker_rgba(96, 96, [30, 180, 255], [255, 120, 40], 12),
        &app.gpu,
    );

    app.run(move |app| {
        let win = app.window_ref(&window).unwrap();

        let mut batch = DrawBatch::new();
        batch.custom_material = Some(material.clone());
        batch.set_texture(Some(&texture));
        batch.set_sdf_feather(Some(1.5));
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
