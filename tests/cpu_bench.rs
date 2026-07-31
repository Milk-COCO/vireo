//! CPU 端微基准：测量不依赖 GPU 的核心操作吞吐量。
//! 运行方式：cargo test --release --test cpu_bench -- --nocapture
//!
//! 测试内容：SDF/几何顶点生成、DrawBatch 生命周期、Transform API、文本创建、典型帧模拟。

use std::time::Instant;
use vireo::prelude::*;

fn time_ms<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = Instant::now();
    let result = f();
    (result, start.elapsed().as_secs_f64() * 1000.0)
}

fn bench(name: &str, iterations: u32, mut f: impl FnMut()) {
    for _ in 0..(iterations / 10).min(100) { f(); }
    let (_, total) = time_ms(|| { for _ in 0..iterations { f(); } });
    println!("  {:<40} {:>8.3} µs/op", name, total * 1000.0 / iterations as f64);
}

// ═══════════════════════════════════════════════════════════════

#[test]
fn bench_shape_generation() {
    println!("\n=== Shape Generation (SDF vs Geometry) ===");

    bench("SDF rect (4 vertices)", 50_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
    });

    bench("SDF rounded_rect", 50_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_rounded_rect(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, 4.0, Some(RED));
    });

    bench("SDF circle", 50_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_circle(&mut b, Pos::new(5.0, 5.0), 4.0, Some(RED));
    });

    bench("SDF triangle", 50_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_triangle(&mut b, 0.0, 0.0, 10.0, 0.0, 5.0, 10.0, Some(RED));
    });

    bench("SDF polygon (6 edges)", 10_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        let pts = [(0.,0.),(10.,0.),(14.,5.),(10.,10.),(0.,10.),(-4.,5.)];
        draw_polygon(&mut b, &pts, Some(RED));
    });

    bench("geo rounded_rect (fan)", 10_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        draw_rounded_rect(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, 4.0, Some(RED));
    });

    bench("geo circle (fan, ~260 verts)", 10_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        draw_circle(&mut b, Pos::new(5.0, 5.0), 4.0, Some(RED));
    });
}

#[test]
fn bench_batch_lifecycle() {
    println!("\n=== Batch Lifecycle ===");

    bench("new + 100 shapes + clear", 5_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        for i in 0..100 {
            b.set_position((i * 10) as f32, 0.0);
            draw_rectangle(&mut b, Pos::new(0.0, 0.0), 8.0, 8.0, Some(BLUE));
        }
        b.clear();
    });

    bench("clear reuse x10 (100 each)", 5_000, || {
        let mut b = DrawBatch::new();
        for _ in 0..10 {
            b.sdf_feather = Some(1.0);
            for i in 0..100 {
                draw_rectangle(&mut b, Pos::new(i as f32, 0.0), 8.0, 8.0, Some(BLUE));
            }
            b.clear();
        }
    });
}

#[test]
fn bench_transform_api() {
    println!("\n=== Transform API ===");

    bench("set_position only", 100_000, || {
        let mut b = DrawBatch::new();
        b.set_position(10.0, 20.0);
    });

    bench("set_deg + set_scale", 100_000, || {
        let mut b = DrawBatch::new();
        b.set_deg(45.0);
        b.set_scale(2.0, 1.5);
    });

    bench("translate (accumulate)", 100_000, || {
        let mut b = DrawBatch::new();
        b.translate(1.0, 2.0);
    });

    bench("rotate_deg + scale_by", 100_000, || {
        let mut b = DrawBatch::new();
        b.rotate_deg(30.0);
        b.scale_by(0.5, 0.5);
    });

    bench("set_position + draw + clear", 100_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(100.0, 200.0);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 5.0, 5.0, Some(RED));
        b.clear_transform();
    });
}

#[test]
fn bench_transform_dedup() {
    println!("\n=== Transform Dedup (via shape drawing) ===");

    // 高命中率：全部相同 transform
    bench("identical xform (hit:100%)", 10_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(100.0, 200.0);
        for _ in 0..100 {
            draw_rectangle(&mut b, Pos::new(0.0, 0.0), 8.0, 8.0, Some(RED));
        }
    });

    // 零命中率：每个形状不同 transform
    bench("unique xform (hit:0%)", 5_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        for i in 0..100 {
            b.set_position(i as f32, (i * 2) as f32);
            draw_rectangle(&mut b, Pos::new(0.0, 0.0), 8.0, 8.0, Some(RED));
        }
    });

    // 50% 命中率
    bench("mixed xform (hit:50%)", 5_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        for i in 0..100 {
            if i % 2 == 0 { b.set_position(10.0, 20.0); }
            else { b.set_position(i as f32, i as f32); }
            draw_rectangle(&mut b, Pos::new(0.0, 0.0), 8.0, 8.0, Some(RED));
        }
    });
}

#[test]
fn bench_text_entries() {
    println!("\n=== Text Entry Creation ===");

    bench("draw_text single", 50_000, || {
        let mut list = vireo::text::TextEntryList::new();
        draw_text(&mut list, "Hello Vireo!", Pos::new(10.0, 20.0),
            TextDef::default().font_size(16.0),
            TextOverride::default().color(WHITE));
    });

    bench("draw_text x100", 1_000, || {
        let mut list = vireo::text::TextEntryList::new();
        for i in 0..100 {
            draw_text(&mut list, &format!("Line {}", i), Pos::new(10.0, i as f32 * 20.0),
                TextDef::default().font_size(14.0),
                TextOverride::default().color(WHITE));
        }
    });
}

#[test]
fn bench_typical_frames() {
    println!("\n=== Typical Frame Simulation ===");

    // SDF 2000 shapes
    let (_, t1) = time_ms(|| {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        for i in 0..2000 {
            b.set_position((i % 50) as f32 * 15.0, (i / 50) as f32 * 15.0);
            match i % 5 {
                0 => draw_rectangle(&mut b, Pos::new(0., 0.), 12., 12., Some(RED)),
                1 => draw_circle(&mut b, Pos::new(6., 6.), 5., Some(GREEN)),
                2 => draw_rounded_rect(&mut b, Pos::new(0., 0.), 12., 12., 3., Some(BLUE)),
                3 => draw_ellipse(&mut b, Pos::new(6., 6.), 5., 3., Some(YELLOW)),
                4 => draw_triangle(&mut b, 0., 0., 12., 0., 6., 12., Some(MAGENTA)),
                _ => {}
            }
        }
    });
    println!("  SDF 2000 shapes:           {:.3} ms  ({} vtx, {} idx)",
        t1, 2000 * 4, 2000 * 6);

    // Geo 500 shapes
    let (_, t2) = time_ms(|| {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        for i in 0..500 {
            b.set_position((i % 25) as f32 * 30.0, (i / 25) as f32 * 30.0);
            draw_rounded_rect(&mut b, Pos::new(0., 0.), 25., 25., 6., Some(RED));
        }
    });
    println!("  Geo 500 rounded_rects:     {:.3} ms", t2);

    // 1000 unique transforms
    let (_, t3) = time_ms(|| {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(0.5);
        for i in 0..1000 {
            b.set_position((i % 40) as f32 * 20., (i / 40) as f32 * 20.);
            b.set_deg((i as f32) * 7.0);
            b.set_scale(1.0 + (i%3) as f32 * 0.3, 1.0);
            draw_rounded_rect(&mut b, Pos::new(-5., -5.), 5., 5., 2., Some(WHITE));
            b.clear_transform();
        }
    });
    println!("  1000 unique transforms:    {:.3} ms  ({} dedup entries)", t3, 1000);

    // 200 text entries
    let (_, t4) = time_ms(|| {
        let mut list = vireo::text::TextEntryList::new();
        for i in 0..200 {
            draw_text(&mut list, &format!("Text {i}"),
                Pos::new((i%20) as f32 * 40., (i/20) as f32 * 25.),
                TextDef::default().font_size(14.0),
                TextOverride::default().color(WHITE));
        }
    });
    println!("  200 text entries:          {:.3} ms", t4);
}

#[test]
fn bench_outline_and_chain() {
    println!("\n=== Outline / LineChain / High-edge Polygon ===");

    bench("SDF line_chain 8pts", 10_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        let pts = [
            (0., 0.), (10., 2.), (18., 8.), (20., 16.),
            (14., 22.), (6., 20.), (0., 12.), (0., 0.),
        ];
        draw_line_chain(&mut b, &pts, 2.0, Some(RED));
    });

    bench("geo line_chain 8pts", 5_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        let pts = [
            (0., 0.), (10., 2.), (18., 8.), (20., 16.),
            (14., 22.), (6., 20.), (0., 12.), (0., 0.),
        ];
        draw_line_chain(&mut b, &pts, 2.0, Some(RED));
    });

    bench("SDF polygon 12 edges", 10_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        let mut pts = [(0f32, 0f32); 12];
        for j in 0..12 {
            let a = std::f32::consts::TAU * j as f32 / 12.0;
            pts[j] = (10.0 * a.cos(), 10.0 * a.sin());
        }
        draw_polygon(&mut b, &pts, Some(BLUE));
    });

    bench("SDF rect_outline", 10_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_rect_outline(&mut b, Pos::new(0.0, 0.0), 20.0, 20.0, 2.0, Some(GREEN));
    });

    bench("SDF rounded_rect_outline", 5_000, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_rounded_rect_outline(&mut b, Pos::new(0.0, 0.0), 30.0, 20.0, 6.0, 2.0, Some(WHITE), 8);
    });

    bench("instance polygon repeated x1000", 500, || {
        let mut b = DrawBatch::new();
        let pts = [(0., 0.), (12., 0.), (16., 8.), (8., 16.), (0., 8.)];
        for i in 0..1000 {
            b.set_position((i % 50) as f32 * 18.0, (i / 50) as f32 * 18.0);
            b.instance_polygon(&pts, Some(BLUE));
        }
        std::hint::black_box(b.polygon_edges.len());
    });
}

#[test]
fn bench_merge_path_cpu() {
    println!("\n=== Draw-path CPU (merge/patch simulation) ===");

    // 单 batch：append 顶点 + 读 has_sdf 标志路径（顶点扫描作为对照）
    bench("merge single batch 2000 SDF", 500, || {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        for i in 0..2000 {
            b.set_position((i % 50) as f32 * 15.0, (i / 50) as f32 * 15.0);
            draw_rectangle(&mut b, Pos::new(0., 0.), 12., 12., Some(RED));
        }
        let mut combined: Vec<Vertex> = Vec::with_capacity(b.vertices.len());
        combined.extend_from_slice(&b.vertices);
        let _needs_sdf = b.vertices.iter().any(|v| v.sdf_type > 0);
        let _ = combined.len();
    });

    // 多 batch：合并 + transform/poly 就地 patch（模拟 Renderer.draw）
    bench("merge 4 batches + patch", 200, || {
        let mut batches = Vec::new();
        for bi in 0..4 {
            let mut b = DrawBatch::new();
            b.sdf_feather = Some(1.0);
            for i in 0..500 {
                b.set_position((i % 25) as f32 * 15.0 + bi as f32, (i / 25) as f32 * 15.0);
                if i % 10 == 0 {
                    let pts = [(0., 0.), (10., 0.), (8., 8.), (0., 10.)];
                    draw_polygon(&mut b, &pts, Some(BLUE));
                } else {
                    draw_rectangle(&mut b, Pos::new(0., 0.), 10., 10., Some(RED));
                }
            }
            batches.push(b);
        }
        let total_v: usize = batches.iter().map(|b| b.vertices.len()).sum();
        let mut combined: Vec<Vertex> = Vec::with_capacity(total_v);
        let mut xform_base = 0u32;
        let mut poly_base = 0f32;
        for b in &batches {
            let start = combined.len();
            combined.extend_from_slice(&b.vertices);
            if xform_base > 0 {
                for v in &mut combined[start..] {
                    v.transform_index += xform_base;
                }
            }
            if !b.polygon_edges.is_empty() {
                for v in &mut combined[start..] {
                    if v.sdf_type == 6 || v.sdf_type == 7 {
                        v.sdf_params[0] += poly_base;
                    }
                }
            }
            let max_idx = b.vertices.iter().map(|v| v.transform_index).max().unwrap_or(0);
            xform_base += max_idx + 1;
            poly_base += b.polygon_edges.len() as f32 / 4.0;
        }
        let _ = combined.len();
    });
}
