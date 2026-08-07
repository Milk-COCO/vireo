//! 布局跟随的平滑模式（`set_layout_follow_smoothing`），拖动窗口观察。
//!
//! ## 背景
//!
//! `layout_follow`（默认开）在窗口尺寸已变但 surface 未重配（拖动中）时，把几何/文字
//! 实时重排到新窗口（`Renderer::update_layout`），而不是停在旧布局纯拉伸。重排的「节奏」
//! 由平滑模式 [`FollowAmount`] 决定，强度可用真实时长 `Time(d)` 或帧数 `Frames(n)` 表达：
//!
//! - **`PerFrame`**：每帧都追到最新窗口尺寸。最跟手，但窗动得快时画面容易「抖/闪一帧」。
//! - **`Average`（平均窗）**：camera 目标 = 最近 `amt` 内观察到的窗口尺寸的**均值**，
//!   连续渐变不跳格；窗越大越平滑（反应越慢）、越小越跟手。
//!
//! 强度统一可调：`Time` 与刷新率无关；`Frames` 跟随实际刷新频率。
//!
//! ## 操作
//!
//! 拖动窗口边缘/标题栏，切换下列键观察重排节奏：
//! - `[` / `]`：循环切换平滑预设（窗宽 + 单位，见 HUD）。
//! - `L`：开关 `layout_follow`（关闭 = 拖动中内容完全停旧布局、纯拉伸）。
//! - `V`：切换 present mode（`AutoVsync` ↔ `Immediate`）。
//!
//! 画面中央的动画方块与网格会跟着 camera 同步拉伸/重排，方便对比不同档位。
//! 建议从 `Average(Time 16ms)` 拖起，逐步往右调大窗，体会从跟手到平滑的变化。
//! 默认 preset 与库默认（`Average(Time 16ms)`）一致。

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Layout Follow Smoothing", 720, 480).present_mode(PresentMode::AutoVsync),
        None::<fn()>,
    );

    // (模式, 描述)；`[` `]` 循环这组预设
    const PRESETS: &[(FollowAmount, &str)] = &[
        (FollowAmount::PerFrame, "PerFrame — 每帧追（最跟手，易抖）"),
        (
            FollowAmount::Average(FollowFramesOrTime::Time(std::time::Duration::from_millis(16))),
            "Average Time 16ms — 平均窗（默认）",
        ),
        (
            FollowAmount::Average(FollowFramesOrTime::Time(std::time::Duration::from_millis(66))),
            "Average Time 66ms — 平均窗（较平滑）",
        ),
        (
            FollowAmount::Average(FollowFramesOrTime::Time(std::time::Duration::from_millis(132))),
            "Average Time 132ms — 平均窗（很平滑）",
        ),
        (
            FollowAmount::Average(FollowFramesOrTime::Frames(2)),
            "Average Frames 2 — 平均窗（最近2帧均值）",
        ),
        (
            FollowAmount::Average(FollowFramesOrTime::Frames(8)),
            "Average Frames 8 — 平均窗（最近8帧均值）",
        ),
    ];
    let mut preset_i = 1usize; // Average(Time 16ms) = 库默认
    let mut follow_enabled = true;
    let mut key_was = [false; 4]; // [ ] L V

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        let keys = [
            win.key_down(KeyCode::BracketLeft),
            win.key_down(KeyCode::BracketRight),
            win.key_down(KeyCode::KeyL),
            win.key_down(KeyCode::KeyV),
        ];
        for i in 0..4 {
            if keys[i] && !key_was[i] {
                match i {
                    0 => {
                        preset_i = if preset_i == 0 { PRESETS.len() - 1 } else { preset_i - 1 };
                        win.set_layout_follow_smoothing(PRESETS[preset_i].0);
                    }
                    1 => {
                        preset_i = (preset_i + 1) % PRESETS.len();
                        win.set_layout_follow_smoothing(PRESETS[preset_i].0);
                    }
                    2 => {
                        follow_enabled = !follow_enabled;
                        win.set_layout_follow(follow_enabled);
                    }
                    3 => {
                        let next = match win.present_mode() {
                            PresentMode::Immediate => PresentMode::AutoVsync,
                            _ => PresentMode::Immediate,
                        };
                        win.set_present_mode(next);
                    }
                    _ => {}
                }
            }
            key_was[i] = keys[i];
        }

        // 与当前 smoothing 一致（预设被 `[` `]` 应用）。
        let current = win.layout_follow_smoothing();

        let metrics = win.metrics();
        let mut batch = DrawBatch::new();

        // 动画方块（验证重排节奏；正方形在拉伸中变成分率）
        let t = app.frame_count as f32 * 0.05;
        let w = metrics.width.max(1) as f32;
        let h = metrics.height.max(1) as f32;
        let bx = (w - 120.0) * (t.sin() * 0.5 + 0.5);
        let by = (h - 120.0) * (t.cos() * 0.5 + 0.5);
        draw_rounded_rect(
            &mut batch,
            Pos::new(bx, by),
            120.0, 120.0, 16.0,
            Some(Color::new(0.3, 0.6, 1.0, 0.85)),
        );

        // 参考网格（拖动时观察拉伸/重排）
        let step = 64.0f32;
        let mut y = step;
        while y < h {
            draw_line(&mut batch, 0.0, y, w, y, 1.0, Some(Color::new(0.2, 0.2, 0.3, 0.6)));
            y += step;
        }
        let mut x = step;
        while x < w {
            draw_line(&mut batch, x, 0.0, x, h, 1.0, Some(Color::new(0.2, 0.2, 0.3, 0.6)));
            x += step;
        }

        let follow_label = if follow_enabled { "on" } else { "off (pure stretch)" };
        let lines = [
            format!("Layout follow: {}   present: {:?}", follow_label, win.present_mode()),
            format!("Smoothing: {}", PRESETS[preset_i].1),
            format!("current(): {:?}", current),
            format!(
                "window: {}x{} (logical)   Update FPS: {:.1}",
                metrics.width, metrics.height, app.fps
            ),
            "[ ]=切预设  L=开/关跟随  V=present mode  — 拖动窗口边缘/标题栏观察".into(),
        ];
        for (i, line) in lines.iter().enumerate() {
            draw_text(
                &mut batch.texts,
                line,
                Pos::new(16.0, 20.0 + i as f32 * 22.0),
                TextDef::default().font_size(15.0),
                TextOverride::from_color(if i == 0 { GOLD } else { WHITE }),
            );
        }

        let report = win.draw(Color::new(0.06, 0.07, 0.10, 1.0), &[&batch]);
        let _ = report;
        true
    });
}