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
