//! HUD 分段文字对比示例。
//!
//! - 整段：`draw_text(format!("分数: {n}"))` — 每帧新字符串 → shape 几乎全 miss  
//! - 分段：`Text("分数: ") + Digits(...)` — 前缀缓存，数字用 0-9 表  
//!
//! 顶栏 score / fps 也用 Digits，避免每帧 format 污染 shape cache。
//!
//! 操作：
//! - `Space` 切换显示模式：Both / Whole only / Parts only
//!
//! ```bash
//! cargo run --example text_hud
//! ```

use vireo::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Both,
    WholeOnly,
    PartsOnly,
}

impl Mode {
    fn next(self) -> Self {
        match self {
            Mode::Both => Mode::WholeOnly,
            Mode::WholeOnly => Mode::PartsOnly,
            Mode::PartsOnly => Mode::Both,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::Both => "Both",
            Mode::WholeOnly => "Whole only (format!)",
            Mode::PartsOnly => "Parts only (HUD)",
        }
    }
}

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("HUD Text Parts", 800, 480), None::<fn()>);

    let mut score: u32 = 0;
    let mut tick: u32 = 0;
    let mut mode = Mode::Both;
    let mut score_for_string = String::new(); // pre-formatted score string
    let mut fps_buckets: [u32; 12] = [0; 12]; // 0,5,10,...,55

    // 低频变化的固定字符串（仅在模式变化时改字符串）
    let mut mode_line = String::from("mode: Both  (Space to cycle)");

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        if win.key_down(KeyCode::Space) {
            mode = mode.next();
            app.gpu.clear_shape_cache();
            app.gpu.reset_shape_cache_stats();
        }

        tick = tick.wrapping_add(1);
        if tick % 8 == 0 {
            score = score.wrapping_add(1);
        }

        // score 字符串：仅值变化时 format（每 8 帧一次）
        let cur_score_str = score.to_string();
        if cur_score_str != score_for_string {
            score_for_string = cur_score_str;
        }

        // mode 字符串：按键时变
        let cur_mode = format!("mode: {}  (Space to cycle)", mode.label());
        if cur_mode != mode_line {
            mode_line = cur_mode;
        }

        // FPS 直方图：12 桶 × 5 FPS = 0..60
        let fps = app.fps;
        let bucket = ((fps as u32) / 5).min(11) as usize;
        if fps_buckets[bucket] == 0 {
            // 新桶点亮（用固定 string 避免 format! 每帧）
        }
        let ft_ms = app.frame_time * 1000.0;

        let mut batch = DrawBatch::new();

        // --- 顶栏 HUD：固定/低频文字 + 数字走 Digits ---
        draw_text(
            &mut batch.texts,
            "HUD Text Parts",
            TextOptions::default()
                .x(24.0)
                .y(20.0)
                .font_size(26.0)
                .color(WHITE),
        );
        draw_text(
            &mut batch.texts,
            &mode_line,
            TextOptions::default()
                .x(24.0)
                .y(56.0)
                .font_size(15.0)
                .color(Color::new(0.75, 0.85, 1.0, 1.0)),
        );
        // "score = N"：Text("score = ") + Digits(...)
        draw_text_parts(
            &mut batch.texts,
            &[TextPart::Text("score = "), TextPart::Digits(&score_for_string)],
            TextOptions::default()
                .x(24.0)
                .y(82.0)
                .font_size(15.0)
                .color(Color::new(0.9, 0.9, 0.5, 1.0)),
        );

        // --- 左栏：整段 format（压力）---
        let left_x = 40.0;
        let col_y = 140.0;
        draw_rectangle(
            &mut batch,
            left_x - 12.0,
            col_y - 16.0,
            340.0,
            160.0,
            Color::new(0.18, 0.12, 0.12, 1.0),
        );
        draw_text(
            &mut batch.texts,
            "WHOLE STRING",
            TextOptions::default()
                .x(left_x)
                .y(col_y)
                .font_size(14.0)
                .color(Color::new(1.0, 0.55, 0.55, 1.0)),
        );
        draw_text(
            &mut batch.texts,
            "draw_text(format!(\"分数: {{n}}\"))",
            TextOptions::default()
                .x(left_x)
                .y(col_y + 28.0)
                .font_size(13.0)
                .color(Color::new(0.7, 0.55, 0.55, 1.0)),
        );

        if matches!(mode, Mode::Both | Mode::WholeOnly) {
            let whole = format!("分数: {score}");
            draw_text(
                &mut batch.texts,
                &whole,
                TextOptions::default()
                    .x(left_x)
                    .y(col_y + 70.0)
                    .font_size(36.0)
                    .color(Color::new(1.0, 0.45, 0.45, 1.0)),
            );
        } else {
            draw_text(
                &mut batch.texts,
                "(hidden)",
                TextOptions::default()
                    .x(left_x)
                    .y(col_y + 70.0)
                    .font_size(20.0)
                .color(Color::new(0.4, 0.35, 0.35, 1.0)),
            );
        }
        draw_text(
            &mut batch.texts,
            "each new score => new ShapeKey",
            TextOptions::default()
                .x(left_x)
                .y(col_y + 120.0)
                .font_size(12.0)
                .color(Color::new(0.65, 0.5, 0.5, 1.0)),
        );

        // --- 右栏：分段 HUD（cache 友好）---
        let right_x = 420.0;
        draw_rectangle(
            &mut batch,
            right_x - 12.0,
            col_y - 16.0,
            340.0,
            160.0,
            Color::new(0.10, 0.16, 0.12, 1.0),
        );
        draw_text(
            &mut batch.texts,
            "HUD PARTS",
            TextOptions::default()
                .x(right_x)
                .y(col_y)
                .font_size(14.0)
                .color(Color::new(0.5, 1.0, 0.65, 1.0)),
        );
        draw_text(
            &mut batch.texts,
            "Text(\"分数: \") + Digits(n)",
            TextOptions::default()
                .x(right_x)
                .y(col_y + 28.0)
                .font_size(13.0)
                .color(Color::new(0.5, 0.75, 0.55, 1.0)),
        );

        if matches!(mode, Mode::Both | Mode::PartsOnly) {
            draw_text_parts(
                &mut batch.texts,
                &[TextPart::Text("分数: "), TextPart::Digits(&score_for_string)],
                TextOptions::default()
                    .x(right_x)
                    .y(col_y + 70.0)
                    .font_size(36.0)
                    .color(Color::new(0.45, 1.0, 0.6, 1.0)),
            );
        } else {
            draw_text(
                &mut batch.texts,
                "(hidden)",
                TextOptions::default()
                    .x(right_x)
                    .y(col_y + 70.0)
                    .font_size(20.0)
                    .color(Color::new(0.35, 0.4, 0.35, 1.0)),
            );
        }
        draw_text(
            &mut batch.texts,
            "prefix cached; digits 0-9 table",
            TextOptions::default()
                .x(right_x)
                .y(col_y + 120.0)
                .font_size(12.0)
                .color(Color::new(0.5, 0.7, 0.55, 1.0)),
        );

        // --- 底部多段示例：固定数字 87 / 100（不每帧变）---
        draw_text(
            &mut batch.texts,
            "Also: multi-part HUD (static digits)",
            TextOptions::default()
                .x(24.0)
                .y(340.0)
                .font_size(14.0)
                .color(Color::new(0.7, 0.7, 0.75, 1.0)),
        );
        draw_text_parts(
            &mut batch.texts,
            &[
                TextPart::Text("HP "),
                TextPart::Digits("87"),
                TextPart::Text(" / "),
                TextPart::Digits("100"),
            ],
            TextOptions::default()
                .x(24.0)
                .y(372.0)
                .font_size(24.0)
                .color(YELLOW),
        );

        // --- 统计：固定/低频文字 + 数字走 Digits ---
        let n = app.gpu.shape_cache_len();
        let total = app.gpu.shape_cache_stats().hits + app.gpu.shape_cache_stats().misses;
        let hit_pct = if total > 0 {
            (100.0 * app.gpu.shape_cache_stats().hits as f64 / total as f64) as u32
        } else {
            0
        };
        let hit_str = hit_pct.to_string();
        let ft_str = (ft_ms as u32).max(1).to_string();
        let fps_str = (fps as u32).to_string();
        let n_str = n.to_string();

        // 行 1: cache 信息（每帧都画，但 parts 共享 cache）
        draw_text_parts(
            &mut batch.texts,
            &[
                TextPart::Text("cache entries="),
                TextPart::Digits(&n_str),
                TextPart::Text("  hit~"),
                TextPart::Digits(&hit_str),
                TextPart::Text("%"),
            ],
            TextOptions::default()
                .x(24.0)
                .y(420.0)
                .font_size(15.0)
                .color(Color::new(0.55, 0.9, 1.0, 1.0)),
        );

        // 行 2: FPS / frame
        draw_text_parts(
            &mut batch.texts,
            &[
                TextPart::Text("FPS~"),
                TextPart::Digits(&fps_str),
                TextPart::Text("  frame~"),
                TextPart::Digits(&ft_str),
                TextPart::Text("ms"),
            ],
            TextOptions::default()
                .x(320.0)
                .y(420.0)
                .font_size(15.0)
                .color(Color::new(1.0, 0.85, 0.5, 1.0)),
        );

        // FPS 直方图：12 桶×5FPS，用 parts 画 "█"（受 emoji 限制，这里用 #）
        let bar_y = 340.0;
        let bar_w = 12.0;
        let bar_gap = 2.0;
        let bar_x0 = 24.0;
        let mut hx = bar_x0;
        for i in 0..12usize {
            let count = fps_buckets[i];
            if count > 0 {
                draw_text_parts(
                    &mut batch.texts,
                    &[
                        TextPart::Digits(&count.to_string()),
                        TextPart::Text("|"),
                    ],
                    TextOptions::default()
                        .x(hx)
                        .y(bar_y)
                        .font_size(10.0)
                        .color(Color::new(0.6, 0.6, 0.8, 1.0)),
                );
            } else {
                draw_text(
                    &mut batch.texts,
                    ".",
                    TextOptions::default()
                        .x(hx)
                        .y(bar_y)
                        .font_size(10.0)
                        .color(Color::new(0.4, 0.4, 0.5, 1.0)),
                );
            }
            hx += bar_w + bar_gap;
        }
        fps_buckets[bucket] = fps_buckets[bucket].saturating_add(1);
        // 重置直方图：每 60 帧（避免无限增长）
        if tick % 60 == 0 {
            for v in fps_buckets.iter_mut() {
                *v = 0;
            }
        }

        draw_text(
            &mut batch.texts,
            "Tip: Parts-only => entries stay small. Whole-only => entries climb with score.",
            TextOptions::default()
                .x(24.0)
                .y(460.0)
                .font_size(12.0)
                .color(Color::new(0.55, 0.55, 0.6, 1.0)),
        );

        let _ = bar_x0;

        win.draw(Some(Color::new(0.07, 0.07, 0.10, 1.0)), &[&batch]);
        true
    });
}
