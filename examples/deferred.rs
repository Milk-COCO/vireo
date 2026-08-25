//! 延迟任务示例：`after_frames(0)` vs `after_frames(1)` 与 `after_secs`
//!
//! 交互：
//! - 空格：触发一组延迟任务
//!     · `after_frames(0)` → 本帧（注册帧）末尾把方块变红
//!     · `after_frames(1)` → 下一帧末尾把方块变绿
//!     · `after_secs(1.0)`  → 1 秒墙钟后把方块变回白
//!   HUD 打印每个回调触发时记录的 frame 编号，可见 `0` 比 `1` 早一帧。
//!
//! 语义（`src/window/mod.rs`）：`after_frames(k)` 在「注册帧 + k」的帧末执行；
//! 若在 `run()` 之前注册（`frame_count == 0`），`after_frames(0)` 在第一个 `on_frame`
//! 之前执行、`after_frames(1)` 在第 1 帧末尾执行。

use std::sync::{Arc, Mutex};

use vireo::prelude::*;

struct Demo {
    color: Color,
    log: Vec<(u64, String)>,
    arm: bool,
}

fn prune(log: &mut Vec<(u64, String)>) {
    if log.len() > 6 {
        log.drain(0..log.len() - 6);
    }
}

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Deferred Tasks", 640, 420), None::<fn()>);

    let demo: Arc<Mutex<Demo>> = Arc::new(Mutex::new(Demo {
        color: WHITE,
        log: Vec::new(),
        arm: false,
    }));

    let d_key = Arc::clone(&demo);
    app.on_key_down(idx, move |event| {
        if event.repeat {
            return;
        }
        if event.key == KeyCode::Space {
            d_key.lock().unwrap().arm = true;
        }
    });

    app.run(move |app| {
        let win = match app.window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };

        // 取走本帧的触发标记
        let arm = {
            let mut d = demo.lock().unwrap();
            let a = d.arm;
            d.arm = false;
            a
        };

        if arm {
            let fc = app.frame_count;
            // after_frames(0)：注册帧（fc）末尾执行
            app.after_frames(0, {
                let d = Arc::clone(&demo);
                move || {
                    let mut d = d.lock().unwrap();
                    d.color = Color::new(1.0, 0.25, 0.25, 1.0);
                    d.log.push((fc, "after_frames(0) -> red".into()));
                    prune(&mut d.log);
                }
            });
            // after_frames(1)：注册帧 + 1（fc+1）末尾执行
            app.after_frames(1, {
                let d = Arc::clone(&demo);
                move || {
                    let mut d = d.lock().unwrap();
                    d.color = Color::new(0.25, 1.0, 0.35, 1.0);
                    d.log.push((fc + 1, "after_frames(1) -> green".into()));
                    prune(&mut d.log);
                }
            });
            // after_secs：墙钟 1 秒后执行（落在某帧末尾）
            app.after_secs(1.0, {
                let d = Arc::clone(&demo);
                move || {
                    let mut d = d.lock().unwrap();
                    d.color = WHITE;
                    d.log.push((fc, "after_secs(1.0) -> white".into()));
                    prune(&mut d.log);
                }
            });
        }

        let (color, log) = {
            let d = demo.lock().unwrap();
            (d.color, d.log.clone())
        };
        let fc = app.frame_count;

        // 背景
        let mut bg = DrawBatch::new();
        draw_rectangle(&mut bg, Pos::new(0.0, 0.0), 640.0, 420.0, Some(Color::new(0.05, 0.06, 0.1, 1.0)));

        // 居中方块（颜色由延迟任务控制）
        let mut shape = DrawBatch::new();
        let t = fc as f32 * 0.05;
        let s = 120.0 + t.sin() * 10.0;
        draw_rectangle(
            &mut shape,
            Pos::new(320.0 - s / 2.0, 180.0 - s / 2.0),
            s,
            s,
            Some(color),
        );

        // HUD
        let mut hud = DrawBatch::new();
        let mut y = 16.0;
        let line = |hud: &mut DrawBatch, text: &str, y: f32| {
            draw_text(
                &mut hud.texts,
                text,
                Pos::new(16.0, y),
                TextDef::default().font_size(18.0),
                TextOverride::from_color(WHITE),
            );
        };
        line(&mut hud, &format!("frame: {fc}"), y);
        y += 24.0;
        line(&mut hud, "SPACE: after_frames(0) red / (1) green / after_secs(1.0) white", y);
        y += 28.0;
        for (f, msg) in log.iter().rev() {
            line(&mut hud, &format!("  f{f}  {msg}"), y);
            y += 20.0;
        }

        win.draw(Color::new(0.05, 0.06, 0.1, 1.0), &[&bg, &shape, &hud]);

        true
    })
    .unwrap();
}
