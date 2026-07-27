//! Custom Material + Dynamic Storage Offset (A-path)
//!
//! 4 个矩形用同一个 buffer 上的 4 套参数，逐 draw 切 offset。
//! 用 Storage（非 Uniform）因为 uniform dynamic offset 对齐 256B 太浪费。

use vireo::prelude::*;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RectParams {
    r: f32,
    g: f32,
    b: f32,
    _pad0: f32,
    _pad: [u8; 240],
}

const WGSL: &str = r#"
struct RectParams {
    r: f32,
    g: f32,
    b: f32,
};

fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(u_params.r, u_params.g, u_params.b, 1.0);
}
"#;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Custom Material Dynamic Offset", 600, 400),
        None::<fn()>,
    );

    let stride = std::mem::size_of::<RectParams>() as u64;
    let mat = app
        .material_with_resources(
            WGSL,
            MaterialResources(&[MaterialResource {
                name: "u_params",
                kind: MaterialResourceKind::Storage {
                    read_only: true,
                    size: stride,
                    type_name: "RectParams",
                    dynamic: true,
                },
            }]),
        )
        .expect("WGSL compile");

    let params = [
        RectParams { r: 0.8, g: 0.2, b: 0.2, _pad0: 0.0, _pad: [0u8; 240] },
        RectParams { r: 0.2, g: 0.8, b: 0.2, _pad0: 0.0, _pad: [0u8; 240] },
        RectParams { r: 0.2, g: 0.2, b: 0.8, _pad0: 0.0, _pad: [0u8; 240] },
        RectParams { r: 0.8, g: 0.8, b: 0.2, _pad0: 0.0, _pad: [0u8; 240] },
    ];
    mat.set_uniform_bytes(
        &app.gpu.queue,
        "u_params",
        bytemuck::cast_slice(&params),
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        let stride = std::mem::size_of::<RectParams>() as u32;
        let mut batches: Vec<DrawBatch> = Vec::new();
        for i in 0..4u32 {
            let mut b = DrawBatch::new();
            b.custom_material = Some(mat.clone());
            b.set_position(50.0 + i as f32 * 130.0, 150.0);
            b.dynamic_offsets = vec![i * stride];
            draw_rectangle(&mut b, Pos::new(-50.0, -50.0), 100.0, 100.0, Some(WHITE));
            batches.push(b);
        }

        let mut title = DrawBatch::new();
        draw_text(
            &mut title.texts,
            "Dynamic Storage Offset: 1 buffer, per-draw stride",
            Pos::new(16.0, 16.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(WHITE),
        );

        let refs: Vec<&DrawBatch> = batches.iter().chain(std::iter::once(&title)).collect();
        win.draw(Some(Color::new(0.05, 0.07, 0.11, 1.0)), &refs);
        true
    });
}
