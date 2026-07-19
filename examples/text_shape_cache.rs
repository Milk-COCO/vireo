//! 文字 shape 缓存配置示例。
//!
//! 注意：任何「每帧 format 出不同字符串」的 draw_text 都会增加 cache 条目。
//! STATIC 模式只画固定文案；统计字符串仅在数值变化时更新。
//!
//! 操作：
//! - `1`  TTL = 2 秒
//! - `2`  TTL = None（永不按时间回收）
//! - `3`  max_entries = 64
//! - `4`  max_entries = None（不限制条数）
//! - `C`  立即清空缓存
//! - 空格  切换 STATIC / DYNAMIC
//!
//! ```bash
//! cargo run --example text_shape_cache
//! ```

use std::time::Duration;

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Text Shape Cache Config", 720, 480),
        None::<fn()>,
    );

    app.gpu.set_shape_cache_ttl(Some(Duration::from_secs(2)));
    app.gpu.set_shape_cache_max_entries(Some(4096));

    let mut dynamic = false;
    let mut frame: u32 = 0;
    let mut ttl_label = "TTL=2s";
    let mut max_label = "max=4096";
    let mut status_line = String::from("mode: STATIC | TTL=2s | max=4096");
    let mut stats_line = String::from("cache entries=0  hit~0%");
    let mut cfg_dirty = true;
    let mut last_entries = usize::MAX;
    let mut last_hit_bucket = u32::MAX;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

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
            last_entries = usize::MAX;
            last_hit_bucket = u32::MAX;
        }
        if win.key_down(KeyCode::Space) {
            dynamic = !dynamic;
            app.gpu.clear_shape_cache();
            app.gpu.reset_shape_cache_stats();
            last_entries = usize::MAX;
            last_hit_bucket = u32::MAX;
            cfg_dirty = true;
        }

        if cfg_dirty {
            let mode = if dynamic { "DYNAMIC" } else { "STATIC" };
            status_line = format!("mode: {mode} | {ttl_label} | {max_label}");
            cfg_dirty = false;
        }

        // 仅当 entries / 命中率分桶变化时改字符串，避免 format 污染 cache
        {
            let n = app.gpu.shape_cache_len();
            let stats = app.gpu.shape_cache_stats();
            let total = stats.hits + stats.misses;
            let hit_bucket = if total > 0 {
                ((100.0 * stats.hits as f64 / total as f64) as u32 / 5) * 5
            } else {
                0
            };
            if n != last_entries || hit_bucket != last_hit_bucket {
                stats_line = format!("cache entries={n}  hit~{hit_bucket}%");
                last_entries = n;
                last_hit_bucket = hit_bucket;
            }
        }

        frame = frame.wrapping_add(1);
        let mut batch = DrawBatch::new();

        draw_text(
            &mut batch.texts,
            "Shape cache demo",
            TextOptions::default()
                .x(16.0)
                .y(16.0)
                .font_size(22.0)
                .color(WHITE),
        );
        draw_text(
            &mut batch.texts,
            &status_line,
            TextOptions::default()
                .x(16.0)
                .y(48.0)
                .font_size(16.0)
                .color(Color::new(0.7, 0.9, 1.0, 1.0)),
        );

        const LABELS: &[&str] = &["开始", "设置", "背包", "地图", "退出", "Vireo"];
        for (i, label) in LABELS.iter().enumerate() {
            if dynamic {
                let text = format!("{label} #{frame}");
                draw_text(
                    &mut batch.texts,
                    &text,
                    TextOptions::default()
                        .x(40.0 + (i as f32) * 100.0)
                        .y(120.0)
                        .font_size(18.0)
                        .color(YELLOW),
                );
            } else {
                draw_text(
                    &mut batch.texts,
                    label,
                    TextOptions::default()
                        .x(40.0 + (i as f32) * 100.0)
                        .y(120.0)
                        .font_size(18.0)
                        .color(YELLOW),
                );
            }
        }

        draw_text(
            &mut batch.texts,
            &stats_line,
            TextOptions::default()
                .x(16.0)
                .y(180.0)
                .font_size(15.0)
                .color(Color::new(0.6, 1.0, 0.6, 1.0)),
        );

        draw_text(
            &mut batch.texts,
            "STATIC: entries stabilize. DYNAMIC: grows until max/TTL.",
            TextOptions::default()
                .x(16.0)
                .y(220.0)
                .font_size(13.0)
                .color(Color::new(1.0, 0.8, 0.4, 1.0)),
        );

        draw_text(
            &mut batch.texts,
            "1:TTL=2s  2:TTL=None  3:max=64  4:max=None  C:clear  Space:static/dynamic",
            TextOptions::default()
                .x(16.0)
                .y(440.0)
                .font_size(13.0)
                .color(Color::new(0.5, 0.5, 0.55, 1.0)),
        );

        win.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&batch]);
        true
    });
}
