//! Resize 刷新策略 + 去抖时长（拖动窗口时观察）
//!
//! ## 背景：为什么需要这套东西
//!
//! **这是针对 Windows 的 workaround/优化**：
//! wgpu-hal 在 DX12 上每次 `surface.configure` 都会无条件 `wait_for_present_queue_idle`
//! 来等 present queue 排空（我电脑不算差吧，实测每帧 ~60-90ms），于是就有了这两个 API：
//! 1. **`set_resize_refresh_policy`**：拖动中要不要（以及怎样）实时刷新。
//!    `ResizeRefreshPolicy` 提供 `OnRelease` / `EveryFrame` / `Periodic(interval)` 三档，详见其文档。
//! 2. **`set_resize_debounce`**：尺寸**稳定**之后的多久之后，一次性 `surface.configure`
//!    （松手 snap 的延迟），默认 100ms。
//!
//! 两者默认情况下配合的效果：拖动全程不 configure（内容按旧尺寸拉伸 present、帧流满速），
//! 尺寸已停止变化后满 debounce 时长即 snap 到新尺寸。
//! 判定「尺寸已停止变化」靠渲染线程逐帧轮询 `inner_size()` 与上一帧对比，**移动**会重置计时器。
//!
//! **如果只面向非 Windows 平台开发，且可忽略卡顿、需要拖动时画面更实时**，可以完全关掉这些 workaround：
//!
//! ```rust,ignore
//! // 拖动中每帧实时 configure（无 DX12 阻塞代价时这就是最实时路径）
//! win.set_resize_refresh_policy(ResizeRefreshPolicy::EveryFrame);
//! // 去抖时长缩到几乎为零（EveryFrame 下由 Live 路径接管，Stable 优先仅兜底）
//! win.set_resize_debounce(std::time::Duration::from_millis(1));
//! ```
//!
//! 也就是说：非 Windows 上直接选 `EveryFrame` + 极小去抖即可拿到最实时画面；
//! 本示例的 OnRelease / Periodic 是专为 DX12 阻塞代价设计的取舍。
//!
//! ## 操作
//!
//! 拖动窗口边缘实时观察三种策略：
//! - `O`：OnRelease（默认）——拖动全程不 `surface.configure`（内容按旧尺寸拉伸、
//!   帧流满速），松手尺寸稳定满去抖时长后一次性 snap。
//! - `F`：EveryFrame——拖动中每帧 configure 实时跟踪（DX12 每次阻塞 ~50-80ms，
//!   掉帧是预期代价）。
//! - `P`：Periodic(400ms)——拖动中每 400ms 强制 configure 一次（折中）。
//!
//! `D`：循环切去抖时长 0 / 32 / 100 / 250 / 500ms（0 = 尺寸变化当帧就 snap，
//! 去抖退化为每帧实时 configure；其余影响「松手」到 snap 的延迟）。
//!
//! ## 看什么
//!
//! 画面中央有动画方块验证拖动期间帧流是否持续；HUD 的 `suboptimal`
//! 在拖动中为 true（旧尺寸拉伸 present），松手 snap 后回到 false。
//! PresentMode 建议切到 `Immediate`（`win.set_present_mode`）避免 vsync 干扰帧流观察。
//! 
//! 想看更详细的帧用时，可以把 `frame_stats` 示例与本示例结合。或者直接改那个示例的刷新策略/去抖也行。

use vireo::prelude::*;

fn policy_label(p: ResizeRefreshPolicy) -> String {
    match p {
        ResizeRefreshPolicy::OnRelease => "OnRelease".to_string(),
        ResizeRefreshPolicy::EveryFrame => "EveryFrame".to_string(),
        ResizeRefreshPolicy::Periodic(iv) => format!("Periodic({}ms)", iv.as_millis()),
    }
}

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Resize Refresh Policy", 640, 360).present_mode(PresentMode::Immediate),
        None::<fn()>,
    );

    // 去抖时长候选（毫秒）；0 = 变化即 snap（等价每帧实时 configure）
    const DEBOUNCE_MS: [u64; 5] = [0, 32, 100, 250, 500];
    let mut debounce_i = 2usize;

    let mut policy = ResizeRefreshPolicy::OnRelease;
    let mut key_was = [false; 4]; // O F P D
    let mut last_acq_ms = 0.0f64;
    let mut last_enc_ms = 0.0f64;
    let mut last_suboptimal = false;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        let keys = [
            win.key_down(KeyCode::KeyO),
            win.key_down(KeyCode::KeyF),
            win.key_down(KeyCode::KeyP),
            win.key_down(KeyCode::KeyD),
        ];
        for i in 0..4 {
            if keys[i] && !key_was[i] {
                match i {
                    0 => policy = ResizeRefreshPolicy::OnRelease,
                    1 => policy = ResizeRefreshPolicy::EveryFrame,
                    2 => policy = ResizeRefreshPolicy::Periodic(std::time::Duration::from_millis(400)),
                    3 => {
                        debounce_i = (debounce_i + 1) % DEBOUNCE_MS.len();
                        win.set_resize_debounce(std::time::Duration::from_millis(DEBOUNCE_MS[debounce_i]));
                    }
                    _ => {}
                }
                if i != 3 {
                    win.set_resize_refresh_policy(policy);
                }
            }
            key_was[i] = keys[i];
        }

        let metrics = win.metrics();
        let mut batch = DrawBatch::new();

        // 动画方块（验证拖动期间帧流是否持续）
        let t = app.frame_count as f32 * 0.05;
        let w = metrics.width.max(1) as f32;
        let h = metrics.height.max(1) as f32;
        let bx = (w - 80.0) * (t.sin() * 0.5 + 0.5);
        let by = (h - 80.0) * (t.cos() * 0.5 + 0.5);
        draw_rounded_rect(&mut batch, Pos::new(bx, by), 80.0, 80.0, 12.0, Some(Color::new(0.3, 0.6, 1.0, 1.0)));

        // 参考网格（拖动时观察拉伸）
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

        let debounce_ms = win.resize_debounce().as_millis();
        let lines = [
            format!("Resize refresh: {}  (O/F/P)", policy_label(policy)),
            format!("Debounce: {}ms  (D)   present mode: {:?}", debounce_ms, win.present_mode()),
            format!(
                "window: {}x{} (logical)  FPS: {:.1}  frame_time: {:.2} ms",
                metrics.width, metrics.height, app.fps, app.frame_time * 1000.0
            ),
            format!(
                "last acquire: {:.2} ms  encode: {:.2} ms",
                last_acq_ms, last_enc_ms
            ),
            format!("suboptimal: {}  (true = 拖动拉伸中)", last_suboptimal),
            "drag edge: O=OnRelease(默认) F=EveryFrame P=Periodic(400ms) D=去抖".into(),
        ];
        for (i, line) in lines.iter().enumerate() {
            draw_text(
                &mut batch.texts,
                line,
                Pos::new(16.0, 20.0 + i as f32 * 22.0),
                TextDef::default().font_size(15.0),
                TextOverride::from_color(if i == 0 { GOLD } else if i == 4 { Color::new(0.6, 0.85, 0.7, 1.0) } else { WHITE }),
            );
        }

        let report = win.draw(Some(Color::new(0.06, 0.07, 0.1, 1.0)), &[&batch]);
        match report.outcome {
            DrawOutcome::Presented { suboptimal } => last_suboptimal = suboptimal,
            DrawOutcome::Skipped(_) | DrawOutcome::Failed(_) => {}
        }
        last_acq_ms = report.timings.acquire_secs * 1000.0;
        last_enc_ms = report.timings.encode_secs * 1000.0;
        true
    });
}
