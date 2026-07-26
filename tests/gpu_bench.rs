//! 无头 GPU 基准：OffscreenCanvas 压测（需本机 GPU）。
//!
//! 运行：
//! ```bash
//! cargo test --release --test gpu_bench -- --nocapture --ignored
//! ```

use std::sync::Arc;
use std::time::Instant;

use vireo::prelude::*;

fn time_ms(f: impl FnOnce()) -> f64 {
    let start = Instant::now();
    f();
    start.elapsed().as_secs_f64() * 1000.0
}

fn measure_scene(name: &str, canvas: &OffscreenCanvas, build: impl Fn(&mut DrawBatch), frames: u32) {
    // warmup
    for _ in 0..10 {
        let mut b = DrawBatch::new();
        build(&mut b);
        canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    }

    let mut times = Vec::with_capacity(frames as usize);
    let mut verts = 0usize;
    for _ in 0..frames {
        let mut b = DrawBatch::new();
        let t = time_ms(|| {
            build(&mut b);
            canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
        });
        verts = b.vertices.len();
        times.push(t);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let min = times[0];
    let max = *times.last().unwrap();
    let p50 = times[times.len() / 2];
    let fps = 1000.0 / avg;
    println!(
        "  {:<32} avg {:>7.3} ms  p50 {:>7.3}  min {:>7.3}  max {:>7.3}  ~{:>6.1} FPS  vtx {}",
        name, avg, p50, min, max, fps, verts
    );
}

fn scene_sdf(b: &mut DrawBatch) {
    for i in 0..2000 {
        let x = (i % 50) as f32 * 17.0 + 10.0;
        let y = (i / 50) as f32 * 17.0 + 10.0;
        b.set_position(x, y);
        b.sdf_feather = Some(1.0);
        match i % 5 {
            0 => draw_rectangle(b, Pos::new(0.0, 0.0), 14.0, 14.0, Some(RED)),
            1 => draw_circle(b, Pos::new(7.0, 7.0), 6.0, Some(GREEN)),
            2 => draw_rounded_rect(b, Pos::new(0.0, 0.0), 14.0, 14.0, 3.0, Some(BLUE)),
            3 => draw_ellipse(b, Pos::new(7.0, 7.0), 6.0, 4.0, Some(YELLOW)),
            _ => draw_triangle(b, 0.0, 0.0, 14.0, 0.0, 7.0, 14.0, Some(MAGENTA)),
        }
    }
}

fn scene_geo(b: &mut DrawBatch) {
    b.sdf_feather = None;
    for i in 0..500 {
        let x = (i % 25) as f32 * 35.0 + 15.0;
        let y = (i / 25) as f32 * 35.0 + 15.0;
        b.set_position(x, y);
        match i % 4 {
            0 => draw_circle(b, Pos::new(14.0, 14.0), 12.0, Some(RED)),
            1 => draw_ellipse(b, Pos::new(14.0, 14.0), 12.0, 8.0, Some(GREEN)),
            2 => draw_rounded_rect(b, Pos::new(0.0, 0.0), 28.0, 28.0, 6.0, Some(BLUE)),
            _ => {
                let pts = [(0.0, 0.0), (28.0, 4.0), (24.0, 28.0), (4.0, 22.0)];
                draw_polygon(b, &pts, Some(YELLOW));
            }
        }
    }
}

fn scene_transforms(b: &mut DrawBatch) {
    b.sdf_feather = Some(0.5);
    for i in 0..1000 {
        let x = (i % 40) as f32 * 22.0 + 15.0;
        let y = (i / 40) as f32 * 22.0 + 15.0;
        b.set_position(x, y);
        b.set_deg((i as f32) * 7.0);
        b.set_scale(1.0 + (i % 3) as f32 * 0.3, 1.0 + (i % 2) as f32 * 0.2);
        draw_rounded_rect(b, Pos::new(-6.0, -6.0), 6.0, 6.0, 2.0, Some(WHITE));
        b.clear_transform();
    }
}

fn scene_polygons(b: &mut DrawBatch) {
    b.sdf_feather = Some(1.0);
    for i in 0..200 {
        let x = (i % 20) as f32 * 44.0 + 30.0;
        let y = (i / 20) as f32 * 50.0 + 30.0;
        b.set_position(x, y);
        let sides = 5 + (i % 7);
        let r = 16.0;
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(sides);
        for j in 0..sides {
            let angle = std::f32::consts::TAU * j as f32 / sides as f32 - std::f32::consts::FRAC_PI_2;
            pts.push((r * angle.cos(), r * angle.sin()));
        }
        draw_polygon(b, &pts, Some(Color::new(0.5, 0.4, 0.8, 1.0)));
    }
}

fn scene_full(b: &mut DrawBatch) {
    scene_sdf(b);
    scene_transforms(b);
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn gpu_bench_scenes() {
    println!("\n=== GPU Offscreen Benchmark (900x700, 60 frames/scene) ===");

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    const FRAMES: u32 = 60;
    measure_scene("SDF feather x2000", &canvas, scene_sdf, FRAMES);
    measure_scene("Geometry x500", &canvas, scene_geo, FRAMES);
    measure_scene("Unique transforms x1000", &canvas, scene_transforms, FRAMES);
    measure_scene("Polygons x200", &canvas, scene_polygons, FRAMES);
    measure_scene("Full SDF+xform", &canvas, scene_full, FRAMES);
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_shape_text_ssaa_paths() {
    const MATERIAL_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(in.color.rgb * vec3<f32>(0.8, 1.0, 0.9), in.color.a);
}
"#;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let source = OffscreenCanvas::with_aa(
        &gpu,
        320,
        180,
        AntiAliasing::Ssaa { samples: 4, alpha_to_coverage: false },
        0.0,
    );
    let material = gpu.create_material(MATERIAL_WGSL).expect("material pipelines");

    let mut batch = DrawBatch::new();
    batch.custom_material = Some(material.clone());
    batch.sdf_feather = Some(1.0);
    draw_rounded_rect(&mut batch, Pos::new(20.0, 20.0), 280.0, 140.0, 20.0, Some(WHITE));
    draw_text(
        &mut batch.texts,
        "shape + text + SSAA",
        Pos::new(60.0, 75.0),
        TextDef::default().font_size(20.0),
        TextOverride::from_color(WHITE),
    );

    source.draw(Some(Color::new(0.05, 0.06, 0.09, 1.0)), &[&batch]);
}

fn solid_rgba(w: u32, h: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        v.extend_from_slice(&[r, g, b, 255]);
    }
    v
}

/// Batch base texture via `vireo_base_sample(in.base_uv)` on shape + text,
/// white fallback when no texture, UV remap, and MSAA path.
///
/// 默认跑（需本机 GPU；无 GPU 环境会失败）。
#[test]
fn material_base_sample_shape_text_paths() {
    const MATERIAL_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let base = vireo_base_sample(in.base_uv);
    if in.target_type == VIREO_TARGET_TEXT {
        return vec4<f32>(base.rgb, in.color.a);
    }
    return vec4<f32>(base.rgb * in.color.rgb, in.color.a);
}
"#;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let material = gpu.create_material(MATERIAL_WGSL).expect("material pipelines");
    let tex_red = Texture::from_rgba(32, 32, &solid_rgba(32, 32, 220, 40, 40), &gpu);
    let tex_blue = Texture::from_rgba(32, 32, &solid_rgba(32, 32, 40, 80, 220), &gpu);

    // 1) No set_texture: white base fallback for shape + text.
    {
        let canvas = OffscreenCanvas::new(&gpu, 256, 128);
        let mut batch = DrawBatch::new();
        batch.custom_material = Some(material.clone());
        batch.sdf_feather = Some(1.0);
        draw_rectangle(&mut batch, Pos::new(8.0, 8.0), 100.0, 48.0, Some(WHITE));
        draw_text(
            &mut batch.texts,
            "white base",
            Pos::new(16.0, 70.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(Color::new(0.04, 0.05, 0.07, 1.0)), &[&batch]);
    }

    // 2) Shape + text share one batch texture via vireo_base_sample.
    {
        let canvas = OffscreenCanvas::new(&gpu, 320, 160);
        let mut batch = DrawBatch::new();
        batch.custom_material = Some(material.clone());
        batch.set_texture(Some(&tex_red));
        batch.sdf_feather = Some(1.0);
        draw_rounded_rect(&mut batch, Pos::new(16.0, 16.0), 200.0, 80.0, 12.0, Some(WHITE));
        draw_text(
            &mut batch.texts,
            "base sample",
            Pos::new(40.0, 110.0),
            TextDef::default().font_size(18.0),
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(Color::new(0.04, 0.05, 0.07, 1.0)), &[&batch]);
    }

    // 3) Text texture segments: later set_texture only affects later text entries.
    {
        let canvas = OffscreenCanvas::new(&gpu, 360, 120);
        let mut batch = DrawBatch::new();
        batch.custom_material = Some(material.clone());
        batch.set_texture(Some(&tex_red));
        batch.text(
            "red",
            Pos::new(12.0, 40.0),
            TextDef::default().font_size(20.0),
            TextOverride::from_color(WHITE),
        );
        batch.set_texture(Some(&tex_blue));
        batch.text(
            "blue",
            Pos::new(120.0, 40.0),
            TextDef::default().font_size(20.0),
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(Color::new(0.04, 0.05, 0.07, 1.0)), &[&batch]);
    }

    // 4) UV remap on shape path.
    {
        let canvas = OffscreenCanvas::new(&gpu, 200, 100);
        let mut batch = DrawBatch::new();
        batch.custom_material = Some(material.clone());
        batch.set_texture(Some(&tex_blue));
        batch.set_uv(0.25, 0.25, 0.75, 0.75);
        draw_rectangle(&mut batch, Pos::new(10.0, 10.0), 180.0, 80.0, Some(WHITE));
        canvas.draw(Some(Color::new(0.04, 0.05, 0.07, 1.0)), &[&batch]);
    }

    // 5) MSAA path with base sample.
    {
        let canvas = OffscreenCanvas::with_aa(
            &gpu,
            240,
            120,
            AntiAliasing::Msaa {
                samples: 4,
                alpha_to_coverage: false,
            },
            0.0,
        );
        let mut batch = DrawBatch::new();
        batch.custom_material = Some(material.clone());
        batch.set_texture(Some(&tex_red));
        batch.sdf_feather = Some(1.0);
        draw_circle(&mut batch, Pos::new(60.0, 60.0), 40.0, Some(WHITE));
        draw_text(
            &mut batch.texts,
            "msaa",
            Pos::new(120.0, 50.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(Color::new(0.04, 0.05, 0.07, 1.0)), &[&batch]);
    }

    // 6) Stencil/clip path with custom material + base sample.
    {
        let canvas = OffscreenCanvas::new(&gpu, 240, 160);
        let mut parent = DrawBatch::new();
        parent.clips_children = true;
        parent.sdf_feather = Some(1.0);
        draw_rounded_rect(&mut parent, Pos::new(20.0, 20.0), 200.0, 120.0, 16.0, Some(WHITE));

        let mut child = DrawBatch::new();
        child.custom_material = Some(material.clone());
        child.set_texture(Some(&tex_blue));
        child.sdf_feather = Some(1.0);
        draw_rectangle(&mut child, Pos::new(40.0, 40.0), 160.0, 80.0, Some(WHITE));
        child.text(
            "clip",
            Pos::new(70.0, 70.0),
            TextDef::default().font_size(18.0),
            TextOverride::from_color(WHITE),
        );
        parent.children.push(child);
        canvas.draw(Some(Color::new(0.04, 0.05, 0.07, 1.0)), &[&parent]);
    }
}
