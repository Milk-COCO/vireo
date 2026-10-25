//! 退出 / 关窗拦截：四个钩子全在这一份里。
//!
//! - `on_close_requested`（**否决**，winit 线程同步，参数 `CloseSource`、返回 `CloseAction`）
//! - `on_closing`（已获准、即将拆除，winit 线程，`FnOnce`，每窗口一次）
//! - `on_exit`（进程收尾，winit 线程，每进程恰好一次）
//! - `on_exit_render`（渲染线程收尾，每 loop 线程一次，能碰 GPU）
//!
//! 跑法：`cargo run --example exit_guard`（需显示器）。
//!
//! 三档验收：
//!
//! 1. **干净状态**（画面上的「脏」= no）→ 点 X：直接关，进程退出。
//! 2. 按 `D` 标脏，再点 X：**窗口关不掉**，画面弹出确认框
//!    （`on_close_requested` 返回 `CloseAction::Cancel` 把这次关闭否决了；X 按钮已经
//!    处理完，窗口原样存活）。同理 `Q` / `ESC` 走 `win.close()` 也会被同一票否决 ——
//!    程序化关窗和 X 共用一条许可路径，日志里 `[veto]` 后面会打出是哪一种来源。
//! 3. 确认框里按 `Y`：置「允许」旗后重新 `close()`，这次放行。随后依次看到
//!    `[on_closing]`（winit 线程）→ `[on_exit_render]`（渲染线程）→ `[on_exit]`（winit 线程）。
//!    `N` / `ESC` 取消，回到第 2 档。
//!
//! 关键点：否决回调**跑在 winit 线程**（与 `on_key_down` 等一致），同步执行，
//! 不许阻塞、也画不了帧。所以「弹确认框」不是在这里弹的 —— 这里只置一个原子旗，
//! 由正在绘制的 loop 下一帧画出来。这是这个模式下唯一可行的做法。
//!
//! ## 自检模式（不用手点）
//!
//! `VIREO_EXIT_GUARD_AUTO=1 cargo run --release --example exit_guard`：
//! 第 20 tick 标脏并 `close()`（应被否决、窗口存活），第 40 tick 断言「确认旗已置位
//! 且窗口仍在」后放行 `close()`，随后进程以退出码 0 结束并打出三条收尾日志。
//! 断言失败会 `process::exit(2)` 并打 `[auto] FAIL`。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use vireo::prelude::*;

#[vireo::main]
async fn main() {
    let idx = app.window(WindowDesc::new("vireo exit guard", 640, 420));
    let auto = std::env::var_os("VIREO_EXIT_GUARD_AUTO").is_some_and(|v| v != "0");

    // 共享状态（winit 线程的否决回调 ⇄ 渲染线程的绘制/交互）
    let dirty = Arc::new(AtomicBool::new(false));
    let allow_close = Arc::new(AtomicBool::new(false));
    let confirm_open = Arc::new(AtomicBool::new(false));
    let denied = Arc::new(AtomicU32::new(0));

    // ---- 1. 关窗许可（否决钩子）：winit 线程、同步 ----
    {
        let (dirty, allow_close, confirm_open, denied) = (
            dirty.clone(),
            allow_close.clone(),
            confirm_open.clone(),
            denied.clone(),
        );
        app.on_close_requested(idx, move |source| {
            if allow_close.load(Ordering::SeqCst) || !dirty.load(Ordering::SeqCst) {
                return CloseAction::Allow; // 放行
            }
            // 有未保存改动 → 否决本次关闭，让 loop 下一帧弹确认框。
            confirm_open.store(true, Ordering::SeqCst);
            denied.fetch_add(1, Ordering::SeqCst);
            println!("[veto] {source:?} 请求关闭 → 已否决（有未保存改动）");
            CloseAction::Cancel
        });
    }

    // ---- 2. 已获准、即将拆除（winit 线程，FnOnce）：此时窗口对象和 surface 都还在 ----
    //
    // 只覆盖「主动关窗」（X / `close()`）。main 返回后的批量收尾与设备丢失不走这条，
    // 那种场景的收尾属于下面的 `on_exit`。
    app.on_closing(idx, || {
        println!("[on_closing] 窗口即将拆除（winit 线程，FnOnce，每窗口一次）")
    });

    // ---- 3. 退出收尾：winit 线程，每进程恰好一次 ----
    app.on_exit(|reason| {
        println!("[on_exit] 进程即将退出，原因 = {reason:?}（winit 线程，纯 CPU 收尾）");
    });

    // ---- 4. 渲染线程收尾：每个 loop 线程一次，能碰 App / GPU ----
    {
        let app_for_hook = app.clone();
        app.on_exit_render(move || {
            println!(
                "[on_exit_render] 渲染线程收尾（device_lost = {}）",
                app_for_hook.gpu().is_device_lost()
            );
        });
    }

    let mut prev = [false; 5]; // D / Y / N / Q / ESC 的上一帧状态（做边沿检测）

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false, // 窗口已关（干净时点 X / 确认后关）
        };

        // 全部走**边沿**：ESC/Q 按住不放时每帧调一次 `close()` 会让否决钩子被反复触发
        // （`denied` 计数虚涨、通道里堆一串关窗请求）。
        let down = [
            win.key_down(KeyCode::KeyD),
            win.key_down(KeyCode::KeyY),
            win.key_down(KeyCode::KeyN),
            win.key_down(KeyCode::KeyQ),
            win.key_down(KeyCode::Escape),
        ];
        let mut just = [false; 5];
        for i in 0..5 {
            just[i] = down[i] && !prev[i];
        }
        prev = down;

        let confirming = confirm_open.load(Ordering::SeqCst);

        // 自检模式：不依赖人手，按 tick 走完「被否决 → 放行」两段。
        if auto {
            let t = ctx.tick_count();
            if t == 20 {
                dirty.store(true, Ordering::SeqCst);
                win.close();
            } else if t == 40 {
                let held = confirm_open.load(Ordering::SeqCst)
                    && ctx.app().window_ref(&idx).is_ok()
                    && !allow_close.load(Ordering::SeqCst);
                if held {
                    eprintln!(
                        "[auto] OK: close vetoed at t=20, window still alive, \
                         confirm flag set (denied = {})",
                        denied.load(Ordering::SeqCst)
                    );
                } else {
                    eprintln!("[auto] FAIL: on_close_requested did not hold the window");
                    std::process::exit(2);
                }
                allow_close.store(true, Ordering::SeqCst);
                confirm_open.store(false, Ordering::SeqCst);
                win.close();
            }
        }

        if just[0] {
            // 切换「脏」状态：脏了之后关窗就要先问
            dirty.fetch_xor(true, Ordering::SeqCst);
            if !dirty.load(Ordering::SeqCst) {
                confirm_open.store(false, Ordering::SeqCst);
            }
        }
        if confirming {
            if just[1] {
                // 用户确认：**先放行再关**，否则同一条 `win.close()` 又被自己否决。
                allow_close.store(true, Ordering::SeqCst);
                confirm_open.store(false, Ordering::SeqCst);
                win.close();
            } else if just[2] || just[4] {
                confirm_open.store(false, Ordering::SeqCst);
            }
        } else if just[3] || just[4] {
            // 注意：脏的时候这里同样会被否决 —— 程序化关窗不是后门。
            win.close();
        }

        // ---- 绘制 ----
        let mut batch = DrawBatch::new();
        let is_dirty = dirty.load(Ordering::SeqCst);

        draw_rectangle(
            &mut batch,
            Pos::new(20.0, 20.0),
            600.0,
            380.0,
            Some(Color::new(0.10, 0.11, 0.14, 1.0)),
        );

        let info = format!(
            "dirty: {}\nclose denied: {}\n\nD = toggle dirty\nX / Q / ESC = try to close\n\n{}",
            if is_dirty {
                "YES（关窗会先问）"
            } else {
                "no（关窗直接生效）"
            },
            denied.load(Ordering::SeqCst),
            if confirming {
                ">>> 有未保存改动：Y = 仍然退出 / N 或 ESC = 取消"
            } else {
                "点标题栏 X，或按 Q / ESC"
            },
        );
        draw_text(
            &mut batch.texts,
            &info,
            Pos::new(40.0, 40.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(0.85, 0.88, 0.92, 1.0)),
        );

        if confirming {
            // 确认框（在 loop 里画 —— 否决回调本身画不了）
            draw_rectangle(
                &mut batch,
                Pos::new(80.0, 180.0),
                480.0,
                140.0,
                Some(Color::new(0.45, 0.15, 0.15, 1.0)),
            );
            draw_text(
                &mut batch.texts,
                "有未保存改动",
                Pos::new(110.0, 205.0),
                TextDef::default().font_size(24.0),
                TextOverride::from_color(WHITE),
            );
            draw_text(
                &mut batch.texts,
                "Y = 退出（丢弃改动）    N / ESC = 取消",
                Pos::new(110.0, 260.0),
                TextDef::default().font_size(18.0),
                TextOverride::from_color(Color::new(1.0, 0.92, 0.80, 1.0)),
            );
        }

        win.draw(Color::new(0.05, 0.05, 0.07, 1.0), &[&batch]);
        true
    })
    .await
    .unwrap();
}
