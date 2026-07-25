use std::cell::RefCell;
use vireo::prelude::*;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MixParams {
    time: f32,
    mix: f32,
    _pad0: f32,
    _pad1: f32,
}

const WGSL: &str = r#"
struct Mix {
    time: f32,
    mix: f32,
};

@group(3) @binding(0) var<storage> u_mix: Mix;
@group(3) @binding(1) var tex0: texture_2d<f32>;
@group(3) @binding(2) var samp0: sampler;
@group(3) @binding(3) var tex1: texture_2d<f32>;
@group(3) @binding(4) var samp1: sampler;

fn material_main(in: MaterialInput) -> vec4<f32> {
    let uv = in.uv;
    let a = textureSample(tex0, samp0, uv);
    let b = textureSample(tex1, samp1, uv);
    let w = 0.5 + 0.5 * sin(u_mix.time * 2.0 + uv.x * 6.2831);
    let t = clamp(u_mix.mix + w * 0.3, 0.0, 1.0);
    return mix(a, b, t);
}
"#;

fn solid_rgba(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        v.extend_from_slice(&[r, g, b, 255]);
    }
    v
}

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
    let idx = app.window(
        WindowDesc::new("Custom Material Multi-Texture", 560, 360),
        None::<fn()>,
    );

    let mat: RefCell<Option<std::sync::Arc<Material>>> = RefCell::new(None);
    let tex_keep: RefCell<Option<(Texture, Texture)>> = RefCell::new(None);
    let start = std::time::Instant::now();

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mat = if mat.borrow().is_none() {
            let gpu = win.gpu();
            let m = gpu
                .create_material(WGSL)
                .expect("WGSL compile");

            let tex0 = Texture::from_rgba(64, 64, &solid_rgba(64, 64, 40, 120, 255), gpu);
            let tex1 = Texture::from_rgba(
                64,
                64,
                &checker_rgba(64, 64, [255, 80, 40], [40, 200, 120], 8),
                gpu,
            );
            m.set_texture_slots(
                &gpu.device,
                &[Some(&tex0.view), Some(&tex1.view), None, None],
            );
            *tex_keep.borrow_mut() = Some((tex0, tex1));
            *mat.borrow_mut() = Some(m.clone());
            m
        } else {
            mat.borrow().as_ref().unwrap().clone()
        };

        let t = start.elapsed().as_secs_f32();
        mat.set_uniform(
            &win.gpu().queue,
            &MixParams {
                time: t,
                mix: 0.5,
                _pad0: 0.0,
                _pad1: 0.0,
            },
        );

        let mut b = DrawBatch::new();
        b.custom_material = Some(mat);
        b.set_position(280.0, 180.0);
        b.set_rad(t * 0.25);
        draw_rectangle(&mut b, Pos::new(-120.0, -120.0), 240.0, 240.0, Some(WHITE));

        let mut title = DrawBatch::new();
        draw_text(
            &mut title.texts,
            "tex0 + tex1 mix (group3 bindings 1-4)",
            Pos::new(16.0, 16.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(
            Some(Color::new(0.06, 0.08, 0.12, 1.0)),
            &[&b, &title],
        );
        true
    });
}
