//! 文字 shape 缓存压力示例 + 动态 transform。
//!
//! 统计每秒打印到终端（不画动态 HUD 数字，避免污染 shape cache）。
//!
//! STATIC：固定文案池（应高 hit，entries 稳定）  
//! DYNAMIC：每帧 format 新串（应低 hit，entries 涨）  
//!
//! 操作：
//! - `1`  TTL = 2 秒
//! - `2`  TTL = None（永不按时间回收）
//! - `3`  max_entries = 64
//! - `4`  max_entries = None
//! - `C`  立即清空缓存
//! - 空格  STATIC / DYNAMIC
//!
//! ```bash
//! cargo run --example text_shape_cache
//! ```

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use vireo::prelude::*;

const STATIC_POOL: &[&str] = &[
    "Vireo",
    "Shape Cache",
    "你好世界",
    "击杀",
    "金币",
    "HP",
    "MP",
    "FPS",
    "wgpu",
    "SDF",
    "Transform",
    "缓存测试",
    "ABC-012",
    "中英 Mix",
    "Settings",
    "背包",
    "地图",
    "退出",
    "Ready",
    "Loading…",
    "Player-1",
    "Player-2",
    "Wave",
    "Boss",
    "Combo",
    "Critical!",
    "MISS",
    "完美",
    "再试一次",
    "Press Space",
];

const FONT_SIZES: &[f32] = &[12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 28.0];

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Text Shape Cache Stress", 960, 640),
        None::<fn()>,
    );

    app.gpu.set_shape_cache_ttl(Some(Duration::from_secs(2)));
    app.gpu.set_shape_cache_max_entries(Some(4096));

    let mut dynamic = false;
    let mut frame: u32 = 0;
    let mut ttl_label = "TTL=2s";
    let mut max_label = "max=4096";
    // 顶栏仅用固定/低频变化字符串 + parts
    let mut status_line = String::from("mode: STATIC | TTL=2s | max=4096");
    let mut cfg_dirty = true;
    let mut last_log = Instant::now() - Duration::from_secs(1);
    let t0 = Instant::now();

    eprintln!("[text_shape_cache] logging stats to terminal every 1s");

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let t = t0.elapsed().as_secs_f32();

        if win.key_down(KeyCode::Digit1) {
            app.gpu.set_shape_cache_ttl(Some(Duration::from_secs(2)));
            ttl_label = "TTL=2s";
            cfg_dirty = true;
        }
        if win.key_down(KeyCode::Digit2) {
            app.gpu.set_shape_cache_ttl(None);
            ttl_label = "TTL=None";
            cfg_dirty = true;
        }
        if win.key_down(KeyCode::Digit3) {
            app.gpu.set_shape_cache_max_entries(Some(64));
            max_label = "max=64";
            cfg_dirty = true;
        }
        if win.key_down(KeyCode::Digit4) {
            app.gpu.set_shape_cache_max_entries(None);
            max_label = "max=None";
            cfg_dirty = true;
        }
        if win.key_down(KeyCode::KeyC) {
            app.gpu.clear_shape_cache();
            app.gpu.reset_shape_cache_stats();
            eprintln!("[text_shape_cache] cache cleared");
        }
        if win.key_down(KeyCode::Space) {
            dynamic = !dynamic;
            app.gpu.clear_shape_cache();
            app.gpu.reset_shape_cache_stats();
            cfg_dirty = true;
            eprintln!(
                "[text_shape_cache] mode => {}",
                if dynamic { "DYNAMIC" } else { "STATIC" }
            );
        }

        if cfg_dirty {
            let mode = if dynamic { "DYNAMIC" } else { "STATIC" };
            status_line = format!("mode: {mode} | {ttl_label} | {max_label}");
            cfg_dirty = false;
        }

        // 每秒终端输出（不画动态数字 HUD）
        if last_log.elapsed() >= Duration::from_secs(1) {
            let n = app.gpu.shape_cache_len();
            let stats = app.gpu.shape_cache_stats();
            let total = stats.hits + stats.misses;
            let hit_pct = if total > 0 {
                100.0 * stats.hits as f64 / total as f64
            } else {
                0.0
            };
            let avg_gc = if stats.gc_runs > 0 {
                stats.total_gc_us / stats.gc_runs
            } else {
                0
            };
            eprintln!(
                "[text_shape_cache] FPS={:.1} frame={:.2}ms | entries={n} hit={hit_pct:.1}% (h={} m={}) | gc_runs={} last_gc={}us avg_gc={}us | {}",
                app.fps,
                app.frame_time * 1000.0,
                stats.hits,
                stats.misses,
                stats.gc_runs,
                stats.last_gc_us,
                avg_gc,
                status_line,
            );
            last_log = Instant::now();
        }

        frame = frame.wrapping_add(1);
        let mut batches: Vec<DrawBatch> = Vec::with_capacity(120);

        // 顶栏：固定标题 + 低频 status（仅按键时变）+ 固定帮助
        let mut hud = DrawBatch::new();
        draw_text(
            &mut hud.texts,
            "Shape cache stress + transform",
            TextOptions::default()
                .x(16.0)
                .y(12.0)
                .font_size(20.0)
                .color(WHITE),
        );
        draw_text(
            &mut hud.texts,
            &status_line,
            TextOptions::default()
                .x(16.0)
                .y(40.0)
                .font_size(14.0)
                .color(Color::new(0.7, 0.9, 1.0, 1.0)),
        );
        // 用 parts 显示「帧号」类动态数，避免整段 format 污染
        let frame_digits = frame.to_string();
        draw_text_parts(
            &mut hud.texts,
            &[TextPart::Static("frame "), TextPart::Digits(&frame_digits)],
            TextOptions::default()
                .x(16.0)
                .y(64.0)
                .font_size(14.0)
                .color(Color::new(0.85, 0.85, 0.55, 1.0)),
        );
        draw_text(
            &mut hud.texts,
            "stats -> terminal every 1s | 1/2 TTL  3/4 max  C clear  Space mode",
            TextOptions::default()
                .x(16.0)
                .y(86.0)
                .font_size(12.0)
                .color(Color::new(0.5, 0.5, 0.55, 1.0)),
        );
        batches.push(hud);

        // 网格 12×8 = 96
        const COLS: usize = 12;
        const ROWS: usize = 8;
        let cell_w = 76.0f32;
        let cell_h = 48.0f32;
        let grid_x0 = 20.0f32;
        let grid_y0 = 120.0f32;

        for row in 0..ROWS {
            for col in 0..COLS {
                let i = row * COLS + col;
                let base_x = grid_x0 + col as f32 * cell_w;
                let base_y = grid_y0 + row as f32 * cell_h;
                let sz = FONT_SIZES[i % FONT_SIZES.len()];

                let phase = t * 1.2 + i as f32 * 0.17;
                let ox = phase.sin() * 6.0;
                let oy = (phase * 1.3).cos() * 4.0;
                let rot = (phase * 0.4).sin() * 0.25;
                let sc = 0.92 + (phase * 0.7).sin().abs() * 0.18;

                let mut cell = DrawBatch::new();
                cell.set_position(base_x + ox, base_y + oy);
                cell.set_pivot(18.0, 10.0);
                cell.set_rad(rot);
                cell.set_scale(sc, sc);
                let c = Color::new(
                    0.40 + (col as f32) * 0.045,
                    0.50 + (row as f32) * 0.045,
                    0.95,
                    1.0,
                );

                if dynamic {
                    // 动态：整段 format（压力 miss）
                    let label = format!(
                        "{}#{}",
                        STATIC_POOL[i % STATIC_POOL.len()],
                        frame.wrapping_add(i as u32)
                    );
                    cell.text(&label, TextOptions::default().font_size(sz).color(c));
                } else {
                    // 静态：池内固定串 + 可选 frame 用 Digits 叠在旁（不占新整段 key）
                    cell.text(
                        STATIC_POOL[i % STATIC_POOL.len()],
                        TextOptions::default().font_size(sz).color(c),
                    );
                }
                batches.push(cell);
            }
        }

        // 轨道 16
        let orbit_n = 16;
        let cx = 820.0f32;
        let cy = 200.0f32;
        let radius = 90.0f32;
        for k in 0..orbit_n {
            let a = t * 0.8 + k as f32 * (TAU / orbit_n as f32);
            let x = cx + a.cos() * radius;
            let y = cy + a.sin() * radius;
            let mut ob = DrawBatch::new();
            ob.set_position(x, y);
            ob.set_rad(a + 1.57);
            if dynamic {
                let label = format!("R{k}:{frame}");
                ob.text(
                    &label,
                    TextOptions::default()
                        .font_size(13.0)
                        .color(Color::new(1.0, 0.75, 0.35, 1.0)),
                );
            } else {
                // 固定 "R" + 数字位
                let kd = k.to_string();
                ob.text_parts(
                    &[TextPart::Static("R"), TextPart::Digits(&kd)],
                    TextOptions::default()
                        .font_size(13.0)
                        .color(Color::new(1.0, 0.75, 0.35, 1.0)),
                );
            }
            batches.push(ob);
        }

        let mut footer = DrawBatch::new();
        draw_text(
            &mut footer.texts,
            "load: ~116 text draws / frame (grid 96 + orbit 16 + HUD)",
            TextOptions::default()
                .x(16.0)
                .y(560.0)
                .font_size(13.0)
                .color(Color::new(0.75, 0.75, 0.55, 1.0)),
        );
        draw_text(
            &mut footer.texts,
            "STATIC: pool+transform. DYNAMIC: new strings. Stats on terminal every 1s.",
            TextOptions::default()
                .x(16.0)
                .y(590.0)
                .font_size(12.0)
                .color(Color::new(0.5, 0.5, 0.55, 1.0)),
        );
        batches.push(footer);

        let refs: Vec<&DrawBatch> = batches.iter().collect();
        win.draw(Some(Color::new(0.07, 0.07, 0.10, 1.0)), &refs);
        true
    });
}
