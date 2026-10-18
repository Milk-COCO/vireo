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

fn measure_scene(
    name: &str,
    canvas: &OffscreenCanvas,
    build: impl Fn(&mut DrawBatch),
    frames: u32,
) {
    // warmup
    for _ in 0..10 {
        let mut b = DrawBatch::new();
        build(&mut b);
        canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    }

    let mut times = Vec::with_capacity(frames as usize);
    let mut stats = vireo::render::ShapeStats {
        mesh_vertices: 0,
        sdf_instances: 0,
        geo_instances: 0,
        geo_templates: 0,
        geo_template_vertices: 0,
        particles: 0,
    };
    let mut draw_calls: u32 = 0;
    for _ in 0..frames {
        let mut b = DrawBatch::new();
        let t = time_ms(|| {
            build(&mut b);
            canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
            loop {
                if let Ok(wgpu::PollStatus::QueueEmpty) =
                    canvas.gpu().device.poll(wgpu::PollType::Poll)
                {
                    break;
                }
            }
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
        name,
        avg,
        p50,
        min,
        max,
        fps,
        stats.mesh_vertices,
        stats.sdf_instances,
        stats.geo_instances,
        stats.geo_templates,
        stats.geo_template_vertices,
        draw_calls
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
            let angle =
                std::f32::consts::TAU * j as f32 / sides as f32 - std::f32::consts::FRAC_PI_2;
            pts.push((r * angle.cos(), r * angle.sin()));
        }
        draw_polygon(b, &pts, Some(Color::new(0.5, 0.4, 0.8, 1.0)));
    }
}

fn scene_full(b: &mut DrawBatch) {
    scene_sdf(b);
    scene_transforms(b);
}

/// 模拟 frame_stats HUD：约 10 行文字 + 600 个 glyph 实例（动态数字每帧变化）。
fn scene_text_hud(b: &mut DrawBatch) {
    let def = TextDef::default().font_size(12.0);
    for row in 0..10 {
        let s = format!(
            "FPS: {:.1}  Frame time: {:6.2}ms  avg {:5.2}  / p50 {:5.2}  / p99 {:5.2}  GPU: {:6.2}",
            60.0 + row as f64,
            16.6 + row as f64,
            16.6,
            16.6,
            16.6,
            1.5
        );
        draw_text(
            &mut b.texts,
            &s,
            Pos::new(16.0, 12.0 + row as f32 * 14.0),
            def.clone(),
            TextOverride::from_color(Color::new(0.9, 0.9, 1.0, 1.0)),
        );
    }
}

/// 模拟 frame_stats HUD 的动态数字：Glyphs parts + dynamic 每帧新字符串。
fn scene_text_hud_glyphs(b: &mut DrawBatch) {
    let def = TextDef::default().font_size(12.0);
    let rows: [(&[TextPart], f32); 8] = [
        (
            &[
                TextPart::normal("FPS: "),
                TextPart::glyphs(format!("{:.1}", 60.0)),
            ],
            12.0,
        ),
        (
            &[
                TextPart::normal("Frame time: "),
                TextPart::glyphs(format!("{:6.2}", 16.6)),
                TextPart::normal("ms  avg "),
                TextPart::glyphs(format!("{:5.2}", 16.6)),
                TextPart::normal(" / p50 "),
                TextPart::glyphs(format!("{:5.2}", 16.6)),
                TextPart::normal(" / p95 "),
                TextPart::glyphs(format!("{:5.2}", 16.6)),
                TextPart::normal(" / p99 "),
                TextPart::glyphs(format!("{:5.2}", 16.6)),
            ],
            26.0,
        ),
        (
            &[
                TextPart::normal("GPU queue: "),
                TextPart::glyphs(format!("{:6.2}", 1.5)),
                TextPart::normal("ms (previous completed submission)"),
            ],
            40.0,
        ),
        (
            &[
                TextPart::normal("min "),
                TextPart::glyphs(format!("{:5.2}", 1.0)),
                TextPart::normal(" / max "),
                TextPart::glyphs(format!("{:5.2}", 40.0)),
                TextPart::normal(" / stddev "),
                TextPart::glyphs(format!("{:4.2}", 3.0)),
                TextPart::normal("  (n="),
                TextPart::glyphs(300u32.to_string()),
                TextPart::normal(")"),
            ],
            54.0,
        ),
        (
            &[
                TextPart::normal("Spikes (>"),
                TextPart::glyphs(format!("{:.0}", 50.0)),
                TextPart::normal("ms): "),
                TextPart::glyphs(0u32.to_string()),
                TextPart::normal(" / "),
                TextPart::glyphs(300u32.to_string()),
            ],
            68.0,
        ),
        (
            &[
                TextPart::normal("AA: "),
                TextPart::dynamic("None".to_string()),
                TextPart::normal("  |  Present: "),
                TextPart::dynamic("AutoVsync".to_string()),
                TextPart::normal("  |  Cap: "),
                TextPart::dynamic("240".to_string()),
                TextPart::normal("  |  Latency: "),
                TextPart::dynamic("2".to_string()),
                TextPart::normal("  |  Text: "),
                TextPart::dynamic("on".to_string()),
            ],
            82.0,
        ),
        (
            &[
                TextPart::normal("Focus: "),
                TextPart::dynamic("yes".to_string()),
                TextPart::normal("  |  Frames: "),
                TextPart::glyphs(100u32.to_string()),
            ],
            96.0,
        ),
        (
            &[
                TextPart::normal("Init: app "),
                TextPart::glyphs(format!("{:.0}", 10.0)),
                TextPart::normal("ms + win "),
                TextPart::glyphs(format!("{:.0}", 20.0)),
                TextPart::normal("ms"),
            ],
            110.0,
        ),
    ];
    for (row, y) in rows.iter() {
        b.text_parts(
            row,
            Pos::new(16.0, *y),
            def.clone(),
            TextOverride::from_color(Color::new(0.9, 0.9, 1.0, 1.0)),
        );
    }
}

/// frame_stats 的 graph：300 根不同高度的 bar（走 geo template 路径）。
fn scene_graph_300(b: &mut DrawBatch) {
    b.clear_sdf_feather();
    for i in 0..300 {
        let h = ((i as f64 * 0.31).sin().abs() * 90.0 + 1.0) as f32;
        draw_rectangle(b, Pos::new(16.0 + i as f32 * 7.0, 250.0), 6.0, h, Some(RED));
    }
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

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    const FRAMES: u32 = 60;
    measure_scene("SDF feather x2000", &canvas, scene_sdf, FRAMES);
    measure_scene("Geometry x500", &canvas, scene_geo, FRAMES);
    measure_scene("Unique transforms x1000", &canvas, scene_transforms, FRAMES);
    measure_scene("Polygons x200", &canvas, scene_polygons, FRAMES);
    measure_scene("Full SDF+xform", &canvas, scene_full, FRAMES);
    measure_scene("Text HUD x10 rows", &canvas, scene_text_hud, FRAMES);
    measure_scene("Graph 300 bars", &canvas, scene_graph_300, FRAMES);
    measure_scene(
        "Graph 300 + Text",
        &canvas,
        |b| {
            scene_graph_300(b);
            scene_text_hud(b);
        },
        FRAMES,
    );
    measure_scene(
        "Text glyphs x8 rows",
        &canvas,
        scene_text_hud_glyphs,
        FRAMES,
    );
    measure_scene(
        "Graph 300 + Glyphs",
        &canvas,
        |b| {
            scene_graph_300(b);
            scene_text_hud_glyphs(b);
        },
        FRAMES,
    );
    measure_scene(
        "Graph 300 SDF + Glyphs",
        &canvas,
        |b| {
            b.set_sdf_feather(Some(1.0));
            for i in 0..300 {
                let h = ((i as f64 * 0.31).sin().abs() * 90.0 + 1.0) as f32;
                draw_rectangle(b, Pos::new(16.0 + i as f32 * 7.0, 250.0), 6.0, h, Some(RED));
            }
            scene_text_hud_glyphs(b);
        },
        FRAMES,
    );
    // Phase 0（粒子计划基线）：同贴图 quad x500。1b 验收：同场景 draw call
    // 不超过此基线（预期 1 dc，单纹理段跨参数合并）且像素一致。
    let tex_quad = Texture::from_rgba(32, 32, &solid_rgba(32, 32, 230, 200, 130), &gpu);
    measure_scene(
        "Textured quads x500 (particle proxy)",
        &canvas,
        |b| {
            b.set_sdf_feather(Some(1.0));
            b.set_texture(Some(&tex_quad));
            for i in 0..500 {
                let x = (i % 25) as f32 * 35.0 + 10.0;
                let y = (i / 25) as f32 * 35.0 + 10.0;
                b.set_position(x, y);
                draw_rectangle(b, Pos::new(0.0, 0.0), 28.0, 28.0, Some(WHITE));
            }
        },
        FRAMES,
    );
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn geo_instance_template_dedup_and_draw_calls() {
    println!("\n=== geo-instance 模板去重 + draw call (同参数几何 ×1000) ===");

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
    assert_eq!(
        stats.geo_instances, 1000,
        "同参数圆应为 1000 个 geo instance"
    );
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
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
fn merge_geo_templates_with_texture_segments() {
    println!("\n=== merge_geo_templates + 多纹理段 ===");
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);
    let tex_blue = Texture::from_rgba(16, 16, &solid_rgba(16, 16, 40, 80, 220), &gpu);

    // 交替绘制圆/圆角矩形 + 每 50 个切一次纹理 → 同模板实例同时被不同纹理段分割
    let mut b = DrawBatch::new();
    b.clear_sdf_feather();
    for i in 0..200 {
        let x = (i % 20) as f32 * 44.0 + 10.0;
        let y = (i / 20) as f32 * 44.0 + 10.0;
        b.set_position(x, y);
        if i == 50 {
            b.set_texture(Some(&tex_blue));
        }
        if i % 2 == 0 {
            draw_circle(&mut b, Pos::new(16.0, 16.0), 14.0, Some(RED));
        } else {
            draw_rounded_rect(&mut b, Pos::ZERO, 28.0, 28.0, 6.0, Some(BLUE));
        }
    }
    let stats = b.shape_stats();
    assert_eq!(stats.geo_instances, 200);
    assert_eq!(stats.geo_templates, 2, "2 种形状各 1 个模板");

    // 不开 merge：实例各一段 → 200 draw calls
    canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    let normal_calls = canvas.last_draw_calls();
    println!("  多纹理交替 draw calls = {normal_calls}");
    assert_eq!(normal_calls, 200, "默认每实例一段");

    // 开 merge：按 (texture segment, 模板) 分组 → 2 段纹理 × 2 模板 = 4 draw calls
    b.merge_geo_templates = true;
    canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    let merged_calls = canvas.last_draw_calls();
    println!("  merge + 多纹理 draw calls = {merged_calls}");
    assert_eq!(merged_calls, 4, "每 (纹理段, 模板) 组合 1 个 draw call");
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn preserve_order_reduces_draw_calls() {
    println!("\n=== preserve_order draw call 对比 (Mixed x1000) ===");

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let source = OffscreenCanvas::with_aa(
        &gpu,
        320,
        180,
        AntiAliasing::Ssaa {
            samples: 4,
            alpha_to_coverage: false,
        },
        0.0,
    );
    let material = gpu
        .create_material(MATERIAL_WGSL)
        .expect("material pipelines");

    let mut batch = DrawBatch::new();
    batch.set_custom_material(Some(material.clone()));
    batch.set_sdf_feather(Some(1.0));
    draw_rounded_rect(
        &mut batch,
        Pos::new(20.0, 20.0),
        280.0,
        140.0,
        20.0,
        Some(WHITE),
    );
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
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
    child.instance_arc(
        Pos::new(250.0, 142.0),
        22.0,
        0.0,
        std::f32::consts::PI,
        Some(BLUE),
    );
    child.instance_polygon(
        &[
            (210.0, 112.0),
            (232.0, 102.0),
            (246.0, 118.0),
            (230.0, 132.0),
        ],
        Some(WHITE),
    );
    child.instance_line_chain(
        &[
            (262.0, 108.0),
            (280.0, 118.0),
            (266.0, 132.0),
            (286.0, 146.0),
        ],
        3.0,
        Some(RED),
    );
    child.instance_rect_outline(Pos::new(200.0, 4.0), 30.0, 14.0, 2.0, Some(WHITE));
    child.instance_circle_outline(Pos::new(244.0, 12.0), 8.0, 2.0, Some(RED), 12);
    child.instance_ellipse_outline(Pos::new(274.0, 12.0), 11.0, 6.0, 2.0, Some(GREEN), 16);
    child.instance_rounded_rect_outline(Pos::new(200.0, 42.0), 30.0, 16.0, 4.0, 2.0, Some(BLUE), 4);
    child.instance_triangle_outline(244.0, 42.0, 264.0, 58.0, 276.0, 42.0, 2.0, Some(WHITE));
    child.instance_polygon_outline(
        &[(282.0, 42.0), (300.0, 42.0), (291.0, 58.0)],
        2.0,
        Some(RED),
    );
    child.instance_arc_outline(
        Pos::new(290.0, 76.0),
        12.0,
        0.0,
        std::f32::consts::PI,
        2.0,
        Some(GREEN),
        8,
    );
    parent.push_child(child);
    canvas.draw(Some(BLACK), &[&parent]);
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn ordered_instance_mesh_instance_path() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 160, 120);
    let material = gpu
        .create_material(MATERIAL_WGSL)
        .expect("material pipeline");

    let mut first = DrawBatch::new();
    draw_polygon(
        &mut first,
        &[(10.0, 10.0), (70.0, 10.0), (40.0, 70.0)],
        Some(RED),
    );
    first.set_custom_material(Some(material));

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

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let material = gpu
        .create_material(MATERIAL_WGSL)
        .expect("material pipelines");
    let tex_red = Texture::from_rgba(32, 32, &solid_rgba(32, 32, 220, 40, 40), &gpu);
    let tex_blue = Texture::from_rgba(32, 32, &solid_rgba(32, 32, 40, 80, 220), &gpu);

    // 1) No set_texture: white base fallback for shape + text.
    {
        let canvas = OffscreenCanvas::new(&gpu, 256, 128);
        let mut batch = DrawBatch::new();
        batch.set_custom_material(Some(material.clone()));
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
        batch.set_custom_material(Some(material.clone()));
        batch.set_texture(Some(&tex_red));
        batch.set_sdf_feather(Some(1.0));
        draw_rounded_rect(
            &mut batch,
            Pos::new(16.0, 16.0),
            200.0,
            80.0,
            12.0,
            Some(WHITE),
        );
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
        batch.set_custom_material(Some(material.clone()));
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
        batch.set_custom_material(Some(material.clone()));
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
        batch.set_custom_material(Some(material.clone()));
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
        draw_rounded_rect(
            &mut parent,
            Pos::new(20.0, 20.0),
            200.0,
            120.0,
            16.0,
            Some(WHITE),
        );

        let mut child = DrawBatch::new();
        child.set_custom_material(Some(material.clone()));
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

// ---------------------------------------------------------------------------
// Round 44：Custom Material 走 SDF/Geo instance layout 的 GPU 回归
// ---------------------------------------------------------------------------

/// 像素回读 helper：判断指定区域是否非背景色（任何 RGBA8 != 0）。
fn region_has_color(pixels: &[u8], w: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> bool {
    for y in y0..y1 {
        for x in x0..x1 {
            let idx = ((y * w + x) * 4) as usize;
            // 检查 RGB 任一通道 > 0（alpha 可能被 clear 颜色置 255 而不可靠）
            if pixels[idx] > 0 || pixels[idx + 1] > 0 || pixels[idx + 2] > 0 {
                return true;
            }
        }
    }
    false
}

/// 断言指定区域内全部像素「接近红色」（R 高、G/B 低）。
fn region_is_red(pixels: &[u8], w: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> bool {
    for y in y0..y1 {
        for x in x0..x1 {
            let idx = ((y * w + x) * 4) as usize;
            if !(pixels[idx] > 150 && pixels[idx + 1] < 80 && pixels[idx + 2] < 80) {
                return false;
            }
        }
    }
    true
}

/// 断言指定区域内全部像素「接近蓝色」（B 高、R/G 低）。
fn region_is_blue(pixels: &[u8], w: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> bool {
    for y in y0..y1 {
        for x in x0..x1 {
            let idx = ((y * w + x) * 4) as usize;
            if !(pixels[idx + 2] > 150 && pixels[idx] < 80 && pixels[idx + 1] < 80) {
                return false;
            }
        }
    }
    true
}

/// `Area::rect` 走 SDF stencil：掩码内可见、掩码外保持 clear 色。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn area_rect_clips_own_content() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 200, 200);

    let mut b = DrawBatch::new();
    b.area_include = Some(Area::rect(Rect::new(50.0, 50.0, 100.0, 100.0)));
    draw_rectangle(
        &mut b,
        Pos::new(0.0, 0.0),
        200.0,
        200.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let pixels = canvas.read_pixels();
    assert_eq!(canvas.last_draw_calls(), 3, "cover + content + erase");
    assert_eq!(
        canvas.last_clip_paths(),
        vec![ClipPath::AreaStencil],
        "诊断表应报告 Area 路径"
    );
    assert!(
        region_is_red(&pixels, 200, 70, 70, 130, 130),
        "Area::rect 内应显示红色矩形"
    );
    assert!(
        !region_has_color(&pixels, 200, 0, 0, 40, 40),
        "Area::rect 外应保持 clear 色"
    );
}

/// `OffscreenCanvas::draw_with_passes`：回调在引擎 pass 之后、submit 之前执行，
/// 同一 command buffer 内追加的 clear pass 覆盖引擎内容。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn offscreen_draw_with_passes_runs_after_engine() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 64, 64);

    let mut b = DrawBatch::new();
    draw_rectangle(
        &mut b,
        Pos::new(0.0, 0.0),
        64.0,
        64.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );

    let mut calls = 0u32;
    canvas.draw_with_passes(
        Some(Color::new(0.0, 0.0, 0.0, 1.0)),
        &[&b],
        |encoder, target| {
            calls += 1;
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.color_attachments[0].view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 1.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        },
    );
    assert_eq!(calls, 1, "回调应执行一次");

    let pixels = canvas.read_pixels();
    let i = (32 * 64 + 32) * 4;
    assert!(
        pixels[i + 1] > 200 && pixels[i] < 50 && pixels[i + 2] < 50,
        "自定义 pass 应覆盖引擎内容，实际像素 {:?}",
        &pixels[i..i + 4]
    );
}

/// Fragment-only Custom Material + SDF instance path：
/// - 2000 个交替参数 SDF instance 应合并为 1 个 draw call
/// - 像素回读：material 修改颜色后矩形区域非空
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_fragment_only_sdf_instance_path() {
    const MATERIAL_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    // 把颜色乘 0.5 让效果可见（与原 in.color 区分）
    return vec4<f32>(in.color.rgb * 0.5, in.color.a);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);
    let material = gpu
        .create_material(MATERIAL_WGSL)
        .expect("material pipelines");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material));
    b.set_sdf_feather(Some(1.0));
    for i in 0..2000 {
        let x = (i % 50) as f32 * 17.0 + 10.0;
        let y = (i / 50) as f32 * 17.0 + 10.0;
        b.set_position(x, y);
        match i % 5 {
            0 => draw_rectangle(&mut b, Pos::new(0.0, 0.0), 14.0, 14.0, Some(RED)),
            1 => draw_circle(&mut b, Pos::new(7.0, 7.0), 6.0, Some(GREEN)),
            2 => draw_rounded_rect(&mut b, Pos::new(0.0, 0.0), 14.0, 14.0, 3.0, Some(BLUE)),
            3 => draw_ellipse(&mut b, Pos::new(7.0, 7.0), 6.0, 4.0, Some(YELLOW)),
            _ => draw_triangle(&mut b, 0.0, 0.0, 14.0, 0.0, 7.0, 14.0, Some(MAGENTA)),
        }
    }
    let stats = b.shape_stats();
    assert_eq!(
        stats.sdf_instances, 2000,
        "fragment-only material 应仍走 SDF instance 路径"
    );
    assert_eq!(
        stats.mesh_vertices, 0,
        "fragment-only material 不应 mesh 展开"
    );

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let draw_calls = canvas.last_draw_calls();
    println!("  fragment-only Material + SDF x2000 draw calls = {draw_calls}");
    assert_eq!(
        draw_calls, 1,
        "fragment-only Material 应走 SDF instance pipeline，1 dc"
    );

    // 像素回读：第一个矩形位置 (10, 10) ~ (24, 24) 应有 material 修改后的非背景色
    let pixels = canvas.read_pixels();
    assert!(
        region_has_color(&pixels, 900, 12, 12, 22, 22),
        "SDF instance material 像素应可见"
    );
}

/// Fragment-only Custom Material + Geo instance path：
/// - 500 个 geo instance 应合并为 1 个 draw call
/// - 像素回读：material 修改颜色后圆形区域非空
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_fragment_only_geo_instance_path() {
    const MATERIAL_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(in.color.rgb * 0.5, in.color.a);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);
    let material = gpu
        .create_material(MATERIAL_WGSL)
        .expect("material pipelines");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material));
    b.clear_sdf_feather();
    for i in 0..500 {
        let x = (i % 25) as f32 * 35.0 + 15.0;
        let y = (i / 25) as f32 * 35.0 + 15.0;
        b.set_position(x, y);
        draw_circle(&mut b, Pos::new(14.0, 14.0), 12.0, Some(RED));
    }
    let stats = b.shape_stats();
    assert_eq!(stats.geo_instances, 500);
    assert_eq!(stats.geo_templates, 1, "同参数圆应共享 1 个模板");

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let draw_calls = canvas.last_draw_calls();
    println!("  fragment-only Material + Geo x500 draw calls = {draw_calls}");
    assert_eq!(
        draw_calls, 1,
        "fragment-only Material 应走 Geo instance pipeline，1 dc"
    );

    let pixels = canvas.read_pixels();
    // 第一个圆位置 (15, 15) ~ (39, 39)
    assert!(
        region_has_color(&pixels, 900, 17, 17, 37, 37),
        "Geo instance material 像素应可见"
    );
}

/// 带 custom vertex shader 的 Material + SDF instance：
/// custom VS 按 SDF 实例契约读取（QuadVertex + group2 binding2 实例 storage），
/// 参与实例化路径（不 mesh 回退），跨实例合并为 1 dc。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_custom_vs_sdf_instance_path() {
    const VERTEX_WGSL: &str = r#"
struct QuadVertex {
    @location(0) corner: vec2<f32>,
};

struct InstanceData {
    bounds: vec4<f32>,
    uv_bounds: vec4<f32>,
    uv_rect: vec4<f32>,
    color: vec4<f32>,
    sdf_params: vec4<f32>,
    sdf_extra: vec2<f32>,
    sdf_type: u32,
    sdf_feather: f32,
    transform_index: u32,
    _pad: u32,
    _pad2: vec2<u32>,
};

struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;
@group(2) @binding(2) var<storage> instances: array<InstanceData>;

// 必须提供完整 VertexOutput（auto-generated material FS 契约）
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) sdf_params: vec4<f32>,
    @location(3) @interpolate(linear) local_pos: vec2<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
    @location(6) sdf_extra: vec2<f32>,
};

@vertex
fn vs_main(vertex: QuadVertex, @builtin(instance_index) inst_idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let inst = instances[inst_idx];
    let t = vertex.corner * 0.5 + vec2<f32>(0.5);
    var local_pos = mix(inst.bounds.xy, inst.bounds.zw, t);
    local_pos.x += 2.0 * sin(local_pos.y * 0.25);
    let world_pos = transforms[inst.transform_index] * vec3<f32>(local_pos, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    let uv_t = (local_pos - inst.uv_bounds.xy) / (inst.uv_bounds.zw - inst.uv_bounds.xy);
    out.uv = mix(inst.uv_rect.xy, inst.uv_rect.zw, uv_t);
    out.color = inst.color;
    out.sdf_params = inst.sdf_params;
    out.local_pos = local_pos;
    out.sdf_type = inst.sdf_type;
    out.sdf_feather = inst.sdf_feather;
    out.sdf_extra = inst.sdf_extra;
    return out;
}
"#;
    const FRAGMENT_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(in.color.rgb * 0.3, in.color.a);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 320, 180);
    let material = gpu
        .create_material_with_vertex_shader(FRAGMENT_WGSL, VERTEX_WGSL)
        .expect("material with custom VS");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material));
    b.set_sdf_feather(Some(1.0));
    for i in 0..20 {
        let x = (i % 5) as f32 * 60.0 + 20.0;
        let y = (i / 5) as f32 * 60.0 + 20.0;
        b.set_position(x, y);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 40.0, 40.0, Some(WHITE));
    }

    let stats = b.shape_stats();
    println!("  custom VS Material + SDF stats = {stats:?}");
    assert_eq!(
        stats.sdf_instances, 20,
        "custom VS 应走 SDF instance 路径（不 mesh 回退）"
    );
    assert_eq!(stats.mesh_vertices, 0);

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let draw_calls = canvas.last_draw_calls();
    println!("  custom VS Material + SDF x20 draw calls = {draw_calls}");
    assert_eq!(draw_calls, 1, "SDF instance 段应合并为 1 dc");

    let pixels = canvas.read_pixels();
    // 第一个矩形 (20, 20) ~ (60, 60)
    assert!(
        region_has_color(&pixels, 320, 22, 22, 58, 58),
        "custom VS Material SDF instance 像素应可见"
    );
}

/// 带 custom vertex shader 的 Material + Geo（sdf_feather=None）：
/// custom VS 按 geo 契约读取（GeoVertex 模板 @0/@1 + GeoInstance 属性 @2/@3），
/// 参与模板实例化路径（不 mesh 回退），同半径圆共享模板。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_custom_vs_geo_instance_path() {
    const VERTEX_WGSL: &str = r#"
struct TemplateVertex {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct GeoInstanceInput {
    @location(2) color: vec4<f32>,
    @location(3) @interpolate(flat) transform_index: u32,
};

struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;

// 必须提供完整 VertexOutput（auto-generated material FS 契约）
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) sdf_params: vec4<f32>,
    @location(3) @interpolate(linear) local_pos: vec2<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
    @location(6) sdf_extra: vec2<f32>,
};

@vertex
fn vs_main(vertex: TemplateVertex, instance: GeoInstanceInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = transforms[instance.transform_index] * vec3<f32>(vertex.position, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    out.uv = vertex.uv;
    out.color = instance.color;
    out.sdf_params = vec4<f32>(0.0);
    out.local_pos = vertex.position;
    out.sdf_type = 0u;
    out.sdf_feather = 0.0;
    out.sdf_extra = vec2<f32>(0.0);
    return out;
}
"#;
    const FRAGMENT_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(in.color.rgb * 0.3, in.color.a);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 320, 180);
    let material = gpu
        .create_material_with_vertex_shader(FRAGMENT_WGSL, VERTEX_WGSL)
        .expect("material with custom VS");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material));
    b.clear_sdf_feather();
    for i in 0..20 {
        let x = (i % 5) as f32 * 60.0 + 20.0;
        let y = (i / 5) as f32 * 60.0 + 20.0;
        b.set_position(x, y);
        draw_circle(&mut b, Pos::new(20.0, 20.0), 20.0, Some(WHITE));
    }

    let stats = b.shape_stats();
    println!("  custom VS Material + Geo stats = {stats:?}");
    assert_eq!(
        stats.geo_instances, 20,
        "custom VS 应走 geo instance 路径（不 mesh 回退）"
    );
    assert_eq!(stats.geo_templates, 1, "同半径圆应共享 1 个模板");
    assert_eq!(stats.mesh_vertices, 0);

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let draw_calls = canvas.last_draw_calls();
    println!("  custom VS Material + Geo x20 draw calls = {draw_calls}");
    assert_eq!(draw_calls, 1, "geo instance 段应合并为 1 dc");

    let pixels = canvas.read_pixels();
    assert!(
        region_has_color(&pixels, 320, 42, 42, 78, 78),
        "custom VS Material geo instance 像素应可见"
    );
}

/// 带 custom vertex shader 的 Material 但 VS 只支持其它布局（纯 mesh 契约）：
/// 与绘制路径所需布局不匹配时形状被跳过（不回落 mesh、不静默画错），像素保持清屏色。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_custom_vs_unsupported_layout_skips_shape() {
    const VERTEX_WGSL: &str = r#"
struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) sdf_params: vec4<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
    @location(6) sdf_extra: vec2<f32>,
    @location(7) @interpolate(flat) transform_index: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) sdf_params: vec4<f32>,
    @location(3) @interpolate(linear) local_pos: vec2<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
    @location(6) sdf_extra: vec2<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = transforms[in.transform_index] * vec3<f32>(in.position, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.sdf_params = in.sdf_params;
    out.local_pos = in.position;
    out.sdf_type = in.sdf_type;
    out.sdf_feather = in.sdf_feather;
    out.sdf_extra = in.sdf_extra;
    return out;
}
"#;
    const FRAGMENT_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(in.color.rgb * 0.3, in.color.a);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 320, 180);
    // mesh 契约 VS 可编译（Mesh layout 提供 @0-@7），但 SDF/geo 实例布局缺 @1..@7
    let material = gpu
        .create_material_with_vertex_shader(FRAGMENT_WGSL, VERTEX_WGSL)
        .expect("mesh-only contract 仍可创建 material");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material));
    b.set_sdf_feather(Some(1.0));
    b.set_position(20.0, 20.0);
    draw_rectangle(&mut b, Pos::new(0.0, 0.0), 40.0, 40.0, Some(WHITE));

    let stats = b.shape_stats();
    println!("  unsupported custom VS stats = {stats:?}");
    assert_eq!(stats.sdf_instances, 0, "布局不匹配时应跳过（不回落 mesh）");
    assert_eq!(stats.mesh_vertices, 0);

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let draw_calls = canvas.last_draw_calls();
    println!("  unsupported custom VS draw calls = {draw_calls}");
    assert_eq!(draw_calls, 0, "被跳过的形状不应产生 draw call");

    let pixels = canvas.read_pixels();
    assert!(
        !region_has_color(&pixels, 320, 22, 22, 58, 58),
        "被跳过的形状不应有像素（保持清屏色）"
    );
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn device_lost_flag_is_shared_and_clear_by_default() {
    println!("\n=== device-lost 标志：默认 false，App/GpuContext 共享同一 Arc ===");
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    assert!(!gpu.is_device_lost(), "新设备不应处于 lost 状态");
    let flag = gpu.device_lost();
    flag.store(true, std::sync::atomic::Ordering::Release);
    assert!(
        gpu.is_device_lost(),
        "设置返回的 Arc 后 GpuContext 应观测到 lost"
    );
    assert!(
        flag.load(std::sync::atomic::Ordering::Acquire),
        "同一 Arc 应被置位"
    );
}

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_non_default_texture_kind_creates_lazily() {
    println!(
        "\n=== 非默认 texture kind（D2 Sint + NonFiltering）材质创建不再 panic（懒构建初始 bind group）==="
    );
    const WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let v = textureLoad(tex, vec2<i32>(0, 0), 0);
    return vec4<f32>(f32(v.x) / 255.0, 0.1, 0.1, 1.0);
}

"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));

    let resources = vireo::material::MaterialResources(&[vireo::material::MaterialResource {
        name: "tex",
        kind: vireo::material::MaterialResourceKind::Texture {
            view: vireo::material::TexKind::D2(vireo::material::TexSample::Sint),
            sampler: vireo::material::SampKind::NonFiltering,
        },
    }]);
    // 修复前：build_auto_defaults 用 white_view（Rgba8UnormSrgb float）绑 Sint 槽，
    // create_bind_group 在创建期校验 sample type 不匹配 → validation panic。
    // 修复后：placeholder_ok=false → 初始 bind group 懒构建，创建成功。
    let material = gpu
        .create_material_with_resources(WGSL, resources)
        .expect("非默认 kind 材质创建不应 panic");

    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sint tex"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Sint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &200_i32.to_le_bytes(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    material.set_texture(&gpu.device, "tex", &view, &sampler);

    let canvas = OffscreenCanvas::new(&gpu, 64, 64);
    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material.clone()));
    draw_rectangle(&mut b, Pos::new(0.0, 0.0), 64.0, 64.0, Some(WHITE));
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);

    let pixels = canvas.read_pixels();
    let mut red: u32 = 0;
    for y in 8..24 {
        for x in 8..24 {
            let idx = ((y * 64 + x) * 4) as usize;
            red += pixels[idx] as u32;
        }
    }
    let red = red / (16 * 16);
    println!("  region red channel (linear 200 / 255; sRGB surface 输出应 ~229) = {red}");
    assert!(region_has_color(&pixels, 64, 8, 8, 24, 24), "区域应有内容");
    assert!(
        (red as i32 - 229).abs() < 24,
        "Sint 纹理 200 经 sRGB 编码应显示为红 ~229，实际 {red}"
    );
}

/// 移除材质纹理槽白色兜底后的回归：
/// - float 纹理槽未 set_texture 时：创建/绘制不 panic，整批跳过（区域为 clear 色，无色）。
/// - set_texture 填齐后：整批正常绘制（区域有色）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_unset_texture_slot_skips_batch() {
    println!("\n=== 未填 float 纹理槽：整批跳过 + 填齐后恢复绘制 ===");
    const WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let c = textureLoad(tex, vec2<i32>(0, 0), 0);
    return vec4<f32>(c.rgb, in.color.a);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));

    let resources = vireo::material::MaterialResources(&[vireo::material::MaterialResource {
        name: "tex",
        kind: vireo::material::MaterialResourceKind::Texture {
            view: vireo::material::TexKind::D2(vireo::material::TexSample::Float),
            sampler: vireo::material::SampKind::NonFiltering,
        },
    }]);
    let material = gpu
        .create_material_with_resources(WGSL, resources)
        .expect("材质创建不应 panic（纹理槽未填时不再用 white fallback 绑组）");

    let canvas = OffscreenCanvas::new(&gpu, 64, 64);

    // 1) 未填纹理槽：整批跳过，不应 panic，区域应为 clear 色（黑）无色
    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material.clone()));
    draw_rectangle(&mut b, Pos::new(0.0, 0.0), 64.0, 64.0, Some(WHITE));
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let pixels = canvas.read_pixels();
    assert!(
        !region_has_color(&pixels, 64, 8, 8, 24, 24),
        "未填纹理槽：整批应跳过，区域应保持 clear 背景色"
    );

    // 2) 填齐纹理后：整批正常绘制，区域有色
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("float tex"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255_u8, 0, 0, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    material.set_texture(&gpu.device, "tex", &view, &sampler);

    let mut b2 = DrawBatch::new();
    b2.set_custom_material(Some(material.clone()));
    draw_rectangle(&mut b2, Pos::new(0.0, 0.0), 64.0, 64.0, Some(WHITE));
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b2]);
    let pixels = canvas.read_pixels();
    assert!(
        region_has_color(&pixels, 64, 8, 8, 24, 24),
        "填齐纹理槽后：整批应正常绘制，区域应有内容"
    );
}

/// 粒子默认路径：500 不朽粒子 1 dc＋像素可见；未出生粒子被 VS 剔除。
/// 时间走 0 号钟定值，证明 time uniform 真正驱动 VS。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_default_path_single_draw_call_and_time_driven_cull() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    gpu.set_clock_time(ClockIndex::ZERO, 50.0);
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    let mut pool = ParticlePool::new();
    for i in 0..500 {
        let x = (i % 25) as f32 * 35.0 + 10.0;
        let y = (i / 25) as f32 * 35.0 + 10.0;
        pool.spawn(Particle {
            pos: [x + 14.0, y + 14.0],
            vel: [0.0, 0.0],
            size: [14.0, 14.0],
            rotation: 0.0,
            damping: 0.0,
            gravity: [0.0, 0.0],
            size_mul_end: 1.0,
            atlas: None,
            fade_mid: None,
            color_end: None,
            color: WHITE,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            birth: 0.0,
            life: -1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            seed: 0.0,
        });
    }
    // 未出生：birth 在 override 之后 → age < 0 → VS 退化剔除
    let mut future_pool = ParticlePool::new();
    future_pool.spawn(Particle {
        pos: [450.0, 350.0],
        vel: [0.0, 0.0],
        size: [40.0, 40.0],
        rotation: 0.0,
        damping: 0.0,
        gravity: [0.0, 0.0],
        size_mul_end: 1.0,
        atlas: None,
        fade_mid: None,
        color_end: None,
        color: WHITE,
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        birth: 100.0,
        life: -1.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });

    let mut b = DrawBatch::new();
    draw_particles(&mut b, &pool);
    let stats = b.shape_stats();
    assert_eq!(stats.particles, 500);
    assert_eq!(stats.mesh_vertices, 0, "默认路径不应 mesh 展开");
    assert_eq!(b.shape_vertex_count(), 2000);

    let mut b2 = DrawBatch::new();
    draw_particles(&mut b2, &future_pool);

    // 分两次 draw（各清屏）：活集查可见，未出生集独占一帧查全黑。
    // 未出生检查区若与活集同帧，会撞上网格粒子（col12/row9 落在区内），故分开。
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    assert_eq!(canvas.last_draw_calls(), 1, "活集单纹理段跨参数合并，1 dc");
    let pixels = canvas.read_pixels();
    assert!(
        region_has_color(&pixels, 900, 14, 14, 34, 34),
        "存活粒子像素应可见"
    );

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b2]);
    assert_eq!(
        canvas.last_draw_calls(),
        1,
        "全剔除批次仍发一次 draw（GPU 侧剔除，CPU 不剪枝）"
    );
    let pixels = canvas.read_pixels();
    assert!(
        !region_has_color(&pixels, 900, 410, 310, 490, 390),
        "未出生粒子应被 VS 剔除（time uniform 生效）"
    );
}

/// G2 闭式积分像素验证：重力抛物线分支（damping=0）＋指数分支（damping≠0）
/// 各一位移断言。`set_clock_time` 定死 age，两分支各一帧。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_gravity_and_damping_displace() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    gpu.set_clock_time(ClockIndex::ZERO, 1.0);
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    // 重力：birth 上方 200px 处出生，age=1 下坠 0.5*400*1²=200 → 落回中心
    let mut pool = ParticlePool::new();
    pool.spawn(Particle {
        pos: [450.0, 150.0],
        vel: [0.0, 0.0],
        size: [10.0, 10.0],
        rotation: 0.0,
        damping: 0.0,
        gravity: [0.0, 400.0],
        size_mul_end: 1.0,
        atlas: None,
        fade_mid: None,
        color_end: None,
        color: WHITE,
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        birth: 0.0,
        life: 10.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });
    let mut b = DrawBatch::new();
    draw_particles(&mut b, &pool);
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let pixels = canvas.read_pixels();
    assert!(
        region_has_color(&pixels, 900, 440, 340, 460, 360),
        "重力粒子应落在出生点下方 200px"
    );
    assert!(
        !region_has_color(&pixels, 900, 440, 140, 460, 160),
        "出生点处应已无粒子（整 quad 移走）"
    );

    // 阻尼：vel 200/damping -2/age 1 → x=(e^-2-1)/(-2)*200≈86.5（非线性 200）
    let mut pool2 = ParticlePool::new();
    pool2.spawn(Particle {
        pos: [100.0, 350.0],
        vel: [200.0, 0.0],
        size: [10.0, 10.0],
        rotation: 0.0,
        damping: -2.0,
        gravity: [0.0, 0.0],
        size_mul_end: 1.0,
        atlas: None,
        fade_mid: None,
        color_end: None,
        color: WHITE,
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        birth: 0.0,
        life: 10.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });
    let mut b2 = DrawBatch::new();
    draw_particles(&mut b2, &pool2);
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b2]);
    let pixels = canvas.read_pixels();
    assert!(
        region_has_color(&pixels, 900, 176, 340, 196, 360),
        "阻尼粒子应落在 x≈186（指数衰减，非线性 300）"
    );
    assert!(
        !region_has_color(&pixels, 900, 290, 340, 310, 360),
        "线性终点 x=300 处应无粒子"
    );
}
/// G3 终点像素验证：k=0.5 时颜色 mix＋尺寸收缩各一断言。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_endpoints_mix_color_and_size() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    gpu.set_clock_time(ClockIndex::ZERO, 1.0);
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    let mut pool = ParticlePool::new();
    // 颜色：白→纯红，life 2/age 1 → k=0.5 → linear(1,0.5,0.5) → sRGB 亮粉
    pool.spawn(Particle {
        pos: [300.0, 350.0],
        vel: [0.0, 0.0],
        size: [10.0, 10.0],
        rotation: 0.0,
        damping: 0.0,
        gravity: [0.0, 0.0],
        size_mul_end: 1.0,
        atlas: None,
        fade_mid: None,
        color_end: Some(Color::new(1.0, 0.0, 0.0, 1.0)),
        color: WHITE,
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        birth: 0.0,
        life: 2.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });
    // 尺寸：半 30→mul 0，k=0.5 → 半 15（出生盒 ±30 处应黑）
    pool.spawn(Particle {
        pos: [600.0, 350.0],
        vel: [0.0, 0.0],
        size: [30.0, 30.0],
        rotation: 0.0,
        damping: 0.0,
        gravity: [0.0, 0.0],
        size_mul_end: 0.0,
        atlas: None,
        fade_mid: None,
        color_end: None,
        color: WHITE,
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        birth: 0.0,
        life: 2.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });
    let mut b = DrawBatch::new();
    draw_particles(&mut b, &pool);
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let pixels = canvas.read_pixels();
    let px = |x: u32, y: u32| {
        let o = ((y * 900 + x) * 4) as usize;
        (pixels[o], pixels[o + 1], pixels[o + 2])
    };
    let (r, g, bl) = px(300, 350);
    assert!(
        r > 200 && (100..=220).contains(&g) && (100..=220).contains(&bl),
        "k=0.5 白→红应为亮粉，got ({r},{g},{bl})"
    );
    let (r2, g2, b2) = px(600, 350);
    assert!(r2 > 200 && g2 > 200 && b2 > 200, "收缩粒子中心应仍亮");
    assert!(
        !region_has_color(&pixels, 900, 622, 340, 638, 360),
        "出生盒边缘（±22~38）收缩后应黑"
    );
}

/// G5 序列像素验证：2×2 四色表，frame 0 = 红（左上），frame 1 = 绿（右上）。
/// 行优先，row 0 = v0 边。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_atlas_seq_advances_frames() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);
    // 64×64 四象限：TL 红 / TR 绿 / BL 蓝 / BR 白（row 0 = 数据前 32 行 = v0 边）
    let mut data = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64 {
        for x in 0..64 {
            let c = match (x < 32, y < 32) {
                (true, true) => [255, 0, 0, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [0, 0, 255, 255],
                (false, false) => [255, 255, 255, 255],
            };
            data.extend_from_slice(&c);
        }
    }
    let tex = Texture::from_rgba(64, 64, &data, &gpu);
    let spawn_seq = |pool: &mut ParticlePool| {
        pool.spawn(Particle {
            pos: [450.0, 350.0],
            vel: [0.0, 0.0],
            size: [20.0, 20.0],
            rotation: 0.0,
            damping: 0.0,
            gravity: [0.0, 0.0],
            size_mul_end: 1.0,
            color_end: None,
            atlas: Some(AtlasSeq {
                cols: 2,
                rows: 2,
                fps: 1.0,
                start: 0,
            }),
            fade_mid: None,
            color: WHITE,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            birth: 0.0,
            life: 10.0,
            fade_in: 0.0,
            fade_out: 0.0,
            seed: 0.0,
        });
    };
    let draw_at = |t: f32| {
        gpu.set_clock_time(ClockIndex::ZERO, t);
        let mut pool = ParticlePool::new();
        spawn_seq(&mut pool);
        let mut b = DrawBatch::new();
        b.set_texture(Some(&tex));
        draw_particles(&mut b, &pool);
        canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
        canvas.read_pixels()
    };
    let px = |pixels: &[u8]| {
        let o = ((350 * 900 + 450) * 4) as usize;
        (pixels[o], pixels[o + 1], pixels[o + 2])
    };
    let (r0, g0, b0) = px(&draw_at(0.2));
    assert!(
        r0 > 200 && g0 < 80 && b0 < 80,
        "frame 0 应为红，got ({r0},{g0},{b0})"
    );
    let (r1, g1, b1) = px(&draw_at(1.2));
    assert!(
        g1 > 200 && r1 < 80 && b1 < 80,
        "frame 1 应为绿，got ({r1},{g1},{b1})"
    );
}

/// G7 包络像素验证：同条件下线性中点（0.7→约 218）vs smoothstep（0.5→约 187）
/// 并排可分。注意 `Some(0.5)` ≠ smoothstep 外观（线性 vs 曲线），故对照只看分得开。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_fade_mid_linear_vs_smoothstep() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    gpu.set_clock_time(ClockIndex::ZERO, 1.0);
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    let mut pool = ParticlePool::new();
    let mut spawn_at = |x: f32, mid: Option<f32>| {
        pool.spawn(Particle {
            pos: [x, 350.0],
            vel: [0.0, 0.0],
            size: [20.0, 20.0],
            rotation: 0.0,
            damping: 0.0,
            gravity: [0.0, 0.0],
            size_mul_end: 1.0,
            color_end: None,
            atlas: None,
            fade_mid: mid,
            color: WHITE,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            birth: 0.0,
            life: 2.0,
            fade_in: 0.0,
            fade_out: 2.0,
            seed: 0.0,
        });
    };
    spawn_at(300.0, Some(0.7));
    spawn_at(600.0, None);
    let mut b = DrawBatch::new();
    draw_particles(&mut b, &pool);
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let pixels = canvas.read_pixels();
    let px = |x: u32| {
        let o = ((350 * 900 + x) * 4) as usize;
        pixels[o]
    };
    let lin = px(300);
    let sm = px(600);
    assert!(
        (200..=235).contains(&lin),
        "线性中点 0.7 应约 218，got {lin}"
    );
    assert!(
        (165..=200).contains(&sm),
        "smoothstep 中点应约 187，got {sm}"
    );
    assert!(lin > sm + 10, "两档必须分得开（{lin} vs {sm}）");
}

/// Fragment-only Custom Material + 粒子路径：500 粒子 1 dc＋像素可见。
/// 验证 `ensure_material_pipeline(Particle)` 真实编译出可用管线。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn material_fragment_only_particle_path() {
    const MATERIAL_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(in.color.rgb * 0.5, in.color.a);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);
    let material = gpu
        .create_material(MATERIAL_WGSL)
        .expect("material pipelines");

    let mut pool = ParticlePool::new();
    for i in 0..500 {
        let x = (i % 25) as f32 * 35.0 + 10.0;
        let y = (i / 25) as f32 * 35.0 + 10.0;
        pool.spawn(Particle {
            pos: [x + 14.0, y + 14.0],
            vel: [0.0, 0.0],
            size: [14.0, 14.0],
            rotation: 0.0,
            damping: 0.0,
            gravity: [0.0, 0.0],
            size_mul_end: 1.0,
            atlas: None,
            fade_mid: None,
            color_end: None,
            color: WHITE,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            birth: 0.0,
            life: -1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            seed: i as f32,
        });
    }

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material));
    draw_particles(&mut b, &pool);
    let stats = b.shape_stats();
    assert_eq!(
        stats.particles, 500,
        "fragment-only material 应仍走粒子 instance 路径"
    );
    assert_eq!(
        stats.mesh_vertices, 0,
        "fragment-only material 不应 mesh 展开"
    );

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let draw_calls = canvas.last_draw_calls();
    println!("  fragment-only Material + particle x500 draw calls = {draw_calls}");
    assert_eq!(
        draw_calls, 1,
        "fragment-only Material 应走粒子 instance pipeline，1 dc"
    );

    let pixels = canvas.read_pixels();
    assert!(
        region_has_color(&pixels, 900, 14, 14, 34, 34),
        "粒子 material 像素应可见"
    );
}

/// 粒子 reorder 合并：P(2)＋geo 圆＋P(2)，同白纹理。
/// 保序 → 3 dc；`preserve_order=false` → 两段 Particles 按 bind group 聚拢
/// （范围在粒子缓冲内连续）合并 → 2 dc。覆盖 sort_key/try_merge 粒子臂运行时。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_reorder_merges_across_gap() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    let mut pool = ParticlePool::new();
    for (x, y) in [(100.0, 100.0), (200.0, 100.0)] {
        pool.spawn(Particle {
            pos: [x, y],
            vel: [0.0, 0.0],
            size: [10.0, 10.0],
            rotation: 0.0,
            damping: 0.0,
            gravity: [0.0, 0.0],
            size_mul_end: 1.0,
            atlas: None,
            fade_mid: None,
            color_end: None,
            color: WHITE,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            birth: 0.0,
            life: -1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            seed: 0.0,
        });
    }

    let mut b = DrawBatch::new();
    draw_particles(&mut b, &pool);
    b.clear_sdf_feather();
    draw_circle(&mut b, Pos::new(400.0, 300.0), 30.0, Some(RED));
    draw_particles(&mut b, &pool);

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    assert_eq!(canvas.last_draw_calls(), 3, "保序：粒子/几何/粒子各一段");
    b.preserve_order = false;
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    assert_eq!(
        canvas.last_draw_calls(),
        2,
        "重排：两段同 bg 粒子合并为 1 dc ＋几何 1 dc"
    );
}

/// 带纹理粒子：左红右蓝 8×8 纹理，两粒子各取半区 uv。
/// 断言 {A,B}=={红,蓝}（集合相等，不依赖 U 翻转约定）＋同纹理 1 dc。
/// 证明 batch 纹理绑定＋逐粒子 `uv_rect` 生效（非白 fallback）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_textured_uv_rects() {
    fn region_dominant(
        pixels: &[u8],
        w: u32,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        want_red: bool,
    ) -> bool {
        let mut hit = 0u32;
        let mut total = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let idx = ((y * w + x) * 4) as usize;
                let (r, g, b) = (pixels[idx], pixels[idx + 1], pixels[idx + 2]);
                total += 1;
                if want_red {
                    if r > 150 && g < 120 && b < 120 {
                        hit += 1;
                    }
                } else if b > 150 && r < 120 && g < 120 {
                    hit += 1;
                }
            }
        }
        hit * 2 > total
    }

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);
    // 左半红、右半蓝
    let mut tex_bytes = Vec::with_capacity(8 * 8 * 4);
    for _ in 0..8 {
        for x in 0..8 {
            if x < 4 {
                tex_bytes.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                tex_bytes.extend_from_slice(&[0, 0, 255, 255]);
            }
        }
    }
    let tex = Texture::from_rgba(8, 8, &tex_bytes, &gpu);

    let mut pool = ParticlePool::new();
    pool.spawn(Particle {
        pos: [100.0, 100.0],
        vel: [0.0, 0.0],
        size: [40.0, 40.0],
        rotation: 0.0,
        damping: 0.0,
        gravity: [0.0, 0.0],
        size_mul_end: 1.0,
        atlas: None,
        fade_mid: None,
        color_end: None,
        color: WHITE,
        uv_rect: [0.0, 0.0, 0.5, 1.0],
        birth: 0.0,
        life: -1.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });
    pool.spawn(Particle {
        pos: [340.0, 100.0],
        vel: [0.0, 0.0],
        size: [40.0, 40.0],
        rotation: 0.0,
        damping: 0.0,
        gravity: [0.0, 0.0],
        size_mul_end: 1.0,
        atlas: None,
        fade_mid: None,
        color_end: None,
        color: WHITE,
        uv_rect: [0.5, 0.0, 1.0, 1.0],
        birth: 0.0,
        life: -1.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });

    let mut b = DrawBatch::new();
    b.set_texture(Some(&tex));
    draw_particles(&mut b, &pool);
    assert_eq!(b.shape_stats().particles, 2);

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    assert_eq!(canvas.last_draw_calls(), 1, "同纹理两粒子应合并为 1 dc");
    let pixels = canvas.read_pixels();
    // A=(60..140)^2 取左半，B=(300..380)×(60..140) 取右半；检查区避开中缝过滤带
    let a_red = region_dominant(&pixels, 900, 70, 70, 95, 125, true);
    let a_blue = region_dominant(&pixels, 900, 70, 70, 95, 125, false);
    let b_red = region_dominant(&pixels, 900, 345, 70, 375, 125, true);
    let b_blue = region_dominant(&pixels, 900, 345, 70, 375, 125, false);
    assert!(
        (a_red && !a_blue && b_blue && !b_red) || (a_blue && !a_red && b_red && !b_blue),
        "two particles must sample opposite texture halves regardless of U direction"
    );
}

/// 双钟独立时间线：A 钟定值 5.0（红）＋B 钟定值 2.0（蓝），同几何不同速。
/// 同速 10px/s 下 A 中心 x=50、B 中心 x=20；断言各就各位且不串台＋各 1 dc。
/// 单共享 uniform 永远做不出此布局（TEMP 桥时代两 quad 会重叠），故为本轮回归核心。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_dual_clock_independent_timelines() {
    fn region_dominant(
        pixels: &[u8],
        w: u32,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        want_red: bool,
    ) -> bool {
        // 相对判定：vireo 调色板非纯色（RED=(0.9,0.16,0.22)），经 sRGB 输出后
        // 绝对阈值失效（如蓝的 g 通道约 179）。相对差 40 足以区分红/蓝/底。
        let mut hit = 0u32;
        let mut total = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let idx = ((y * w + x) * 4) as usize;
                let (r, g, b) = (
                    pixels[idx] as u16,
                    pixels[idx + 1] as u16,
                    pixels[idx + 2] as u16,
                );
                total += 1;
                if want_red {
                    if r > g + 40 && r > b + 40 {
                        hit += 1;
                    }
                } else if b > r + 40 && b > g + 40 {
                    hit += 1;
                }
            }
        }
        hit * 2 > total
    }

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);
    let clock_a = gpu.create_clock();
    let clock_b = gpu.create_clock();
    assert!(gpu.set_clock_time(clock_a, 5.0));
    assert!(gpu.set_clock_time(clock_b, 2.0));

    let mut pool_a = ParticlePool::new();
    pool_a.spawn(Particle {
        pos: [0.0, 350.0],
        vel: [10.0, 0.0],
        size: [5.0, 5.0],
        rotation: 0.0,
        damping: 0.0,
        gravity: [0.0, 0.0],
        size_mul_end: 1.0,
        atlas: None,
        fade_mid: None,
        color_end: None,
        color: RED,
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        birth: 0.0,
        life: 100.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });
    let mut pool_b = ParticlePool::new();
    pool_b.spawn(Particle {
        pos: [0.0, 350.0],
        vel: [10.0, 0.0],
        size: [5.0, 5.0],
        rotation: 0.0,
        damping: 0.0,
        gravity: [0.0, 0.0],
        size_mul_end: 1.0,
        atlas: None,
        fade_mid: None,
        color_end: None,
        color: BLUE,
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        birth: 0.0,
        life: 100.0,
        fade_in: 0.0,
        fade_out: 0.0,
        seed: 0.0,
    });

    let mut batch_a = DrawBatch::new();
    batch_a.particle_clock = clock_a;
    draw_particles(&mut batch_a, &pool_a);
    let mut batch_b = DrawBatch::new();
    batch_b.particle_clock = clock_b;
    draw_particles(&mut batch_b, &pool_b);

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&batch_a, &batch_b]);
    assert_eq!(
        canvas.last_draw_calls(),
        2,
        "两批不同钟，各 1 dc（不同 bind group 不合并）"
    );
    let pixels = canvas.read_pixels();
    // A 中心 (50,350)：x 47..53 红；B 中心 (20,350)：x 17..23 蓝
    assert!(
        region_dominant(&pixels, 900, 47, 347, 53, 353, true),
        "A 钟粒子应在其时间线位置显红"
    );
    assert!(
        !region_dominant(&pixels, 900, 47, 347, 53, 353, false),
        "A 区不应串入 B 的蓝"
    );
    assert!(
        region_dominant(&pixels, 900, 17, 347, 23, 353, false),
        "B 钟粒子应在其时间线位置显蓝"
    );
    assert!(
        !region_dominant(&pixels, 900, 17, 347, 23, 353, true),
        "B 区不应串入 A 的红"
    );
}

/// 时钟注册表语义（零渲染，纯注册表数学）：定值精确、暂停冻结 bit 精确、
/// 恢复不跳变、destroy/stale 代数、0 号保护、槽位复用新代数。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn clock_registry_semantics() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));

    // 默认钟走时＋定值精确
    assert!(gpu.clock_time(ClockIndex::ZERO) >= 0.0);
    assert!(gpu.set_clock_time(ClockIndex::ZERO, 123.0));
    assert!((gpu.clock_time(ClockIndex::ZERO) - 123.0).abs() < 0.01);

    // 暂停冻结（elapsed×0 恒 0，bit 精确）；恢复不跳变
    let c = gpu.create_clock();
    assert!(gpu.set_clock_scale(c, 0.0));
    let frozen_a = gpu.clock_time(c);
    let frozen_b = gpu.clock_time(c);
    assert_eq!(frozen_a, frozen_b);
    assert!(gpu.set_clock_scale(c, 1.0));
    assert!((gpu.clock_time(c) - frozen_a).abs() < 0.01);

    // destroy 语义：一次成功，重复/0号/stale 全 false
    assert!(gpu.destroy_clock(c));
    assert!(!gpu.destroy_clock(c));
    assert!(!gpu.destroy_clock(ClockIndex::ZERO));
    assert!(!gpu.set_clock_scale(c, 1.0));
    assert!(!gpu.set_clock_time(c, 0.0));
    // 野 handle 回落（warn 一条，不 panic；回落值即 0 号钟时间）
    let fallback = gpu.clock_time(c);
    assert!((fallback - gpu.clock_time(ClockIndex::ZERO)).abs() < 0.01);

    // 槽位复用＋新代数
    let c2 = gpu.create_clock();
    assert_ne!(c, c2);
    assert!(gpu.set_clock_scale(c2, 2.0));
    assert!(gpu.set_clock_time(c2, 7.0));
}

/// 粒子混合路径像素回归：重叠红＋绿粒子，alpha 版只留顶层绿、additive 版黄。
/// （形状版由 `blend_modes_*` 覆盖；此处专测粒子 instance 管线选管线正确。）
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn particle_blend_overlap_path() {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState};
    const ADD: BlendState = BlendState {
        color: BlendComponent {
            src_factor: BlendFactor::SrcAlpha,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        },
    };
    fn center(px: &[u8], w: u32) -> (u32, u32, u32) {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for y in 190..210 {
            for x in 190..210 {
                let i = ((y * w + x) * 4) as usize;
                r += px[i] as u32;
                g += px[i + 1] as u32;
                b += px[i + 2] as u32;
                n += 1;
            }
        }
        (r / n, g / n, b / n)
    }
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 400, 400);
    const PR: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    const PG: Color = Color::new(0.0, 1.0, 0.0, 1.0);
    let mk = |blend: Option<BlendState>| {
        let mut pool = ParticlePool::new();
        for (c, q) in [(PR, [0.0, 0.0, 1.0, 1.0]), (PG, [0.0, 0.0, 1.0, 1.0])] {
            pool.spawn(Particle {
                pos: [200.0, 200.0],
                vel: [0.0, 0.0],
                size: [60.0, 60.0],
                rotation: 0.0,
                damping: 0.0,
                gravity: [0.0, 0.0],
                size_mul_end: 1.0,
                atlas: None,
                fade_mid: None,
                color_end: None,
                color: c,
                uv_rect: q,
                birth: 0.0,
                life: -1.0,
                fade_in: 0.0,
                fade_out: 0.0,
                seed: 0.0,
            });
        }
        let mut b = DrawBatch::new();
        if let Some(bl) = blend {
            b.set_blend_state(bl);
        }
        draw_particles(&mut b, &pool);
        b
    };
    let ba = mk(None);
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&ba]);
    let ca = center(&canvas.read_pixels(), 400);
    let bb = mk(Some(ADD));
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&bb]);
    let cb = center(&canvas.read_pixels(), 400);
    println!("[pblend] alpha={ca:?} additive={cb:?}");
    assert!(ca.1 > 200 && ca.0 < 120, "alpha 应只留顶层绿：{ca:?}");
    assert!(cb.0 > 200 && cb.1 > 200, "additive 应黄：{cb:?}");
}

/// 混合三档像素：红＋绿交叠。
/// additive（SrcAlpha/One）中心黄；multiply（Dst/Zero）白×红得红；
/// screen（One/OneMinusSrc）蓝底＋红得品红；a=0.5 加色是 SrcAlpha/One 的证据
///（One/One 会给出纯黄）。默认 batch 与显式 Alpha 逐字节相等。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn blend_modes_additive_multiply_screen() {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState};
    const ADD: BlendState = BlendState {
        color: BlendComponent {
            src_factor: BlendFactor::SrcAlpha,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        },
    };
    const MULT: BlendState = BlendState {
        color: BlendComponent {
            src_factor: BlendFactor::Dst,
            dst_factor: BlendFactor::Zero,
            operation: BlendOperation::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::Zero,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        },
    };
    const SCREEN: BlendState = BlendState {
        color: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::OneMinusSrc,
            operation: BlendOperation::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            operation: BlendOperation::Add,
        },
    };
    fn mean(px: &[u8], w: u32, x0: u32, x1: u32, y0: u32, y1: u32) -> (u32, u32, u32) {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * w + x) * 4) as usize;
                r += px[i] as u32;
                g += px[i + 1] as u32;
                b += px[i + 2] as u32;
                n += 1;
            }
        }
        (r / n, g / n, b / n)
    }

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 1000, 700);
    // 纯色（命名色非纯色：RED 带绿蓝、GREEN 带蓝，断言用裸值）。
    const PR: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    const PG: Color = Color::new(0.0, 1.0, 0.0, 1.0);
    const PB: Color = Color::new(0.0, 0.0, 1.0, 1.0);
    let mut b = DrawBatch::new();
    // additive：红(40,40)＋绿(120,80)，交叠中心 (150..170, 100..120) 应黄
    b.set_blend_state(ADD);
    draw_rectangle(&mut b, Pos::new(40.0, 40.0), 160.0, 120.0, Some(PR));
    draw_rectangle(&mut b, Pos::new(120.0, 80.0), 160.0, 120.0, Some(PG));
    // a=0.5 半透明红＋不透明绿：(127,255,0)；One/One 会给出 (255,255,0)
    draw_rectangle(
        &mut b,
        Pos::new(340.0, 40.0),
        160.0,
        120.0,
        Some(Color::new(1.0, 0.0, 0.0, 0.5)),
    );
    draw_rectangle(&mut b, Pos::new(420.0, 80.0), 160.0, 120.0, Some(PG));
    // multiply：白底（alpha）＋红（mult）→ 红（黑底上 mult 恒黑，无意义）
    draw_rectangle(&mut b, Pos::new(620.0, 40.0), 160.0, 120.0, Some(WHITE));
    b.set_blend_state(MULT);
    draw_rectangle(&mut b, Pos::new(700.0, 80.0), 160.0, 120.0, Some(PR));
    // screen：蓝＋红 → 品红
    b.set_blend_state(SCREEN);
    draw_rectangle(&mut b, Pos::new(40.0, 300.0), 160.0, 120.0, Some(PB));
    draw_rectangle(&mut b, Pos::new(120.0, 340.0), 160.0, 120.0, Some(PR));
    // alpha 对照：默认 vs 显式（同几何，不同 x）
    b.clear_blend_state();
    draw_rectangle(&mut b, Pos::new(340.0, 300.0), 160.0, 120.0, Some(PR));
    draw_rectangle(&mut b, Pos::new(420.0, 340.0), 160.0, 120.0, Some(PG));
    b.set_blend_state(BlendState::ALPHA_BLENDING);
    draw_rectangle(&mut b, Pos::new(620.0, 300.0), 160.0, 120.0, Some(PR));
    draw_rectangle(&mut b, Pos::new(700.0, 340.0), 160.0, 120.0, Some(PG));

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let px = canvas.read_pixels();
    let add = mean(&px, 1000, 150, 170, 100, 120);
    let half = mean(&px, 1000, 450, 470, 100, 120);
    let mult = mean(&px, 1000, 730, 750, 100, 120);
    let scr = mean(&px, 1000, 150, 170, 360, 380);
    let adef = mean(&px, 1000, 450, 470, 360, 380);
    let aexp = mean(&px, 1000, 730, 750, 360, 380);
    println!(
        "[blend] add={add:?} half={half:?} mult={mult:?} scr={scr:?} adef={adef:?} aexp={aexp:?}"
    );
    assert!(
        add.0 > 200 && add.1 > 200 && add.2 < 120,
        "additive 红＋绿应黄：{add:?}"
    );
    // 回读是 sRGB 编码：linear 0.5 → 约 184（One/One 会给出 255，此即 SrcAlpha/One 证据）
    assert!(
        half.0 > 160 && half.0 < 215 && half.1 > 200 && half.2 < 120,
        "a=0.5 加色应 linear(0.5,1,0)：{half:?}"
    );
    assert!(
        mult.0 > 200 && mult.1 < 120 && mult.2 < 120,
        "multiply 白×红应红：{mult:?}"
    );
    assert!(
        scr.0 > 200 && scr.2 > 200 && scr.1 < 120,
        "screen 蓝＋红应品红：{scr:?}"
    );
    assert!(
        adef.1 > 200 && adef.0 < 120,
        "alpha 对照应只留顶层绿：{adef:?}"
    );
    assert_eq!(
        adef, aexp,
        "默认与显式 Alpha 应逐字节相等：{adef:?} vs {aexp:?}"
    );
}

/// blend 常量管道：白 quad＋color=(Constant,Zero)＋红常量 → 红输出。
/// constant 不同步会输出黑。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn blend_constant_plumbing() {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState};
    const CONST_RED: BlendState = BlendState {
        color: BlendComponent {
            src_factor: BlendFactor::Constant,
            dst_factor: BlendFactor::Zero,
            operation: BlendOperation::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::Zero,
            operation: BlendOperation::Add,
        },
    };
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 400, 300);
    let mut b = DrawBatch::new();
    b.set_blend_state(CONST_RED);
    b.set_blend_constant(wgpu::Color {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    });
    draw_rectangle(&mut b, Pos::new(40.0, 40.0), 160.0, 120.0, Some(WHITE));
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let px = canvas.read_pixels();
    let (mut r, mut g, mut n) = (0u32, 0u32, 0u32);
    for y in 80..120 {
        for x in 80..160 {
            let i = ((y * 400 + x) * 4) as usize;
            r += px[i] as u32;
            g += px[i + 1] as u32;
            n += 1;
        }
    }
    let (rm, gm) = (r / n, g / n);
    println!("[blend-const] r={rm} g={gm}");
    assert!(
        rm > 200 && gm < 120,
        "Constant 红常量应输出红：r={rm} g={gm}"
    );
}

/// 文字混合冒烟：蓝底红字，alpha 版红占优、additive 版品红占优（多数票，抗抗锯齿边）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn blend_additive_text() {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState};
    const ADD: BlendState = BlendState {
        color: BlendComponent {
            src_factor: BlendFactor::SrcAlpha,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        },
        alpha: BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        },
    };
    fn votes(px: &[u8], w: u32) -> (u32, u32) {
        let (mut red, mut mag) = (0u32, 0u32);
        for y in 60..130 {
            for x in 60..160 {
                let i = ((y * w + x) * 4) as usize;
                let (r, g, bl) = (px[i], px[i + 1], px[i + 2]);
                if r > 150 && g < 120 && bl < 120 {
                    red += 1;
                } else if r > 150 && bl > 150 && g < 120 {
                    mag += 1;
                }
            }
        }
        (red, mag)
    }
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 400, 300);
    let blue = Color::new(0.0, 0.0, 1.0, 1.0);
    let mut ba = DrawBatch::new();
    draw_text(
        &mut ba.texts,
        "RR",
        Pos::new(60.0, 60.0),
        TextDef::default().font_size(48.0),
        TextOverride::from_color(RED),
    );
    canvas.draw(Some(blue), &[&ba]);
    let (ared, amag) = votes(&canvas.read_pixels(), 400);
    let mut bb = DrawBatch::new();
    bb.set_blend_state(ADD);
    draw_text(
        &mut bb.texts,
        "RR",
        Pos::new(60.0, 60.0),
        TextDef::default().font_size(48.0),
        TextOverride::from_color(RED),
    );
    canvas.draw(Some(blue), &[&bb]);
    let (bred, bmag) = votes(&canvas.read_pixels(), 400);
    println!("[blend-text] alpha red={ared} mag={amag}; add red={bred} mag={bmag}");
    assert!(ared > amag, "alpha 版应红占优：red={ared} mag={amag}");
    assert!(bmag > bred, "additive 版应品红占优：red={bred} mag={bmag}");
}

/// 同一张图两种采样：左红右蓝 8x8，三 quad 同用 uv `[0,0,2,1]`。
/// A＝`with_address_mode(Repeat)`，B＝默认 Clamp，C＝`set_address_mode(Repeat)`。
/// 断言 A/C 右半 wrap 回红（与左半同色）、B 右半钳成蓝；三段不同 bind group → 3 dc。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn texture_repeat_wraps() {
    fn col(pixels: &[u8], w: u32, x0: u32, x1: u32, y0: u32, y1: u32) -> (u32, u32, u32) {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * w + x) * 4) as usize;
                r += pixels[i] as u32;
                g += pixels[i + 1] as u32;
                b += pixels[i + 2] as u32;
                n += 1;
            }
        }
        (r / n, g / n, b / n)
    }
    fn is_red(c: (u32, u32, u32)) -> bool {
        c.0 > 150 && c.1 < 120 && c.2 < 120
    }
    fn is_blue(c: (u32, u32, u32)) -> bool {
        c.2 > 150 && c.0 < 120 && c.1 < 120
    }

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);
    let mut tex_bytes = Vec::with_capacity(8 * 8 * 4);
    for _ in 0..8 {
        for x in 0..8 {
            if x < 4 {
                tex_bytes.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                tex_bytes.extend_from_slice(&[0, 0, 255, 255]);
            }
        }
    }
    let tex = Texture::from_rgba(8, 8, &tex_bytes, &gpu);
    let tiled = tex.with_address_mode(&gpu, wgpu::AddressMode::Repeat);
    assert_eq!((tiled.width, tiled.height), (8, 8));
    let mut tex2 = Texture::from_rgba(8, 8, &tex_bytes, &gpu);
    tex2.set_address_mode(&gpu, wgpu::AddressMode::Repeat);

    let mut b = DrawBatch::new();
    // A：repeat（with_），x 40..200
    b.set_texture(Some(&tiled));
    b.set_uv(0.0, 0.0, 2.0, 1.0);
    draw_rectangle(&mut b, Pos::new(40.0, 40.0), 160.0, 120.0, Some(WHITE));
    // B：clamp，x 250..410
    b.set_texture(Some(&tex));
    b.set_uv(0.0, 0.0, 2.0, 1.0);
    draw_rectangle(&mut b, Pos::new(250.0, 40.0), 160.0, 120.0, Some(WHITE));
    // C：repeat（setter），x 460..620
    b.set_texture(Some(&tex2));
    b.set_uv(0.0, 0.0, 2.0, 1.0);
    draw_rectangle(&mut b, Pos::new(460.0, 40.0), 160.0, 120.0, Some(WHITE));

    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    assert_eq!(canvas.last_draw_calls(), 3, "三段不同 bind group 应 3 dc");
    let px = canvas.read_pixels();
    // 每 quad 采左平坦（u≈0.15..0.3，红）与右平坦（u≈1.15..1.3；repeat 回红 / clamp 钳蓝）；
    // 带宽 12px（≈1.2 texel），离 u=0.5/1.0/1.5 缝线与几何边都 ≥1 texel
    let (al, ar) = (
        col(&px, 900, 52, 64, 90, 110),
        col(&px, 900, 132, 144, 90, 110),
    );
    let (bl, br) = (
        col(&px, 900, 262, 274, 90, 110),
        col(&px, 900, 372, 392, 90, 110),
    );
    let (cl, cr) = (
        col(&px, 900, 472, 484, 90, 110),
        col(&px, 900, 552, 564, 90, 110),
    );
    println!("[repeat] A={al:?}/{ar:?} B={bl:?}/{br:?} C={cl:?}/{cr:?}");
    assert!(
        is_red(al) && is_red(ar),
        "with_ Repeat 右半应 wrap 回红：{al:?}/{ar:?}"
    );
    assert!(
        is_red(bl) && is_blue(br),
        "Clamp 右半应钳成蓝：{bl:?}/{br:?}"
    );
    assert!(
        is_red(cl) && is_red(cr),
        "setter Repeat 右半应 wrap 回红：{cl:?}/{cr:?}"
    );
}

// ---------------------------------------------------------------------------
// COLRv1：emoji 端到端 + atlas grow 回归
// ---------------------------------------------------------------------------
//
// 背景见 `.opencode/android_colrv1_plan.md`：swash 0.2.10 按 COLR v0 布局读表头，
// Android 12+ 的 `NotoColorEmoji.ttf`（COLRv1）交回 `0×0` 被静默丢弃。
// vireo 在 swash 交回空位图时用自研的 `skrifa + zeno` 路径二次尝试。

/// 这台机器上可用的 COLRv1 字体（路径，pinned 家族名）。
///
/// `seguiemj.ttf` 本身就是 COLRv1，且它的 v0 记录不覆盖测试字形 —— swash 对它们
/// 返回 `None`，所以桌面同样走 COLRv1 路径。没有可用字体时调用方直接 return（跳过）。
fn colr_v1_test_font() -> Option<(&'static str, &'static str)> {
    const CANDIDATES: &[(&str, &str)] = &[
        (r"C:\Windows\Fonts\seguiemj.ttf", "Segoe UI Emoji"),
        (
            "/System/Library/Fonts/Apple Color Emoji.ttc",
            "Apple Color Emoji",
        ),
        (
            "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
            "Noto Color Emoji",
        ),
    ];
    CANDIDATES
        .iter()
        .copied()
        .find(|(path, _)| std::path::Path::new(path).exists())
}

/// COLRv1 端到端：`draw_text` 画 emoji → 图集 → shader → 像素回读，可见。
///
/// 判据是「空白 vs 非空白」：没有 COLRv1 路径时，shaping 能排到字形、
/// swash 交回 `0×0`、`SkipRasterization` 静默丢弃 → 区域全空。
/// 显式 pin 家族，避免回退到其他字体的单色字形造成假阳性。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn colr_v1_emoji_visible_end_to_end() {
    let Some((path, family)) = colr_v1_test_font() else {
        eprintln!("skipping: no COLRv1 font on this machine");
        return;
    };

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    gpu.load_font_file(path).expect("load COLRv1 test font");
    let canvas = OffscreenCanvas::new(&gpu, 400, 300);

    let def = TextDef::default()
        .font_size(64.0)
        .with_family(Family::Name(family));
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        "😀",
        Pos::new(100.0, 200.0),
        def,
        TextOverride::from_color(WHITE),
    );

    canvas.draw(Some(BLACK), &[&b]);
    let pixels = canvas.read_pixels();
    assert!(
        region_has_color(&pixels, 400, 100, 120, 200, 220),
        "COLRv1 emoji 应可见（若回退到旧路径，该区域全空）"
    );
}

/// COLRv1 + atlas grow：逼图集扩容后，原有字形仍可见。
///
/// `InnerAtlas::grow` 重建纹理并把每个缓存字形重光栅化写回去。旧代码对此不做任何
/// 校验 —— 重光栅化失败或尺寸不符时写入变成 no-op，字形指向空白区域且无日志。
/// 本测试先画一个 emoji（进图集），再用一批又大又多的不同字形逼出至少一次 grow，
/// 最后重画第一个 emoji 并验证仍可见。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn colr_v1_atlas_grow_keeps_glyphs() {
    let Some((path, family)) = colr_v1_test_font() else {
        eprintln!("skipping: no COLRv1 font on this machine");
        return;
    };

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    gpu.load_font_file(path).expect("load COLRv1 test font");
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    // 8 个经单元测试确认走 COLRv1 的字形（见 src/glyphon/colr.rs）。
    const EMOJI: &[char] = &['😀', '🍑', '🌈', '🦄', '🎵', '🍭', '👀', '🐛'];

    let probe = |canvas: &OffscreenCanvas| {
        let def = TextDef::default()
            .font_size(64.0)
            .with_family(Family::Name(family));
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "😀",
            Pos::new(100.0, 200.0),
            def,
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(BLACK), &[&b]);
        region_has_color(&canvas.read_pixels(), 900, 100, 120, 200, 220)
    };

    assert!(probe(&canvas), "grow 前：emoji 应可见");

    // 洪水：8 字形 × 3 尺寸（64/128/200px）= 24 个不同的大号彩色字形，
    // 同一帧进图集，总量远超初始 256² color atlas，必然触发 grow。
    // （全部同 generation，LRU 驱逐不适用，只能 grow。）
    let mut flood = DrawBatch::new();
    for (row, size) in [64.0, 128.0, 200.0].iter().enumerate() {
        let def = TextDef::default()
            .font_size(*size)
            .with_family(Family::Name(family));
        for (col, ch) in EMOJI.iter().enumerate() {
            let mut s = String::new();
            s.push(*ch);
            draw_text(
                &mut flood.texts,
                &s,
                Pos::new(20.0 + col as f32 * 100.0, 100.0 + row as f32 * 220.0),
                def.clone(),
                TextOverride::from_color(WHITE),
            );
        }
    }
    canvas.draw(Some(BLACK), &[&flood]);

    assert!(
        probe(&canvas),
        "grow 后：emoji 仍应可见（若重光栅化失败，该区域变空）"
    );
}

/// COLRv1 首帧 vs 稳态帧耗时。
///
/// COLRv1 光栅化只在图集 miss（首见某个 glyph+字号组合）时发生；
/// 稳态时全部命中缓存，理论上跟纯文本一样快。
/// 测量方法：同一批 8 个 emoji × 13 个字号 = 104 个 cache key，
/// 首帧跑一次（104 次冷光栅化），再跑一次（全命中）。
/// 对照组：同形状的 8 个 ASCII 字母 × 13 字号（走 swash 轮廓路径）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn colr_v1_perf_cold_vs_warm() {
    let Some((path, family)) = colr_v1_test_font() else {
        eprintln!("skipping: no COLRv1 font on this machine");
        return;
    };

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    gpu.load_font_file(path).expect("load COLRv1 test font");
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    // 8 个确认走 COLRv1 的字形 × 13 个字号 = 104 个独立 cache key。
    const EMOJI: &[char] = &['😀', '🍑', '🌈', '🦄', '🎵', '🍭', '👀', '🐛'];
    const SIZES: &[f32] = &[
        16.0, 20.0, 24.0, 28.0, 32.0, 36.0, 40.0, 48.0, 56.0, 64.0, 72.0, 80.0, 96.0,
    ];

    let build_colr = |b: &mut DrawBatch| {
        for (row, ch) in EMOJI.iter().enumerate() {
            for (size_i, size) in SIZES.iter().enumerate() {
                let def = TextDef::default()
                    .font_size(*size)
                    .with_family(Family::Name(family));
                let mut s = String::new();
                s.push(*ch);
                draw_text(
                    &mut b.texts,
                    &s,
                    Pos::new(10.0 + size_i as f32 * 60.0, 20.0 + row as f32 * 40.0),
                    def,
                    TextOverride::from_color(WHITE),
                );
            }
        }
    };

    // 对照组：8 个 ASCII 字符 × 同样 13 个字号，走 swash 轮廓路径。
    const ASCII: &[char] = &['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'];
    let build_ascii = |b: &mut DrawBatch| {
        for (row, ch) in ASCII.iter().enumerate() {
            for (size_i, size) in SIZES.iter().enumerate() {
                let def = TextDef::default()
                    .font_size(*size)
                    .with_family(Family::Name(family));
                let mut s = String::new();
                s.push(*ch);
                draw_text(
                    &mut b.texts,
                    &s,
                    Pos::new(10.0 + size_i as f32 * 60.0, 20.0 + row as f32 * 40.0),
                    def,
                    TextOverride::from_color(WHITE),
                );
            }
        }
    };

    let mut cold = Vec::new();
    let mut warm = Vec::new();
    let mut ascii_cold = Vec::new();
    let mut ascii_warm = Vec::new();

    // emoji 冷/热
    for _ in 0..3 {
        let mut b = DrawBatch::new();
        build_colr(&mut b);
        let t = Instant::now();
        canvas.draw(Some(BLACK), &[&b]);
        loop {
            if let Ok(wgpu::PollStatus::QueueEmpty) = canvas.gpu().device.poll(wgpu::PollType::Poll)
            {
                break;
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if cold.is_empty() {
            cold.push(ms);
        } else {
            warm.push(ms);
        }
    }

    // ASCII 冷/热
    for _ in 0..3 {
        let mut b = DrawBatch::new();
        build_ascii(&mut b);
        let t = Instant::now();
        canvas.draw(Some(BLACK), &[&b]);
        loop {
            if let Ok(wgpu::PollStatus::QueueEmpty) = canvas.gpu().device.poll(wgpu::PollType::Poll)
            {
                break;
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ascii_cold.is_empty() {
            ascii_cold.push(ms);
        } else {
            ascii_warm.push(ms);
        }
    }

    let cold_avg = cold.iter().sum::<f64>() / cold.len() as f64;
    let warm_avg = warm.iter().sum::<f64>() / warm.len() as f64;
    let ascii_cold_avg = ascii_cold.iter().sum::<f64>() / ascii_cold.len() as f64;
    let ascii_warm_avg = ascii_warm.iter().sum::<f64>() / ascii_warm.len() as f64;

    println!(
        "  COLRv1 emoji x104: cold {:.3} ms, warm {:.3} ms",
        cold_avg, warm_avg
    );
    println!(
        "  ASCII x104:        cold {:.3} ms, warm {:.3} ms",
        ascii_cold_avg, ascii_warm_avg
    );

    // 稳态时 COLRv1 应退化到跟 ASCII 同量级（只是 draw 一批贴图 quad）。
    // 若 warm 是 ascii_warm 的 10 倍以上，说明 atlas 缓存没生效或 COLRv1 每帧都重跑。
    assert!(
        warm_avg < ascii_warm_avg * 10.0,
        "COLRv1 稳态耗时 {warm_avg:.3}ms 异常（ASCII {ascii_warm_avg:.3}ms 的 10 倍以上）—— \
         atlas 缓存可能没生效"
    );

    // 冷热差异即 COLRv1 光栅化成本（104 个字形的总和）。
    // 给一个宽松上限：即使最坏情况也不该是秒级。
    assert!(
        cold_avg < 500.0,
        "104 个 COLRv1 字形冷光栅化应 < 500ms，实际 {cold_avg:.3}ms"
    );
}

/// P1 验收：深度预通道 + 着色通道，跨 Batch 深度保留。
///
/// Pass 1（深度预通道）：红色左半边，`depth_write=true`，写入 z=0。
/// Pass 2（着色通道）：蓝色全屏，`depth_write=false`、`compare=Less`，z=0。
///   - 左半边：已有深度 0，`0 < 0` 不通过，红保留。
///   - 右半边：预通道未写（清空值 1.0），`0 < 1.0` 通过，蓝覆盖。
/// 对照组：两 pass 均不挂载 DS → Pass 2 无深度测试，全部覆为蓝。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn depth_prepass_and_shading_preserve_depth() {
    const FRAG: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(in.color.rgb, in.color.a);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 400, 300);

    // pipeline 会按材质的 depth_write/depth_compare 做 key，故两 pass 用两个材质。
    let material_red = gpu.create_material(FRAG).expect("material pipelines");
    material_red.set_depth_write(true);
    let material_blue = gpu.create_material(FRAG).expect("material pipelines");
    material_blue.set_depth_write(false);
    material_blue.set_depth_compare(wgpu::CompareFunction::Less);

    let ops_clear = DepthStencilOps {
        depth: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
        }),
        stencil: None,
    };
    let ops_load = DepthStencilOps {
        depth: Some(wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        }),
        stencil: None,
    };

    // ---------- Pass 1：深度预通道，红色左半边（z=0 写入）----------
    // 画布格式为 sRGB：用纯色（1/0）避免 sRGB 编码后阈值判断失真。
    let pure_red = Color::new(1.0, 0.0, 0.0, 1.0);
    let mut b1 = DrawBatch::new();
    b1.set_custom_material(Some(material_red.clone()));
    draw_rectangle(&mut b1, Pos::new(0.0, 0.0), 200.0, 300.0, Some(pure_red));
    canvas.draw_with_depth_stencil(
        Some(BLACK),
        &[&b1],
        Some(DepthStencilTarget::Engine(ops_clear)),
    );

    // ---------- Pass 2：着色通道，蓝色全屏（z=0，Less 测试）----------
    let pure_blue = Color::new(0.0, 0.0, 1.0, 1.0);
    let mut b2 = DrawBatch::new();
    b2.set_custom_material(Some(material_blue.clone()));
    draw_rectangle(&mut b2, Pos::new(0.0, 0.0), 400.0, 300.0, Some(pure_blue));
    canvas.draw_with_depth_stencil(None, &[&b2], Some(DepthStencilTarget::Engine(ops_load)));

    let pixels = canvas.read_pixels();
    assert!(
        region_is_red(&pixels, 400, 20, 20, 100, 100),
        "深度预通道：左半边 red 写入 z=0，Pass 2 Less 测试应剔除"
    );
    assert!(
        region_is_blue(&pixels, 400, 300, 20, 380, 100),
        "深度预通道：右半边无已写深度，Pass 2 blue z=0 < 1.0 应覆盖"
    );

    // ---------- 对照：两 pass 均不挂载 DS，Pass 2 无深度测试应全部覆盖 ----------
    let mut b3 = DrawBatch::new();
    b3.set_custom_material(Some(material_red));
    draw_rectangle(&mut b3, Pos::new(0.0, 0.0), 200.0, 300.0, Some(pure_red));
    canvas.draw_with_depth_stencil(Some(BLACK), &[&b3], None);

    let mut b4 = DrawBatch::new();
    b4.set_custom_material(Some(material_blue));
    draw_rectangle(&mut b4, Pos::new(0.0, 0.0), 400.0, 300.0, Some(pure_blue));
    canvas.draw_with_depth_stencil(None, &[&b4], None);

    let pixels = canvas.read_pixels();
    assert!(
        region_is_blue(&pixels, 400, 20, 20, 100, 100),
        "无 DS 对照：Pass 2 不做深度测试，应全部覆为蓝"
    );
}

/// MRT（P4）：一次 pass 写两个颜色目标，两个目标各自可回读不同颜色。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn mrt_writes_distinct_colors_to_both_targets() {
    const MATERIAL_WGSL: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) aux: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    var out: MaterialOutput;
    out.color = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    out.aux = vec4<f32>(0.0, 0.0, 1.0, 1.0);
    return out;
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 200, 200);
    let aux = OffscreenCanvas::new(&gpu, 200, 200);
    let material = gpu
        .create_material(MATERIAL_WGSL)
        .expect("MRT material pipelines");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material));
    draw_rectangle(&mut b, Pos::new(20.0, 20.0), 160.0, 160.0, Some(WHITE));

    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    base.draw_mrt(Some(BLACK), &[aux_target], &[&b]);

    let base_pixels = base.read_pixels();
    assert!(
        region_is_red(&base_pixels, 200, 40, 40, 160, 160),
        "target 0 应写入 MRT shader 的 color 输出（红）"
    );
    let aux_pixels = aux.read_pixels();
    assert!(
        region_is_blue(&aux_pixels, 200, 40, 40, 160, 160),
        "target 1 应写入 MRT shader 的 aux 输出（蓝）"
    );
    println!(
        "  MRT: target0=red target1=blue draw_calls={}",
        base.last_draw_calls()
    );
}

/// P5：compute dispatch 写 storage buffer，CPU 回读校验。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn compute_dispatch_writes_storage_buffer() {
    const WGSL: &str = r#"
@group(0) @binding(0) var<storage, read_write> data: array<u32>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= arrayLength(&data) {
        return;
    }
    data[i] = i * i + 7u;
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));

    let n: u32 = 64;
    let bytes = (n * 4) as u64;
    let data_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("compute data"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("compute readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let bgl = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("compute bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: data_buf.as_entire_binding(),
        }],
    });
    let kernel = gpu
        .create_compute_pipeline(WGSL, "cs_main", &[(0, &bgl)])
        .expect("compute pipeline");
    gpu.dispatch(&kernel, &[(0, &bg)], [1, 1, 1]);

    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(&data_buf, 0, &staging, 0, bytes);
    gpu.queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = gpu.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    rx.recv().expect("map callback").expect("map ok");
    let mapped = slice.get_mapped_range().expect("mapped range");
    let values: Vec<u32> = mapped
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    drop(mapped);
    staging.unmap();

    for (i, v) in values.iter().enumerate() {
        assert_eq!(*v, i as u32 * i as u32 + 7, "compute 输出第 {i} 项不符");
    }
    println!("  compute: {} 项 storage buffer 写入正确", values.len());
}

/// P5：compute 写 storage texture，随后 render pass 采样该纹理可见。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn compute_storage_texture_visible_to_render() {
    const WGSL: &str = r#"
@group(0) @binding(1) var out_tex: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = i32(gid.x);
    let y = i32(gid.y);
    var color = vec4<f32>(0.0, 0.0, 1.0, 1.0);
    if gid.x < 100u {
        color = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    }
    textureStore(out_tex, vec2<i32>(x, y), color);
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let size = 200u32;

    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("compute storage texture"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    let display_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("compute display bg"),
        layout: &gpu.texture_bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&gpu.default_sampler),
            },
        ],
    });
    let field = Texture {
        texture: tex,
        view: view.clone(),
        bind_group: display_bg,
        width: size,
        height: size,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };

    let bgl = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute tex bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            }],
        });
    let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("compute tex bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::TextureView(&view),
        }],
    });
    let kernel = gpu
        .create_compute_pipeline(WGSL, "cs_main", &[(0, &bgl)])
        .expect("compute texture pipeline");
    gpu.dispatch(
        &kernel,
        &[(0, &bg)],
        [size.div_ceil(8), size.div_ceil(8), 1],
    );

    let canvas = OffscreenCanvas::new(&gpu, size, size);
    let mut b = DrawBatch::new();
    b.set_texture(Some(&field));
    draw_rectangle(
        &mut b,
        Pos::new(0.0, 0.0),
        size as f32,
        size as f32,
        Some(WHITE),
    );
    canvas.draw(Some(BLACK), &[&b]);

    let pixels = canvas.read_pixels();
    assert!(
        region_is_red(&pixels, size, 20, 20, 80, 180),
        "compute 写入的左半边应可见（红）"
    );
    assert!(
        region_is_blue(&pixels, size, 120, 20, 180, 180),
        "compute 写入的右半边应可见（蓝）"
    );
    println!("  compute: storage texture 写入对 render pass 可见");
}

/// `merge_instance_states`：同 batch 内交替 blend 的 SDF 实例按状态重排后可合并 dc。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn merge_instance_states_reduces_draw_calls() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 400, 400);

    let build = |merge: bool| {
        let mut b = DrawBatch::new();
        b.preserve_order = false;
        b.merge_instance_states = merge;
        for i in 0..1000u32 {
            b.set_blend_state(if i % 2 == 0 {
                wgpu::BlendState::ALPHA_BLENDING
            } else {
                wgpu::BlendState::REPLACE
            });
            b.set_position((i % 40) as f32 * 9.0 + 5.0, (i / 40) as f32 * 15.0 + 5.0);
            draw_rectangle(&mut b, Pos::new(0.0, 0.0), 8.0, 8.0, Some(RED));
        }
        b
    };

    let clear = Some(Color::new(0.0, 0.0, 0.0, 1.0));
    let off = build(false);
    canvas.draw(clear, &[&off]);
    let dc_off = canvas.last_draw_calls();
    let on = build(true);
    canvas.draw(clear, &[&on]);
    let dc_on = canvas.last_draw_calls();
    println!("  merge_instance_states: off={dc_off} on={dc_on}");
    assert_eq!(dc_off, 1000, "未重排：交替 blend 每段 1 dc");
    assert_eq!(dc_on, 2, "重排后：两种 blend 各合并为 1 dc");
}

/// `draw_with_inline`：hook 在引擎批次之后、同一个 pass 内绘制（自定义管线覆盖引擎内容）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn offscreen_draw_with_inline_runs_in_engine_pass() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 64, 64);

    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("inline test shader"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}
@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 1.0, 0.0, 1.0);
}
"#
                .into(),
            ),
        });
    let layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("inline test layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("inline test pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: gpu.surface_format(),
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

    let mut b = DrawBatch::new();
    draw_rectangle(
        &mut b,
        Pos::new(0.0, 0.0),
        64.0,
        64.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );

    let mut called = false;
    canvas.draw_with_inline(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b], |pass| {
        called = true;
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    });
    assert!(called, "inline hook 应被执行");

    let pixels = canvas.read_pixels();
    let i = (32 * 64 + 32) * 4;
    assert!(
        pixels[i + 1] > 200 && pixels[i] < 50 && pixels[i + 2] < 50,
        "hook 应与引擎批次在同一 pass 内、覆盖引擎内容，实际 {:?}",
        &pixels[i..i + 4]
    );
}

/// `draw_with_frame`：用户自建 pass，按契约编码引擎批次。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn offscreen_draw_with_frame_creates_own_pass() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 64, 64);

    let mut scene = DrawBatch::new();
    draw_rectangle(
        &mut scene,
        Pos::new(0.0, 0.0),
        32.0,
        64.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );

    let mut contract = None;
    canvas.draw_with_frame(
        Some(Color::new(0.0, 0.0, 0.0, 1.0)),
        &[&scene],
        |frame, encoder| {
            contract = Some((
                frame.sample_count(),
                frame.has_ds(),
                frame.is_empty(),
                frame.target_format(),
            ));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("custom scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &frame.target().color_attachments[0].view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            assert_eq!(frame.encode(&mut pass), 1, "引擎批次应发出 1 个 draw call");
        },
    );

    let (sample_count, has_ds, is_empty, fmt) = contract.expect("回调应执行");
    assert_eq!(sample_count, 1);
    assert!(!has_ds);
    assert!(!is_empty);
    assert_eq!(fmt, gpu.surface_format());
    assert_eq!(canvas.last_draw_calls(), 1);

    let pixels = canvas.read_pixels();
    assert!(
        region_is_red(&pixels, 64, 5, 5, 25, 25),
        "引擎红色矩形应可见"
    );
    assert!(
        region_is_blue(&pixels, 64, 40, 5, 60, 25),
        "自建 pass 的蓝色 clear 应在引擎批次之外保留"
    );
}

/// 嵌套 `prepare_frame` 会 panic（不会静默死锁），且 unwind 后状态可恢复。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn nested_prepare_panics_and_recovers() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 64, 64);

    let mut b = DrawBatch::new();
    draw_rectangle(
        &mut b,
        Pos::new(0.0, 0.0),
        64.0,
        64.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        canvas.draw_with_frame(
            Some(Color::new(0.0, 0.0, 0.0, 1.0)),
            &[&b],
            |_frame, _encoder| {
                canvas.draw_with_inline(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b], |_| {});
            },
        );
    }));
    assert!(res.is_err(), "嵌套 prepare 应 panic");

    // 恢复：guard 在 unwind 时已复位，后续绘制正常。
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let pixels = canvas.read_pixels();
    assert!(region_is_red(&pixels, 64, 20, 20, 44, 44));
}
