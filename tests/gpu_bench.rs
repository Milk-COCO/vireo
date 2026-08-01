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
    let mut stats = vireo::context::ShapeStats { mesh_vertices: 0, sdf_instances: 0, geo_instances: 0, geo_templates: 0, geo_template_vertices: 0 };
    let mut draw_calls: u32 = 0;
    for _ in 0..frames {
        let mut b = DrawBatch::new();
        let t = time_ms(|| {
            build(&mut b);
            canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
        });
        stats = b.shape_stats();
        draw_calls = canvas.last_draw_calls();
        times.push(t);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let min = times[0];
    let max = *times.last().unwrap();
    let p50 = times[times.len() / 2];
    let fps = 1000.0 / avg;
    println!(
        "  {:<32} avg {:>7.3} ms  p50 {:>7.3}  min {:>7.3}  max {:>7.3}  ~{:>6.1} FPS  meshV {:>6}  sdfInst {:>6}  geoInst {:>6}  geoTmpl {:>4}  geoTmplV {:>6}  drawCalls {:>4}",
        name, avg, p50, min, max, fps, stats.mesh_vertices, stats.sdf_instances, stats.geo_instances, stats.geo_templates, stats.geo_template_vertices, draw_calls
    );
}

fn scene_sdf(b: &mut DrawBatch) {
    for i in 0..2000 {
        let x = (i % 50) as f32 * 17.0 + 10.0;
        let y = (i / 50) as f32 * 17.0 + 10.0;
        b.set_position(x, y);
        b.set_sdf_feather(Some(1.0));
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
    b.clear_sdf_feather();
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
    b.set_sdf_feather(Some(0.5));
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
    b.set_sdf_feather(Some(1.0));
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

/// SDF + 几何混合 ×1000（交替）：`preserve_order=true` 时 1000 个 draw call，
/// `false` 时排序合并为 2 个。
fn scene_mixed(b: &mut DrawBatch) {
    for i in 0..1000 {
        let x = (i % 40) as f32 * 22.0 + 10.0;
        let y = (i / 40) as f32 * 22.0 + 10.0;
        b.set_position(x, y);
        if i % 2 == 0 {
            b.set_sdf_feather(Some(0.8));
            draw_rounded_rect(b, Pos::new(0.0, 0.0), 18.0, 18.0, 4.0, Some(RED));
        } else {
            b.clear_sdf_feather();
            draw_circle(b, Pos::new(9.0, 9.0), 8.0, Some(BLUE));
        }
    }
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
fn geo_instance_template_dedup_and_draw_calls() {
    println!("\n=== geo-instance 模板去重 + draw call (同参数几何 ×1000) ===");

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    let mut b = DrawBatch::new();
    b.clear_sdf_feather();
    for i in 0..1000 {
        let x = (i % 40) as f32 * 22.0 + 10.0;
        let y = (i / 40) as f32 * 22.0 + 10.0;
        b.set_position(x, y);
        draw_circle(&mut b, Pos::new(8.0, 8.0), 7.0, Some(RED));
    }
    let stats = b.shape_stats();
    assert_eq!(stats.geo_instances, 1000, "同参数圆应为 1000 个 geo instance");
    assert_eq!(stats.geo_templates, 1, "同参数圆应共享 1 个模板");
    assert!(
        b.shape_vertex_count() < 1000 * 258,
        "模板应复用：shape_vertex_count = {}",
        b.shape_vertex_count()
    );

    canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    let draw_calls = canvas.last_draw_calls();
    println!("  geo-instance x1000 draw calls = {draw_calls}");
    assert_eq!(draw_calls, 1, "同参数几何实例应合并为 1 个 draw call");

    // 不同参数 → 各自模板；同模板连续绘制时命令合并
    let mut c = DrawBatch::new();
    c.clear_sdf_feather();
    for i in 0..50 {
        let x = (i % 25) as f32 * 36.0 + 10.0;
        let y = (i / 25) as f32 * 36.0 + 10.0;
        c.set_position(x, y);
        draw_circle(&mut c, Pos::new(8.0, 8.0), 8.0, Some(RED));
    }
    for i in 0..50 {
        let x = (i % 25) as f32 * 36.0 + 20.0;
        let y = (i / 25) as f32 * 36.0 + 20.0;
        c.set_position(x, y);
        draw_rounded_rect(&mut c, Pos::ZERO, 20.0, 16.0, 4.0, Some(BLUE));
    }
    let cstats = c.shape_stats();
    assert_eq!(cstats.geo_instances, 100, "两种形状各 50 实例");
    assert_eq!(cstats.geo_templates, 2, "两种形状应各 1 个模板");
    canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&c]);
    let draw_calls = canvas.last_draw_calls();
    println!("  2 模板各 50 连续 draw calls = {draw_calls}");
    assert_eq!(draw_calls, 2, "同模板连续段应各自合并为 1 个 draw call");
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn merge_geo_templates_groups_interleaved_instances() {
    println!("\n=== merge_geo_templates 交替实例按模板分组合并 ===");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    // 交替绘制：圆/圆角矩形轮着画 → 同模板实例物理不连续
    let mut b = DrawBatch::new();
    b.clear_sdf_feather();
    for i in 0..500 {
        let x = (i % 25) as f32 * 35.0 + 15.0;
        let y = (i / 25) as f32 * 35.0 + 15.0;
        b.set_position(x, y);
        match i % 4 {
            0 => draw_circle(&mut b, Pos::new(14.0, 14.0), 12.0, Some(RED)),
            1 => draw_ellipse(&mut b, Pos::new(14.0, 14.0), 12.0, 8.0, Some(GREEN)),
            2 => draw_rounded_rect(&mut b, Pos::ZERO, 28.0, 28.0, 6.0, Some(BLUE)),
            3 => {
                let pts = [(0.0, 0.0), (28.0, 4.0), (24.0, 28.0), (4.0, 22.0)];
                draw_polygon(&mut b, &pts, Some(YELLOW));
            }
            _ => {}
        }
    }
    let stats = b.shape_stats();
    assert_eq!(stats.geo_instances, 500);
    assert_eq!(stats.geo_templates, 4, "4 种形状各 1 个模板");

    // 不开 merge：交替实例不连续 → 每实例一段（500 draw calls）
    canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    let normal_calls = canvas.last_draw_calls();
    println!("  preserve_order 交替 draw calls = {normal_calls}");
    assert_eq!(normal_calls, 500, "默认保序 + 不合并，交替实例各一段");

    // 开 merge_geo_templates：同模板实例按模板分组 → 4 个 draw call
    b.merge_geo_templates = true;
    canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    let merged_calls = canvas.last_draw_calls();
    println!("  merge_geo_templates draw calls = {merged_calls}");
    assert_eq!(merged_calls, 4, "同模板实例应合并，每模板 1 个 draw call");
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn preserve_order_reduces_draw_calls() {
    println!("\n=== preserve_order draw call 对比 (Mixed x1000) ===");

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    // 保序：1000 个 draw call
    {
        let mut b = DrawBatch::new();
        scene_mixed(&mut b);
        assert!(b.preserve_order, "默认应保序");
        canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
        let preserved = canvas.last_draw_calls();
        println!("  preserve_order=true  draw calls = {preserved}");
        assert_eq!(preserved, 1000, "保序时混合场景应为 1000 个 draw call");
    }

    // 重排：合并为 2 个 draw call
    {
        let mut b = DrawBatch::new();
        b.preserve_order = false;
        scene_mixed(&mut b);
        canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
        let reordered = canvas.last_draw_calls();
        println!("  preserve_order=false draw calls = {reordered}");
        assert_eq!(reordered, 2, "重排合并后混合场景应为 2 个 draw call");
    }
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
    batch.set_sdf_feather(Some(1.0));
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

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn instanced_shape_and_stencil_paths() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 320, 180);

    let mut parent = DrawBatch::new();
    parent.clips_children = true;
    parent.instance_circle(Pos::new(90.0, 90.0), 70.0, Some(WHITE));

    let mut child = DrawBatch::new();
    for i in 0..128 {
        let x = 20.0 + (i % 16) as f32 * 18.0;
        let y = 20.0 + (i / 16) as f32 * 18.0;
        match i % 3 {
            0 => child.instance_rectangle(Pos::new(x, y), 14.0, 10.0, Some(RED)),
            1 => child.instance_circle(Pos::new(x, y), 6.0, Some(GREEN)),
            _ => child.instance_ellipse(Pos::new(x, y), 7.0, 4.0, Some(BLUE)),
        }
    }
    child.instance_rounded_rect(Pos::new(220.0, 24.0), 64.0, 30.0, 8.0, Some(WHITE));
    child.instance_line(210.0, 72.0, 292.0, 98.0, 3.0, Some(RED));
    child.instance_triangle(220.0, 110.0, 280.0, 110.0, 250.0, 150.0, Some(GREEN));
    child.instance_arc(Pos::new(250.0, 142.0), 22.0, 0.0, std::f32::consts::PI, Some(BLUE));
    child.instance_polygon(&[(210.0, 112.0), (232.0, 102.0), (246.0, 118.0), (230.0, 132.0)], Some(WHITE));
    child.instance_line_chain(&[(262.0, 108.0), (280.0, 118.0), (266.0, 132.0), (286.0, 146.0)], 3.0, Some(RED));
    child.instance_rect_outline(Pos::new(200.0, 4.0), 30.0, 14.0, 2.0, Some(WHITE));
    child.instance_circle_outline(Pos::new(244.0, 12.0), 8.0, 2.0, Some(RED), 12);
    child.instance_ellipse_outline(Pos::new(274.0, 12.0), 11.0, 6.0, 2.0, Some(GREEN), 16);
    child.instance_rounded_rect_outline(Pos::new(200.0, 42.0), 30.0, 16.0, 4.0, 2.0, Some(BLUE), 4);
    child.instance_triangle_outline(244.0, 42.0, 264.0, 58.0, 276.0, 42.0, 2.0, Some(WHITE));
    child.instance_polygon_outline(&[(282.0, 42.0), (300.0, 42.0), (291.0, 58.0)], 2.0, Some(RED));
    child.instance_arc_outline(Pos::new(290.0, 76.0), 12.0, 0.0, std::f32::consts::PI, 2.0, Some(GREEN), 8);
    parent.push_child(child);
    canvas.draw(Some(BLACK), &[&parent]);
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn ordered_instance_mesh_instance_path() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 160, 120);

    let mut batch = DrawBatch::new();
    draw_rectangle(&mut batch, Pos::new(10.0, 10.0), 100.0, 80.0, Some(RED));
    batch.clear_sdf_feather();
    draw_triangle(
        &mut batch,
        20.0,
        100.0,
        80.0,
        20.0,
        140.0,
        100.0,
        Some(GREEN),
    );
    batch.set_sdf_feather(Some(1.0));
    draw_circle(&mut batch, Pos::new(80.0, 60.0), 24.0, Some(BLUE));

    canvas.draw(Some(BLACK), &[&batch]);
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn automatic_instance_custom_fallback_then_batch() {
    const MATERIAL_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return in.color;
}
"#;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 160, 120);
    let material = gpu.create_material(MATERIAL_WGSL).expect("material pipeline");

    let mut first = DrawBatch::new();
    draw_polygon(
        &mut first,
        &[(10.0, 10.0), (70.0, 10.0), (40.0, 70.0)],
        Some(RED),
    );
    first.custom_material = Some(material);

    let mut second = DrawBatch::new();
    draw_rectangle(&mut second, Pos::new(80.0, 20.0), 60.0, 80.0, Some(BLUE));
    canvas.draw(Some(BLACK), &[&first, &second]);
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
        batch.set_sdf_feather(Some(1.0));
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
        batch.set_sdf_feather(Some(1.0));
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
        batch.set_sdf_feather(Some(1.0));
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
        parent.set_sdf_feather(Some(1.0));
        draw_rounded_rect(&mut parent, Pos::new(20.0, 20.0), 200.0, 120.0, 16.0, Some(WHITE));

        let mut child = DrawBatch::new();
        child.custom_material = Some(material.clone());
        child.set_texture(Some(&tex_blue));
        child.set_sdf_feather(Some(1.0));
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
