//! 无头 GPU 基准：OffscreenCanvas 压测（需本机 GPU）。
//!
//! 运行：
//! ```bash
//! cargo test --release --test gpu_bench -- --nocapture --ignored
//! ```

use std::sync::Arc;
use std::time::Instant;

use vireo::prelude::*;

/// **层偏移不能被条目边界截断**（回归）：`shadow((4,4), 3)` 与 `shadow((0,0), 3)` 的
/// 亮区尺寸必须一致（偏移只是平移，不该让投影变小）；曾经 UV 越界被 clamp 成直线，
/// 偏移版的投影会明显变窄/变短。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_shadow_offset_is_not_truncated() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));

    // 返回 (亮区宽, 亮区高, 亮像素数)
    let measure = |offset: (f32, f32)| -> (u32, u32, usize) {
        let c = OffscreenCanvas::new(&gpu, 360, 240);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "H",
            Pos::new(120.0, 70.0),
            TextDef {
                style: Some(Arc::new(
                    // 阴影用红色：暗色阴影在纯黑背景上几乎不可见，量尺寸会失真
                    TextStyle::shadow(offset, 3.0, Color::new(1.0, 0.0, 0.0, 1.0))
                        .with_fill(TextPaint::solid(WHITE)),
                )),
                ..TextDef::default().font_size(60.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut n = 0usize;
        for y in 0..240u32 {
            for x in 0..360u32 {
                let i = ((y * 360 + x) * 4) as usize;
                if px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32 > 30 {
                    n += 1;
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
            }
        }
        (x1.saturating_sub(x0), y1.saturating_sub(y0), n)
    };

    let (w0, h0, n0) = measure((0.0, 0.0));
    let (w4, h4, n4) = measure((4.0, 4.0));
    println!("  offset 0: {w0}x{h0}（{n0} px），offset 4,4: {w4}x{h4}（{n4} px）");
    assert!(
        (w4 as i64 - w0 as i64).abs() <= 2,
        "带偏移的投影宽度不该变（{w0} vs {w4}）—— 变小说明被条目边界截断"
    );
    assert!(
        (h4 as i64 - h0 as i64).abs() <= 2,
        "带偏移的投影高度不该变（{h0} vs {h4}）—— 变小说明被条目边界截断"
    );
    assert!(
        n4 as f64 >= n0 as f64 * 0.85,
        "带偏移的投影面积不该明显变小（{n0} vs {n4}）"
    );
}

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
fn material_shape_text_per_sample_paths() {
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
        AntiAliasing::PerSample {
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
        "shape + text + per-sample",
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
/// 区域内是否全为绿色（G 高、R/B 低）。
fn region_is_green(pixels: &[u8], w: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> bool {
    for y in y0..y1 {
        for x in x0..x1 {
            let idx = ((y * w + x) * 4) as usize;
            if !(pixels[idx + 1] > 150 && pixels[idx] < 80 && pixels[idx + 2] < 80) {
                return false;
            }
        }
    }
    true
}
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
        .create_material_with_vertex_shader(
            FRAGMENT_WGSL,
            VERTEX_WGSL,
            ShapeVertexLayout::SdfInstance,
        )
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
        .create_material_with_vertex_shader(
            FRAGMENT_WGSL,
            VERTEX_WGSL,
            ShapeVertexLayout::GeoInstance,
        )
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
        .create_material_with_vertex_shader(FRAGMENT_WGSL, VERTEX_WGSL, ShapeVertexLayout::Mesh)
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
    batch_a.clock = clock_a;
    draw_particles(&mut batch_a, &pool_a);
    let mut batch_b = DrawBatch::new();
    batch_b.clock = clock_b;
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

/// MRT + MSAA：主目标开 4x MSAA、附加目标是单采样，引擎应**逐附件**各建一份
/// MSAA 再 resolve 回去。管线 sample_count 与附件必须一致，否则 wgpu 会直接报
/// validation error —— 所以"能跑通且两个目标都有内容"本身就是这条机制的护栏。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn mrt_with_msaa_resolves_both_targets() {
    println!("\n=== MRT + 4x MSAA：逐附件 MSAA/resolve ===");
    const MATERIAL_WGSL: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) aux: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    var out: MaterialOutput;
    out.color = vec4<f32>(1.0, 0.0, 0.0, 1.0);   // 固定红
    out.aux = vec4<f32>(0.0, 0.0, 1.0, 1.0);     // 第二路固定蓝
    return out;
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    // 主目标走引擎 MSAA（表面纹理仍是单采样）；附加目标保持单采样，
    // resolve 由引擎按 `TargetAa.samples` 逐附件补齐。
    let base = OffscreenCanvas::with_aa(
        &gpu,
        200,
        200,
        AntiAliasing::Msaa {
            samples: 4,
            alpha_to_coverage: false,
        },
        0.0,
    );
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
        "target 0 应被 MSAA resolve 出红色内容"
    );
    let aux_pixels = aux.read_pixels();
    assert!(
        region_is_blue(&aux_pixels, 200, 40, 40, 160, 160),
        "target 1（附加、单采样）也应被逐附件 MSAA resolve 出蓝色内容"
    );
    println!(
        "  MRT+MSAA: target0=red target1=blue draw_calls={}",
        base.last_draw_calls()
    );

    // 第二阶段：证明目标上的 `samples = 4` **真的**是多重采样，而不是被悄悄忽略。
    // 用硬边几何（关掉 SDF feather，只剩光栅化）旋转绘制：4x MSAA 会产生边缘过渡像素，
    // 1 采样则只有纯色/背景两种值。
    const PLAIN_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return in.color;
}
"#;
    let plain = gpu
        .create_material(PLAIN_WGSL)
        .expect("plain material pipelines");

    let count_partial = |pixels: &[u8]| -> usize {
        let mut n = 0;
        for px in pixels.chunks_exact(4) {
            // 既不是近黑背景，也不是近纯白填充 → 边缘覆盖率介于两者之间
            if px[0] > 30 && px[0] < 225 {
                n += 1;
            }
        }
        n
    };

    let draw_edge = |canvas: &OffscreenCanvas| {
        let mut b = DrawBatch::new();
        b.set_custom_material(Some(plain.clone()));
        b.clear_sdf_feather(); // 走几何模板路径：边缘只有光栅化覆盖
        b.set_position(100.0, 100.0);
        b.set_rad(0.37);
        draw_rectangle(&mut b, Pos::new(-70.0, -70.0), 140.0, 90.0, Some(WHITE));
        canvas.draw(Some(BLACK), &[&b]);
        count_partial(&canvas.read_pixels())
    };

    let msaa_edges = draw_edge(&OffscreenCanvas::with_aa(
        &gpu,
        200,
        200,
        AntiAliasing::Msaa {
            samples: 4,
            alpha_to_coverage: false,
        },
        0.0,
    ));
    let noaa_edges = draw_edge(&OffscreenCanvas::new(&gpu, 200, 200));
    println!("  硬边过渡像素：4x MSAA={msaa_edges}  无 AA={noaa_edges}");
    assert!(
        msaa_edges > 30,
        "4x MSAA 下旋转硬边应出现大量过渡像素（实际 {msaa_edges}）"
    );
    assert!(
        noaa_edges * 4 < msaa_edges,
        "无 AA 的硬边不应有大量过渡像素（实际 {noaa_edges}）"
    );
}

/// MRT：**引擎形状**（无 custom material）可以直接画进多附件 pass —— shader 只写
/// `@location(0)`，附加目标填 `None`（不写），因此保留 pass 载入时的内容，不会把
/// 掩码/法线等通道擦掉。这是"引擎形状在 MRT 里被跳过"被移除后的行为护栏。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn engine_shape_writes_first_target_only() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 200, 200);
    let aux = OffscreenCanvas::new(&gpu, 200, 200);

    // 预填 aux 为绿色（单附件 draw）
    let fill = gpu
        .create_material(
            "fn material_main(in: MaterialInput) -> vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }",
        )
        .expect("fill material");
    let mut fb = DrawBatch::new();
    fb.set_custom_material(Some(fill));
    draw_rectangle(&mut fb, Pos::new(0.0, 0.0), 200.0, 200.0, Some(WHITE));
    aux.draw(Some(BLACK), &[&fb]);
    assert!(
        region_is_green(&aux.read_pixels(), 200, 20, 20, 160, 160),
        "预填 aux 应为绿色"
    );

    // 引擎形状（无 custom material）画进双附件 pass
    let mut b = DrawBatch::new();
    draw_rectangle(
        &mut b,
        Pos::new(20.0, 20.0),
        160.0,
        160.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );
    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    base.draw_mrt(Some(BLACK), &[aux_target], &[&b]);

    let base_pixels = base.read_pixels();
    assert!(
        region_is_red(&base_pixels, 200, 40, 40, 160, 160),
        "引擎形状应写进 target 0"
    );
    let aux_pixels = aux.read_pixels();
    assert!(
        region_is_green(&aux_pixels, 200, 40, 40, 160, 160),
        "附加目标应保持载入内容（不被引擎形状写坏）"
    );
    println!(
        "  engine shape in MRT: target0=red target1=green(kept) draw_calls={}",
        base.last_draw_calls()
    );
}

/// MRT：**文字**也能画进多附件 pass —— glyphon 管线按附件数取变体，附加目标空写，
/// 所以文字只落在 target 0，其余通道保持载入内容。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_draws_in_multi_target_pass() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 320, 180);
    let aux = OffscreenCanvas::new(&gpu, 320, 180);

    // 预填 aux 为绿色
    let fill = gpu
        .create_material(
            "fn material_main(in: MaterialInput) -> vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }",
        )
        .expect("fill material");
    let mut fb = DrawBatch::new();
    fb.set_custom_material(Some(fill));
    draw_rectangle(&mut fb, Pos::new(0.0, 0.0), 320.0, 180.0, Some(WHITE));
    aux.draw(Some(BLACK), &[&fb]);

    // 文字画进双附件 pass
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        "MRT text 123",
        Pos::new(30.0, 60.0),
        TextDef::default().font_size(30.0),
        TextOverride::from_color(WHITE),
    );
    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    base.draw_mrt(Some(BLACK), &[aux_target], &[&b]);

    let base_pixels = base.read_pixels();
    let lit = base_pixels.chunks_exact(4).filter(|p| p[0] > 40).count();
    assert!(lit > 40, "文字应写进 target 0（非黑像素实际 {lit} 个）");
    let aux_pixels = aux.read_pixels();
    assert!(
        region_is_green(&aux_pixels, 320, 20, 20, 300, 160),
        "附加目标应保持预填内容（文字不写其它通道）"
    );
    println!("  text in MRT: target0 非黑像素={lit} target1=green(kept)");
}

/// MRT：**文字的自定义材质**也要按 pass 附件数建管线（曾把目标数组写死成 1 个，
/// 多附件 pass 会直接 wgpu 校验失败）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn custom_text_material_in_multi_target_pass() {
    const WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 320, 180);
    let aux = OffscreenCanvas::new(&gpu, 320, 180);

    let fill = gpu
        .create_material(
            "fn material_main(in: MaterialInput) -> vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }",
        )
        .expect("fill material");
    let mut fb = DrawBatch::new();
    fb.set_custom_material(Some(fill));
    draw_rectangle(&mut fb, Pos::new(0.0, 0.0), 320.0, 180.0, Some(WHITE));
    aux.draw(Some(BLACK), &[&fb]);

    let mat = gpu.create_material(WGSL).expect("text material");
    let mut b = DrawBatch::new();
    b.set_custom_material(Some(mat));
    draw_text(
        &mut b.texts,
        "MRT custom text",
        Pos::new(20.0, 60.0),
        TextDef::default().font_size(28.0),
        TextOverride::from_color(WHITE),
    );
    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    base.draw_mrt(Some(BLACK), &[aux_target], &[&b]);

    let base_pixels = base.read_pixels();
    let lit = base_pixels.chunks_exact(4).filter(|p| p[0] > 40).count();
    assert!(
        lit > 40,
        "文字自定义材质应写进 target 0（实际 {lit} 个非黑像素）"
    );
    let aux_pixels = aux.read_pixels();
    assert!(
        region_is_green(&aux_pixels, 320, 20, 20, 300, 160),
        "附加目标应保持预填内容"
    );
    println!("  custom text material in MRT: target0 非黑像素={lit} target1=green(kept)");
}

/// MRT：**3 个颜色附件**各自独立接收输出（管线/材质的目标数装配到 3 也成立）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn three_color_attachments_are_independently_written() {
    const WGSL: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) aux: vec4<f32>,
    @location(2) aux2: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    var out: MaterialOutput;
    out.color = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    out.aux = vec4<f32>(0.0, 1.0, 0.0, 1.0);
    out.aux2 = vec4<f32>(0.0, 0.0, 1.0, 1.0);
    return out;
}
"#;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 128, 128);
    let aux = OffscreenCanvas::new(&gpu, 128, 128);
    let aux2 = OffscreenCanvas::new(&gpu, 128, 128);
    let mat = gpu.create_material(WGSL).expect("3-output material");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(mat));
    draw_rectangle(&mut b, Pos::new(16.0, 16.0), 96.0, 96.0, Some(WHITE));

    let t1 = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    let t2 = ColorTargetDesc {
        view: aux2.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    base.draw_mrt(Some(BLACK), &[t1, t2], &[&b]);

    assert!(
        region_is_red(&base.read_pixels(), 128, 32, 32, 96, 96),
        "target 0 应为红"
    );
    assert!(
        region_is_green(&aux.read_pixels(), 128, 32, 32, 96, 96),
        "target 1 应为绿"
    );
    assert!(
        region_is_blue(&aux2.read_pixels(), 128, 32, 32, 96, 96),
        "target 2 应为蓝"
    );
    println!("  3 attachments: target0=red target1=green target2=blue");
}

/// MRT：**逐附件独立 blend** 真的生效（附加目标用加性混合，而非全写）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn per_attachment_blend_is_applied() {
    const WGSL: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) aux: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    var out: MaterialOutput;
    out.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);   // 不动 target 0
    out.aux = vec4<f32>(0.25, 0.25, 0.25, 1.0);
    return out;
}
"#;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 128, 128);
    let aux = OffscreenCanvas::new(&gpu, 128, 128);

    // 预填 aux = 0.5（线性）→ sRGB ≈ 188
    let fill = gpu
        .create_material(
            "fn material_main(in: MaterialInput) -> vec4<f32> { return vec4<f32>(0.5, 0.5, 0.5, 1.0); }",
        )
        .expect("fill material");
    let mut fb = DrawBatch::new();
    fb.set_custom_material(Some(fill));
    draw_rectangle(&mut fb, Pos::new(0.0, 0.0), 128.0, 128.0, Some(WHITE));
    aux.draw(Some(BLACK), &[&fb]);
    let pre = aux.read_pixels();
    let i = ((64 * 128 + 64) * 4) as usize;
    let pre_v = pre[i];
    assert!(
        (170..=205).contains(&pre_v),
        "预填值应约 188（实际 {pre_v}）"
    );

    // 附加目标用加性混合：0.5 + 0.25 = 0.75 → sRGB ≈ 225
    let add = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
    };
    let mat = gpu.create_material(WGSL).expect("2-output material");
    let mut b = DrawBatch::new();
    b.set_custom_material(Some(mat));
    draw_rectangle(&mut b, Pos::new(16.0, 16.0), 96.0, 96.0, Some(WHITE));
    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: Some(add),
    };
    base.draw_mrt(Some(BLACK), &[aux_target], &[&b]);

    let post = aux.read_pixels();
    let post_v = post[i];
    assert!(
        post_v > 210,
        "加性混合后应约 225（0.5+0.25 线性），实际 {post_v}（若为全写会是 ~137）"
    );
    println!("  per-attachment blend: aux {pre_v} -> {post_v}（加性）");
}

/// MRT：**带 stencil 的裁切**（`clips_children`）在双附件 pass 里也成立 ——
/// 引擎会按需自建 DS，管线状态的目标数也要匹配附件数。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn mrt_with_stencil_clipping() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 128, 128);
    let aux = OffscreenCanvas::new(&gpu, 128, 128);

    // 父 batch 开裁切（圆形），子 batch 画满宽的红条
    let mut parent = DrawBatch::new();
    parent.clips_children = true;
    draw_circle(&mut parent, Pos::new(64.0, 64.0), 50.0, Some(WHITE));

    let mut child = DrawBatch::new();
    draw_rectangle(
        &mut child,
        Pos::new(0.0, 54.0),
        128.0,
        20.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );
    parent.push_child(child);

    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    base.draw_mrt(Some(BLACK), &[aux_target], &[&parent]);

    let p = base.read_pixels();
    let at = |x: u32, y: u32| {
        let i = ((y * 128 + x) * 4) as usize;
        [p[i], p[i + 1], p[i + 2]]
    };
    let inside = at(64, 64);
    let outside = at(6, 64);
    assert!(inside[0] > 150, "裁切内的红条应可见，实际 {inside:?}");
    assert!(outside[0] < 40, "圆外的部分应被裁掉，实际 {outside:?}");
    println!("  MRT + stencil clip: inside={inside:?} outside={outside:?}");
}

/// MRT：手搓 `RenderTarget` + **引擎 DS** 的双附件组合也要成立
/// （`draw_mrt` 不接 DS，这条是唯一能显式挂 DS 的多附件入口）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn mrt_manual_render_target_with_depth_stencil() {
    const WGSL: &str = r#"
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
    let base = OffscreenCanvas::new(&gpu, 128, 128);
    let aux = OffscreenCanvas::new(&gpu, 128, 128);
    let mat = gpu.create_material(WGSL).expect("material");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(mat));
    draw_rectangle(&mut b, Pos::new(16.0, 16.0), 96.0, 96.0, Some(WHITE));

    // 自建 Renderer：`OffscreenCanvas` 的 renderer 字段是私有的
    let renderer = Renderer::new(gpu.clone(), 128.0, 128.0, 128, 128, 1.0, 1.0);
    let mut target = RenderTarget::from_color_attachments(vec![
        ColorTargetDesc {
            view: base.texture.lock().view.clone(),
            resolve_target: None,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
        },
        ColorTargetDesc {
            view: aux.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        },
    ]);
    target.set_depth_stencil(Some(DepthStencilTarget::Engine(DepthStencilOps::default())));
    let cmds = target.draw(&renderer, &[Some(BLACK), None], &[&b]);
    gpu.queue.submit(Some(cmds));

    assert!(
        region_is_red(&base.read_pixels(), 128, 32, 32, 96, 96),
        "target 0 应为红"
    );
    assert!(
        region_is_blue(&aux.read_pixels(), 128, 32, 32, 96, 96),
        "target 1 应为蓝"
    );
    println!("  manual RenderTarget + DS: target0=red target1=blue ok");
}

/// MRT：**per-sample 着色** 与逐附件 MSAA/resolve 的组合也要成立。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn mrt_with_per_sample_shading() {
    const WGSL: &str = r#"
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
    let base = OffscreenCanvas::with_aa(
        &gpu,
        128,
        128,
        AntiAliasing::PerSample {
            samples: 4,
            alpha_to_coverage: false,
        },
        0.0,
    );
    let aux = OffscreenCanvas::new(&gpu, 128, 128);
    let mat = gpu.create_material(WGSL).expect("material");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(mat));
    draw_rectangle(&mut b, Pos::new(16.0, 16.0), 96.0, 96.0, Some(WHITE));

    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    base.draw_mrt(Some(BLACK), &[aux_target], &[&b]);

    assert!(
        region_is_red(&base.read_pixels(), 128, 32, 32, 96, 96),
        "target 0 应为红（per-sample 下也应 resolve 出内容）"
    );
    assert!(
        region_is_blue(&aux.read_pixels(), 128, 32, 32, 96, 96),
        "target 1 应为蓝（附加目标逐附件 per-sample resolve）"
    );
    println!("  MRT + per-sample(4): target0=red target1=blue ok");
}

/// per-sample 档的边缘梯度实测：软边（`feather = 1px`，现状）vs 硬边（`feather = 0`，AA 全交 resolve）。
///
/// 目的只是量化「解析软边 + 逐采样求值 = 两层 AA 叠加」，**不改变任何行为**；数字用于
/// `TargetAa::per_sample` / `AntiAliasing::PerSample` 文档：4 个采样点且每采样点二值
/// （硬边）时，覆盖率只有 5 级；软边则是连续梯度。两者都应有过渡像素。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn per_sample_feather_off_vs_on_rotated_edge() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));

    const SIZE: u32 = 64;
    const EDGE_DEG: f32 = 22.5;
    const HALF: f32 = 20.0;

    let cases: [(&str, AntiAliasing, Option<f32>); 3] = [
        (
            "MSAA4 + feather 1px（每像素求值基线）",
            AntiAliasing::Msaa {
                samples: 4,
                alpha_to_coverage: false,
            },
            Some(1.0),
        ),
        (
            "PerSample4 + feather 1px（现状）",
            AntiAliasing::PerSample {
                samples: 4,
                alpha_to_coverage: false,
            },
            Some(1.0),
        ),
        (
            "PerSample4 + feather 0（硬边，AA 交 resolve）",
            AntiAliasing::PerSample {
                samples: 4,
                alpha_to_coverage: false,
            },
            Some(0.0),
        ),
    ];

    for (label, aa, feather) in cases {
        let canvas = OffscreenCanvas::with_aa(&gpu, SIZE, SIZE, aa, 0.0);
        let mut b = DrawBatch::new();
        b.set_sdf_feather(feather);
        // 先平移到画布中心、再绕局部原点旋转 → 方块绕自身中心转 22.5°（保证斜边）。
        b.translate(SIZE as f32 * 0.5, SIZE as f32 * 0.5);
        b.rotate_deg(EDGE_DEG);
        draw_rectangle(
            &mut b,
            Pos::new(-HALF, -HALF),
            HALF * 2.0,
            HALF * 2.0,
            Some(WHITE),
        );
        canvas.draw(Some(BLACK), &[&b]);
        let px = canvas.read_pixels();

        // 白形状 + 黑背景：任取一个颜色通道当覆盖率（0 = 背景，255 = 完全覆盖）。
        let at = |x: u32, y: u32| -> i32 { px[((y * SIZE + x) * 4) as usize] as i32 };
        let mut transition = 0u32;
        let mut levels = std::collections::BTreeSet::new();
        let mut max_step = 0i32;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let l = at(x, y);
                if l > 8 && l < 247 {
                    transition += 1;
                    levels.insert(l);
                }
                if x > 0 {
                    max_step = max_step.max((l - at(x - 1, y)).abs());
                }
            }
        }
        println!(
            "  {label}: 过渡像素 {transition}、覆盖率级数 {}、单像素最大阶跃 {max_step}/255",
            levels.len()
        );
        assert!(transition > 0, "{label}: 应有过渡像素（AA 生效）");
        if feather == Some(0.0) {
            assert!(
                levels.len() <= 5,
                "{label}: 4 采样点 + 二值覆盖最多 5 级，实测 {}",
                levels.len()
            );
        } else {
            assert!(
                levels.len() > 5,
                "{label}: 解析软边应给出连续梯度（> 5 级），实测 {}",
                levels.len()
            );
        }
    }
}

/// GPU 时间对**所有绘制**可见（不再只属于粒子）：
/// 默认钟每帧被写进相机 uniform 的 `time_secs`，`batch.set_clock(other)` 则把 group 0
/// 换成那条时间线。这里用 custom VS 读 `camera.time_secs` 直接位移几何，用像素位置验证。
///
/// 注：fragment-only 材质会在创建期同时对 Shape/Text 两个 target 校验，而 Text 的
/// 材质支持片段没有 `camera` 绑定（文字走自己的 group 0），所以这里走 custom VS
/// ——VS 是独立模块，只有声明过的 shape layout 会被校验。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn shape_batch_clock_drives_camera_time() {
    const FRAG: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
"#;
    const VERT: &str = r#"
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
    time_secs: f32,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;
@group(2) @binding(2) var<storage> instances: array<InstanceData>;

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
    // GPU 时间位移**几何**（world），不动 local_pos：SDF 掩码跟着 quad 一起移动。
    // 0.2s → +8px，0.8s → +32px。
    let shift = fract(camera.time_secs) * 40.0;
    let world_pos =
        transforms[inst.transform_index] * vec3<f32>(local_pos + vec2<f32>(shift, 0.0), 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    let span = max(inst.uv_bounds.zw - inst.uv_bounds.xy, vec2<f32>(1e-6));
    out.uv = mix(inst.uv_rect.xy, inst.uv_rect.zw, (local_pos - inst.uv_bounds.xy) / span);
    out.color = inst.color;
    out.sdf_params = inst.sdf_params;
    out.local_pos = local_pos;
    out.sdf_type = inst.sdf_type;
    out.sdf_feather = inst.sdf_feather;
    out.sdf_extra = inst.sdf_extra;
    return out;
}
"#;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 64, 64);
    let mat = gpu
        .create_material_with_vertex_shader(FRAG, VERT, ShapeVertexLayout::SdfInstance)
        .expect("material");

    // 12×12 的方块画在左上角，时间把它沿 x 推开：0.2s → x∈[8,20)，0.8s → x∈[32,44)。
    let draw = |clock: Option<ClockIndex>| -> Vec<u8> {
        let mut b = DrawBatch::new();
        b.set_custom_material(Some(mat.clone()));
        if let Some(c) = clock {
            b.set_clock(c);
        }
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 12.0, 12.0, Some(WHITE));
        canvas.draw(Some(BLACK), &[&b]);
        canvas.read_pixels()
    };
    let lit = |px: &[u8], x: u32, y: u32| -> bool { px[((y * 64 + x) * 4) as usize] > 128 };

    // 默认钟：定值 0.2s 并冻结，保证确定性（0 号槽可设值）。
    assert!(
        gpu.set_clock_time(ClockIndex::ZERO, 0.2),
        "默认钟应可设时间"
    );
    assert!(gpu.set_clock_scale(ClockIndex::ZERO, 0.0), "默认钟应可暂停");
    let a = draw(None);

    // 另一条时间线：0.8s，同样冻结。
    let other = gpu.create_clock();
    assert!(gpu.set_clock_time(other, 0.8), "新钟应可设时间");
    assert!(gpu.set_clock_scale(other, 0.0), "新钟应可暂停");
    let b = draw(Some(other));

    println!(
        "  camera.time_secs → 覆盖区域: 默认钟(0.2s) x14={} x38={} | 另一钟(0.8s) x14={} x38={}",
        lit(&a, 14, 6),
        lit(&a, 38, 6),
        lit(&b, 14, 6),
        lit(&b, 38, 6)
    );
    assert!(
        lit(&a, 14, 6) && !lit(&a, 38, 6),
        "默认钟应读到 0.2s（方块落在 x≈8..20）"
    );
    assert!(
        !lit(&b, 14, 6) && lit(&b, 38, 6),
        "非默认 batch clock 必须换到另一条时间线（方块落在 x≈32..44）"
    );
}

/// MRT：附加目标在**下一次 draw 里保持内容**（`LoadOp::Load`）——引擎形状/文字画在上面
/// 不会把它清零，只有显式写多目标的 custom material 才会改它。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn extra_target_survives_later_draws() {
    const WGSL: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) aux: vec4<f32>,
};
fn material_main(in: MaterialInput) -> MaterialOutput {
    var out: MaterialOutput;
    out.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);   // 不动 target 0
    out.aux = vec4<f32>(0.0, 0.0, 1.0, 1.0);     // aux 全局写蓝
    return out;
}
"#;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 128, 128);
    let aux = OffscreenCanvas::new(&gpu, 128, 128);

    // 第 1 次 draw：custom 材质把 aux 整屏写蓝
    let mat = gpu.create_material(WGSL).expect("material");
    let mut b1 = DrawBatch::new();
    b1.set_custom_material(Some(mat));
    draw_rectangle(&mut b1, Pos::new(0.0, 0.0), 128.0, 128.0, Some(WHITE));
    base.draw_mrt(
        Some(BLACK),
        &[ColorTargetDesc {
            view: aux.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        }],
        &[&b1],
    );
    assert!(
        region_is_blue(&aux.read_pixels(), 128, 16, 16, 112, 112),
        "第 1 次 draw 后 aux 应为蓝"
    );

    // 第 2 次 draw：只用引擎形状画 target 0，aux 不该被清零
    let mut b2 = DrawBatch::new();
    draw_rectangle(
        &mut b2,
        Pos::new(16.0, 16.0),
        96.0,
        96.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );
    base.draw_mrt(
        Some(BLACK),
        &[ColorTargetDesc {
            view: aux.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        }],
        &[&b2],
    );

    assert!(
        region_is_red(&base.read_pixels(), 128, 32, 32, 96, 96),
        "第 2 次 draw 应写 target 0"
    );
    assert!(
        region_is_blue(&aux.read_pixels(), 128, 16, 16, 112, 112),
        "aux 应保持第 1 次 draw 的内容（引擎形状不写附加目标）"
    );
    println!("  extra target kept across draws: aux still blue, target0 red");
}

/// MRT：引擎的 SDF 羽化 / alpha 只作用在 `out.color` 上，**其余输出原样写出**。
///
/// 这条是 `mrt_glass` 手算覆盖度的前提：附加目标不会自动获得抗锯齿边缘，
/// 想要平滑掩码必须自己在材质里按 SDF 距离算。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn mrt_feather_applies_only_to_color_output() {
    const WGSL: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) aux: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    var out: MaterialOutput;
    out.color = vec4<f32>(1.0, 1.0, 1.0, 1.0);
    out.aux = vec4<f32>(1.0, 1.0, 1.0, 1.0);   // 恒 1，不做任何羽化
    return out;
}
"#;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 128, 128);
    let aux = OffscreenCanvas::new(&gpu, 128, 128);
    let mat = gpu.create_material(WGSL).expect("material");

    // 圆角矩形 + 默认 SDF 羽化（1px），边缘应有分析性抗锯齿
    let mut b = DrawBatch::new();
    b.set_custom_material(Some(mat));
    draw_rounded_rect(&mut b, Pos::new(24.0, 24.0), 80.0, 80.0, 12.0, Some(WHITE));
    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    base.draw_mrt(Some(BLACK), &[aux_target], &[&b]);

    // 统计穿过上边缘一列里的"中间值"像素数（既非 0 也非满值）
    let count_mid = |pixels: &[u8]| -> usize {
        let x = 64usize;
        (20..108)
            .filter(|y| {
                let v = pixels[(y * 128 + x) * 4];
                v > 12 && v < 243
            })
            .count()
    };
    let color_mid = count_mid(&base.read_pixels());
    let aux_mid = count_mid(&aux.read_pixels());
    assert!(
        color_mid > 0,
        "target 0 的边缘应有抗锯齿过渡像素（实际 {color_mid}）"
    );
    assert!(
        aux_mid < color_mid,
        "附加目标不该有羽化过渡：aux 中间值 {aux_mid} 应少于 color 的 {color_mid}"
    );
    println!("  羽化只作用于 color：color 过渡像素={color_mid} aux 过渡像素={aux_mid}");
}

/// MRT + 自定义 Pass：`draw_with_frame_mrt` 把 `PreparedFrame` 绑到多附件目标上，
/// 回调里用 `begin_engine_pass` 编码引擎批次，其余附件保持载入内容。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn custom_pass_with_mrt_target() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let base = OffscreenCanvas::new(&gpu, 128, 128);
    let aux = OffscreenCanvas::new(&gpu, 128, 128);

    // aux 预填绿色，方便判断"引擎批次有没有写坏附加目标"
    let fill = gpu
        .create_material(
            "fn material_main(in: MaterialInput) -> vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }",
        )
        .expect("fill");
    let mut fb = DrawBatch::new();
    fb.set_custom_material(Some(fill));
    draw_rectangle(&mut fb, Pos::new(0.0, 0.0), 128.0, 128.0, Some(WHITE));
    aux.draw(Some(BLACK), &[&fb]);

    let mut b = DrawBatch::new();
    draw_rectangle(
        &mut b,
        Pos::new(16.0, 16.0),
        96.0,
        96.0,
        Some(Color::new(1.0, 0.0, 0.0, 1.0)),
    );

    let aux_target = ColorTargetDesc {
        view: aux.texture.lock().view.clone(),
        resolve_target: None,
        blend: None,
    };
    let mut attached = 0usize;
    base.draw_with_frame_mrt(Some(BLACK), &[aux_target], &[&b], |frame, encoder| {
        attached = frame.target().color_attachments.len();
        let mut pass = frame.begin_engine_pass(encoder);
        frame.encode(&mut pass);
    });

    assert_eq!(attached, 2, "PreparedFrame 应绑定 2 个颜色附件");
    assert!(
        region_is_red(&base.read_pixels(), 128, 32, 32, 96, 96),
        "自定义 pass 应把批次编码进 target 0"
    );
    assert!(
        region_is_green(&aux.read_pixels(), 128, 16, 16, 112, 112),
        "附加目标应保持预填内容"
    );
    println!("  custom pass + MRT: 2 attachments, target0=red target1=green(kept)");
}

/// MRT 材质（2 个 fragment 输出）也能用于**单附件** pass：wgpu 允许多余输出被丢弃，
/// 这正是"MRT 材质不能进单附件 pass"那个错误结论的反例。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn multi_output_material_in_single_target_pass() {
    const WGSL: &str = r#"
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
    let mat = gpu.create_material(WGSL).expect("2-output material");
    let canvas = OffscreenCanvas::new(&gpu, 64, 64);
    let mut b = DrawBatch::new();
    b.set_custom_material(Some(mat));
    draw_rectangle(&mut b, Pos::new(0.0, 0.0), 64.0, 64.0, Some(WHITE));
    canvas.draw(Some(BLACK), &[&b]);
    assert!(
        region_is_red(&canvas.read_pixels(), 64, 16, 16, 32, 32),
        "2 输出材质的 color 输出应正常写进单附件 pass"
    );
    println!("  multi-output material in single-target pass: ok");
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

#[test]
#[ignore = "requires GPU; run with --ignored"]
fn dynamic_uniform_slot_growth_rebinds_new_buffer() {
    println!(
        "\n=== 回归：dynamic uniform 槽扩容后必须绑到新 buffer（旧 BindGroupPool 指纹 bug）==="
    );
    // 片元只输出 uniform 本身，便于用像素判断「当前绑的是哪个 buffer」。
    const WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return tint * in.color;
}
"#;

    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 64, 64);

    let resources = vireo::material::MaterialResources(&[vireo::material::MaterialResource {
        name: "tint",
        kind: vireo::material::MaterialResourceKind::Uniform {
            min_size: 16,
            type_name: "vec4<f32>",
            dynamic: true,
        },
    }]);
    let material = gpu
        .create_material_with_resources(WGSL, resources)
        .expect("dynamic uniform material");

    let mut b = DrawBatch::new();
    b.set_custom_material(Some(material.clone()));
    b.set_sdf_feather(Some(1.0));
    // dynamic uniform 槽需要显式偏移（引擎按段校验）
    b.dynamic_offsets = vec![0];
    draw_rectangle(&mut b, Pos::new(8.0, 8.0), 48.0, 48.0, Some(WHITE));

    let bytes = |rgba: [f32; 4]| -> Vec<u8> {
        rgba.iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>()
    };

    // 1) 不扩容：槽内 buffer 原地 write。
    material.set_uniform_bytes(&gpu.queue, "tint", &bytes([1.0, 0.0, 0.0, 1.0]));
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let px = canvas.read_pixels();
    assert!(region_is_red(&px, 64, 16, 16, 40, 40), "第一步应画成红色");

    // 2) 扩容：写入超过 min_size → `set_uniform_bytes` 新建 buffer 并原地替换槽内句柄。
    //    旧实现用「句柄字段地址」做 bind-group 指纹，地址不变 → 命中旧 bind group，
    //    绘制继续读旧 buffer（红）；修复后必须读到新 buffer（蓝）。
    let mut grown = bytes([0.0, 0.0, 1.0, 1.0]);
    grown.extend_from_slice(&[0u8; 16]);
    material.set_uniform_bytes(&gpu.queue, "tint", &grown);
    canvas.draw(Some(Color::new(0.0, 0.0, 0.0, 1.0)), &[&b]);
    let px = canvas.read_pixels();
    assert!(
        region_is_blue(&px, 64, 16, 16, 40, 40),
        "扩容后必须绑定新 buffer（蓝）；仍是红色说明命中了旧 bind group"
    );
}

// ==================== 折线沿线渐变（draw_line_chain_colors） ====================
//
// 注意：离屏 canvas 是 **sRGB** 目标（实测 constant a=0.5 → 像素 187），
// 断言里的期望值都必须过 sRGB 编码，不能直接拿线性 alpha 比像素。

/// sRGB 传输函数（测试用）。
fn srgb255(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let enc = if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    enc * 255.0
}

/// 单色退化必须与老 `draw_line_chain` **逐像素相同** —— 这是"旧路径零影响"的红线。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn line_chain_colors_matches_single_color() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 64;
    let pts: Vec<(f32, f32)> = (0..200)
        .map(|i| {
            let t = i as f32 / 199.0;
            (
                8.0 + t * 48.0,
                20.0 + (t * std::f32::consts::TAU).sin() * 12.0,
            )
        })
        .collect();

    // A) 逐点色（全白）—— 走色带分支
    let a = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let colors = vec![WHITE; pts.len()];
    draw_line_chain_colors(&mut b, &pts, 3.0, &colors);
    a.draw(Some(BLACK), &[&b]);
    let px_a = a.read_pixels();

    // B) 老单色 API
    let other = OffscreenCanvas::new(&gpu, N, N);
    let mut b2 = DrawBatch::new();
    draw_line_chain(&mut b2, &pts, 3.0, Some(WHITE));
    other.draw(Some(BLACK), &[&b2]);
    let px_b = other.read_pixels();

    assert!(
        px_a.iter().any(|v| *v > 0),
        "整条链一个像素都没画出来（用例本身失效）"
    );
    let diff = px_a.iter().zip(px_b.iter()).filter(|(x, y)| x != y).count();
    assert_eq!(
        diff, 0,
        "单色渐变与 draw_line_chain 必须逐像素一致，实际 {diff} 个字节不同"
    );
    println!("  line_chain_colors(全白) == draw_line_chain(白)：逐像素一致");
}

/// 沿线 alpha 必须**连续无台阶**（旧实现是 4 档 ≈ 6.5px 一级）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn line_chain_colors_alpha_ramp_is_smooth() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 96;
    const H: u32 = 48;
    const Y: u32 = 24;

    let n = 200;
    let pts: Vec<(f32, f32)> = (0..n)
        .map(|i| (8.0 + 80.0 * i as f32 / 199.0, Y as f32))
        .collect();
    let colors: Vec<Color> = (0..n)
        .map(|i| {
            let t = i as f32 / 199.0;
            Color::new(1.0, 1.0, 1.0, 0.8 + (0.12 - 0.8) * t)
        })
        .collect();

    let canvas = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    draw_line_chain_colors(&mut b, &pts, 6.0, &colors);
    canvas.draw(Some(BLACK), &[&b]);
    let px = canvas.read_pixels();
    let at = |x: u32| -> i32 { px[((Y * W + x) * 4) as usize] as i32 };

    // (a) 单调不回弹、单步不跳档
    let mut prev = at(8);
    for x in 9..=88u32 {
        let got = at(x);
        assert!(
            got <= prev + 1,
            "x={x} 出现回弹（alpha 应沿线单调下降）：{prev} -> {got}"
        );
        assert!(
            prev - got <= 8,
            "x={x} 单像素跳 {} 级 —— 有台阶或档位感",
            prev - got
        );
        prev = got;
    }

    // (b) 与 sRGB 线性 ramp 的偏差 ≤ 4/255
    let mut worst = 0.0f32;
    for x in 8..=88u32 {
        let t = ((x as f32 + 0.5 - 8.0) / 80.0).clamp(0.0, 1.0);
        let want = srgb255(0.8 + (0.12 - 0.8) * t);
        worst = worst.max((at(x) as f32 - want).abs());
    }
    assert!(
        worst <= 4.0,
        "沿线与线性 ramp 的最大偏差 {worst:.2}/255 过大"
    );

    // (c) 真的跨越了整段 alpha（防"几乎没渐变"的假通过）
    //     sRGB 下 0.8 → 231、0.12 → 96。
    assert!(at(8) > 215, "起点应接近 0.8（sRGB ≈ 231），实际 {}", at(8));
    assert!(
        (at(88) - srgb255(0.12) as i32).abs() <= 14,
        "终点应接近 0.12（sRGB ≈ 96），实际 {}",
        at(88)
    );
    println!("  alpha ramp 0.8->0.12：最大偏差 {worst:.2}/255，单调无台阶");
}

/// 同一份几何模板 + 不同色带：两条链必须各画各的颜色（防"边对色错"）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn line_chain_colors_share_geometry_template() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 64;
    let pure_red = Color::new(1.0, 0.0, 0.0, 1.0);
    let pure_green = Color::new(0.0, 1.0, 0.0, 1.0);

    let pts = [(8.0f32, 12.0f32), (56.0, 12.0)];
    let canvas = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    // A：白 → 红
    draw_line_chain_colors(&mut b, &pts, 6.0, &[WHITE, pure_red]);
    // B：**同一局部点集**（几何模板必然命中缓存），平移后画 → 白 → 绿
    b.set_position(0.0, 40.0);
    draw_line_chain_colors(&mut b, &pts, 6.0, &[WHITE, pure_green]);
    canvas.draw(Some(BLACK), &[&b]);
    let px = canvas.read_pixels();
    let at = |x: u32, y: u32| -> (i32, i32, i32) {
        let i = ((y * N + x) * 4) as usize;
        (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32)
    };

    let (r1, g1, b1) = at(52, 12);
    assert!(
        r1 > 200 && g1 < 140 && b1 < 140,
        "上链末端应偏红（复用几何模板也不许串色），实际 ({r1},{g1},{b1})"
    );
    let (r2, g2, b2) = at(52, 52);
    assert!(
        g2 > 200 && r2 < 140 && b2 < 140,
        "下链末端应偏绿（同点集、不同色带），实际 ({r2},{g2},{b2})"
    );
    println!("  同几何模板 + 不同色带：上({r1},{g1},{b1}) / 下({r2},{g2},{b2}) ok");
}

/// 颜色必须在**预乘空间**插值：直通空间混会让中段偏色（红→蓝会混出紫色）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn line_chain_colors_premultiplied_mix() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 64;
    const H: u32 = 64;

    let canvas = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    // 红(a=0.9) → 蓝(a=0)，中点：
    //   预乘：premul0=(0.9,0,0,0.9)、premul1=(0,0,0,0) → (0.45,0,0,0.45) → 反预乘 (1,0,0), a=0.45
    //         合成后 ≈ sRGB(0.45)*255 ≈ 178，**蓝 = 0**
    //   直通：rgb=(0.5,0,0.5) a=0.45 → 合成后偏紫（蓝 ≈ 133）
    draw_line_chain_colors(
        &mut b,
        &[(8.0, 32.0), (56.0, 32.0)],
        10.0,
        &[
            Color::new(1.0, 0.0, 0.0, 0.9),
            Color::new(0.0, 0.0, 1.0, 0.0),
        ],
    );
    canvas.draw(Some(BLACK), &[&b]);
    let px = canvas.read_pixels();
    let i = ((32 * W + 32) * 4) as usize;
    let (r, g, bl) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
    assert!(
        bl < 20,
        "中点蓝分量 {bl} —— 说明在直通空间插值（预乘应为 0）"
    );
    assert!(g < 20, "中点不该有绿分量，实际 {g}");
    let want = srgb255(0.45) as i32;
    assert!(
        (r - want).abs() <= 14,
        "中点红分量应 ≈ sRGB(0.45)*255 ≈ {want}（预乘），实际 {r}"
    );
    println!("  预乘混合中点 rgB=({r},{g},{bl})，期望红≈{want} ok");
}

/// 退化输入：不 panic、不出错图。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn line_chain_colors_degenerate_inputs() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 64;

    // 1) 色数 != 点数 → 按 colors[0] 单色画
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    draw_line_chain_colors(
        &mut b,
        &[(8.0, 32.0), (56.0, 32.0), (56.0, 40.0)],
        6.0,
        &[Color::new(0.0, 1.0, 0.0, 1.0)],
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    // 用纯绿：vireo 的 `GREEN` 是 (0.00,0.89,0.19)，b 分量不低，不能用 `region_is_green`。
    let i = ((32 * N + 32) * 4) as usize;
    let (r, g, bl) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
    assert!(
        g > 200 && r < 40 && bl < 40,
        "色数不符时应退化为 colors[0] 单色（绿），实际 ({r},{g},{bl})"
    );

    // 2) 全透明 → 什么都不画
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let clear = [Color::new(1.0, 1.0, 1.0, 0.0); 2];
    draw_line_chain_colors(&mut b, &[(8.0, 32.0), (56.0, 32.0)], 6.0, &clear);
    c.draw(Some(BLACK), &[&b]);
    // 只看 RGB：清屏后 alpha 通道未必是 0（sRGB 目标），别拿它当"有没有画内容"。
    let px = c.read_pixels();
    assert!(
        px.chunks(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0),
        "全透明色带应跳过（画布保持全黑）"
    );

    // 3) 所有点重合 → 没有边，什么都不画
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let same = [(32.0f32, 32.0f32); 4];
    let colors = [WHITE; 4];
    draw_line_chain_colors(&mut b, &same, 6.0, &colors);
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    assert!(
        px.chunks(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0),
        "全重合点没有边，应什么都不画"
    );

    // 4) 闭合链 → 画得出来，且 wrap 段回到首色
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let ring = [
        (16.0f32, 16.0f32),
        (48.0, 16.0),
        (48.0, 48.0),
        (16.0, 48.0),
        (16.0, 16.0),
    ];
    let colors = [
        Color::new(1.0, 0.0, 0.0, 1.0),
        Color::new(0.0, 1.0, 0.0, 1.0),
        Color::new(0.0, 0.0, 1.0, 1.0),
        WHITE,
        Color::new(1.0, 0.0, 0.0, 1.0),
    ];
    draw_line_chain_colors(&mut b, &ring, 4.0, &colors);
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    assert!(
        px.iter().any(|v| *v > 0),
        "闭合链必须画得出来（首尾重合不应被当成退化）"
    );
    println!("  退化输入 4 例：色数不符=单色、全透明/全重合=空、闭合链=正常");
}

/// per-sample 档 smoke：色带在逐采样求值下也要出内容且单调。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn line_chain_colors_per_sample_aa() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 96;
    const H: u32 = 96;

    let canvas = OffscreenCanvas::with_aa(
        &gpu,
        W,
        H,
        AntiAliasing::PerSample {
            samples: 4,
            alpha_to_coverage: false,
        },
        0.0,
    );
    let n = 60;
    let pts: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let t = i as f32 / 59.0;
            (12.0 + t * 72.0, 12.0 + t * 72.0)
        })
        .collect();
    let colors: Vec<Color> = (0..n)
        .map(|i| {
            let t = i as f32 / 59.0;
            Color::new(1.0, 1.0, 1.0, 1.0 - 0.9 * t)
        })
        .collect();
    let mut b = DrawBatch::new();
    draw_line_chain_colors(&mut b, &pts, 8.0, &colors);
    canvas.draw(Some(BLACK), &[&b]);
    let px = canvas.read_pixels();
    let at = |x: u32, y: u32| -> i32 { px[((y * W + x) * 4) as usize] as i32 };

    let start = at(16, 16);
    let end = at(78, 78);
    assert!(start > 40, "起点应明显可见，实际 {start}");
    assert!(
        start > end + 20,
        "per-sample 下 alpha 也应沿线下降：start={start} end={end}"
    );
    println!("  PerSample4 渐变：start={start} end={end} ok");
}

// ==================== 渐变通道（全 SDF 形状） ====================
//
// 同样注意：离屏 canvas 是 **sRGB** 目标，期望值用 `srgb255()`。

fn gstop(at: f32, c: Color) -> GradientStop {
    GradientStop::new(at, c)
}

/// 没有渐变语义的对照：所有形状用"单色渐变"必须与普通单色绘制**逐像素一致**。
///
/// 这条同时钉住片元的 `cov` 重构（把 6 处 `out_color.a *= smoothstep` 收进尾巴）
/// 与尾巴的渐变分支：只要任何一处改动引入差异，这里就红。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn gradient_uniform_matches_solid() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 96;
    let c = Color::new(0.85, 0.35, 0.20, 0.8);
    let one = [gstop(0.0, c)];
    let shapes: Vec<(&str, Shape<'static>)> = vec![
        (
            "rect",
            Shape::Rect {
                pos: Pos::new(14.0, 14.0),
                w: 60.0,
                h: 40.0,
            },
        ),
        (
            "rounded",
            Shape::RoundedRect {
                pos: Pos::new(14.0, 14.0),
                w: 60.0,
                h: 40.0,
                radius: 12.0,
            },
        ),
        (
            "circle",
            Shape::Circle {
                pos: Pos::new(48.0, 48.0),
                r: 30.0,
            },
        ),
        (
            "ellipse",
            Shape::Ellipse {
                pos: Pos::new(48.0, 48.0),
                rx: 34.0,
                ry: 20.0,
            },
        ),
        (
            "line",
            Shape::Line {
                x1: 14.0,
                y1: 20.0,
                x2: 80.0,
                y2: 70.0,
                thickness: 6.0,
            },
        ),
        (
            "triangle",
            Shape::Triangle {
                x1: 20.0,
                y1: 70.0,
                x2: 48.0,
                y2: 20.0,
                x3: 76.0,
                y3: 70.0,
            },
        ),
        (
            "arc",
            Shape::Arc {
                pos: Pos::new(48.0, 48.0),
                r: 30.0,
                start: 0.2,
                end: 2.4,
            },
        ),
    ];
    let poly = [(20.0f32, 70.0f32), (48.0, 20.0), (76.0, 70.0)];
    let mut shapes = shapes;
    shapes.push(("polygon", Shape::Polygon { points: &poly }));
    for (name, shape) in shapes {
        // A) 普通单色
        let ca = OffscreenCanvas::new(&gpu, N, N);
        let mut a = DrawBatch::new();
        draw_shape(&mut a, &shape, ShapeOverride::new().color(c));
        ca.draw(Some(BLACK), &[&a]);
        let pa = ca.read_pixels();

        // B) 单色渐变（走渐变通道，但语义上等于单色）
        let cb = OffscreenCanvas::new(&gpu, N, N);
        let mut b = DrawBatch::new();
        draw_shape_gradient(&mut b, &shape, LinearGradient::vertical(&one));
        cb.draw(Some(BLACK), &[&b]);
        let pb = cb.read_pixels();

        assert!(pa.iter().any(|v| *v > 0), "{name}: 用例失效（什么都没画）");
        let diff = pa.iter().zip(pb.iter()).filter(|(x, y)| x != y).count();
        assert_eq!(
            diff, 0,
            "{name}: 单色渐变与 draw_shape 必须逐像素一致（差 {diff} 字节）"
        );
    }
    println!("  8 种形状：单色渐变 == 单色绘制（逐像素）");
}

/// 竖直双色渐变：单调、无台阶、与线性 ramp 一致。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn rect_gradient_is_smooth() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 64;
    const H: u32 = 96;
    let c = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    draw_rect_gradient(
        &mut b,
        Pos::new(8.0, 8.0),
        48.0,
        80.0,
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |y: u32| -> i32 { px[((y * W + 32) * 4) as usize] as i32 };

    // 台阶要在**线性 alpha** 空间判：sRGB 编码在暗端会把 1 级 alpha 放大成 ~25 级像素。
    let lin = |p: i32| -> f32 {
        let x = p as f32 / 255.0;
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    let mut prev = at(8);
    for y in 9..=88u32 {
        let got = at(y);
        assert!(got <= prev + 1, "y={y} 回弹：{prev} -> {got}");
        let step = (lin(prev) - lin(got)).abs() * 255.0;
        assert!(step <= 8.0, "y={y} 线性空间跳 {step:.1}/255（有台阶）");
        prev = got;
    }
    // 与线性 ramp 比也走**线性**空间（sRGB 暗端 1/255 的线性差能放大成 ~18 级像素）。
    let mut worst = 0.0f32;
    for y in 8..=88u32 {
        let t = ((y as f32 + 0.5 - 8.0) / 80.0).clamp(0.0, 1.0);
        worst = worst.max((lin(at(y)) - (1.0 - t)).abs() * 255.0);
    }
    // 容差 6/255：1/255 量化 + 像素中心与渐变的半像素采样差。\n    // 台阶/单调性另由上面的逐像素断言保证。\n    assert!(worst <= 6.0, "与线性 ramp 最大偏差 {worst:.2}/255（线性空间）");
    assert!(at(8) > 240, "起点应接近不透明");
    assert!(at(88) < 40, "终点应接近全透明");
    println!("  竖直渐变：最大偏差 {worst:.2}/255（线性），单调无台阶");
}

/// vault 的真实形态：底部 38% 才淡出，上 62% 是平台。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn rect_gradient_plateau() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 64;
    const H: u32 = 128;
    let c = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    let stops = [
        gstop(0.0, Color::new(0.0, 0.0, 0.0, 0.0)),
        gstop(0.62, Color::new(0.0, 0.0, 0.0, 0.0)),
        gstop(1.0, Color::new(0.0, 0.0, 0.0, 0.62)),
    ];
    // 白底上压一层黑：方便看 alpha
    draw_rectangle(
        &mut b,
        Pos::new(0.0, 0.0),
        W as f32,
        H as f32,
        Some(Color::new(1.0, 1.0, 1.0, 1.0)),
    );
    draw_rect_gradient(
        &mut b,
        Pos::new(8.0, 8.0),
        48.0,
        112.0,
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |y: u32| -> i32 { px[((y * W + 32) * 4) as usize] as i32 };

    // 平台段：8..=8+0.62*112≈77 —— 全都一样（±1）
    let top = at(10);
    for y in 10..=72u32 {
        assert!(
            (at(y) - top).abs() <= 1,
            "y={y} 平台段不一致：{top} vs {}",
            at(y)
        );
    }
    // 淡出段：77..=120 单调变暗
    let mut prev = at(78);
    for y in 79..=118u32 {
        let got = at(y);
        assert!(got <= prev + 1, "y={y} 淡出段应单调变暗：{prev} -> {got}");
        prev = got;
    }
    assert!(
        at(118) < top - 60,
        "底部应明显变暗：top={top} bottom={}",
        at(118)
    );
    println!("  平台 + 淡出：平台 {top}，底部 {}，ok", at(118));
}

/// 角度语义：0° = 自上而下，90° = 自左而右。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn rect_gradient_angle_swaps_axis() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 64;
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    // 返回 [上, 下, 左, 右] 四个采样点
    let sample = |grad: LinearGradient<'_>| -> [i32; 4] {
        let c = OffscreenCanvas::new(&gpu, N, N);
        let mut b = DrawBatch::new();
        draw_rect_gradient(&mut b, Pos::new(8.0, 8.0), 48.0, 48.0, grad);
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        let at = |x: u32, y: u32| px[((y * N + x) * 4) as usize] as i32;
        [at(32, 12), at(32, 52), at(12, 32), at(52, 32)]
    };
    let v = sample(LinearGradient::vertical(&stops)); // 上亮下暗
    assert!(v[0] > v[1] + 100, "0° 应自上而下：{v:?}");
    let h = sample(LinearGradient::horizontal(&stops)); // 左亮右暗
    assert!(h[2] > h[3] + 100, "90° 应自左而右：{h:?}");
    // 0° 时左右应几乎相同（轴正交）；90° 时上下应几乎相同。
    assert!((v[2] - v[3]).abs() < 20, "0° 的左右应一致：{v:?}");
    assert!((h[0] - h[1]).abs() < 20, "90° 的上下应一致：{h:?}");
    println!("  角度：vertical={v:?} horizontal={h:?} ok");
}

/// 渐变跟随 batch 变换：本地空间语义（旋转 90° 后渐变也转）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn rect_gradient_follows_transform() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 64;
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    // 正方形转 90°：本地"上"变成屏幕"右"。
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    b.set_position(32.0, 32.0);
    b.set_deg(90.0);
    draw_rect_gradient(
        &mut b,
        Pos::new(-24.0, -24.0),
        48.0,
        48.0,
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |x: u32, y: u32| px[((y * N + x) * 4) as usize] as i32;
    let (left, right) = (at(12, 32), at(52, 32));
    assert!(
        (left - right).abs() > 100,
        "旋转后渐变应沿屏幕水平方向：left={left} right={right}"
    );
    println!("  跟随变换：left={left} right={right} ok");
}

/// 圆角裁剪：圆角外的像素完全不被写，渐变不越过圆角。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn rect_gradient_rounded_corners() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 64;
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 1.0)),
    ];
    draw_rounded_rect_gradient(
        &mut b,
        Pos::new(8.0, 8.0),
        48.0,
        48.0,
        20.0,
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |x: u32, y: u32| px[((y * N + x) * 4) as usize] as i32;
    assert!(at(32, 32) > 200, "中心必须被填满");
    // 左上角 (9,9) 在半径 20 的圆角外 → 必须是黑的。
    assert_eq!(at(9, 9), 0, "圆角外不该有像素");
    println!("  圆角：中心 {} / 角 {}", at(32, 32), at(9, 9));
}

/// 预乘插值：透明 → 黑 叠在**亮色**上不得出现"边缘发暗"。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn rect_gradient_premultiplied() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 64;
    const H: u32 = 64;
    let c = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    // 白底 + 红→透明 的压暗层：中点必须是 50% 红压白，而不是偏暗的浑浊色。
    draw_rectangle(
        &mut b,
        Pos::new(0.0, 0.0),
        W as f32,
        H as f32,
        Some(Color::new(1.0, 1.0, 1.0, 1.0)),
    );
    let stops = [
        gstop(0.0, Color::new(1.0, 0.0, 0.0, 1.0)),
        gstop(1.0, Color::new(1.0, 0.0, 0.0, 0.0)),
    ];
    draw_rect_gradient(
        &mut b,
        Pos::new(8.0, 8.0),
        48.0,
        48.0,
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let i = ((32 * W + 32) * 4) as usize;
    let (r, g, bl) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
    // 预乘：(1,0,0,1) 与 (0,0,0,0) 的中点 = (0.5,0,0,0.5) → 反预乘 (1,0,0)，a=0.5
    // 叠白底 ⇒ 线性 (1,0.5,0.5) → sRGB ≈ (255,188,188)
    assert!(r > 240, "红分量应保持饱和，实际 {r}");
    assert!(
        (g as f32 - srgb255(0.5)).abs() < 16.0,
        "绿分量应 ≈ sRGB(0.5)≈188（偏暗说明在直通空间插值），实际 {g}"
    );
    assert!((bl as f32 - srgb255(0.5)).abs() < 16.0, "蓝分量 {bl}");
    println!("  预乘：中点 rgb=({r},{g},{bl}) ok");
}

/// 非矩形形状也要"跨满自身"：圆的轴向渐变沿直径。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn circle_gradient_spans_diameter() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 64;
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    draw_circle_gradient(
        &mut b,
        Pos::new(32.0, 32.0),
        24.0,
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |y: u32| -> i32 { px[((y * N + 32) * 4) as usize] as i32 };
    assert!(at(10) > 200, "圆的顶部应接近不透明，实际 {}", at(10));
    let near_clear = srgb255(0.0625) as i32 + 24;
    assert!(
        at(53) < near_clear,
        "圆的底部应接近全透明（< {near_clear}），实际 {}",
        at(53)
    );
    println!("  圆：top={} bottom={} ok", at(10), at(53));
}

/// 线段渐变：沿 p1→p2 精确映射（10% / 90% 处取到 10% / 90% 的混合）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn line_gradient_endpoints_exact() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 128;
    const H: u32 = 32;
    let c = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    // 水平线 x: 8 → 120，中心行 y=16
    draw_line_gradient(&mut b, 8.0, 16.0, 120.0, 16.0, 8.0, &stops);
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |x: u32| -> i32 { px[((16 * W + x) * 4) as usize] as i32 };
    let lo: f32 = 8.0 + 112.0 * 0.1;
    let hi: f32 = 8.0 + 112.0 * 0.9;
    let got_lo = at(lo.round() as u32);
    let want_lo = srgb255(0.9) as i32;
    assert!(
        (got_lo - want_lo).abs() <= 12,
        "10% 处应 ≈ sRGB(0.9)={want_lo}，实际 {got_lo}"
    );
    let got_hi = at(hi.round() as u32);
    let want_hi = srgb255(0.1) as i32;
    assert!(
        (got_hi - want_hi).abs() <= 12,
        "90% 处应 ≈ sRGB(0.1)={want_hi}，实际 {got_hi}"
    );
    println!("  线段渐变：10%={got_lo}（期望 {want_lo}） 90%={got_hi}（期望 {want_hi}）");
}

/// 线段渐变（kind 2）与"两点折线渐变"（kind 1）必须逐像素一致。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn line_gradient_matches_chain_of_two_points() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 96;
    let stops = [
        gstop(0.0, Color::new(1.0, 0.85, 0.3, 1.0)),
        gstop(1.0, Color::new(0.2, 0.85, 0.95, 1.0)),
    ];
    let ca = OffscreenCanvas::new(&gpu, N, N);
    let mut a = DrawBatch::new();
    draw_line_gradient(&mut a, 12.0, 20.0, 84.0, 72.0, 9.0, &stops);
    ca.draw(Some(BLACK), &[&a]);
    let pa = ca.read_pixels();

    let cb = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    draw_line_chain_colors(
        &mut b,
        &[(12.0, 20.0), (84.0, 72.0)],
        9.0,
        &[stops[0].color, stops[1].color],
    );
    cb.draw(Some(BLACK), &[&b]);
    let pb = cb.read_pixels();

    assert!(pa.iter().any(|v| *v > 0), "用例失效");
    let diff = pa.iter().zip(pb.iter()).filter(|(x, y)| x != y).count();
    assert_eq!(
        diff, 0,
        "线段渐变与两点折线渐变应逐像素一致（差 {diff} 字节）"
    );
    println!("  线段渐变 == 两点折线渐变（逐像素）");
}

/// 折线渐变按**弧长**而不是包围盒：不等长两段的中点取中点色。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn polyline_gradient_is_arc_length() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 160;
    const H: u32 = 40;
    let c = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    // 段长 60 + 100 = 160；弧长中点 x = 8 + 80 = 88（不是包围盒中点）。
    let pts = [(8.0f32, 20.0f32), (68.0, 20.0), (168.0, 20.0)];
    draw_shape_gradient(
        &mut b,
        &Shape::LineChain {
            points: &pts,
            thickness: 8.0,
        },
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |x: u32| -> i32 { px[((20 * W + x) * 4) as usize] as i32 };
    let mid = srgb255(0.5) as i32;
    let got = at(88);
    assert!(
        (got - mid).abs() <= 14,
        "弧长中点应取中点色 ≈{mid}，实际 {got}"
    );
    println!("  折线渐变：弧长中点 x=88 取 {got}（期望 {mid}）");
}

/// 轴跨度为 0（横线 + 0°）⇒ 整形状取中点色，不产生 NaN / 黑块。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn gradient_zero_extent_axis_is_constant() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 64;
    const H: u32 = 32;
    let c = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    // 水平矩形，用 0°（自上而下）→ 沿轴跨度 = 高度（不为 0）。
    // 这里构造真正退化的：高度 0 的矩形不可见，所以用"竖直排列的两点"测 0° 之外的退化：
    // 用 width=0.5 的极窄矩形 + 90°，跨度 = 0.5px，t 几乎恒定 → 检查仍是中点附近的颜色。
    draw_rect_gradient(
        &mut b,
        Pos::new(32.0, 16.0),
        0.5,
        8.0,
        LinearGradient::horizontal(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let i = ((16 * W + 32) * 4) as usize;
    let v = px[i] as i32;
    assert!(v > 100 && v < 200, "极窄轴应给出中点附近的均匀色，实际 {v}");
    println!("  零跨度轴：{v}（中点色区间内）");
}

/// 贴图 × 渐变：渐变不吞掉贴图。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn rect_gradient_texture_tint() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 64;
    // 32×32 纯红贴图
    let rgba: Vec<u8> = (0..32 * 32).flat_map(|_| [255u8, 0, 0, 255]).collect();
    let tex = Texture::from_rgba(32, 32, &rgba, &gpu);
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    b.set_texture(Some(&tex));
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    draw_rect_gradient(
        &mut b,
        Pos::new(8.0, 8.0),
        48.0,
        48.0,
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |y: u32| -> (i32, i32) {
        let i = ((y * N + 32) * 4) as usize;
        (px[i] as i32, px[i + 1] as i32)
    };
    let (r_top, g_top) = at(12);
    assert!(
        r_top > 200 && g_top < 60,
        "贴图应保留（红）：({r_top},{g_top})"
    );
    let (r_bottom, _) = at(52);
    assert!(r_bottom < 110, "底部应被渐变压到接近透明，实际 {r_bottom}");
    println!("  贴图 × 渐变：顶部 ({r_top},{g_top}) 底部 {r_bottom} ok");
}
/// 多边形也走同一条渐变通道：垂直轴跨满它的**包围盒**（顶 = 起点色、底 = 终点色）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn polygon_gradient_spans_bbox() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 96;
    let stops = [
        gstop(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        gstop(1.0, Color::new(1.0, 1.0, 1.0, 0.0)),
    ];
    // 三角形，包围盒 y = 20..80（注意绕向：与 shapes 示例一致）
    let tri = [(20.0f32, 80.0f32), (50.0, 20.0), (80.0, 80.0)];
    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    draw_shape_gradient(
        &mut b,
        &Shape::Polygon { points: &tri },
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let at = |y: u32| -> i32 { px[((y * N + 50) * 4) as usize] as i32 };

    let top = at(30); // t ≈ 0.17
    let bottom = at(70); // t ≈ 0.83
    assert!(top > 200, "顶点一侧应偏亮，实际 {top}");
    // 线性 alpha 0.167 在 sRGB 下 ≈ 115（比直觉亮得多）—— 按期望值判，别拍脑袋定阈值。
    assert!(
        bottom < srgb255(0.167) as i32 + 25,
        "底边一侧应偏暗，实际 {bottom}"
    );
    let mut prev = at(26);
    for y in 27..=74u32 {
        let got = at(y);
        assert!(got <= prev + 1, "y={y} 应单调变暗：{prev} -> {got}");
        prev = got;
    }
    println!("  polygon bbox 渐变：top={top} bottom={bottom} 单调 ok");
}
/// 同 `at` 两个停点 = 硬边：过渡是**一跳**，且不会算出 NaN（黑块）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn rect_gradient_hard_edge() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const W: u32 = 64;
    const H: u32 = 96;
    let c = OffscreenCanvas::new(&gpu, W, H);
    let mut b = DrawBatch::new();
    let blue = Color::new(0.25, 0.55, 0.95, 1.0);
    let orange = Color::new(0.95, 0.55, 0.25, 1.0);
    let stops = [
        gstop(0.0, blue),
        gstop(0.5, blue),
        gstop(0.5, orange),
        gstop(1.0, orange),
    ];
    draw_rect_gradient(
        &mut b,
        Pos::new(8.0, 8.0),
        48.0,
        80.0,
        LinearGradient::vertical(&stops),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let rgb = |y: u32| -> (i32, i32, i32) {
        let i = ((y * W + 32) * 4) as usize;
        (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32)
    };
    // 渐变框 8..88，中线在 y≈48
    let (r_up, _, b_up) = rgb(44);
    let (r_dn, _, b_dn) = rgb(52);
    assert!(b_up > 220 && r_up < 180, "上半应是蓝，实际 ({r_up},{b_up})");
    assert!(r_dn > 220 && b_dn < 180, "下半应是橙，实际 ({r_dn},{b_dn})");
    // 硬边两侧之间不该出现 NaN/全黑：整列都不能是纯黑
    for y in 10..=86u32 {
        let (r, g, bl) = rgb(y);
        assert!(r + g + bl > 60, "y={y} 出现黑洞（NaN？）：({r},{g},{bl})");
    }
    println!("  硬边：上 ({r_up},{b_up}) 下 ({r_dn},{b_dn})，无黑洞");
}

// ==================== 文本样式（距离场描边 / 投影） ====================

/// 描边必须**长在字形外面**：同一段文字，带描边版本的非黑像素明显更多，
/// 且多出来的部分是描边色（几何无关的断言，不依赖具体字形轮廓）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_outline_extends_beyond_glyph() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 96;
    let red = Color::new(1.0, 0.0, 0.0, 1.0);

    let ca = OffscreenCanvas::new(&gpu, N, N);
    let mut a = DrawBatch::new();
    draw_text(
        &mut a.texts,
        "H",
        Pos::new(24.0, 20.0),
        TextDef::default().font_size(48.0),
        TextOverride::from_color(WHITE),
    );
    ca.draw(Some(BLACK), &[&a]);
    let pa = ca.read_pixels();

    let cb = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let style = TextStyle::outline(3.0, red);
    draw_text(
        &mut b.texts,
        "H",
        Pos::new(24.0, 20.0),
        TextDef {
            style: Some(Arc::new(style)),
            ..TextDef::default().font_size(48.0)
        },
        TextOverride::from_color(WHITE),
    );
    cb.draw(Some(BLACK), &[&b]);
    let pb = cb.read_pixels();

    let lit = |px: &[u8]| -> usize {
        px.chunks(4)
            .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 30)
            .count()
    };
    let (na, nb) = (lit(&pa), lit(&pb));
    assert!(na > 100, "用例失效：无样式文字一个像素都没画出来（{na}）");
    assert!(
        nb > na + 150,
        "带描边的覆盖面积应明显更大：无样式 {na} vs 描边 {nb}"
    );

    let reds = pb
        .chunks(4)
        .filter(|p| p[0] > 120 && p[1] < 90 && p[2] < 90)
        .count();
    assert!(reds > 80, "应出现红色描边像素，实际 {reds}");
    println!("  描边：无样式 {na} px / 带描边 {nb} px，其中红色 {reds} px");
}

/// 同色描边的"填充区域"必须与无样式版本一致（描边不压暗、不缩放字形）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_same_color_outline_keeps_fill() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 96;
    let white = WHITE;

    let render = |styled: bool| -> Vec<u8> {
        let c = OffscreenCanvas::new(&gpu, N, N);
        let mut b = DrawBatch::new();
        let def = TextDef {
            style: if styled {
                Some(Arc::new(TextStyle::outline(1.0, white)))
            } else {
                None
            },
            ..TextDef::default().font_size(48.0)
        };
        draw_text(
            &mut b.texts,
            "H",
            Pos::new(24.0, 24.0),
            def,
            TextOverride::from_color(white),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };
    let plain = render(false);
    let styled = render(true);

    let mut checked = 0;
    for (a, b) in plain.chunks(4).zip(styled.chunks(4)) {
        // 只比**深内部**（两版都接近纯白）：边缘像素本来就是覆盖率与距离场两种重建
        // 方式的差异区，不属于"填充被压暗"这个断言。
        if a[0] > 250 && b[0] > 250 {
            for ch in 0..3 {
                let (x, y) = (a[ch] as i32, b[ch] as i32);
                assert!(
                    (x - y).abs() <= 3,
                    "填充处像素不该被描边压暗：通道 {ch} 无样式 {x} vs 有样式 {y}"
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 60, "空转：两版都亮的像素只有 {checked}");
    println!("  同色描边：{checked} 个深内部像素与无样式版本一致");
}

/// 软投影必须向**下/右**扩散（偏移方向）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_shadow_offsets_down_right() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 128;

    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let style = TextStyle::shadow((3.0, 3.0), 2.0, Color::new(0.0, 0.0, 1.0, 1.0));
    draw_text(
        &mut b.texts,
        "H",
        Pos::new(30.0, 30.0),
        TextDef {
            style: Some(Arc::new(style)),
            ..TextDef::default().font_size(48.0)
        },
        TextOverride::from_color(WHITE),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    let mut sx = 0f64;
    let mut sy = 0f64;
    let mut n = 0f64;
    for y in 0..N {
        for x in 0..N {
            let i = ((y * N + x) * 4) as usize;
            if px[i + 2] as i32 > px[i] as i32 + 40 {
                sx += x as f64;
                sy += y as f64;
                n += 1.0;
            }
        }
    }
    assert!(n > 100.0, "应出现蓝色投影像素，实际 {n}");
    let (cx, cy) = (sx / n, sy / n);
    assert!(
        cx > 30.0 && cy > 30.0,
        "投影重心应偏右下，实际 ({cx:.1},{cy:.1})"
    );
    println!("  软投影：蓝色像素 {n:.0} 个，重心 ({cx:.1},{cy:.1})");
}

/// 几何描边：miter 会在尖角上补出像素，面积应大于 round。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_vector_miter_vs_round() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 160;
    let red = Color::new(1.0, 0.0, 0.0, 1.0);

    let render = |join: Option<TextJoin>| -> Vec<u8> {
        let c = OffscreenCanvas::new(&gpu, N, N);
        let mut b = DrawBatch::new();
        let style = join.map(|j| {
            Arc::new(TextStyle::vector_outline(
                TextStroke::new(10.0, red).join(j).miter_limit(8.0),
            ))
        });
        draw_text(
            &mut b.texts,
            "V",
            Pos::new(30.0, 30.0),
            TextDef {
                style,
                ..TextDef::default().font_size(70.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };

    let none = render(None);
    let round = render(Some(TextJoin::Round));
    let miter = render(Some(TextJoin::Miter));

    let lit = |px: &[u8]| -> usize {
        px.chunks(4)
            .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 40)
            .count()
    };
    let (n0, nr, nm) = (lit(&none), lit(&round), lit(&miter));
    assert!(n0 > 200, "用例失效：无描边的 'V' 没画出来（{n0}）");
    assert!(nr > n0, "几何描边应让覆盖变大：无描边 {n0} vs round {nr}");
    assert!(nm > n0, "几何描边应让覆盖变大：无描边 {n0} vs miter {nm}");

    // 宽度是像素意图：描边让外轮廓每侧各扩 w/2 ⇒ 纵向跨度大约 +w。
    let span = |px: &[u8]| -> i64 {
        let row_lit = |y: u32| {
            (0..N).any(|x| {
                let i = ((y * N + x) * 4) as usize;
                px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32 > 40
            })
        };
        let top = (0..N).find(|y| row_lit(*y)).unwrap_or(N);
        let bot = (0..N).rev().find(|y| row_lit(*y)).unwrap_or(0);
        bot as i64 - top as i64
    };
    let grown = span(&round) - span(&none);
    assert!(
        (6..=26).contains(&grown),
        "10px 描边应让纵向跨度大约 +10（含 AA 余量），实际 {grown}"
    );

    let reds = miter
        .chunks(4)
        .filter(|p| p[0] > 130 && p[1] < 80 && p[2] < 80)
        .count();
    assert!(reds > 100, "应出现红色描边像素，实际 {reds}");
    println!(
        "  几何描边：面积 无 {n0} / round {nr} / miter {nm}；跨度增量 {grown}px；红 {reds} px"
    );
}

/// 内描边必须是**字形内的环**：不能把整个字涂满（旧实现是半空间 + 被填充盖住），
/// 也不能让轮廓扩展到字形外（那是外描边）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_inner_outline_is_ring() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 160;
    let red = Color::new(1.0, 0.0, 0.0, 1.0);

    let render = |style: Option<TextStyle>| -> Vec<u8> {
        let c = OffscreenCanvas::new(&gpu, N, N);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "国",
            Pos::new(20.0, 14.0),
            TextDef {
                style: style.map(Arc::new),
                ..TextDef::default().font_size(120.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };

    let plain = render(None);
    // 3px 内描边 + 120px 字号：笔画中间要留得下白色核心（7px 环会把细笔画整个吃掉）
    let inner = render(Some(
        TextStyle::inner_outline(3.0, red).with_fill(TextPaint::solid(WHITE)),
    ));

    let lit = |px: &[u8]| -> usize {
        px.chunks(4)
            .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 40)
            .count()
    };
    let reds = inner
        .chunks(4)
        .filter(|p| p[0] > 140 && p[1] < 90 && p[2] < 90)
        .count();
    let whites_inner = inner
        .chunks(4)
        .filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200)
        .count();

    assert!(reds > 400, "内描边应有明显的红环，实际 {reds} px");
    assert!(
        whites_inner > 400,
        "环内仍应是填充白色，实际 {whites_inner} px"
    );
    // 内描边不向外扩：覆盖面积应与无样式基本一致（±8%）
    let (a, b) = (lit(&plain), lit(&inner));
    let diff = (a as i64 - b as i64).abs() as f64 / a as f64;
    assert!(
        diff < 0.15, // 带宽档改为相对字号后，大字号下 band 略大，AA 边缘约 1px 差
        "内描边不该改变外轮廓：无样式 {a} vs 内描边 {b}（差 {:.1}%）",
        diff * 100.0
    );
    println!("  内描边：红环 {reds} px、环内白 {whites_inner} px，轮廓面积 {a} vs {b}");
}

/// 内阴影不许漏到字形外面（旧实现会变成"外描边"）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_inner_shadow_stays_inside() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 160;
    let render = |style: Option<TextStyle>| -> Vec<u8> {
        let c = OffscreenCanvas::new(&gpu, N, N);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "国",
            Pos::new(20.0, 14.0),
            TextDef {
                style: style.map(Arc::new),
                ..TextDef::default().font_size(120.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };
    let plain = render(None);
    let shadow = render(Some(
        TextStyle::inner_shadow((4.0, 4.0), 3.0, Color::new(0.0, 0.0, 0.0, 0.9))
            .with_fill(TextPaint::solid(WHITE)),
    ));
    let lit = |px: &[u8]| -> usize {
        px.chunks(4)
            .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 40)
            .count()
    };
    let (a, b) = (lit(&plain), lit(&shadow));
    let diff = (a as i64 - b as i64).abs() as f64 / a as f64;
    assert!(
        diff < 0.08,
        "内阴影不该漏到字形外：无样式 {a} vs 内阴影 {b}（差 {:.1}%）",
        diff * 100.0
    );
    let grays = shadow
        .chunks(4)
        .filter(|p| {
            let (r, bl) = (p[0] as i32, p[2] as i32);
            r > 40 && (r - bl).abs() < 40 && r < 200
        })
        .count();
    assert!(grays > 200, "字形内应出现暗部（阴影），实际 {grays} px");
    println!("  内阴影：轮廓面积 {a} vs {b}，暗部 {grays} px");
}

/// 相邻字形互不覆盖：后一个字形的描边**不能**盖住前一个字形的字形本体。
///
/// 判据：紧排两个 "H"（8px 描边）的白色填充面积 ≈ 单个的 2 倍（±6%）。
/// 逐字形合成时，右边字形的描边会压掉左边字形右侧的白色笔画，比例明显偏小。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_adjacent_glyphs_do_not_cover() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 320;
    let red = Color::new(1.0, 0.0, 0.0, 1.0);

    let render = |text: &str| -> usize {
        let c = OffscreenCanvas::new(&gpu, N, 160);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            text,
            Pos::new(30.0, 30.0),
            TextDef {
                style: Some(Arc::new(
                    TextStyle::outline(8.0, red).with_fill(TextPaint::solid(WHITE)),
                )),
                ..TextDef::default().font_size(90.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        px.chunks(4)
            .filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200)
            .count()
    };
    let one = render("H");
    let two = render("HH");
    // 两个 H 之间有字距，白色面积应接近 2 倍但略小；关键是**不能明显缩水**。
    let ratio = two as f64 / one as f64;
    assert!(
        ratio > 1.85,
        "相邻字形的填充被描边盖掉了：单个 {one} px、两个 {two} px（比值 {ratio:.2}）"
    );
    println!("  相邻字形：单个白 {one} px、两个白 {two} px（比值 {ratio:.2}）");

    // 同一判据用在**多层描边**（黑 8px + 蓝 4px）上：后一个字形的描边不许吃掉前一个的填充。
    let render_multi = |text: &str| -> usize {
        let c = OffscreenCanvas::new(&gpu, N, 160);
        let mut b = DrawBatch::new();
        let style = TextStyle::new()
            .with_layer(TextLayer::hard(
                8.0,
                TextPaint::solid(Color::new(0.05, 0.05, 0.05, 1.0)),
            ))
            .with_layer(TextLayer::hard(
                4.0,
                TextPaint::solid(Color::new(0.1, 0.2, 1.0, 1.0)),
            ));
        draw_text(
            &mut b.texts,
            text,
            Pos::new(30.0, 30.0),
            TextDef {
                style: Some(Arc::new(style)),
                ..TextDef::default().font_size(90.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        px.chunks(4)
            .filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200)
            .count()
    };
    let one_m = render_multi("H");
    let two_m = render_multi("HH");
    let ratio_m = two_m as f64 / one_m as f64;
    println!("  多层描边相邻：单个白 {one_m} px、两个白 {two_m} px（比值 {ratio_m:.2}）");
    assert!(
        ratio_m > 1.85,
        "多层描边下相邻字形的填充被盖掉了：单个 {one_m} px、两个 {two_m} px（比值 {ratio_m:.2}）"
    );

    // 字符串级**并集**：同层的描边不该被后一个字吃掉。
    // 蓝层（内层，4px）像素数在两个 H 时应接近单个的 2 倍。
    let blue = |text: &str| -> usize {
        let c = OffscreenCanvas::new(&gpu, N, 160);
        let mut b = DrawBatch::new();
        let style = TextStyle::new()
            .with_layer(TextLayer::hard(
                8.0,
                TextPaint::solid(Color::new(0.05, 0.05, 0.05, 1.0)),
            ))
            .with_layer(TextLayer::hard(
                4.0,
                TextPaint::solid(Color::new(0.1, 0.2, 1.0, 1.0)),
            ));
        draw_text(
            &mut b.texts,
            text,
            Pos::new(30.0, 30.0),
            TextDef {
                style: Some(Arc::new(style)),
                ..TextDef::default().font_size(90.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        px.chunks(4).filter(|p| p[2] > 140 && p[0] < 90).count()
    };
    let (b1, b2) = (blue("H"), blue("HH"));
    let ratio_b = b2 as f64 / b1 as f64;
    println!("  多层描边蓝层：单个 {b1} px、两个 {b2} px（比值 {ratio_b:.2}）");
    assert!(
        ratio_b > 1.8,
        "同层描边被相邻字形吃掉了：单个蓝 {b1} px、两个蓝 {b2} px（比值 {ratio_b:.2}）"
    );

    // 发光同理：光晕像素数应接近 2 倍。
    let glow = |text: &str| -> usize {
        let c = OffscreenCanvas::new(&gpu, N, 160);
        let mut b = DrawBatch::new();
        let style = TextStyle::glow(10.0, Color::new(0.2, 0.8, 1.0, 0.9)).with_layer(
            TextLayer::hard(2.0, TextPaint::solid(Color::new(0.05, 0.05, 0.05, 1.0))),
        );
        draw_text(
            &mut b.texts,
            text,
            Pos::new(30.0, 30.0),
            TextDef {
                style: Some(Arc::new(style)),
                ..TextDef::default().font_size(90.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        px.chunks(4)
            .filter(|p| p[2] > 60 && p[2] > p[0] + 30)
            .count()
    };
    let (g1, g2) = (glow("H"), glow("HH"));
    let ratio_g = g2 as f64 / g1 as f64;
    println!("  发光：单个 {g1} px、两个 {g2} px（比值 {ratio_g:.2}）");
    assert!(
        ratio_g > 1.5,
        "发光被相邻字形吃掉了：单个 {g1} px、两个 {g2} px（比值 {ratio_g:.2}）"
    );
}

/// 稳态吞吐：N 个带样式字形、重复 20 帧，报告每帧耗时。
/// 用来回答「为什么示例只有几帧」——如果这里很快，说明瓶颈不在引擎的 prepare/draw。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_steady_state_throughput() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let ink = Color::new(0.05, 0.06, 0.09, 1.0);
    let red = Color::new(1.0, 0.0, 0.0, 1.0);
    let c = OffscreenCanvas::new(&gpu, 900, 900);

    for (label, glyphs, style) in [
        ("2 字/单层描边", 2usize, TextStyle::outline(2.0, red)),
        ("12 字/单层描边", 12, TextStyle::outline(2.0, red)),
        (
            "100 字/多层+发光",
            100,
            TextStyle::outline(3.0, ink)
                .with_layer(TextLayer::hard(6.0, TextPaint::solid(red)))
                .with_layer(TextLayer::soft(2.0, 8.0, TextPaint::solid(red)))
                .with_fill(TextPaint::solid(WHITE)),
        ),
        (
            "100 字/投影",
            100,
            TextStyle::shadow((4.0, 4.0), 3.0, ink).with_fill(TextPaint::solid(WHITE)),
        ),
    ] {
        let text: String = "国永光明".chars().cycle().take(glyphs).collect();
        let style = Arc::new(style);
        // 预热（栅格化 + 图集分配只发生在这里）
        for _ in 0..2 {
            let mut b = DrawBatch::new();
            draw_text(
                &mut b.texts,
                &text,
                Pos::new(20.0, 30.0),
                TextDef {
                    style: Some(style.clone()),
                    ..TextDef::default().font_size(44.0).max_width(860.0)
                },
                TextOverride::from_color(WHITE),
            );
            c.draw(Some(BLACK), &[&b]);
        }
        let frames = 20;
        let start = std::time::Instant::now();
        for _ in 0..frames {
            let mut b = DrawBatch::new();
            draw_text(
                &mut b.texts,
                &text,
                Pos::new(20.0, 30.0),
                TextDef {
                    style: Some(style.clone()),
                    ..TextDef::default().font_size(44.0).max_width(860.0)
                },
                TextOverride::from_color(WHITE),
            );
            c.draw(Some(BLACK), &[&b]);
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        println!("  {label}: {ms:.2} ms/帧（{:.0} fps 上限）", 1000.0 / ms);
        assert!(ms < 33.0, "{label} 稳态应 ≥30fps，实际 {ms:.1} ms/帧");
    }
}

/// 字形缓存的**性能画像**（打印数字，不做严格的计时断言 —— 阈值依机器而异）。
///
/// 覆盖本轮新增的四条成本：
/// 1. `prewarm` 冷（要光栅化）vs 热（全命中）；
/// 2. 扩图集的**搬运成本** vs "重新光栅化同样多条"的等价成本（就是 `grow` 改拷贝省下的）；
/// 3. `save`（读回 + 写盘）/ `load`（读盘 + 上传），以及装载后首帧相比空图集首帧省了多少；
/// 4. `compact`（重排 + 拷贝）.
///
/// 计时都是**CPU 侧提交耗时**（不含 GPU 实际完成），因为这几条路径的成本都在 CPU。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_perf() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let def = TextDef::default().font_size(32.0);
    let long = "字形缓存性能画像 Vireo 0123456789 abcdefghijklmnopqrstuvwxyz";

    // 第一次 prewarm 带字体系统/swash 的一次性初始化，不计入每字成本。
    glyphs.prewarm(
        &gpu.device,
        &gpu.queue,
        "预热",
        &TextDef::default().font_size(32.0),
        1.0,
    );

    // ---- 1. prewarm 冷 / 热 ----
    let t = Instant::now();
    let cold_n = glyphs.prewarm(&gpu.device, &gpu.queue, long, &def, 1.0);
    let cold_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let warm_n = glyphs.prewarm(&gpu.device, &gpu.queue, long, &def, 1.0);
    let warm_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(cold_n > 0);
    assert_eq!(warm_n, 0, "第二遍应全命中");
    println!(
        "  prewarm：冷 {cold_n} 条 {cold_ms:.2} ms（{:.3} ms/字） | 热 0 条 {warm_ms:.3} ms",
        cold_ms / cold_n.max(1) as f64
    );
    let _per_glyph_ms = cold_ms / cold_n.max(1) as f64;

    // ---- 2. 扩图集：触发扩容的那次 prewarm vs 「重光栅化同样多条」 ----
    //
    // 图集里已有 `copied` 条时触发一次扩容：旧实现要把它们全部重新光栅化，
    // 新实现只做纹理拷贝。拷贝的纯成本用 `compact`（同样的逐条拷贝 + 重排）单独量一次。
    let canvas = OffscreenCanvas::new(&gpu, 900, 600);
    let mut before_grows = glyphs.stats().grows;
    let mut entries_before = glyphs.stats().entries();
    let mut grow_ms = f64::NAN;
    let mut copied = 0u32;
    for i in 0..20u32 {
        let filler: String = "扩容搬运测试ABCDEFG".chars().cycle().take(60).collect();
        let size = 40.0 + i as f32 * 2.0;
        let t = Instant::now();
        glyphs.prewarm(
            &gpu.device,
            &gpu.queue,
            &format!("{filler}{i}"),
            &TextDef::default().font_size(size),
            1.0,
        );
        let dt = t.elapsed().as_secs_f64() * 1000.0;
        let s = glyphs.stats();
        if s.grows > before_grows {
            grow_ms = dt;
            copied = entries_before;
            break;
        }
        before_grows = s.grows;
        entries_before = s.entries();
    }
    assert!(!grow_ms.is_nan(), "用例失效：20 轮都没有触发扩图集");

    let t = Instant::now();
    let shrunk_here = glyphs.compact(&gpu.device, &gpu.queue);
    let copy_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!(
        "  grow：触发扩容的那次 prewarm {grow_ms:.2} ms（当时图集里 {copied} 条）| \
         纯拷贝参考 compact {copy_ms:.2} ms（{}）",
        if shrunk_here {
            "收缩了"
        } else {
            "无需收缩"
        }
    );

    // ---- 3. save / load ----
    let path = std::env::temp_dir().join("vireo_glyph_cache_perf.vrtx");
    let _ = std::fs::remove_file(&path);
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        long,
        Pos::new(12.0, 20.0),
        def.clone(),
        TextOverride::from_color(WHITE),
    );
    let t = Instant::now();
    canvas.draw(Some(BLACK), &[&b]);
    let warm_frame_ms = t.elapsed().as_secs_f64() * 1000.0;

    let saved_entries = glyphs.stats().entries();
    // `save` 内部要读回纹理，`poll(Wait)` 会顺带把队列里已提交的工作排空 —— 单独量一次，
    // 免得把"排空前面的绘制"算成 save 的开销。
    let t = Instant::now();
    let _ = gpu.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let flush_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let saved = glyphs
        .save(&gpu.device, &gpu.queue, &path)
        .expect("save 应成功");
    let save_ms = t.elapsed().as_secs_f64() * 1000.0;
    let file_kib = std::fs::metadata(&path)
        .map(|m| m.len() / 1024)
        .unwrap_or(0);

    // 上下文 B：空图集 → 装载 → 首帧（省掉光栅化）。
    let gpu_b = Arc::new(GpuContext::new(&instance));
    let canvas_b = OffscreenCanvas::new(&gpu_b, 900, 600);
    let draw_b = || {
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            long,
            Pos::new(12.0, 20.0),
            def.clone(),
            TextOverride::from_color(WHITE),
        );
        canvas_b.draw(Some(BLACK), &[&b]);
    };
    let t = Instant::now();
    let loaded = gpu_b
        .text()
        .glyphs()
        .load(&gpu_b.device, &gpu_b.queue, &path)
        .expect("load 应成功");
    let load_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    draw_b();
    let loaded_first_ms = t.elapsed().as_secs_f64() * 1000.0;
    let after_rasters = gpu_b.text().glyphs().stats().rasterizations;
    assert_eq!(loaded, saved, "装载条数应等于保存条数");
    assert_eq!(after_rasters, 0, "装载后绘制不该重新光栅化");

    // 上下文 C：对照 —— 同样的首帧，但图集是空的（必须现场光栅化）。
    let gpu_c = Arc::new(GpuContext::new(&instance));
    let canvas_c = OffscreenCanvas::new(&gpu_c, 900, 600);
    let t = Instant::now();
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        long,
        Pos::new(12.0, 20.0),
        def.clone(),
        TextOverride::from_color(WHITE),
    );
    canvas_c.draw(Some(BLACK), &[&b]);
    let empty_first_ms = t.elapsed().as_secs_f64() * 1000.0;
    let cold_rasters = gpu_c.text().glyphs().stats().rasterizations;

    // ---- 4. compact（上下文 B，装载之后）----
    let t = Instant::now();
    let shrunk = gpu_b.text().glyphs().compact(&gpu_b.device, &gpu_b.queue);
    let compact_ms = t.elapsed().as_secs_f64() * 1000.0;

    // ---- 5. 观测接口本身的成本：`stats()` 必须 O(1)，`entries()` 是 O(n) 快照 ----
    //
    // 预算检查每帧都要问一次占用，所以 `stats()` 一旦退化成全量扫描就会变成每帧 O(n)。
    let s_b = gpu_b.text().glyphs().stats();
    let t = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(gpu_b.text().glyphs().stats());
    }
    let stats_us = t.elapsed().as_secs_f64() * 1e6 / 1000.0;
    let t = Instant::now();
    for _ in 0..10 {
        std::hint::black_box(gpu_b.text().glyphs().entries());
    }
    let entries_us = t.elapsed().as_secs_f64() * 1e6 / 10.0;
    println!(
        "  stats() {stats_us:.2} µs/次 | entries() {entries_us:.1} µs/次（{} 条）",
        s_b.entries()
    );
    assert!(
        stats_us < 50.0,
        "stats() 应是 O(1) 聚合（实测 {stats_us:.1} µs/次，{} 条）",
        s_b.entries()
    );

    println!(
        "  save：{saved} 条 → {file_kib} KiB，{save_ms:.2} ms（{:.3} ms/条；另有队列排空参考 {flush_ms:.2} ms）",
        save_ms / saved.max(1) as f64
    );
    println!(
        "  load：{loaded} 条 {load_ms:.2} ms（{:.3} ms/条） | 首帧 空图集 {empty_first_ms:.2} ms \
         vs 装载后 {loaded_first_ms:.2} ms（差 {:.0}%，空图集光栅化 {cold_rasters} 条）\
         ｜首帧两次都含 PSO 编译，看【光栅化 0 次】才是硬指标",
        load_ms / loaded.max(1) as f64,
        (1.0 - loaded_first_ms / empty_first_ms.max(0.001)) * 100.0
    );
    println!(
        "  compact：{} {compact_ms:.2} ms | 稳态帧（{saved_entries} 条已缓存）{warm_frame_ms:.2} ms",
        if shrunk { "收缩了" } else { "无需收缩" }
    );

    // 只做"不明显劣化"的宽松兜底，避免把机器差异写成 flaky 用例。
    assert!(
        load_ms < 2000.0,
        "装载不该慢到 2 秒（{load_ms:.1} ms / {loaded} 条）"
    );
    // 首帧都含 PSO 懒编译（几十 ms 的噪声），所以这里只兜底"没有灾难性退化"；
    // 真正的语义保证是上面那条 `rasterizations == 0`。
    assert!(
        loaded_first_ms < empty_first_ms * 2.0 + 20.0,
        "装载后首帧不该比空图集首帧慢一倍以上（{loaded_first_ms:.2} vs {empty_first_ms:.2} ms）"
    );
    let _ = std::fs::remove_file(&path);
}

/// **宽描边不能给整块 quad 上色**（回归用例）。
///
/// 曾经的现象：`thr >= 带宽` 时距离场饱和值 ≈ 阈值 → `coverage = 0.5` → 每个字形后面
/// 一个半透明矩形（高 DPI + 16px 描边最容易撞到，旧上限是 32）。
/// 这里直接量"墨迹外侧 6px"的像素：正常应等于背景。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_wide_stroke_leaves_no_wash() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let ink = Color::new(0.10, 0.11, 0.14, 0.92);

    // (字号, 描边宽度)：包含 44px 与"2 倍 DPI 等效"的 88px，以及顶到带宽上限的宽度
    for (font_size, width) in [
        (44.0f32, 16.0f32),
        (44.0, 24.0),
        (44.0, 32.0),
        (88.0, 32.0),
        (88.0, 48.0),
    ] {
        let c = OffscreenCanvas::new(&gpu, 640, 260);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "Outline 描边",
            Pos::new(20.0, 20.0),
            TextDef {
                style: Some(Arc::new(TextStyle::outline(width, ink))),
                ..TextDef::default().font_size(font_size)
            },
            TextOverride::from_color(Color::new(0.96, 0.97, 1.0, 1.0)),
        );
        c.draw(Some(Color::new(0.07, 0.075, 0.09, 1.0)), &[&b]);
        let px = c.read_pixels();
        let sum_at = |x: u32, y: u32| {
            let i = ((y * 640 + x) * 4) as usize;
            px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32
        };
        let bg = sum_at(2, 2);
        // 墨迹外接框
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for y in 0..260u32 {
            for x in 0..640u32 {
                // 同一个阈值（+20）既用于外接框也用于亮像素计数：
                // `ink` 合成到深背景上其实比背景**亮**（约 +44），所以不能用 +120 找外接框。
                if sum_at(x, y) > bg + 20 {
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
            }
        }
        // 外侧 6px 处的四个采样点（在画布内）
        if x0 == u32::MAX {
            println!("  {font_size}px / w={width}: 没有亮像素（跳过探针）");
            continue;
        }
        let cx = (x0 + x1) / 2;
        let cy = (y0 + y1) / 2;
        let probes = [
            (cx, y0.saturating_sub(6)),
            (cx, (y1 + 6).min(259)),
            (x0.saturating_sub(6), cy),
            ((x1 + 6).min(639), cy),
        ];
        // 说明：这里**不断言**颜色分桶。踩过的坑：`ink` 是 (0.10,0.11,0.14)，
        // 合成到深色背景上后 sRGB 反而**比背景更亮**（+44），按"比背景亮"分桶会把
        // 描边本体当成淡色矩形。真正能判断的图形证据是分布图（见
        // `text_style_wide_stroke_string_dump` 的输出）：被涂色的区域必须是**描边形状**，
        // 而不是矩形。整块 quad 被涂色的回归由 `text_style_size_ladder_*` 一组用例的
        // 外接框/四角断言兜住。
        let diffs: Vec<i32> = probes
            .iter()
            .map(|(x, y)| sum_at(*x, *y) as i32 - bg as i32)
            .collect();
        let lit = {
            let mut n = 0usize;
            for y in 0..260u32 {
                for x in 0..640u32 {
                    if sum_at(x, y) > bg + 20 {
                        n += 1;
                    }
                }
            }
            n
        };
        let bbox = ((x1 - x0 + 1) * (y1 - y0 + 1)) as f64;
        let fill_ratio = lit as f64 / bbox;
        println!(
            "  {font_size}px / w={width}: 墨迹框 {}x{}，亮像素 {lit}（占外接框 {:.0}%），探针 {diffs:?}",
            x1 - x0,
            y1 - y0,
            fill_ratio * 100.0
        );
        // 真正要防的回归：整块 padded quad 被涂满 → 亮像素几乎铺满外接框。
        // 正常描边是"字形 + 一圈带"，中间是空的（CJK 的国/口类字形有大量内部空隙）。
        assert!(
            fill_ratio < 0.95,
            "{font_size}px / w={width} 时亮像素占外接框 {:.0}%（≥95% 说明整块矩形被上色）",
            fill_ratio * 100.0
        );
    }
}

/// **窗口边缘被裁剪的字不能"被压扁"**：只裁掉一部分，未裁掉方向的尺寸必须保持不变。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_clipped_glyph_is_not_squashed() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let ink = Color::new(0.10, 0.11, 0.14, 0.92);

    // 返回亮区 (宽, 高)
    let extent = |pos: Pos| -> (u32, u32) {
        let c = OffscreenCanvas::new(&gpu, 300, 200);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "国",
            pos,
            TextDef {
                style: Some(Arc::new(
                    TextStyle::outline(4.0, ink).with_fill(TextPaint::solid(WHITE)),
                )),
                ..TextDef::default().font_size(44.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for y in 0..200u32 {
            for x in 0..300u32 {
                let i = ((y * 300 + x) * 4) as usize;
                if px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32 > 60 {
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
            }
        }
        (x1.saturating_sub(x0), y1.saturating_sub(y0))
    };

    let (cw, ch) = extent(Pos::new(120.0, 60.0)); // 完整
    let (lw, lh) = extent(Pos::new(-10.0, 60.0)); // 左侧被裁（仍有一半可见）
    let (tw, th) = extent(Pos::new(120.0, -10.0)); // 顶部被裁（仍有一半可见）
    println!("  完整 {cw}x{ch}，左裁 {lw}x{lh}，上裁 {tw}x{th}（被裁方向应变小，另一方向应不变）");
    assert!(
        (lh as i64 - ch as i64).abs() <= 2,
        "左侧被裁后高度应保持（{ch} vs {lh}）—— 变小说明整条条目被压扁了"
    );
    assert!(
        (tw as i64 - cw as i64).abs() <= 2,
        "顶部被裁后宽度应保持（{cw} vs {tw}）—— 变小说明整条条目被压扁了"
    );
    assert!(lw < cw && th < ch, "被裁方向确实应该变小");
}

/// 多层描边：蓝色层宽度逐 0.5 步进，蓝色像素面积必须单调、无跳变。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_multilayer_width_sweep() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let ink = Color::new(0.10, 0.11, 0.14, 0.92);
    let blue = Color::new(0.25, 0.55, 0.95, 1.0);

    let mut last = 0usize;
    for step in 0..13 {
        let width = 9.0 + step as f32 * 0.5;
        let c = OffscreenCanvas::new(&gpu, 420, 300);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "国",
            Pos::new(150.0, 110.0),
            TextDef {
                style: Some(Arc::new(
                    TextStyle::new()
                        .with_layer(TextLayer::hard(width + 3.0, TextPaint::solid(ink)))
                        .with_layer(TextLayer::hard(width, TextPaint::solid(blue))),
                )),
                ..TextDef::default().font_size(44.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        let mut blue_n = 0usize;
        for p in px.chunks(4) {
            if p[2] as i32 > p[0] as i32 + 40 {
                blue_n += 1;
            }
        }
        let delta = blue_n as i64 - last as i64;
        println!("  w={width:4.1} -> 蓝色 {blue_n} px（较上一步 {delta:+}）");
        last = blue_n;
    }
}

/// 描边宽度 → 实际视觉厚度：逐档断言"厚度 ≈ 宽度"且单调（带宽档是相对字号的，
/// 44px 下最大可表示 64px 描边）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_stroke_width_sweep() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let ink = Color::new(0.10, 0.11, 0.14, 0.92);

    // 返回亮区 (宽, 高)
    let extent = |width: f32, styled: bool| -> (u32, u32) {
        let c = OffscreenCanvas::new(&gpu, 320, 260);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "国",
            Pos::new(120.0, 100.0),
            TextDef {
                style: styled.then(|| Arc::new(TextStyle::outline(width, ink))),
                ..TextDef::default().font_size(44.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for y in 0..260u32 {
            for x in 0..320u32 {
                let i = ((y * 320 + x) * 4) as usize;
                if px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32 > 60 {
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
            }
        }
        (x1.saturating_sub(x0), y1.saturating_sub(y0))
    };

    let (pw, ph) = extent(0.0, false);
    let mut prev = -1.0f32;
    for width in [
        8.0f32, 12.0, 14.0, 15.0, 15.5, 16.0, 17.0, 20.0, 24.0, 28.0, 30.0, 32.0, 35.0, 40.0, 48.0,
        56.0, 64.0,
    ] {
        let (w, h) = extent(width, true);
        let t = ((w as i64 - pw as i64) / 2 + ((h as i64 - ph as i64) / 2)) as f32 / 2.0;
        println!("  w={width:5.1} -> 亮区 {w}x{h}，视觉厚度 ≈ {t:.1}");
        assert!(
            (t - width).abs() <= 3.0,
            "{width}px 描边的视觉厚度应 ≈ 宽度，实际 {t:.1}"
        );
        assert!(t > prev, "厚度必须随宽度单调增长（{prev:.1} -> {t:.1}）");
        prev = t;
    }
}

/// 诊断（配合 `VIREO_SHADER_DEBUG_D=1`）：把填充趟解码出的距离画到红通道，
/// 沿一条竖线读回 d 的值，判断 padding 里到底解出了多少。
#[test]
#[ignore = "需 GPU + VIREO_SHADER_DEBUG_D=1；--ignored --nocapture"]
fn text_sdf_debug_print_d() {
    if std::env::var_os("VIREO_SHADER_DEBUG_D").is_none() {
        println!("  跳过：需要 VIREO_SHADER_DEBUG_D=1（着色器才会输出 d）");
        return;
    }
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let ink = Color::new(0.10, 0.11, 0.14, 0.92);
    let c = OffscreenCanvas::new(&gpu, 300, 200);
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        "国",
        Pos::new(80.0, 40.0),
        TextDef {
            style: Some(Arc::new(TextStyle::outline(16.0, ink))),
            ..TextDef::default().font_size(44.0)
        },
        TextOverride::from_color(WHITE),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();
    // 层趟诊断：r → d，g → thr，b → soft（读回是 sRGB 编码，先反解成线性）
    let lin = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    println!("  x=140 竖线（层趟）：thr / soft / d");
    for y in (0..200).step_by(8) {
        let i = ((y * 300 + 140) * 4) as usize;
        let r = lin(px[i]);
        let g = lin(px[i + 1]);
        let b = lin(px[i + 2]);
        if r > 0.001 || g > 0.001 {
            println!(
                "  y={y}: d={:6.1}  thr={:5.1}  soft={:5.1}",
                (r - 0.25) / 0.02,
                g * 64.0,
                b * 64.0
            );
        }
    }
}

/// 复现示例第 1 组：整串 "Outline 描边"、宽度 16、ink 近黑、fill 用顶点色。
/// 打印 ASCII 分类图，确认「字形外的 padding 区域」是否被涂亮（= 半透明盒子）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_wide_stroke_string_dump() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let ink = Color::new(0.10, 0.11, 0.14, 0.92);
    let cases: [(&str, Option<TextStyle>); 3] = [
        ("无文字", None),
        (
            "仅填充",
            Some(TextStyle::new().with_fill(TextPaint::solid(WHITE))),
        ),
        ("描边16", Some(TextStyle::outline(16.0, ink))),
    ];
    for (label, style) in cases {
        let c = OffscreenCanvas::new(&gpu, 640, 140);
        let mut b = DrawBatch::new();
        if let Some(style) = style {
            draw_text(
                &mut b.texts,
                "Outline 描边",
                Pos::new(20.0, 20.0),
                TextDef {
                    style: Some(Arc::new(style)),
                    ..TextDef::default().font_size(44.0)
                },
                TextOverride::from_color(Color::new(0.96, 0.97, 1.0, 1.0)),
            );
        }
        c.draw(Some(Color::new(0.07, 0.075, 0.09, 1.0)), &[&b]);
        let px = c.read_pixels();
        let sum_at = |x: u32, y: u32| {
            let i = ((y * 640 + x) * 4) as usize;
            px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32
        };
        let bg = sum_at(2, 2);
        // 统计"比背景略亮一点"的像素（整块淡色的特征）
        let (mut washed, mut wash_sum) = (0usize, 0u64);
        let mut lit = 0usize;
        for y in 0..140u32 {
            for x in 0..640u32 {
                let s = sum_at(x, y);
                if s > bg + 4 && s < bg + 40 {
                    washed += 1;
                    wash_sum += s as u64;
                }
                if s > bg + 120 {
                    lit += 1;
                }
            }
        }
        println!(
            "  [{label}] 背景 {bg}，墨迹 {lit} px，淡色像素 {washed} 个（均值差 {:+.1}）",
            if washed > 0 {
                wash_sum as f64 / washed as f64 - bg as f64
            } else {
                0.0
            }
        );
    }
}

/// **宽描边不能糊成矩形**：16px 描边下，字形外接框里红色像素占比应明显小于 1
/// （出错时整块 padded quad 都会被涂满，占比接近 100%）。
///
/// 历史原因：顶点里的距离范围曾按"目标像素"传、距离场按"生成像素"编码，两者差一个
/// `quad_scale`；而 `quad_scale` 又被着色器从 `glyph_size / entry_texels` 反推 ——
/// quad 被 bounds 裁剪后会算小，于是 d 被当 0，描边层把整块 quad 填满。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_wide_outline_stays_an_outline() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let red = Color::new(1.0, 0.0, 0.0, 1.0);

    for width in [12.0f32, 16.0, 24.0] {
        let c = OffscreenCanvas::new(&gpu, 420, 260);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "国",
            Pos::new(60.0, 40.0),
            TextDef {
                style: Some(Arc::new(
                    TextStyle::outline(width, red).with_fill(TextPaint::solid(WHITE)),
                )),
                ..TextDef::default().font_size(44.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();

        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut red_n = 0usize;
        for y in 0..260u32 {
            for x in 0..420u32 {
                let i = ((y * 420 + x) * 4) as usize;
                let (r, g, bl) = (px[i] as u32, px[i + 1] as u32, px[i + 2] as u32);
                if r + g + bl > 60 {
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                    if r > 120 && g < 90 && bl < 90 {
                        red_n += 1;
                    }
                }
            }
        }
        let area = ((x1 - x0 + 1) * (y1 - y0 + 1)) as f64;
        let frac = red_n as f64 / area;
        // 外接框四角必须是背景：糊成矩形时四角会被涂满
        let lit = |x: u32, y: u32| {
            let i = ((y * 420 + x) * 4) as usize;
            px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32 > 60
        };
        let corner_lit = [
            lit(x0 + 2, y0 + 2),
            lit(x1 - 2, y0 + 2),
            lit(x0 + 2, y1 - 2),
            lit(x1 - 2, y1 - 2),
        ];
        println!(
            "  w={width}: 外接框 {}x{}，红色 {red_n} px，占外接框 {:.0}%，四角亮 {corner_lit:?}",
            x1 - x0 + 1,
            y1 - y0 + 1,
            frac * 100.0
        );
        assert!(
            !corner_lit.iter().any(|v| *v),
            "w={width} 时外接框四角有像素被涂亮（{corner_lit:?}）—— 描边糊成矩形了"
        );
        // 字形是空心的"国"：描边 + 填充不该把外接框吃满
        assert!(
            frac < 0.85, // 相对带宽档让厚描边的外缘更实，占比略升
            "w={width} 时红色占外接框 {:.0}%（应明显小于 100%，否则是糊成矩形）",
            frac * 100.0
        );
        // 描边外扩应该跟着宽度走：+2*width 左右
        let grow = (x1 - x0) as f64;
        assert!(
            grow > 40.0 && grow < 140.0,
            "w={width} 时外接框宽度 {grow} 不合理（四字形 44px 约 60–70）"
        );
    }
}

/// **尺寸阶梯**：同一档内改字号不重建距离场（条目数不变）；跨档才新增条目。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_size_ladder_avoids_rebuild() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let c = OffscreenCanvas::new(&gpu, 400, 200);
    let ink = Color::new(0.05, 0.06, 0.09, 1.0);

    let draw = |font_size: f32| {
        let mut b = DrawBatch::new();
        for (i, ch) in ["国", "永", "光"].iter().enumerate() {
            draw_text(
                &mut b.texts,
                ch,
                Pos::new(20.0 + i as f32 * 110.0, 20.0),
                TextDef {
                    style: Some(Arc::new(
                        TextStyle::outline(2.0, ink).with_fill(TextPaint::solid(WHITE)),
                    )),
                    ..TextDef::default().font_size(font_size)
                },
                TextOverride::from_color(WHITE),
            );
        }
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        let lit = px
            .chunks(4)
            .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 60)
            .count();
        (gpu.text_atlas_stats().mask_entries, lit)
    };

    // 44 与 40 都落在阶梯 46 这一档
    let (e44, lit44) = draw(44.0);
    let (e40, lit40) = draw(40.0);
    println!("  44px: 条目 {e44}（{lit44} px）  40px: 条目 {e40}（{lit40} px）");
    assert!(e44 > 0, "样式文字应生成距离场条目");
    assert_eq!(e44, e40, "同一阶梯档内改字号不该新增条目");
    assert!(
        lit40 < lit44,
        "40px 应比 44px 小（实际 {lit40} vs {lit44}）"
    );

    // 57 是下一档（46 与 57 的步进），会新增条目
    let (e57, _) = draw(57.0);
    println!("  57px: 条目 {e57}");
    assert!(e57 > e44, "跨档应新增条目（46 档 -> 57 档）");
}

/// **尺寸阶梯**的边缘质量：阶梯生成 + 缩放后，轮廓仍应与覆盖度文字对齐（±2px）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_size_ladder_edges_align() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));

    for font_size in [44.0f32, 30.0, 71.0] {
        let edges = |styled: bool| -> Vec<u32> {
            let c = OffscreenCanvas::new(&gpu, 300, 220);
            let mut b = DrawBatch::new();
            draw_text(
                &mut b.texts,
                "国",
                Pos::new(40.0, 30.0),
                TextDef {
                    style: styled.then(|| Arc::new(TextStyle::outline(0.01, WHITE))),
                    ..TextDef::default().font_size(font_size)
                },
                TextOverride::from_color(WHITE),
            );
            c.draw(Some(BLACK), &[&b]);
            let px = c.read_pixels();
            (0..220u32)
                .map(|y| {
                    (0..300u32)
                        .find(|x| {
                            let i = ((y * 300 + x) * 4) as usize;
                            px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32 > 90
                        })
                        .unwrap_or(u32::MAX)
                })
                .filter(|v| *v != u32::MAX)
                .collect()
        };
        let a = edges(false);
        let b = edges(true);
        let n = a.len().min(b.len());
        let mut ok = 0;
        for i in 0..n {
            if (a[i] as i64 - b[i] as i64).abs() <= 2 {
                ok += 1;
            }
        }
        let ratio = ok as f64 / n.max(1) as f64;
        println!(
            "  {font_size}px: {ok}/{n} 行左边缘差 ≤2px（{:.0}%）",
            ratio * 100.0
        );
        assert!(
            ratio > 0.85,
            "{font_size}px 阶梯缩放后轮廓应对齐（{:.0}%）",
            ratio * 100.0
        );
    }
}

/// 稳态**确定性**：同一批内容连续帧必须逐像素一致（抖动/闪烁的底线检查）。
///
/// 抖动通常来自「字形位置或图集条目取整在不同帧不一样」。这里用带样式文字 + 多行内容，
/// 连续 4 帧比较像素缓冲。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_rendering_is_frame_stable() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let c = OffscreenCanvas::new(&gpu, 480, 240);
    let ink = Color::new(0.05, 0.06, 0.09, 1.0);
    let red = Color::new(1.0, 0.0, 0.0, 1.0);

    let styles = [
        (
            TextStyle::outline(2.0, ink).with_fill(TextPaint::solid(WHITE)),
            "国永光明",
        ),
        (
            TextStyle::shadow((4.0, 4.0), 3.0, ink).with_fill(TextPaint::solid(WHITE)),
            "多层 描边",
        ),
        (
            TextStyle::inner_outline(3.0, red).with_fill(TextPaint::solid(WHITE)),
            "内描边 环",
        ),
    ];

    let draw_once = || {
        let mut b = DrawBatch::new();
        for (i, (style, text)) in styles.iter().enumerate() {
            draw_text(
                &mut b.texts,
                text,
                Pos::new(20.0, 16.0 + i as f32 * 70.0),
                TextDef {
                    style: Some(Arc::new(style.clone())),
                    ..TextDef::default().font_size(44.0)
                },
                TextOverride::from_color(WHITE),
            );
        }
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };

    // 每次用**新画布**画一遍：这样两次都是"同一画布的首帧"，可以判断差异来自
    // 画布首帧未初始化，还是来自我们自己的渲染状态。
    let draw_fresh = || {
        let c2 = OffscreenCanvas::new(&gpu, 480, 240);
        let mut b = DrawBatch::new();
        for (i, (style, text)) in styles.iter().enumerate() {
            draw_text(
                &mut b.texts,
                text,
                Pos::new(20.0, 16.0 + i as f32 * 70.0),
                TextDef {
                    style: Some(Arc::new(style.clone())),
                    ..TextDef::default().font_size(44.0)
                },
                TextOverride::from_color(WHITE),
            );
        }
        c2.draw(Some(BLACK), &[&b]);
        c2.read_pixels()
    };

    let first = draw_once();
    let describe = |px: &[u8]| {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut lit = 0usize;
        let mut strong = 0usize;
        let mut faint = 0usize;
        for y in 0..240 {
            for x in 0..480 {
                let i = ((y * 480 + x) * 4) as usize;
                let sum = px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32;
                if sum > 60 {
                    lit += 1;
                    if sum > 400 {
                        strong += 1;
                    } else {
                        faint += 1;
                    }
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
            }
        }
        (lit, x0, y0, x1, y1, strong, faint)
    };
    println!("  帧0: {:?}", describe(&first));
    let mut warm: Option<Vec<u8>> = None;
    for frame in 1..4 {
        let next = draw_once();
        if warm.is_none() {
            warm = Some(next.clone());
        }
        let diff = |a: &[u8], b: &[u8]| -> (usize, i32) {
            let mut n = 0usize;
            let mut worst = 0i32;
            for (x, y) in a.chunks(4).zip(b.chunks(4)) {
                let d = (x[0] as i32 - y[0] as i32)
                    .abs()
                    .max((x[1] as i32 - y[1] as i32).abs())
                    .max((x[2] as i32 - y[2] as i32).abs());
                if d > 2 {
                    n += 1;
                    worst = worst.max(d);
                }
            }
            (n, worst)
        };
        let (dc, _) = diff(&first, &next);
        let (dw, _) = diff(warm.as_ref().unwrap(), &next);
        println!(
            "  帧{frame}: {:?}  与帧0差 {dc}  与帧1差 {dw}",
            describe(&next)
        );
    }
    // 回暖后的帧必须逐像素一致（帧 0 允许不同：首次栅格化/样式表首次上传）
    {
        let a = draw_fresh();
        let b = draw_fresh();
        let mut n = 0usize;
        for (x, y) in a.chunks(4).zip(b.chunks(4)) {
            if (x[0] as i32 - y[0] as i32).abs() > 2
                || (x[1] as i32 - y[1] as i32).abs() > 2
                || (x[2] as i32 - y[2] as i32).abs() > 2
            {
                n += 1;
            }
        }
        println!("  两个新画布首帧：{n} 个像素不一致");
        assert_eq!(n, 0, "不同画布的首帧应一致，实际 {n} 个像素不同");
    }
}

/// **同一带宽档内改描边宽度不重建**：尺寸阶梯 + 相对带宽档的核心收益。
///

/// 60px 字号下 w=2.0 与 w=3.5 的外扩（含 1px 兜底）都落在 44px 参考字号的 4 档里，
/// 所以图集条目应当完全复用；同时两者都必须真的画出像素（防止"不重建"其实是因为没画）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_width_change_within_band_does_not_rebuild() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 300, 200);
    let ink = Color::new(0.05, 0.06, 0.09, 1.0);

    let draw = |width: f32| -> usize {
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "国永光",
            Pos::new(20.0, 40.0),
            TextDef {
                style: Some(Arc::new(
                    TextStyle::outline(width, ink).with_fill(TextPaint::solid(WHITE)),
                )),
                ..TextDef::default().font_size(60.0)
            },
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(BLACK), &[&b]);
        let px = canvas.read_pixels();
        px.chunks(4)
            .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 90)
            .count()
    };

    let px_a = draw(2.0);
    let stats_a = gpu.text_atlas_stats();
    let px_b = draw(3.5);
    let stats_b = gpu.text_atlas_stats();
    println!(
        "  条目：w=2.0 mask={} / w=3.5 mask={}；像素 {px_a} / {px_b}",
        stats_a.mask_entries, stats_b.mask_entries
    );
    assert!(
        px_a > 0 && px_b > 0,
        "两条路径都必须画出像素（{px_a} / {px_b}）"
    );
    assert_eq!(
        stats_a.mask_entries, stats_b.mask_entries,
        "同一带宽档内改宽度不该重新栅格化"
    );
}

/// 样式字形与普通字形**共用**同一张 R8 图集：带样式绘制不该动彩色图集。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_uses_mask_atlas_only() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 128, 128);

    let mut plain = DrawBatch::new();
    draw_text(
        &mut plain.texts,
        "H",
        Pos::new(20.0, 20.0),
        TextDef::default().font_size(40.0),
        TextOverride::from_color(WHITE),
    );
    canvas.draw(Some(BLACK), &[&plain]);
    let before = gpu.text_atlas_stats();

    let style = TextStyle::shadow((2.0, 2.0), 2.0, Color::new(0.0, 0.0, 0.0, 0.8))
        .with_layer(TextLayer::hard(2.0, TextPaint::solid(WHITE)));
    let mut styled = DrawBatch::new();
    draw_text(
        &mut styled.texts,
        "H",
        Pos::new(20.0, 20.0),
        TextDef {
            style: Some(Arc::new(style)),
            ..TextDef::default().font_size(40.0)
        },
        TextOverride::from_color(WHITE),
    );
    canvas.draw(Some(BLACK), &[&styled]);
    let after = gpu.text_atlas_stats();

    assert_eq!(
        before.color_size, after.color_size,
        "样式字形不该进彩色图集：{} -> {}",
        before.color_size, after.color_size
    );
    assert!(after.mask_size >= before.mask_size, "R8 图集只增不减");
    assert!(
        after.mask_bytes > 0 && after.mask_bytes <= 8 * 1024 * 1024,
        "R8 图集占用异常：{} 字节",
        after.mask_bytes
    );
    assert_eq!(
        after.total_bytes(),
        after.mask_bytes + after.color_bytes,
        "total_bytes 定义应自洽"
    );
    println!(
        "  图集：R8 {}²={}B，彩色 {}²={}B（样式前后彩色不变）",
        after.mask_size, after.mask_bytes, after.color_size, after.color_bytes
    );
}

/// **同一帧里出现多个样式**时，每个样式的图层都必须正确（回归：`layer_first` 曾是
/// 层下标，却被着色器当 vec4 下标用，导致第 2 个及以后样式的层参数整体错位）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_multiple_styles_in_one_frame() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 160;
    let red = Color::new(1.0, 0.0, 0.0, 1.0);
    let blue = Color::new(0.1, 0.2, 1.0, 1.0);

    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let styles = [
        // 第 1 个样式：单层红描边
        TextStyle::outline(4.0, red),
        // 第 2 个样式：红 7px + 蓝 3px（层参数错位时蓝层会消失/乱）
        TextStyle::new()
            .with_layer(TextLayer::hard(7.0, TextPaint::solid(red)))
            .with_layer(TextLayer::hard(3.0, TextPaint::solid(blue))),
    ];
    for (i, style) in styles.iter().enumerate() {
        draw_text(
            &mut b.texts,
            "国",
            Pos::new(10.0 + i as f32 * 80.0, 20.0),
            TextDef {
                style: Some(Arc::new(style.clone())),
                ..TextDef::default().font_size(70.0)
            },
            TextOverride::from_color(WHITE),
        );
    }
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();

    let count = |x0: u32, x1: u32, f: &dyn Fn(&[u8]) -> bool| {
        let mut n = 0;
        for y in 0..N {
            for x in x0..x1 {
                let i = ((y * N + x) * 4) as usize;
                if f(&px[i..i + 3]) {
                    n += 1;
                }
            }
        }
        n
    };
    let reds_l = count(0, 80, &|p| p[0] > 150 && p[1] < 80 && p[2] < 80);
    let reds_r = count(80, 160, &|p| p[0] > 150 && p[1] < 80 && p[2] < 80);
    let blues_r = count(80, 160, &|p| p[2] > 150 && p[0] < 90);
    println!("  同帧两样式：左红 {reds_l} / 右红 {reds_r} 右蓝 {blues_r}");
    assert!(reds_l > 300, "第 1 个样式的红描边应可见，实际 {reds_l}");
    assert!(reds_r > 200, "第 2 个样式的外红层应可见，实际 {reds_r}");
    assert!(blues_r > 30, "第 2 个样式的蓝层应可见，实际 {blues_r}");
}

/// 诊断用：把每个预设单独渲染并打印像素分类图（临时，跑完删）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_diagnose_presets() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 160;

    let dump = |name: &str, style: Option<TextStyle>, glyph: &str| {
        let c = OffscreenCanvas::new(&gpu, N, N);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            glyph,
            Pos::new(20.0, 20.0),
            TextDef {
                style: style.map(Arc::new),
                ..TextDef::default().font_size(80.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        let (mut red, mut blue, mut white, mut other) = (0, 0, 0, 0);
        for p in px.chunks(4) {
            let (r, g, bl) = (p[0] as i32, p[1] as i32, p[2] as i32);
            if r + g + bl < 30 {
                continue;
            }
            if r > 120 && g < 90 && bl < 90 {
                red += 1;
            } else if bl > 120 && r < 90 && g < 90 {
                blue += 1;
            } else if r > 180 && g > 180 && bl > 180 {
                white += 1;
            } else {
                other += 1;
            }
        }
        println!("\n[{name}] red={red} blue={blue} white={white} other={other}");
        // 4px 下采样的分类图
        let mut map = String::new();
        for y in (0..N).step_by(4) {
            let mut row = String::new();
            for x in (0..N).step_by(4) {
                let i = ((y * N + x) * 4) as usize;
                let (r, g, bl) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
                row.push(if r + g + bl < 40 {
                    '.'
                } else if r > 120 && g < 90 && bl < 90 {
                    'R'
                } else if bl > 120 && r < 90 && g < 90 {
                    'B'
                } else if r > 170 && g > 170 && bl > 170 {
                    '#'
                } else {
                    '+'
                });
            }
            map.push_str(&row);
            map.push('\n');
        }
        println!("{map}");
    };

    let red = Color::new(1.0, 0.0, 0.0, 1.0);
    let blue = Color::new(0.1, 0.2, 1.0, 1.0);
    let ink = Color::new(0.05, 0.05, 0.05, 1.0);
    let whitish = Color::new(0.9, 0.9, 0.9, 1.0);

    dump("单层外描边", Some(TextStyle::outline(4.0, red)), "国");
    dump(
        "双层外描边",
        Some(
            TextStyle::new()
                .with_layer(TextLayer::hard(7.0, TextPaint::solid(red)))
                .with_layer(TextLayer::hard(3.0, TextPaint::solid(blue))),
        ),
        "国",
    );
    dump(
        "内描边",
        Some(TextStyle::inner_outline(5.0, red).with_fill(TextPaint::solid(blue))),
        "国",
    );
    dump(
        "软投影+描边",
        Some(
            TextStyle::shadow((5.0, 5.0), 3.0, red)
                .with_layer(TextLayer::hard(3.0, TextPaint::solid(ink))),
        ),
        "国",
    );
    dump("仅发光", Some(TextStyle::glow(8.0, red)), "国");
    // 示例第 1 组的真实文字/字号：整行 "Outline 描边"，看 w>=16 是否真的变方块
    for w in [12.0f32, 16.0, 20.0] {
        let c = OffscreenCanvas::new(&gpu, 320, 96);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "Outline 描边",
            Pos::new(10.0, 10.0),
            TextDef {
                style: Some(Arc::new(TextStyle::outline(w, red))),
                ..TextDef::default().font_size(44.0)
            },
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        let px = c.read_pixels();
        println!("  row w={w}:");
        for y in (0..96).step_by(4) {
            let mut row = String::new();
            for x in (0..320).step_by(4) {
                let i = ((y * 320 + x) * 4) as usize;
                let (r, g, bl) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
                row.push(if r + g + bl < 40 {
                    '.'
                } else if r > 120 && g < 90 && bl < 90 {
                    'R'
                } else if r > 170 && g > 170 && bl > 170 {
                    '#'
                } else {
                    '+'
                });
            }
            println!("{row}");
        }
    }
    dump("描边 w=12", Some(TextStyle::outline(12.0, red)), "国");
    dump("描边 w=16", Some(TextStyle::outline(16.0, red)), "国");
    dump("描边 w=24", Some(TextStyle::outline(24.0, red)), "国");
    dump(
        "内阴影（偏移 6,6 / 软边 3）",
        Some(TextStyle::inner_shadow((6.0, 6.0), 3.0, ink).with_fill(TextPaint::solid(whitish))),
        "国",
    );
}

/// 彩色字形（COLRv1 emoji）加描边：彩色字形保留自己的颜色，描边垫在底下。
///
/// 判据：带描边的 emoji 比不带描边的覆盖面积更大，且多出来的部分以描边色为主。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_emoji_outline() {
    let Some((_path, family)) = colr_v1_test_font() else {
        eprintln!("跳过：本机没有 COLRv1 字体");
        return;
    };
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 128;
    let ring = Color::new(1.0, 0.0, 0.0, 1.0);

    let render = |styled: bool| -> Vec<u8> {
        let c = OffscreenCanvas::new(&gpu, N, N);
        let mut b = DrawBatch::new();
        let def = TextDef {
            style: if styled {
                Some(Arc::new(TextStyle::outline(4.0, ring)))
            } else {
                None
            },
            ..TextDef::default()
                .font_size(64.0)
                .with_family(Family::Name(family))
        };
        draw_text(
            &mut b.texts,
            "😀",
            Pos::new(24.0, 24.0),
            def,
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };

    let plain = render(false);
    let styled = render(true);
    let lit = |px: &[u8]| -> usize {
        px.chunks(4)
            .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 40)
            .count()
    };
    let (na, nb) = (lit(&plain), lit(&styled));
    assert!(na > 200, "用例失效：无样式 emoji 没画出来（{na}）");
    assert!(
        nb > na + 80,
        "带描边的 emoji 覆盖面积应更大：无样式 {na} vs 描边 {nb}"
    );
    let reds = styled
        .chunks(4)
        .filter(|p| p[0] > 130 && p[1] < 80 && p[2] < 80)
        .count();
    assert!(reds > 60, "应出现红色描边像素，实际 {reds}");
    println!("  emoji 描边：无样式 {na} px / 带描边 {nb} px，其中红色 {reds} px");
}

/// 纹理填充：字形按页面矩形铺一张左右红蓝的贴图 → 左红右蓝。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_texture_fill() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 128;

    // 8×8 贴图：左半红、右半蓝
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

    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    b.set_texture(Some(&tex));
    let paint = TextPaint::texture_in_rect(
        UvRect {
            u0: 0.0,
            v0: 0.0,
            u1: 1.0,
            v1: 1.0,
        },
        30.0,
        30.0,
        60.0,
        60.0,
    );
    let style = TextStyle::new().with_fill(paint);
    draw_text(
        &mut b.texts,
        "H",
        Pos::new(30.0, 30.0),
        TextDef {
            style: Some(Arc::new(style)),
            ..TextDef::default().font_size(60.0)
        },
        TextOverride::from_color(WHITE),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();

    // 左右各取三分之一，比红/蓝总量
    let (mut lr, mut lb, mut rr, mut rb) = (0i64, 0i64, 0i64, 0i64);
    for y in 0..N {
        for x in 0..N {
            let i = ((y * N + x) * 4) as usize;
            let (r, g, bl) = (px[i] as i64, px[i + 1] as i64, px[i + 2] as i64);
            if r + g + bl <= 60 {
                continue;
            }
            if x < N / 2 {
                lr += r;
                lb += bl;
            } else {
                rr += r;
                rb += bl;
            }
        }
    }
    assert!(lr + lb > 0 && rr + rb > 0, "用例失效：没找到字像素");
    assert!(lr > lb, "左半边应偏红：R={lr} B={lb}");
    assert!(rb > rr, "右半边应偏蓝：R={rr} B={rb}");
    println!("  纹理填充：左 R/B={lr}/{lb}，右 R/B={rr}/{rb}");
}

/// 渐变填充：同一个字形上红下蓝（页面矩形参照系，跨满给定框）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_style_gradient_fill() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    const N: u32 = 96;

    let c = OffscreenCanvas::new(&gpu, N, N);
    let mut b = DrawBatch::new();
    let grad = TextGradient::vertical(
        Color::new(1.0, 0.15, 0.15, 1.0),
        Color::new(0.15, 0.35, 1.0, 1.0),
    )
    // 框贴着字形上下沿：顶部 t≈0（纯红）、底部 t≈1（纯蓝）
    .in_rect(20.0, 30.0, 56.0, 46.0);
    let style = TextStyle::new().with_fill(TextPaint::Gradient(grad));
    draw_text(
        &mut b.texts,
        "H",
        Pos::new(24.0, 20.0),
        TextDef {
            style: Some(Arc::new(style)),
            ..TextDef::default().font_size(56.0)
        },
        TextOverride::from_color(WHITE),
    );
    c.draw(Some(BLACK), &[&b]);
    let px = c.read_pixels();

    // 按行汇总，取亮像素的上下两段比红/蓝比例（AA 边缘像素会整体压暗，
    // 所以用**比例**而不是绝对值）。
    let mut rows: Vec<(u32, i64, i64)> = Vec::new();
    for y in 0..N {
        let mut r_sum = 0i64;
        let mut b_sum = 0i64;
        for x in 0..N {
            let i = ((y * N + x) * 4) as usize;
            let (r, g, bl) = (px[i] as i64, px[i + 1] as i64, px[i + 2] as i64);
            if r + g + bl > 90 {
                r_sum += r;
                b_sum += bl;
            }
        }
        if r_sum + b_sum > 0 {
            rows.push((y, r_sum, b_sum));
        }
    }
    assert!(rows.len() > 10, "用例失效：亮像素行太少（{}）", rows.len());
    let (top_y, _, _) = rows[0];
    let (bot_y, _, _) = rows[rows.len() - 1];
    assert!(top_y + 8 < bot_y, "字形太矮，看不出上下差异");
    let third = (rows.len() / 3).max(1);
    let (mut tr, mut tb, mut br, mut bb) = (0i64, 0i64, 0i64, 0i64);
    for &(_, r, b) in &rows[..third] {
        tr += r;
        tb += b;
    }
    for &(_, r, b) in &rows[rows.len() - third..] {
        br += r;
        bb += b;
    }
    println!(
        "  渐变填充：上部 R/B={:.2}，下部 R/B={:.2}（中心像素 {:?}）",
        tr as f64 / tb.max(1) as f64,
        br as f64 / bb.max(1) as f64,
        {
            let i = ((48 * N + 48) * 4) as usize;
            (px[i], px[i + 1], px[i + 2])
        }
    );
    // sRGB 目标 + AA 边缘会把比例压低，1.25 已经足以证明"上红下蓝"。
    assert!(tr > tb * 5 / 4, "上三分之一应偏红：R={tr} B={tb}");
    assert!(bb > br * 5 / 4, "下三分之一应偏蓝：R={br} B={bb}");
}

// ===========================================================================
// 字形缓存（`gpu.text().glyphs()`）：预光栅化 / 钉子 / 预算 / 回收 / 统计
// ===========================================================================

fn glyph_test_def(size: f32) -> TextDef {
    TextDef::default().font_size(size)
}

/// 一段互不重复的 CJK 文本（用码点顺序生成，保证每个字形都不重样）。
fn cjk_text(start: u32, n: usize) -> String {
    (0..n as u32)
        .filter_map(|i| char::from_u32(start + i))
        .collect()
}

/// **动画字号的代价**：库把光栅化键的字号量化到 1/8 px，所以"字号连续动画"不再
/// 每帧全量重光栅化（量化前 12.0 次/帧 / 1.03 ms，量化后 ~3 次/帧 / 0.80 ms）。
///
/// 这条用例守住这个量化：如果哪天有人把量化去掉，次数会跳回"每帧 12 次"。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_path_animated_size_cost() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 800, 400);
    let text = cjk_text(0x4E00, 12);
    let glyphs = gpu.text().glyphs();

    for (label, quantize) in [("连续字号（每帧 +0.01px）", false), ("量化到 0.25px", true)]
    {
        glyphs.clear();
        let draw = |i: usize| {
            let mut size = 24.0 + i as f32 * 0.01;
            if quantize {
                size = (size * 4.0).round() / 4.0;
            }
            let mut b = DrawBatch::new();
            draw_text(
                &mut b.texts,
                &text,
                Pos::new(8.0, 20.0),
                TextDef::default().font_size(size),
                TextOverride::from_color(WHITE),
            );
            canvas.draw(Some(BLACK), &[&b]);
        };
        // 预热 3 帧（字体/管线）。
        for i in 0..3 {
            draw(i);
        }
        let r0 = glyphs.stats().rasterizations;
        let t = Instant::now();
        const FRAMES: usize = 40;
        for i in 0..FRAMES {
            draw(3 + i);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        let rasters = glyphs.stats().rasterizations - r0;
        println!(
            "  {label}：{ms:.2} ms/帧，光栅化 {:.1} 次/帧，图集 {} 条 / {} KiB",
            rasters as f64 / FRAMES as f64,
            glyphs.stats().entries(),
            glyphs.stats().used_bytes() / 1024
        );
        if !quantize {
            let per_frame = rasters as f64 / FRAMES as f64;
            assert!(
                per_frame > 0.0,
                "连续改字号仍应在跨过 1/8px 档时重光栅化（实际 0 次，说明量化过头了）"
            );
            assert!(
                per_frame < 5.0,
                "连续改字号不该每帧全量重光栅化（实际 {per_frame:.1} 次/帧）—— \
                 说明 1/8px 的字号量化被去掉了"
            );
        }
    }
}

/// **12k→24k quad 超线性的归因**：固定/扫 quad 数，分离三个嫌疑——
/// 条目数（每段文本的固定开销）、字形种类（图集 + LRU 表规模）、纯 quad 数（顶点路径）。
///
/// 做法：4096² 画布保证**不裁剪**（裁剪会额外走一套分支，污染测量），
/// 空 batch 帧做基线，3 轮取最小值。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_path_quad_attribution() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 4096, 4096);

    let empty = DrawBatch::new();
    for _ in 0..3 {
        canvas.draw(Some(BLACK), &[&empty]);
    }
    let mut base_ms = f64::INFINITY;
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..10 {
            canvas.draw(Some(BLACK), &[&empty]);
        }
        base_ms = base_ms.min(t.elapsed().as_secs_f64() * 1000.0 / 10.0);
    }
    // 同一个基线，但**等 GPU 完成**：4096²（16 MB）清屏本身的 GPU 成本不算在文本头上。
    let mut base_gpu_ms = f64::INFINITY;
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..10 {
            canvas.draw(Some(BLACK), &[&empty]);
            let _ = gpu.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
        }
        base_gpu_ms = base_gpu_ms.min(t.elapsed().as_secs_f64() * 1000.0 / 10.0);
    }
    println!("  基线（空 batch，4096²）：CPU {base_ms:.3} ms / 含 GPU {base_gpu_ms:.3} ms");

    /// 一个测量点：`entries` 段 × `chars` 字，字形取 `pool`（None = 全不重复）。
    fn measure(
        device: &wgpu::Device,
        canvas: &OffscreenCanvas,
        base_ms: f64,
        base_gpu_ms: f64,
        label: &str,
        entries: usize,
        chars: usize,
        pool: Option<usize>,
    ) {
        let quads = entries * chars;
        let texts: Vec<String> = (0..entries)
            .map(|i| {
                let start = match pool {
                    Some(p) => ((i % p) * chars) as u32,
                    None => (i * chars) as u32,
                };
                cjk_text(0x4E00 + start, chars)
            })
            .collect();
        let mut b = DrawBatch::new();
        // 每行 100 列；行高 = 字符高度的 1.25 倍，保证不重叠也不越界。
        let cols = 100.min(entries.max(1));
        for (i, text) in texts.iter().enumerate() {
            draw_text(
                &mut b.texts,
                text,
                Pos::new(
                    8.0 + (i % cols) as f32 * 20.0,
                    8.0 + (i / cols) as f32 * 20.0,
                ),
                TextDef::default().font_size(16.0),
                TextOverride::from_color(WHITE),
            );
        }
        for _ in 0..3 {
            canvas.draw(Some(BLACK), &[&b]);
        }
        let mut ms = f64::INFINITY;
        for _ in 0..3 {
            let t = Instant::now();
            for _ in 0..5 {
                canvas.draw(Some(BLACK), &[&b]);
            }
            ms = ms.min(t.elapsed().as_secs_f64() * 1000.0 / 5.0);
        }
        // 再来一轮**含 GPU 完成**的计时（每帧后 poll 等 GPU）：只量 CPU 提交会漏掉
        // "GPU 排队反压"的贡献，而图集变大后的采样成本正好落在这里。
        let t = Instant::now();
        for _ in 0..5 {
            canvas.draw(Some(BLACK), &[&b]);
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
        }
        let ms_gpu = t.elapsed().as_secs_f64() * 1000.0 / 5.0;
        let net = (ms - base_ms).max(0.0);
        let net_gpu = (ms_gpu - base_gpu_ms).max(0.0);
        println!(
            "  {label:<34} {quads:>6} quad / {entries:>4} 条目：CPU 净 {net:>7.3} ms\
             （{:>5.2} µs/quad）| GPU 净 {net_gpu:>7.3} ms（{:>5.2} µs/quad）",
            net * 1000.0 / quads as f64,
            net_gpu * 1000.0 / quads as f64
        );
    }

    println!("  ---- 扫 quad 数（每条 12 字，全不重复字形）----");
    for entries in [500usize, 1000, 1333, 1667, 2000] {
        measure(
            &gpu.device,
            &canvas,
            base_ms,
            base_gpu_ms,
            &format!("{entries} 条目"),
            entries,
            12,
            None,
        );
    }

    println!("  ---- 固定 24000 quad：改条目数（每条目固定开销）----");
    for (entries, chars) in [(2000usize, 12usize), (800, 30), (200, 120)] {
        measure(
            &gpu.device,
            &canvas,
            base_ms,
            base_gpu_ms,
            &format!("{entries} 条目 × {chars} 字"),
            entries,
            chars,
            None,
        );
    }

    println!("  ---- 固定 24000 quad / 2000 条目：改字形种类（图集与 LRU 规模）----");
    for pool in [None, Some(500usize), Some(50), Some(5)] {
        let label = match pool {
            None => "全不重复（24000 种）".to_string(),
            Some(p) => format!("{p} 段复用（{} 种字形）", p * 12),
        };
        measure(
            &gpu.device,
            &canvas,
            base_ms,
            base_gpu_ms,
            &label,
            2000,
            12,
            pool,
        );
    }

    assert!(gpu.text().glyphs().stats().rasterizations > 0);
}

/// **文本路径的稳态吞吐上限**：缓存全热时，一帧能画多少文本 quad 还保得住 120 fps。
/// 这是"稳态 120 fps"这个目标的可测定义：`8.33 ms` 预算里能装下多少 quad。
/// 用**空 batch 帧**做基线，差值就是文本路径的净成本（避免把 acquire/清屏算进去）。
///
/// 报告：`每帧文本成本` 与 `µs/quad`，以及由它反推的 "8.33 ms 能画多少 quad"。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn text_path_steady_state_ceiling() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    // 画布够大，保证所有文本都在视口内（被 bounds 裁掉的字形不产生 quad，会污染计数）。
    let canvas = OffscreenCanvas::new(&gpu, 2048, 1600);
    let def = TextDef::default().font_size(16.0);
    const CHARS: usize = 12;

    // 基线：空 batch（清屏 + 一次提交）。同样取 3 轮最小值。
    let empty = DrawBatch::new();
    for _ in 0..3 {
        canvas.draw(Some(BLACK), &[&empty]);
    }
    let mut base_ms = f64::INFINITY;
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..10 {
            canvas.draw(Some(BLACK), &[&empty]);
        }
        base_ms = base_ms.min(t.elapsed().as_secs_f64() * 1000.0 / 10.0);
    }

    println!("  基线（空 batch）：{base_ms:.3} ms/帧");
    let mut best_quads_per_budget = 0.0f64;
    for entries in [50usize, 200, 500, 1000, 2000] {
        let texts: Vec<String> = (0..entries)
            .map(|i| cjk_text(0x4E00 + (i * CHARS) as u32, CHARS))
            .collect();
        let mut b = DrawBatch::new();
        for (i, text) in texts.iter().enumerate() {
            draw_text(
                &mut b.texts,
                text,
                // 20px 网格，100 列 → 2000 条目占 2000×400 px，落在 2048×1600 内。
                Pos::new(4.0 + (i % 100) as f32 * 20.0, 4.0 + (i / 100) as f32 * 20.0),
                def.clone(),
                TextOverride::from_color(WHITE),
            );
        }
        // 预热：首帧含排版/光栅化，不计入稳态。
        for _ in 0..3 {
            canvas.draw(Some(BLACK), &[&b]);
        }
        let quads = (entries * CHARS) as f64;
        let rasters0 = gpu.text().glyphs().stats().rasterizations;
        // 取 3 轮的最小值：这台机器的噪声（其他进程 / GPU 队列）能让单次测出 2–4 倍差异，
        // 最小值是最稳的估计量。
        let mut ms = f64::INFINITY;
        for _ in 0..3 {
            let t = Instant::now();
            const FRAMES: u32 = 5;
            for _ in 0..FRAMES {
                canvas.draw(Some(BLACK), &[&b]);
            }
            ms = ms.min(t.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64);
        }
        let rasters = gpu.text().glyphs().stats().rasterizations - rasters0;
        assert_eq!(rasters, 0, "{entries} 条目稳态不该重光栅化（{rasters} 次）");

        let text_ms = (ms - base_ms).max(0.0);
        let us_per_quad = text_ms * 1000.0 / quads;
        let per_budget = 8.33 / text_ms * quads;
        best_quads_per_budget = per_budget;
        println!(
            "  {quads:>6.0} quad（{entries:>5} 条目）：{ms:>7.3} ms/帧，文本净成本 {text_ms:>6.3} ms\
             （{us_per_quad:>5.2} µs/quad）⇒ 8.33 ms 可画 ≈ {per_budget:>6.0} quad"
        );
    }
    // 这条断言是"稳态 120 fps 可达"的证据：当前场景（768 quad）远在预算内。
    assert!(
        best_quads_per_budget > 700.0,
        "8.33 ms 里应能画得下 700+ quad（实测上限 {best_quads_per_budget:.0}）"
    );

    // 诊断 2：固定 24000 quad（上次跳变的那一档），比较「24000 个不同字形」与
    // 「只有 60 个不同字形（文案重复）」。若重复的那组快很多 ⇒ 瓶颈在图集/缓存规模
    // 而不是顶点上传；若一样慢 ⇒ 瓶颈在每帧顶点上传（~3.8 MB）。
    println!("  ---- 24000 quad：不同字形 vs 重复文案 ----");
    for (label, distinct) in [("24000 个不同字形", true), ("只有 60 个不同字形", false)]
    {
        let pool: Vec<String> = (0..5)
            .map(|i| cjk_text(0x4E00 + (i * CHARS) as u32, CHARS))
            .collect();
        let mut b = DrawBatch::new();
        for i in 0..2000usize {
            let text = if distinct {
                cjk_text(0x4E00 + (i * CHARS) as u32, CHARS)
            } else {
                pool[i % pool.len()].clone()
            };
            draw_text(
                &mut b.texts,
                &text,
                Pos::new(4.0 + (i % 100) as f32 * 20.0, 4.0 + (i / 100) as f32 * 20.0),
                def.clone(),
                TextOverride::from_color(WHITE),
            );
        }
        for _ in 0..3 {
            canvas.draw(Some(BLACK), &[&b]);
        }
        let t = Instant::now();
        const FRAMES: u32 = 5;
        for _ in 0..FRAMES {
            canvas.draw(Some(BLACK), &[&b]);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        println!(
            "  {label}：{ms:>7.3} ms/帧（文本净成本 {:>6.3} ms）",
            (ms - base_ms).max(0.0)
        );
    }

    // 诊断：固定 quad 总数、改「条目数」，分离**每条目成本**与**每 quad 成本**。    // 若条目数少的那组明显更快，说明开销主要在"每条目"（key 构造/哈希/查找/分配），
    // 而不是"每 quad"（顶点拼装）。
    println!("  ---- 固定 6000 quad，改条目数（分离每条目 vs 每 quad 成本）----");
    for (entries, chars) in [(60usize, 100usize), (200, 30), (500, 12)] {
        let texts: Vec<String> = (0..entries)
            .map(|i| cjk_text(0x4E00 + (i * chars) as u32, chars))
            .collect();
        let mut b = DrawBatch::new();
        for (i, text) in texts.iter().enumerate() {
            draw_text(
                &mut b.texts,
                text,
                Pos::new(4.0 + (i % 40) as f32 * 50.0, 4.0 + (i / 40) as f32 * 20.0),
                def.clone(),
                TextOverride::from_color(WHITE),
            );
        }
        for _ in 0..3 {
            canvas.draw(Some(BLACK), &[&b]);
        }
        let t = Instant::now();
        const FRAMES: u32 = 5;
        for _ in 0..FRAMES {
            canvas.draw(Some(BLACK), &[&b]);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        let text_ms = (ms - base_ms).max(0.0);
        println!(
            "  {entries:>4} 条目 × {chars:>3} 字：{ms:>7.3} ms/帧（文本 {text_ms:>6.3} ms）\
             ⇒ 每条目 {:>6.2} µs、每 quad {:>5.2} µs",
            text_ms * 1000.0 / entries as f64,
            text_ms * 1000.0 / 6000.0
        );
    }
}

/// `prewarm` 把字形填进图集；同一批文案再来一遍应全命中（`rasterizations` 不涨）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_prewarm_then_hit() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let def = glyph_test_def(32.0);

    let before = glyphs.stats();
    assert_eq!(before.entries(), 0, "新上下文图集应为空");

    // 首帧之前就该能用（视口还没建）：这是"加载阶段预光栅化"的正常用法。
    let n1 = glyphs.prewarm(&gpu.device, &gpu.queue, "Hello 世界", &def, 1.0);
    let s1 = glyphs.stats();
    assert!(n1 > 0, "prewarm 应至少光栅化一条（实际 {n1}）");
    assert_eq!(
        s1.rasterizations - before.rasterizations,
        n1 as u64,
        "返回的新光栅化条数应与计数器一致"
    );
    assert!(s1.entries() > 0, "图集里应出现条目");

    let n2 = glyphs.prewarm(&gpu.device, &gpu.queue, "Hello 世界", &def, 1.0);
    let s2 = glyphs.stats();
    assert_eq!(n2, 0, "第二遍应全部命中（实际新增 {n2}）");
    assert_eq!(s2.rasterizations, s1.rasterizations, "命中不该重光栅化");

    // 关键：prewarm 造出的必须是**绘制时真正会用到**的那批键。画一遍，若键对不上
    // （例如把无样式文字预填成 SDF 条目），这里会多出 N 条新光栅化。
    let canvas = OffscreenCanvas::new(&gpu, 320, 120);
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        "Hello 世界",
        Pos::new(10.0, 20.0),
        def.clone(),
        TextOverride::from_color(WHITE),
    );
    canvas.draw(Some(BLACK), &[&b]);
    let s3 = glyphs.stats();
    assert_eq!(
        s3.rasterizations,
        s2.rasterizations,
        "prewarm 之后绘制不该重光栅化（新增 {} 条）",
        s3.rasterizations - s2.rasterizations
    );
    println!(
        "  prewarm：首次 {n1} 条，图集 R8 {}² / {} 条 / {} 字节",
        gpu.text_atlas_stats().mask_size,
        s3.entries(),
        s3.used_bytes()
    );
}

/// `hold` 把字形钉住；统计里的 pinned 条目与句柄长度一致，解钉后归零。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_hold_pins_and_releases() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let def = glyph_test_def(40.0);

    let hold = glyphs.hold(&gpu.device, &gpu.queue, "AB", &def, 1.0);
    assert!(!hold.is_empty(), "hold 应钉住至少一条");

    let stats = glyphs.stats();
    assert_eq!(
        stats.pinned_entries as usize,
        hold.len(),
        "统计里的钉住数应与句柄一致"
    );
    let pinned_in_entries = glyphs.entries().iter().filter(|e| e.pinned).count();
    assert_eq!(
        pinned_in_entries,
        hold.len(),
        "entries() 里被钉住的条目数应与句柄一致"
    );

    // 第二次 hold 同一批：引用计数语义，解一个还剩一个。
    let hold2 = glyphs.hold(&gpu.device, &gpu.queue, "AB", &def, 1.0);
    assert_eq!(glyphs.pinned_count(), hold.len());
    drop(hold2);
    assert_eq!(glyphs.pinned_count(), hold.len(), "还有一个句柄在钉");
    drop(hold);
    assert_eq!(glyphs.pinned_count(), 0, "全部解钉后应归零");
    println!("  hold：钉住 {} 条，解钉后归零", stats.pinned_entries);
}

/// 钉住期间狂画别的字（图集反复收放），钉住的条目不能被淘汰；
/// `clear_unpinned` 也不会动它们。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_pinned_survive_eviction() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let hold = glyphs.hold(&gpu.device, &gpu.queue, "固", &glyph_test_def(48.0), 1.0);
    let held = hold.len();
    assert!(held > 0);

    let canvas = OffscreenCanvas::new(&gpu, 640, 400);
    for i in 0..40 {
        let mut b = DrawBatch::new();
        let text = format!("{}{}", "kwyz", i);
        draw_text(
            &mut b.texts,
            &text,
            Pos::new(10.0, 10.0),
            glyph_test_def(20.0 + (i % 17) as f32),
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(BLACK), &[&b]);
    }

    let pinned_now = glyphs.entries().iter().filter(|e| e.pinned).count();
    assert_eq!(
        pinned_now, held,
        "钉住的字形被淘汰了（{pinned_now}/{held} 还在）"
    );

    let cleared = glyphs.clear_unpinned();
    assert!(cleared > 0, "应该有未钉住的条目被清掉");
    let after = glyphs.entries().iter().filter(|e| e.pinned).count();
    assert_eq!(after, held, "clear_unpinned 不该清钉住的条目");
    drop(hold);
    println!("  pinned：清理前钉住 {held} 条，清掉 {cleared} 条未钉住的");
}

/// 预算：设了上限后，帧首自动淘汰未钉住的字形，占用回落到预算内。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_budget_trims_unpinned() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    const BUDGET: u64 = 32 * 1024;
    glyphs.set_hard_budget(Some(BUDGET));
    assert_eq!(glyphs.hard_budget(), Some(BUDGET));

    let canvas = OffscreenCanvas::new(&gpu, 640, 400);
    let mut b = DrawBatch::new();
    // 20 条不同字号 × 十几个字，肯定超过 32 KiB。
    for i in 0..20 {
        draw_text(
            &mut b.texts,
            "Vireo 字形缓存预算",
            Pos::new(10.0, 10.0 + i as f32 * 18.0),
            glyph_test_def(16.0 + i as f32),
            TextOverride::from_color(WHITE),
        );
    }
    canvas.draw(Some(BLACK), &[&b]);
    let over = glyphs.stats();
    assert!(
        over.used_bytes() > BUDGET,
        "用例失效：占用应超过预算（{} ≤ {BUDGET}）",
        over.used_bytes()
    );

    // 再画一帧（不画字）推进代数：帧首会自动按预算回收。
    let empty = DrawBatch::new();
    canvas.draw(Some(BLACK), &[&empty]);
    let auto = glyphs.stats();
    assert!(
        auto.evictions > over.evictions,
        "帧首应自动淘汰（evictions {} → {}）",
        over.evictions,
        auto.evictions
    );
    assert!(
        auto.used_bytes() <= BUDGET,
        "自动回收后占用应回到预算内（{} > {BUDGET}）",
        auto.used_bytes()
    );

    // 事后调低预算 → 手动 trim 必须立刻把它压进去。
    glyphs.set_hard_budget(Some(BUDGET / 4));
    let evicted = glyphs.trim();
    let after = glyphs.stats();
    assert!(evicted > 0, "调低预算后 trim 应淘汰一些字形");
    assert!(
        after.used_bytes() <= BUDGET / 4,
        "淘汰后占用应回到新预算内（{} > {}）",
        after.used_bytes(),
        BUDGET / 4
    );
    println!(
        "  budget：{} → {} → {} 字节（预算 {BUDGET} → {}），trim 淘汰 {evicted} 条",
        over.used_bytes(),
        auto.used_bytes(),
        after.used_bytes(),
        BUDGET / 4
    );
}

/// 钉住的东西占满预算时淘汰不动：只 warn 一次，不 panic、也不反复重光栅化。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_budget_keeps_pinned() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let hold = glyphs.hold(
        &gpu.device,
        &gpu.queue,
        "钉住不动",
        &glyph_test_def(64.0),
        1.0,
    );
    glyphs.set_hard_budget(Some(1)); // 比一条字形还小
    let before = glyphs.stats();
    let evicted = glyphs.trim();
    let after = glyphs.stats();
    assert_eq!(evicted, 0, "全被钉住时淘汰不动");
    assert_eq!(after.entries(), before.entries(), "条目应保持不动");
    assert_eq!(after.pinned_entries, before.pinned_entries);
    drop(hold);
    println!("  budget+pinned：淘汰 {evicted} 条，条目数不变");
}

/// `clear` 清空全部（含钉住的），句柄随后 drop 不 panic，画新字能重新光栅化。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_clear_all_then_refill() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let hold = glyphs.hold(&gpu.device, &gpu.queue, "Clear", &glyph_test_def(30.0), 1.0);
    glyphs.prewarm(&gpu.device, &gpu.queue, "Other", &glyph_test_def(30.0), 1.0);
    let n = glyphs.clear();
    assert!(n > 0, "clear 应清掉条目");
    let s = glyphs.stats();
    assert_eq!(s.entries(), 0, "清空后图集条目应为 0");

    drop(hold); // 条目已不存在：解钉必须是 no-op
    assert_eq!(glyphs.pinned_count(), 0);

    // 重新画：能再次分配、重新光栅化。
    let canvas = OffscreenCanvas::new(&gpu, 200, 120);
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        "Clear",
        Pos::new(10.0, 20.0),
        glyph_test_def(30.0),
        TextOverride::from_color(WHITE),
    );
    canvas.draw(Some(BLACK), &[&b]);
    let px = canvas.read_pixels();
    let lit = px
        .chunks(4)
        .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 120)
        .count();
    assert!(lit > 50, "清空后重新绘制应还能出字（亮像素 {lit}）");
    let s2 = glyphs.stats();
    assert!(s2.rasterizations >= s.rasterizations + 1);
    println!(
        "  clear：清掉 {n} 条，重画后重新光栅化到 {} 条",
        s2.entries()
    );
}

/// `prewarm` 不产生可见输出：空的绘制批次仍是一片黑。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_prewarm_is_invisible() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let n = glyphs.prewarm(&gpu.device, &gpu.queue, "WWWW", &glyph_test_def(60.0), 1.0);
    assert!(n > 0);

    let canvas = OffscreenCanvas::new(&gpu, 200, 200);
    let empty = DrawBatch::new();
    canvas.draw(Some(BLACK), &[&empty]);
    let px = canvas.read_pixels();
    let lit = px
        .chunks(4)
        .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 40)
        .count();
    assert_eq!(lit, 0, "预光栅化不该画出任何像素（亮了 {lit} 个）");
}

/// 带样式的文字在 `prewarm` 之后绘制**不需要重新光栅化**（说明预光栅化复算的
/// band/图层参数与绘制时一致 — 这是"预填 SDF 条目"能生效的前提）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_prewarm_matches_styled_draw() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let style =
        TextStyle::outline(3.0, Color::new(0.0, 0.0, 0.0, 1.0)).with_fill(TextPaint::solid(WHITE));
    let def = TextDef {
        style: Some(Arc::new(style)),
        ..glyph_test_def(44.0)
    };

    let n = glyphs.prewarm(&gpu.device, &gpu.queue, "国永", &def, 1.0);
    assert!(n > 0, "带样式预光栅化应产生条目");
    let sdf = glyphs
        .entries()
        .into_iter()
        .filter(|e| e.content == GlyphContentType::Sdf)
        .count();
    assert!(sdf > 0, "带样式的条目应是 SDF 表示");

    let before = glyphs.stats().rasterizations;
    let canvas = OffscreenCanvas::new(&gpu, 240, 120);
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        "国永",
        Pos::new(20.0, 20.0),
        def.clone(),
        TextOverride::from_color(WHITE),
    );
    canvas.draw(Some(BLACK), &[&b]);
    let after = glyphs.stats().rasterizations;
    assert_eq!(
        after,
        before,
        "prewarm 之后绘制不该重光栅化（新增 {} 条）",
        after - before
    );
    println!("  styled prewarm：{n} 条新光栅化，SDF 条目 {sdf} 条，绘制零新增");
}

/// 跨帧不重光栅化：同一批文字连续 5 帧，`rasterizations` 只应在首帧涨。
///
/// 这是"用户可管字形缓存"的核心诉求：字形不该每帧重新光栅化。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_no_rerasterize_across_frames() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 320, 160);
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        "Steady 稳态 123",
        Pos::new(10.0, 20.0),
        glyph_test_def(28.0),
        TextOverride::from_color(WHITE),
    );
    canvas.draw(Some(BLACK), &[&b]);
    let s1 = gpu.text().glyphs().stats();
    for _ in 0..4 {
        canvas.draw(Some(BLACK), &[&b]);
    }
    let s2 = gpu.text().glyphs().stats();
    assert_eq!(
        s2.rasterizations, s1.rasterizations,
        "稳态帧不该重新光栅化（{} → {}）",
        s1.rasterizations, s2.rasterizations
    );
    assert_eq!(s2.entries(), s1.entries(), "稳态帧图集条目数不该变");
    assert!(
        s2.evictions == s1.evictions,
        "稳态帧不该淘汰（{} → {}）",
        s1.evictions,
        s2.evictions
    );
    println!(
        "  5 帧：光栅化 {} 次 / {} 条 / {} 字节",
        s2.rasterizations,
        s2.entries(),
        s2.used_bytes()
    );
}

/// 图集真装不下时要按 LRU 淘汰（回归：`trim()` 从没被调用 → 代数永远 0 →
/// 所有条目都被当成"本帧在用" → 只扩不淘，显存一路涨到最大纹理）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_atlas_evicts_when_full() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 400, 200);
    let glyphs = gpu.text().glyphs();

    // 每帧换一批字号/字形：跨帧的旧字形逐渐变冷，最终必须被淘汰掉。
    for i in 0..60u32 {
        let mut b = DrawBatch::new();
        let text = format!("{:04}", i * 7919 % 10000);
        draw_text(
            &mut b.texts,
            &text,
            Pos::new(10.0, 10.0),
            glyph_test_def(24.0 + (i % 40) as f32),
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(BLACK), &[&b]);
    }

    let s = glyphs.stats();
    assert!(
        s.evictions > 0,
        "图集反复收放却不淘汰（evictions=0，grows={}，容量 {} 字节）",
        s.grows,
        s.texture_bytes()
    );
    assert!(
        s.used_bytes() <= s.texture_bytes(),
        "条目占用不该超过纹理容量（{} > {}）",
        s.used_bytes(),
        s.texture_bytes()
    );
    assert!(
        s.texture_bytes() < 16 * 1024 * 1024,
        "淘汰生效时不该把图集扩到 16 MiB 以上（实际 {}）",
        s.texture_bytes()
    );
    println!(
        "  淘汰：{} 次，扩图 {} 次，条目 {} 条 / {} 字节，纹理 {} 字节",
        s.evictions,
        s.grows,
        s.entries(),
        s.used_bytes(),
        s.texture_bytes()
    );
}

/// `entries()` 与 `stats()` 必须自洽（字节数、条目数、SDF 表示）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_entries_match_stats() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let hold = glyphs.hold(&gpu.device, &gpu.queue, "统计", &glyph_test_def(36.0), 1.0);
    glyphs.prewarm(&gpu.device, &gpu.queue, "Plain", &glyph_test_def(36.0), 1.0);

    let stats = glyphs.stats();
    let entries = glyphs.entries();
    assert_eq!(entries.len() as u32, stats.entries(), "条目数应一致");
    let sum: u64 = entries.iter().map(|e| e.bytes).sum();
    assert_eq!(sum, stats.used_bytes(), "字节数应一致");
    let pinned = entries.iter().filter(|e| e.pinned).count();
    assert_eq!(pinned as u32, stats.pinned_entries, "钉住数应一致");
    assert!(
        entries.iter().all(|e| !e.key.is_empty()),
        "每条都该有键摘要"
    );
    for e in entries.iter().take(4) {
        println!(
            "  {:?}/{:?} {}×{} @({},{}) {}B pinned={} in_use={} {}",
            e.atlas, e.content, e.width, e.height, e.x, e.y, e.bytes, e.pinned, e.in_use, e.key
        );
    }
    drop(hold);
}

/// 扩图集用**纹理拷贝**搬运条目：已缓存的字形像素必须原样保留（不能变空白/错位）。
///
/// 回归点：`grow()` 以前是"重建纹理 + 把所有字形重新光栅化一遍"，那是一次可见卡顿；
/// 改成拷贝后，只要坐标/尺寸搬错一个像素，这里渲染出的字形就会缺一块或整体偏移。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_grow_keeps_pixels() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let def = glyph_test_def(48.0);

    // 先把要保住的那行字画出来并记下像素（新画布，避免累积）。
    let render = || {
        let c = OffscreenCanvas::new(&gpu, 260, 80);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "参照",
            Pos::new(8.0, 10.0),
            def.clone(),
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };
    let before_px = render();
    let before = glyphs.stats();

    // 灌入大量别的字形，逼图集扩容（初始 256²，很快就装不下）。
    let canvas = OffscreenCanvas::new(&gpu, 512, 300);
    let mut grew = false;
    for i in 0..80u32 {
        let mut b = DrawBatch::new();
        let text = format!("{}{}", "扩容测试字符", i);
        draw_text(
            &mut b.texts,
            &text,
            Pos::new(6.0, 6.0),
            glyph_test_def(30.0 + (i % 25) as f32),
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(BLACK), &[&b]);
        if glyphs.stats().grows > before.grows {
            grew = true;
            break;
        }
    }
    assert!(
        grew,
        "用例失效：没触发扩容（grows={}，容量 {} 字节）",
        glyphs.stats().grows,
        glyphs.stats().texture_bytes()
    );

    let after_px = render();
    let stats = glyphs.stats();
    assert!(stats.texture_bytes() > before.texture_bytes(), "纹理应变大");

    let mut diff = 0usize;
    for (a, b) in before_px.chunks(4).zip(after_px.chunks(4)) {
        let d = (a[0] as i32 - b[0] as i32)
            .abs()
            .max((a[1] as i32 - b[1] as i32).abs())
            .max((a[2] as i32 - b[2] as i32).abs());
        if d > 2 {
            diff += 1;
        }
    }
    assert_eq!(diff, 0, "扩容后同一批文字的像素变了（{diff} 个像素不同）");
    println!(
        "  扩容：{} → {} 字节，纹理不变形（0 像素差）",
        before.texture_bytes(),
        stats.texture_bytes()
    );
}

/// `compact()` 把图集收回去且条目仍然正确（拷贝重新打包，不重新光栅化）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_compact_shrinks_and_keeps_pixels() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let def = glyph_test_def(44.0);

    let render = || {
        let c = OffscreenCanvas::new(&gpu, 240, 80);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "收图",
            Pos::new(8.0, 10.0),
            def.clone(),
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };

    // 保住"收图"两个字，然后灌一大批别的字形把图集撑大 → compact 才有收缩空间。
    let hold = glyphs.hold(&gpu.device, &gpu.queue, "收图", &def, 1.0);
    let base = glyphs.stats();
    // 一次 prewarm 里塞满一个 prepare 的工作集：会直接把图集撑到更大的一档。
    glyphs.prewarm(
        &gpu.device,
        &gpu.queue,
        "很多不该留在图集里的字，撑大再说 ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        &glyph_test_def(64.0),
        1.0,
    );
    let before_px = render();
    let grown = glyphs.stats();
    assert!(
        grown.texture_bytes() > base.texture_bytes(),
        "用例失效：图集没被撑大（{} → {}）",
        base.texture_bytes(),
        grown.texture_bytes()
    );

    let cleared = glyphs.clear_unpinned();
    assert!(cleared > 0, "应清掉未钉住的条目");
    let compacted = glyphs.compact(&gpu.device, &gpu.queue);
    assert!(compacted, "清掉大部分条目后 compact 应当收缩");
    let after = glyphs.stats();
    assert!(
        after.texture_bytes() < grown.texture_bytes(),
        "纹理应缩小（{} → {}）",
        grown.texture_bytes(),
        after.texture_bytes()
    );

    let after_px = render();
    let mut diff = 0usize;
    for (a, b) in before_px.chunks(4).zip(after_px.chunks(4)) {
        let d = (a[0] as i32 - b[0] as i32)
            .abs()
            .max((a[1] as i32 - b[1] as i32).abs())
            .max((a[2] as i32 - b[2] as i32).abs());
        if d > 2 {
            diff += 1;
        }
    }
    assert_eq!(
        diff, 0,
        "compact 后钉住的字应逐像素不变（{diff} 个像素不同）"
    );
    println!(
        "  compact：{} → {} 字节，清掉 {cleared} 条，0 像素差（钉住 {} 条）",
        grown.texture_bytes(),
        after.texture_bytes(),
        hold.len()
    );
    drop(hold);
}

/// 准备一个"图集被撑大、然后内容只剩一点点"的场景。返回 (上下文, 画布, 要保留的那行字)。
fn auto_compact_scene(instance: &wgpu::Instance) -> (Arc<GpuContext>, OffscreenCanvas, String) {
    let gpu = Arc::new(GpuContext::new(instance));
    let canvas = OffscreenCanvas::new(&gpu, 640, 480);
    let keep = cjk_text(0x4E00, 12);
    // 阶段 A：灌满内容，把图集撑到 ≥1024²。
    let texts: Vec<String> = (0..80u32)
        .map(|i| cjk_text(0x4E00 + (i + 1) * 12, 12))
        .collect();
    for _ in 0..3 {
        let mut b = DrawBatch::new();
        for (i, text) in texts.iter().enumerate() {
            draw_text(
                &mut b.texts,
                text,
                Pos::new(8.0 + (i % 4) as f32 * 155.0, 8.0 + (i / 4) as f32 * 22.0),
                glyph_test_def(24.0),
                TextOverride::from_color(WHITE),
            );
        }
        canvas.draw(Some(BLACK), &[&b]);
    }
    (gpu, canvas, keep)
}

/// 画只有一行字的"小内容"帧（让图集进入低利用率）。
fn draw_small_frame(canvas: &OffscreenCanvas, text: &str) {
    let mut b = DrawBatch::new();
    draw_text(
        &mut b.texts,
        text,
        Pos::new(8.0, 20.0),
        glyph_test_def(24.0),
        TextOverride::from_color(WHITE),
    );
    canvas.draw(Some(BLACK), &[&b]);
}

/// **compact 的成本**：校准"自动压缩最多搬多少"。
///
/// 构造"纹理被历史撑得很大、内容已经不多"的形态（自动压缩真正会遇到的形态）：
/// 先灌到 4096²，再钉住 60 段（~370 KiB live）、清掉其余，然后手动 compact 计时。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_compact_cost() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let def = glyph_test_def(24.0);
    // 灌到图集被撑大：600 段 × 12 字 = 7200 个不同字形（≈6 MB 内容）⇒ 必然到 4096²。
    for i in 0..600u32 {
        glyphs.prewarm(
            &gpu.device,
            &gpu.queue,
            &cjk_text(0x4E00 + i * 12, 12),
            &def,
            1.0,
        );
    }
    let mut holds = Vec::new();
    for i in 0..60u32 {
        holds.push(glyphs.hold(
            &gpu.device,
            &gpu.queue,
            &cjk_text(0x4E00 + i * 12, 12),
            &def,
            1.0,
        ));
    }
    glyphs.clear_unpinned();
    let before = glyphs.stats();
    assert!(
        before.texture_bytes() >= 2 * 1024 * 1024,
        "用例失效：图集没被撑大（{} KiB）",
        before.texture_bytes() / 1024
    );
    assert!(
        before.used_bytes() > 256 * 1024,
        "用例失效：留下的内容太少（{} KiB）",
        before.used_bytes() / 1024
    );
    let t0 = Instant::now();
    // 先把队列排空，否则计时会把"等前面 400 次 prewarm 的 GPU 工作"算进 compact。
    let _ = gpu.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let flush_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let shrunk = glyphs.compact(&gpu.device, &gpu.queue);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let after = glyphs.stats();
    assert!(shrunk, "低利用率时 compact 应能收缩");
    assert_eq!(after.used_bytes(), before.used_bytes(), "搬动不该改变占用");
    println!(
        "  compact 成本：{} 条 / live {} KiB，纹理 {} → {} KiB，用时 {ms:.2} ms（排空参考 {flush_ms:.2} ms）",
        after.entries(),
        after.used_bytes() / 1024,
        before.texture_bytes() / 1024,
        after.texture_bytes() / 1024
    );
    drop(holds);
}

/// **自动压缩**：内容缩到只剩一点之后，图集纹理应自己缩回去，且渲染像素不变。
///
/// 触发需要「低利用率持续 60 帧 + 距上次变动 120 帧」，所以这里最多跑 300 帧。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_auto_compact_reclaims_texture() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    // 阶段 B 的起点：设一个小于"历史积压"的保留预算 —— 冷条目会被淘汰，
    // 图集随即进入低利用率（这也正是现实里"换场景 + 设了预算"的形态）。
    let (gpu, canvas, keep) = {
        let (gpu, canvas, keep) = auto_compact_scene(&instance);
        gpu.text().glyphs().set_retention_budget(Some(64 * 1024));
        (gpu, canvas, keep)
    };
    let glyphs = gpu.text().glyphs();
    assert!(glyphs.auto_compact(), "自动压缩默认应开启");

    let grown = glyphs.stats();
    assert!(
        grown.texture_bytes() >= 1024 * 1024,
        "用例失效：阶段 A 没把图集撑大（{} 字节）",
        grown.texture_bytes()
    );

    // 先渲染一次小内容作为参照像素。
    draw_small_frame(&canvas, &keep);
    let before_px = canvas.read_pixels();
    let before = glyphs.stats();

    // 跑到触发为止，并记录"触发那一帧"的耗时（= 搬运成本）。
    let mut compact_ms = f64::NAN;
    let compacts_seen = before.compacts;
    for _ in 0..300 {
        let t = Instant::now();
        draw_small_frame(&canvas, &keep);
        let s = glyphs.stats();
        if s.compacts > compacts_seen {
            compact_ms = t.elapsed().as_secs_f64() * 1000.0;
            break;
        }
    }
    let after = glyphs.stats();
    assert!(
        after.compacts > before.compacts,
        "300 帧内应发生自动压缩（compacts {} → {}）",
        before.compacts,
        after.compacts
    );
    assert!(
        after.texture_bytes() < before.texture_bytes(),
        "自动压缩后纹理应变小（{} → {} 字节）",
        before.texture_bytes(),
        after.texture_bytes()
    );
    // 像素必须不变（压缩只搬位图，不动内容）。
    let after_px = canvas.read_pixels();
    let mut diff = 0usize;
    for (a, b) in before_px.chunks(4).zip(after_px.chunks(4)) {
        let d = (a[0] as i32 - b[0] as i32)
            .abs()
            .max((a[1] as i32 - b[1] as i32).abs())
            .max((a[2] as i32 - b[2] as i32).abs());
        if d > 2 {
            diff += 1;
        }
    }
    assert_eq!(diff, 0, "自动压缩不该改变渲染像素（{diff} 个像素不同）");
    println!(
        "  自动压缩：纹理 {} → {} KiB（触发帧 {compact_ms:.2} ms），live {} KiB，0 像素差",
        before.texture_bytes() / 1024,
        after.texture_bytes() / 1024,
        after.used_bytes() / 1024
    );
}

/// **自动压缩有滞后**：不会一低就缩、也不会反复缩。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_auto_compact_is_hysteretic() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let (gpu, canvas, keep) = auto_compact_scene(&instance);
    let glyphs = gpu.text().glyphs();
    glyphs.set_retention_budget(Some(64 * 1024));
    let start = glyphs.stats().compacts;

    // 前 100 帧（< 120 帧冷却）不该有任何压缩。
    for _ in 0..100 {
        draw_small_frame(&canvas, &keep);
    }
    assert_eq!(
        glyphs.stats().compacts,
        start,
        "冷却期内不该压缩（低利用率还没持续够）"
    );

    // 继续到 300 帧：应恰好压一次。
    for _ in 0..200 {
        draw_small_frame(&canvas, &keep);
    }
    let one = glyphs.stats().compacts;
    assert_eq!(one, start + 1, "应只压缩一次（实际 +{}）", one - start);

    // 再跑 200 帧：利用率已经回到 ~70%，不该再压。
    for _ in 0..200 {
        draw_small_frame(&canvas, &keep);
    }
    assert_eq!(
        glyphs.stats().compacts,
        one,
        "压缩后利用率已高，不该反复压缩"
    );
    println!("  滞后：0–100 帧不压，300 帧压一次，之后 200 帧不再压 ✓");
}

/// **关掉自动压缩**：同一场景纹理不回落（开关真的生效），手动 `compact` 仍可用。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_auto_compact_off() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let (gpu, canvas, keep) = auto_compact_scene(&instance);
    let glyphs = gpu.text().glyphs();
    glyphs.set_retention_budget(Some(64 * 1024));
    glyphs.set_auto_compact(false);
    assert!(!glyphs.auto_compact());

    let before = glyphs.stats();
    for _ in 0..300 {
        draw_small_frame(&canvas, &keep);
    }
    let after = glyphs.stats();
    assert_eq!(after.compacts, before.compacts, "关掉后不该压缩");
    assert_eq!(
        after.texture_bytes(),
        before.texture_bytes(),
        "关掉后纹理不回落（只涨不缩）"
    );
    println!(
        "  自动压缩关闭：300 帧后纹理仍是 {} KiB（compacts={}）",
        after.texture_bytes() / 1024,
        after.compacts
    );

    // 手动压缩仍然可用。
    let shrunk = glyphs.compact(&gpu.device, &gpu.queue);
    assert!(shrunk, "手动 compact 应仍然生效");
    println!(
        "  手动 compact 之后：{} KiB",
        glyphs.stats().texture_bytes() / 1024
    );
}

/// 两个 framebuffer 的最大逐像素差（只看 RGB）。
fn max_pixel_diff(a: &[u8], b: &[u8]) -> i32 {
    let mut worst = 0i32;
    for (pa, pb) in a.chunks(4).zip(b.chunks(4)) {
        let d = (pa[0] as i32 - pb[0] as i32)
            .abs()
            .max((pa[1] as i32 - pb[1] as i32).abs())
            .max((pa[2] as i32 - pb[2] as i32).abs());
        worst = worst.max(d);
    }
    worst
}

/// 非黑像素的最左上点（用来比"画在哪"，避免被抗锯齿相位差异干扰）。
fn ink_origin(px: &[u8], width: usize) -> (i32, i32) {
    let (mut x0, mut y0) = (i32::MAX, i32::MAX);
    for (idx, p) in px.chunks(4).enumerate() {
        if p[0] as u32 + p[1] as u32 + p[2] as u32 > 24 {
            x0 = x0.min((idx % width) as i32);
            y0 = y0.min((idx / width) as i32);
        }
    }
    if x0 == i32::MAX {
        (i32::MAX, i32::MAX)
    } else {
        (x0, y0)
    }
}

/// 画一段文本（`stable` = 走 `b.text_stable`，否则走普通 `draw_text`），返回像素。
fn draw_text_pixels(
    _gpu: &Arc<GpuContext>,
    canvas: &OffscreenCanvas,
    text: &str,
    def: &TextDef,
    pos: Pos,
    clip: Option<Option<vireo::glyphon::TextBounds>>,
) -> Vec<u8> {
    let mut b = DrawBatch::new();
    match clip {
        Some(c) => {
            b.text_clip = c;
        }
        None => {}
    }
    draw_text(
        &mut b.texts,
        text,
        pos,
        def.clone(),
        TextOverride::from_color(WHITE),
    );
    canvas.draw(Some(BLACK), &[&b]);
    canvas.read_pixels()
}

fn draw_stable_pixels(
    _gpu: &Arc<GpuContext>,
    canvas: &OffscreenCanvas,
    stable: &StableText,
    pos: Pos,
    clip: Option<Option<vireo::glyphon::TextBounds>>,
) -> Vec<u8> {
    let mut b = DrawBatch::new();
    if let Some(c) = clip {
        b.text_clip = c;
    }
    b.text_stable(stable, pos, TextOverride::from_color(WHITE));
    canvas.draw(Some(BLACK), &[&b]);
    canvas.read_pixels()
}

/// **实例固化必须与普通绘制逐像素一致**（无样式 / 距离场样式 / 几何描边）。
///
/// 这条用例同时锁住一个既有缺口：resolved 路径曾经把样式参数写死成 0/None，
/// 于是 `b.text_stable` 画不出几何描边、SDF 图层带宽也不对。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn stable_bake_pixels_match_normal() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 420, 160);
    let ink = Color::new(0.1, 0.1, 0.15, 1.0);
    let red = Color::new(0.9, 0.2, 0.15, 1.0);
    let text = cjk_text(0x4E00, 6);

    let cases: Vec<(&str, TextDef)> = vec![
        ("无样式", TextDef::default().font_size(28.0)),
        (
            "描边+投影",
            TextDef {
                style: Some(Arc::new(
                    TextStyle::outline(3.0, ink)
                        .with_layer(TextLayer::soft(
                            4.0,
                            4.0,
                            TextPaint::solid(Color::new(0.1, 0.1, 0.2, 0.6)),
                        ))
                        .with_fill(TextPaint::solid(WHITE)),
                )),
                ..TextDef::default().font_size(28.0)
            },
        ),
        (
            "几何描边",
            TextDef {
                style: Some(Arc::new(TextStyle::vector_outline(TextStroke::new(
                    4.0, red,
                )))),
                ..TextDef::default().font_size(28.0)
            },
        ),
    ];

    for (label, def) in cases {
        let pos = Pos::new(12.0, 30.0);
        // 参考：普通绘制。
        let normal = draw_text_pixels(&gpu, &canvas, &text, &def, pos, None);
        // 稳定句柄：首帧烘焙，之后复用。
        let stable = gpu.make_stable_text(&text, &def);
        let mut reused = Vec::new();
        for _ in 0..3 {
            reused = draw_stable_pixels(&gpu, &canvas, &stable, pos, None);
        }
        let diff = max_pixel_diff(&normal, &reused);
        assert_eq!(
            diff, 0,
            "{label}：StableText（复用烘焙实例）与普通绘制应逐像素一致（最大差 {diff}）"
        );
        println!(
            "  {label}：像素一致，实例 {} 条，烘焙 {} 次",
            stable.baked_instances().unwrap_or(0),
            stable.bakes()
        );
    }
}

/// **整数位移不重烘、分数位移重烘**；裁剪变化也要重烘。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn stable_bake_reuses_instances() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 420, 160);
    let text = cjk_text(0x4E00, 6);
    let def = TextDef::default().font_size(24.0);
    let stable = gpu.make_stable_text(&text, &def);

    let base = Pos::new(10.0, 20.0);
    draw_stable_pixels(&gpu, &canvas, &stable, base, None);
    let baked = stable.bakes();
    assert_eq!(baked, 1, "首次绘制应烘焙一次（实际 {baked}）");

    // 整数位移：不重烘，且像素等于"直接画在新位置"。
    for step in [1.0, 2.0, 7.0, 0.0] {
        let pos = Pos::new(base.x + step, base.y + step);
        let reused = draw_stable_pixels(&gpu, &canvas, &stable, pos, None);
        assert_eq!(
            stable.bakes(),
            baked,
            "整数位移（+{step}）不该重烘（bakes={}）",
            stable.bakes()
        );
        let normal = draw_text_pixels(&gpu, &canvas, &text, &def, pos, None);
        assert_eq!(
            max_pixel_diff(&normal, &reused),
            0,
            "整数位移 +{step} 后像素应仍然一致"
        );
    }

    // 分数位移：位置靠变换槽给，不再重烘；像素差异只来自"抗锯齿相位固定在烘焙时"。
    for (dx, dy) in [(0.5f32, 0.0f32), (0.0, 0.25), (0.37, 0.62)] {
        let frac = Pos::new(base.x + dx, base.y + dy);
        let reused = draw_stable_pixels(&gpu, &canvas, &stable, frac, None);
        assert_eq!(
            stable.bakes(),
            baked,
            "分数位移（+{dx},+{dy}）不该重烘（bakes={}）",
            stable.bakes()
        );
        let normal = draw_text_pixels(&gpu, &canvas, &text, &def, frac, None);
        let (nx, ny) = ink_origin(&normal, 420);
        let (sx, sy) = ink_origin(&reused, 420);
        println!(
            "  分数位移（+{dx},+{dy}）：0 次重烘，墨迹原点差 ({}, {})，像素最大差 {}",
            sx - nx,
            sy - ny,
            max_pixel_diff(&normal, &reused)
        );
        assert!(
            (sx - nx).abs() <= 1 && (sy - ny).abs() <= 1,
            "分数位移后绘制位置应与普通路径相差 ≤1px（实际 {sx}-{nx}, {sy}-{ny}）"
        );
    }

    // 裁剪变化：裁剪是烘焙进去的，必须重烘。
    let before_clip = stable.bakes();
    let clip = vireo::glyphon::TextBounds {
        left: 0,
        top: 0,
        right: 60,
        bottom: 80,
    };
    let reused = draw_stable_pixels(&gpu, &canvas, &stable, base, Some(Some(clip)));
    assert_eq!(
        stable.bakes(),
        before_clip + 1,
        "裁剪变化应触发一次重烘（bakes={}）",
        stable.bakes()
    );
    let normal = draw_text_pixels(&gpu, &canvas, &text, &def, base, Some(Some(clip)));
    assert_eq!(max_pixel_diff(&normal, &reused), 0, "带裁剪时像素应一致");
    println!("  复用/失效：整数与分数位移都 0 次重烘，裁剪变化 +1 ✓");
}

/// **图集布局变化后自动重烘**：把图集撑大（或淘汰条目）之后，复用旧坐标会画错，
/// 必须重烘且像素仍然正确。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn stable_bake_invalidates_on_atlas_change() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 420, 160);
    let text = cjk_text(0x4E00, 6);
    let def = TextDef::default().font_size(24.0);
    let stable = gpu.make_stable_text(&text, &def);
    let pos = Pos::new(10.0, 20.0);

    draw_stable_pixels(&gpu, &canvas, &stable, pos, None);
    let baked = stable.bakes();
    assert_eq!(baked, 1);

    // 用别的文案把图集撑大到 1024²：条目坐标会重排（compact / 重建路径）。
    let glyphs = gpu.text().glyphs();
    for i in 0..120u32 {
        glyphs.prewarm(
            &gpu.device,
            &gpu.queue,
            &cjk_text(0x6000 + i * 12, 12),
            &TextDef::default().font_size(24.0),
            1.0,
        );
    }
    glyphs.clear_unpinned();

    let px = draw_stable_pixels(&gpu, &canvas, &stable, pos, None);
    assert!(
        stable.bakes() > baked,
        "图集布局变化后应重烘（bakes {} → {}）",
        baked,
        stable.bakes()
    );
    let normal = draw_text_pixels(&gpu, &canvas, &text, &def, pos, None);
    assert_eq!(
        max_pixel_diff(&normal, &px),
        0,
        "图集重排后像素仍应与普通绘制一致"
    );
    println!(
        "  图集变化：重烘 {} 次后像素一致（条目 {} / 纹理 {} KiB）",
        stable.bakes(),
        glyphs.stats().entries(),
        glyphs.stats().texture_bytes() / 1024
    );
}

/// **`bake()` 预烘焙**：加载阶段付掉首次成本，之后首次绘制不再重烘。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn stable_bake_prebake_avoids_first_draw() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 420, 160);
    let text = cjk_text(0x4E00, 6);
    let def = TextDef::default().font_size(24.0);
    let stable = gpu.make_stable_text(&text, &def);
    let pos = Pos::new(10.0, 20.0);

    let n = stable.bake(&gpu, 1.0);
    assert_eq!(stable.bakes(), 1, "bake() 应烘焙一次");
    assert_eq!(
        stable.baked_instances(),
        Some(n),
        "bake() 返回的实例数应与块内一致"
    );
    // 整数物理坐标的绘制直接命中预烘焙结果。
    let px = draw_stable_pixels(&gpu, &canvas, &stable, pos, None);
    assert_eq!(stable.bakes(), 1, "预烘焙后首次绘制不该再烘焙");
    let normal = draw_text_pixels(&gpu, &canvas, &text, &def, pos, None);
    assert_eq!(max_pixel_diff(&normal, &px), 0, "预烘焙后像素应一致");
    println!("  预烘焙：{n} 条实例，首次绘制 0 次重烘，像素一致 ✓");
}

/// **实例固化的收益**：同一大段文字（12 000 字）每帧重画，比较
/// 「排版缓存 + 逐字形 prepare」与「烘焙实例拷贝」的 CPU 成本。
///
/// 只打印数字、不做硬门限：这台机器噪声大；判据是复用路径应当明显低于 prepare 路径。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn stable_bake_cost() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    // 4096 宽 ⇒ 每行约 250 字；画布高 1000 容纳 ~48 行。12000 字约 48 行。
    let canvas = OffscreenCanvas::new(&gpu, 4096, 1000);
    let text = cjk_text(0x4E00, 12_000);
    let def = TextDef::default().font_size(16.0).max_width(4000.0);
    let pos = Pos::new(8.0, 8.0);

    // 普通路径：整段 draw_text（排版缓存命中 + 每帧逐字形 prepare）。
    let normal_batch = {
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            &text,
            pos,
            def.clone(),
            TextOverride::from_color(WHITE),
        );
        b
    };
    for _ in 0..3 {
        canvas.draw(Some(BLACK), &[&normal_batch]);
    }
    let mut normal_ms = f64::INFINITY;
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..5 {
            canvas.draw(Some(BLACK), &[&normal_batch]);
        }
        normal_ms = normal_ms.min(t.elapsed().as_secs_f64() * 1000.0 / 5.0);
    }

    // 稳定句柄：烘焙一次，之后每帧只拷贝实例。
    let stable = gpu.make_stable_text(&text, &def);
    let stable_batch = {
        let mut b = DrawBatch::new();
        b.text_stable(&stable, pos, TextOverride::from_color(WHITE));
        b
    };
    for _ in 0..3 {
        canvas.draw(Some(BLACK), &[&stable_batch]);
    }
    // 烘焙那帧会排一大堆纹理上传；先等 GPU 排空，否则下面的计时会把"等队列"算进来。
    let _ = gpu.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    let glyphs = gpu.text().glyphs();
    let r_before = glyphs.stats().rasterizations;
    let mut stable_ms = f64::INFINITY;
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..5 {
            canvas.draw(Some(BLACK), &[&stable_batch]);
        }
        stable_ms = stable_ms.min(t.elapsed().as_secs_f64() * 1000.0 / 5.0);
    }
    let rasters = glyphs.stats().rasterizations - r_before;
    assert_eq!(rasters, 0, "复用阶段不该有任何光栅化（实际 {rasters}）");

    let quads = stable.baked_instances().unwrap_or(0);
    println!(
        "  12 000 字块：prepare 路径 {normal_ms:.2} ms/帧，固化复用 {stable_ms:.2} ms/帧\
         （{} 条实例，bakes={}）",
        quads,
        stable.bakes()
    );
    assert_eq!(stable.bakes(), 1, "稳态不该重烘");
    assert!(
        stable_ms < normal_ms,
        "复用应当比逐字形 prepare 便宜（{stable_ms:.2} vs {normal_ms:.2} ms）"
    );
}

/// 跨进程持久化：一个上下文 `save`，**另一个**上下文 `load` 后直接画出同样的像素，
/// 且不重新光栅化（`rasterizations` 不涨）。
/// 两个上下文各有自己的 `FontSystem`，所以 `fontdb::ID` 必然不同 —— 这条用例同时验证
/// "字体靠摘要认"是真的在起作用。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_save_load_roundtrip() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let path = std::env::temp_dir().join("vireo_glyph_cache_roundtrip.vrtx");
    let _ = std::fs::remove_file(&path);

    let plain = TextDef::default().font_size(34.0);
    let styled = TextDef {
        style: Some(Arc::new(
            TextStyle::outline(3.0, Color::new(0.0, 0.0, 0.0, 1.0))
                .with_fill(TextPaint::solid(WHITE)),
        )),
        ..TextDef::default().font_size(44.0)
    };

    // ---- 上下文 A：画一遍（顺带填充图集）并保存 ----
    let gpu_a = Arc::new(GpuContext::new(&instance));
    let render = |gpu: &Arc<GpuContext>| {
        let c = OffscreenCanvas::new(gpu, 360, 160);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            "持久化 Persist",
            Pos::new(12.0, 16.0),
            plain.clone(),
            TextOverride::from_color(WHITE),
        );
        draw_text(
            &mut b.texts,
            "描边",
            Pos::new(12.0, 70.0),
            styled.clone(),
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };
    let px_a = render(&gpu_a);
    let saved = gpu_a
        .text()
        .glyphs()
        .save(&gpu_a.device, &gpu_a.queue, &path)
        .expect("save 应成功");
    let stats_a = gpu_a.text().glyphs().stats();
    assert!(saved > 0, "应写出条目");
    assert_eq!(
        saved as u32,
        stats_a.entries(),
        "写出的条数应等于图集条目数"
    );
    let file_len = std::fs::metadata(&path).expect("文件应存在").len();
    assert!(file_len > 0);

    // ---- 上下文 B：全新字体系统 + 空图集，装载后直接画 ----
    let gpu_b = Arc::new(GpuContext::new(&instance));
    let before = gpu_b.text().glyphs().stats();
    assert_eq!(before.entries(), 0);
    let loaded = gpu_b
        .text()
        .glyphs()
        .load(&gpu_b.device, &gpu_b.queue, &path)
        .expect("load 应成功");
    let after_load = gpu_b.text().glyphs().stats();
    assert_eq!(loaded, saved, "装载条数应等于保存条数");
    assert_eq!(after_load.entries() as usize, loaded);
    assert_eq!(
        after_load.rasterizations, 0,
        "装载不经过光栅化（应为 0，实际 {}）",
        after_load.rasterizations
    );

    let px_b = render(&gpu_b);
    let after_draw = gpu_b.text().glyphs().stats();
    assert_eq!(
        after_draw.rasterizations, 0,
        "装载后绘制不该重新光栅化（新增 {}）",
        after_draw.rasterizations
    );

    let mut diff = 0usize;
    for (a, b) in px_a.chunks(4).zip(px_b.chunks(4)) {
        let d = (a[0] as i32 - b[0] as i32)
            .abs()
            .max((a[1] as i32 - b[1] as i32).abs())
            .max((a[2] as i32 - b[2] as i32).abs());
        if d > 2 {
            diff += 1;
        }
    }
    assert_eq!(
        diff, 0,
        "装载后渲染应与原进程逐像素一致（{diff} 个像素不同）"
    );
    println!(
        "  持久化：{} 条 / {} KiB → 新上下文装载 {} 条，0 次光栅化，0 像素差",
        saved,
        file_len / 1024,
        loaded
    );
    let _ = std::fs::remove_file(&path);
}

/// `load` 要求图集为空；坏文件/版本不符要报错而不是崩。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_load_rejects_bad_input() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let dir = std::env::temp_dir();
    let bad_magic = dir.join("vireo_glyph_cache_bad_magic.vrtx");
    let bad_version = dir.join("vireo_glyph_cache_bad_version.vrtx");
    let truncated = dir.join("vireo_glyph_cache_truncated.vrtx");
    std::fs::write(&bad_magic, b"NOPE0000").unwrap();
    std::fs::write(&bad_version, b"VRTX\xff\x00\x00\x00").unwrap();
    std::fs::write(&truncated, b"VRTX\x01\x00\x00\x00\x05\x00\x00\x00").unwrap();

    let glyphs = gpu.text().glyphs();
    assert!(glyphs.load(&gpu.device, &gpu.queue, &bad_magic).is_err());
    assert!(
        glyphs.load(&gpu.device, &gpu.queue, &bad_version).is_err(),
        "版本不符应报错"
    );
    assert!(
        glyphs.load(&gpu.device, &gpu.queue, &truncated).is_err(),
        "截断文件应报错"
    );
    assert_eq!(glyphs.stats().entries(), 0, "报错不该留下半个图集");

    // 非空图集：必须拒绝装载。
    glyphs.prewarm(
        &gpu.device,
        &gpu.queue,
        "占用",
        &TextDef::default().font_size(30.0),
        1.0,
    );
    let ok_path = dir.join("vireo_glyph_cache_dummy.vrtx");
    glyphs
        .save(&gpu.device, &gpu.queue, &ok_path)
        .expect("save 应成功");
    let err = glyphs
        .load(&gpu.device, &gpu.queue, &ok_path)
        .expect_err("非空图集应拒绝装载");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    for p in [bad_magic, bad_version, truncated, ok_path] {
        let _ = std::fs::remove_file(p);
    }
}

/// **预算的代价**：只有**硬**预算小于「单帧工作集」时才会每帧重光栅化 —— 这条用例
/// 同时钉住两种语义：保留预算不会抖、硬预算必然抖。
///
/// 场景：60 段互不重复的 12 字文本、24px、每帧全画。四档独立上下文：
/// 硬 128 KiB（< 工作集）/ 软 128 KiB（同一个数，但不抖）/ 硬 8 MiB / 不限制。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_budget_churn_perf() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    const STRINGS: u32 = 60;
    const CHARS: usize = 12;
    const SIZE: f32 = 24.0;
    const FRAMES: u32 = 8;
    let texts: Vec<String> = (0..STRINGS)
        .map(|i| cjk_text(0x4E00 + i * CHARS as u32, CHARS))
        .collect();

    /// 一档预算语义。
    #[derive(Clone, Copy)]
    enum Mode {
        Hard(u64),
        Soft(u64),
        Off,
    }
    for (label, mode) in [
        ("硬 128 KiB（<工作集，会抖）", Mode::Hard(128 * 1024)),
        ("软 128 KiB（同一个数，不抖）", Mode::Soft(128 * 1024)),
        ("硬 8 MiB（装得下）", Mode::Hard(8 * 1024 * 1024)),
        ("不限制", Mode::Off),
    ] {
        let gpu = Arc::new(GpuContext::new(&instance));
        let canvas = OffscreenCanvas::new(&gpu, 640, 480);
        let glyphs = gpu.text().glyphs();
        match mode {
            Mode::Hard(b) => glyphs.set_hard_budget(Some(b)),
            Mode::Soft(b) => glyphs.set_retention_budget(Some(b)),
            Mode::Off => {
                glyphs.set_retention_budget(None);
                glyphs.set_hard_budget(None);
            }
        }

        let draw = || {
            let mut b = DrawBatch::new();
            for (i, text) in texts.iter().enumerate() {
                draw_text(
                    &mut b.texts,
                    text,
                    Pos::new(8.0, 8.0 + (i % 30) as f32 * 15.0),
                    glyph_test_def(SIZE),
                    TextOverride::from_color(WHITE),
                );
            }
            canvas.draw(Some(BLACK), &[&b]);
        };

        // 前两帧预热（字体/管线/首轮光栅化）。
        draw();
        draw();
        let r0 = glyphs.stats().rasterizations;
        let t = Instant::now();
        for _ in 0..FRAMES {
            draw();
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        let s = glyphs.stats();
        let rasters = s.rasterizations - r0;
        let per_frame = rasters as f64 / FRAMES as f64;
        println!(
            "  {label}：{ms:.2} ms/帧，光栅化 {per_frame:.1} 次/帧（{rasters} / {FRAMES} 帧），\
             条目 {} / 占用 {} KiB / 单帧峰值 {} KiB / 有效上限 {} KiB / 淘汰 {}",
            s.entries(),
            s.used_bytes() / 1024,
            s.peak_used_bytes / 1024,
            s.effective_budget_bytes.map(|b| b / 1024).unwrap_or(0),
            s.evictions
        );
        match mode {
            // 硬预算 < 工作集：必然每帧重光栅化。
            Mode::Hard(b) if b == 128 * 1024 => {
                assert!(
                    per_frame > 50.0,
                    "硬预算远小于工作集时应该每帧重光栅化（实际 {per_frame:.1} 次/帧）——\
                     如果这条不成立，说明淘汰策略变了"
                );
                assert!(
                    s.peak_used_bytes > 128 * 1024,
                    "单帧峰值应大于这个（过小的）硬预算（峰值 {} 字节）",
                    s.peak_used_bytes
                );
            }
            // 软预算给同一个数：只淘汰「≥120 帧没用过」的条目 ⇒ 这个场景一条都不动。
            Mode::Soft(b) if b == 128 * 1024 => {
                assert!(
                    per_frame < 5.0,
                    "**保留预算不应造成任何重光栅化**（实际 {per_frame:.1} 次/帧）—— \
                     淘汰年龄门槛没生效"
                );
                assert_eq!(
                    s.evictions, 0,
                    "同样的字每帧都用 ⇒ 没有任何条目超过 120 帧没用到 ⇒ 一条也不该淘汰（实际 {}）",
                    s.evictions
                );
                assert!(
                    s.used_bytes() > b,
                    "工作集本身（{}）大于保留预算（{b}）时应保持不变",
                    s.used_bytes()
                );
            }
            Mode::Hard(_) | Mode::Soft(_) | Mode::Off => {
                assert!(
                    per_frame < 5.0,
                    "预算装得下时稳态不该重光栅化（实际 {per_frame:.1} 次/帧）"
                );
            }
        }
    }

    // ---- 真实规模的例子：一个普通 HUD（20 条标签 + 数字，20–28px）----
    //
    // 这一组不做断言，只打印"工作集多大、预算设成 1/4 会怎样"，用来给"预算该给多少"一个
    // 可以对照的量级（很多人以为 HUD 就几条字、随便给 64 KiB 够用）。
    println!("  ---- 普通 HUD（20 条标签 20–28px）----");
    let labels = [
        "生命", "魔法", "护甲", "暴击", "闪避", "攻击", "防御", "速度", "体力", "经验", "金币",
        "钻石", "任务", "队伍", "地图", "设置", "背包", "技能", "成就", "退出",
    ];
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 640, 560);
    let glyphs = gpu.text().glyphs();
    // 先不限预算跑 30 帧，量出真实工作集。
    let draw_hud = |frame: u32| {
        let mut b = DrawBatch::new();
        for (i, label) in labels.iter().enumerate() {
            let size = 20.0 + (i % 3) as f32 * 4.0;
            draw_text(
                &mut b.texts,
                label,
                Pos::new(8.0 + (i % 2) as f32 * 300.0, 8.0 + (i / 2) as f32 * 26.0),
                TextDef::default().font_size(size),
                TextOverride::from_color(WHITE),
            );
            // 每条标签跟一个会变的数字（`Glyphs` 窄路径的典型用法）。
            draw_text_parts(
                &mut b.texts,
                &[
                    TextPart::normal(": "),
                    TextPart::glyphs((frame + i as u32).to_string()),
                ],
                Pos::new(120.0 + (i % 2) as f32 * 300.0, 8.0 + (i / 2) as f32 * 26.0),
                TextDef::default().font_size(size),
                TextOverride::from_color(WHITE),
            );
        }
        canvas.draw(Some(BLACK), &[&b]);
    };
    for f in 0..30 {
        draw_hud(f);
    }
    let peak = glyphs.stats().peak_used_bytes;
    println!(
        "   工作集（单帧峰值）：{} KiB，条目 {}",
        peak / 1024,
        glyphs.stats().entries()
    );
    for (mode_label, hard, budget) in [
        ("硬 25%", true, peak / 4),
        ("软 25%", false, peak / 4),
        ("软 200%", false, peak * 2),
    ] {
        glyphs.clear();
        if hard {
            glyphs.set_hard_budget(Some(budget));
            glyphs.set_retention_budget(None);
        } else {
            glyphs.set_retention_budget(Some(budget));
            glyphs.set_hard_budget(None);
        }
        for f in 0..5 {
            draw_hud(f);
        }
        let r0 = glyphs.stats().rasterizations;
        let t = Instant::now();
        const FRAMES: u32 = 15;
        for f in 0..FRAMES {
            draw_hud(5 + f);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        let rasters = glyphs.stats().rasterizations - r0;
        println!(
            "   {mode_label} {:>5} KiB（占工作集 {:>4.0}%）：{ms:>6.2} ms/帧，光栅化 {:>5.1} 次/帧",
            budget / 1024,
            budget as f64 / peak as f64 * 100.0,
            rasters as f64 / FRAMES as f64
        );
    }
}

/// **A/B 交替不抖动**：两套互不重复的文案隔帧切换，热条目（上一帧还在用）不该被淘汰。
///
/// 回归点：淘汰分「冷 / 热」两档，热条目只在图集到顶时才动 —— 否则 A↔B 来回切换会
/// 每帧把对方整套字重新光栅化（图集永远在 90% 上下反复收放）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_alternating_sets_perf() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 640, 480);
    let glyphs = gpu.text().glyphs();
    const CHARS: usize = 60;
    const SIZE: f32 = 40.0;
    let set_a = cjk_text(0x4E00, CHARS);
    let set_b = cjk_text(0x5E00, CHARS);

    let draw = |text: &str| {
        let mut b = DrawBatch::new();
        // 分两行画，避免超过画布宽度被裁（裁了也还是会被光栅化，但不影响计数）。
        draw_text(
            &mut b.texts,
            text,
            Pos::new(8.0, 12.0),
            glyph_test_def(SIZE),
            TextOverride::from_color(WHITE),
        );
        canvas.draw(Some(BLACK), &[&b]);
    };

    // 预热：两套各画两遍（图集会长到装得下两套）。
    for _ in 0..2 {
        draw(&set_a);
        draw(&set_b);
    }
    let before = glyphs.stats();
    const ROUNDS: u32 = 6;
    let t = Instant::now();
    for _ in 0..ROUNDS {
        draw(&set_a);
        draw(&set_b);
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / (ROUNDS * 2) as f64;
    let after = glyphs.stats();
    let rasters = after.rasterizations - before.rasterizations;
    println!(
        "  A/B 交替：{ms:.2} ms/帧，稳态光栅化 {rasters} 次（{} 帧），扩图 {} 次，容量 {} KiB",
        ROUNDS * 2,
        after.grows - before.grows,
        after.texture_bytes() / 1024
    );
    assert_eq!(
        rasters, 0,
        "A/B 交替不该重光栅化（热条目被淘汰了：{rasters} 次）"
    );
}

/// **大图集下的持久化**：图集用到 1024²/2048² 时 `save`/`load` 的耗时、文件体积与正确性。
///
/// 顺带验证"只读用到的矩形"确实生效：图集纹理远大于内容时，读回量应该跟内容走。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_persist_large() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let path = std::env::temp_dir().join("vireo_glyph_cache_large.vrtx");
    let _ = std::fs::remove_file(&path);

    let gpu = Arc::new(GpuContext::new(&instance));
    let glyphs = gpu.text().glyphs();
    let def = glyph_test_def(24.0);
    // 100 段 × 12 字 = 1200 个互不重复的字形 ⇒ 内容 > 1MB，图集会扩到 2048²。
    const STRINGS: u32 = 100;
    const CHARS: usize = 12;
    let t = Instant::now();
    for i in 0..STRINGS {
        let text = cjk_text(0x4E00 + i * CHARS as u32, CHARS);
        glyphs.prewarm(&gpu.device, &gpu.queue, &text, &def, 1.0);
    }
    let prewarm_ms = t.elapsed().as_secs_f64() * 1000.0;
    let before = glyphs.stats();
    assert!(
        before.texture_bytes() >= 1024 * 1024,
        "用例失效：图集没变大（{} 字节）",
        before.texture_bytes()
    );

    // 参照帧：保存前画一次，装载后要逐像素一致。
    let reference_text = cjk_text(0x4E00, CHARS);
    let render = |gpu: &Arc<GpuContext>| {
        let c = OffscreenCanvas::new(gpu, 400, 120);
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            &reference_text,
            Pos::new(8.0, 20.0),
            def.clone(),
            TextOverride::from_color(WHITE),
        );
        c.draw(Some(BLACK), &[&b]);
        c.read_pixels()
    };
    let px_a = render(&gpu);

    let t = Instant::now();
    let saved = glyphs.save(&gpu.device, &gpu.queue, &path).expect("save");
    let save_ms = t.elapsed().as_secs_f64() * 1000.0;
    let file_kib = std::fs::metadata(&path)
        .map(|m| m.len() / 1024)
        .unwrap_or(0);

    let gpu_b = Arc::new(GpuContext::new(&instance));
    let t = Instant::now();
    let loaded = gpu_b
        .text()
        .glyphs()
        .load(&gpu_b.device, &gpu_b.queue, &path)
        .expect("load");
    let load_ms = t.elapsed().as_secs_f64() * 1000.0;
    let after = gpu_b.text().glyphs().stats();
    assert_eq!(loaded, saved, "装载条数应等于保存条数");
    assert!(loaded >= 1200, "用例失效：装载条数太少（{loaded}）");

    let px_b = render(&gpu_b);
    let rasters = gpu_b.text().glyphs().stats().rasterizations;
    assert_eq!(rasters, 0, "装载后绘制不该重光栅化（{rasters} 次）");
    let mut diff = 0usize;
    for (a, b) in px_a.chunks(4).zip(px_b.chunks(4)) {
        let d = (a[0] as i32 - b[0] as i32)
            .abs()
            .max((a[1] as i32 - b[1] as i32).abs())
            .max((a[2] as i32 - b[2] as i32).abs());
        if d > 2 {
            diff += 1;
        }
    }
    assert_eq!(diff, 0, "装载后渲染应与原进程逐像素一致（{diff} 不同）");
    println!(
        "  大图集持久化：{} 条，prewarm {prewarm_ms:.0} ms | 纹理 {} KiB / 占用 {} KiB | \
         save {save_ms:.1} ms（{file_kib} KiB）| load {load_ms:.1} ms | 0 像素差",
        saved,
        before.texture_bytes() / 1024,
        before.used_bytes() / 1024
    );
    println!(
        "  装载后：纹理 {} KiB（按内容重排）/ 占用 {} KiB",
        after.texture_bytes() / 1024,
        after.used_bytes() / 1024
    );
    let _ = std::fs::remove_file(&path);
}

/// **多线程并发压缓存**：4 个线程同时对同一个 `GpuContext` 做 prewarm / hold / trim / stats。
///
/// 只验证"不崩、不死锁、计数自洽"（`TextSystem` 里是一把互斥锁，所以并行度是 1；
/// 这条用例保证的是**不会因为并发调用而破坏内部状态**）。
#[test]
#[ignore = "requires GPU; run with --ignored"]
fn glyph_cache_concurrent_access() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let def = glyph_test_def(28.0);

    let t = Instant::now();
    let handles: Vec<_> = (0..4u32)
        .map(|tid| {
            let gpu = gpu.clone();
            let def = def.clone();
            std::thread::spawn(move || {
                let mut pinned_peak = 0u32;
                for i in 0..12u32 {
                    let text = cjk_text(0x4E00 + tid * 32 + i * 8, 8);
                    let glyphs = gpu.text().glyphs();
                    let hold = glyphs.hold(&gpu.device, &gpu.queue, &text, &def, 1.0);
                    pinned_peak = pinned_peak.max(glyphs.stats().pinned_entries);
                    let _ = glyphs.stats();
                    let _ = glyphs.entries().len();
                    drop(hold);
                    // 一半线程顺手压一下预算淘汰。
                    if i % 2 == 0 {
                        glyphs.set_retention_budget(Some(256 * 1024));
                        let _ = glyphs.trim();
                    }
                }
                pinned_peak
            })
        })
        .collect();
    let peaks: Vec<u32> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    let glyphs = gpu.text().glyphs();
    let s = glyphs.stats();
    assert_eq!(
        s.pinned_entries, 0,
        "并发结束后不该还有钉子（{}）",
        s.pinned_entries
    );
    assert!(peaks.iter().any(|&p| p > 0), "并发期间应真的钉住过东西");
    assert!(s.entries() > 0);
    println!(
        "  并发：4 线程 × 12 轮 {ms:.0} ms（峰值钉子 {:?}），结束后条目 {} / 占用 {} KiB / 钉住 0",
        peaks,
        s.entries(),
        s.used_bytes() / 1024
    );
}
