//! 文字路径剖析用：无头跑固定帧后退出，便于 cargo flamegraph。
//!
//! ```bash
//! # 静态文案（测 shape 缓存）
//! set VIREO_PROFILE_MODE=static
//! cargo run --release --example text_profile
//!
//! # 动态文案（每帧 format，缓存几乎全 miss）
//! set VIREO_PROFILE_MODE=dynamic
//! cargo run --release --example text_profile
//!
//! cargo flamegraph --dev --example text_profile -o flame-text.svg
//! ```

use std::sync::Arc;
use std::time::Instant;

use vireo::prelude::*;

/// 每帧内容变化（frame 写入字符串）→ shape 几乎全 miss
fn scene_text_dynamic(b: &mut DrawBatch, frame: u32) {
    for i in 0..200 {
        let x = (i % 20) as f32 * 44.0 + 5.0;
        let y = (i / 20) as f32 * 28.0 + 5.0;
        let msg = match i % 6 {
            0 => format!("ABC {i} f{frame}"),
            1 => format!("你好 {i} f{frame}"),
            2 => format!("Test 测试 {frame}"),
            3 => format!("Vireo {i} f{frame}"),
            4 => format!("wgpu 🎨 {frame}"),
            _ => format!("SDF #{i} f{frame}"),
        };
        let sz = 12.0 + (i % 5) as f32 * 2.0;
        draw_text(
            &mut b.texts,
            &msg,
            TextOptions::default()
                .x(x)
                .y(y)
                .font_size(sz)
                .color(WHITE),
        );
    }
}

/// 固定字符串集合，跨帧内容不变 → shape 缓存应几乎全命中。
fn scene_text_static(b: &mut DrawBatch) {
    const LABELS: &[&str] = &[
        "ABC", "你好", "Test 测试", "Vireo", "wgpu 🎨", "SDF #",
    ];
    for i in 0..200 {
        let x = (i % 20) as f32 * 44.0 + 5.0;
        let y = (i / 20) as f32 * 28.0 + 5.0;
        let msg = LABELS[i % LABELS.len()];
        let sz = 12.0 + (i % 5) as f32 * 2.0;
        draw_text(
            &mut b.texts,
            msg,
            TextOptions::default()
                .x(x)
                .y(y)
                .font_size(sz)
                .color(WHITE),
        );
    }
}

fn main() {
    let frames: u32 = std::env::var("VIREO_PROFILE_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let mode = std::env::var("VIREO_PROFILE_MODE").unwrap_or_else(|_| "static".into());
    let dynamic = mode.eq_ignore_ascii_case("dynamic");

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let gpu = Arc::new(GpuContext::new(&instance));
    let canvas = OffscreenCanvas::new(&gpu, 900, 700);

    for f in 0..10u32 {
        let mut b = DrawBatch::new();
        if dynamic {
            scene_text_dynamic(&mut b, f);
        } else {
            scene_text_static(&mut b);
        }
        canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    }

    gpu.reset_shape_cache_stats();

    let t0 = Instant::now();
    for f in 0..frames {
        let mut b = DrawBatch::new();
        if dynamic {
            scene_text_dynamic(&mut b, f + 100);
        } else {
            scene_text_static(&mut b);
        }
        canvas.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&b]);
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let stats = gpu.shape_cache_stats();
    let total = stats.hits + stats.misses;
    let hit_pct = if total > 0 {
        100.0 * stats.hits as f64 / total as f64
    } else {
        0.0
    };
    eprintln!(
        "text_profile mode={mode}: {frames} frames in {ms:.1} ms ({:.2} ms/frame, ~{:.0} FPS)  shape hit={}/{} ({hit_pct:.1}%)",
        ms / frames as f64,
        frames as f64 / (ms / 1000.0),
        stats.hits,
        total,
    );
}
