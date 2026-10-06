//! 触摸的两种读法：`on_touch` 回调（事件流）+ `win.touches()` 轮询（当前状态）
//!
//! 桌面无触摸屏时：用鼠标左键模拟（按住拖动 = 单指）。
//! 触屏设备可多点；显示 id / force。
//!
//! # 两件事这里被刻意分开
//!
//! 1. **读**用公开的 `win.touches()` → `TouchPoint`（按 id 升序、坐标 `PixelPos`
//!    双视图）。这是正常用法。
//! 2. **写**（模拟手指注入）仍需碰 `win.input.touches` —— 引擎只从 winit 的
//!    `WindowEvent::Touch` 写这个表，**没有公开的触摸注入 API**。桌面调试需要
//!    凭空造出触摸事件时只能这样，属于「引擎外模拟」而非正常路径。
//! 3. `on_touch` **在 `app.run(..)` 之前**用 `app.on_touch(idx, ..)` 注册即可，
//!    不需要挪进 run、也不需要 `registered` 标志位（回调注册只依赖
//!    `WindowIndex`，与窗口对象何时创建无关）。

use std::sync::{Arc, Mutex};
use vireo::prelude::*;
use wgpu::PresentMode::Immediate;

struct TouchLog {
    last: Option<TouchEvent>,
    count: u32,
}

#[vireo::main]
async fn main() {
    let idx = app.window(
        WindowDesc::new("Input Touch", 800, 560).present_mode(Immediate),
        None::<fn()>,
    );

    let log = Arc::new(Mutex::new(TouchLog {
        last: None,
        count: 0,
    }));

    // 回调注册：run 之前即可（见文件头第 3 点）。
    {
        let log_cb = Arc::clone(&log);
        app.on_touch(idx, move |ev| {
            let mut g = log_cb.lock().unwrap();
            g.count += 1;
            g.last = Some(ev.clone());
        });
    }

    let mut mouse_touch_active = false;
    let sim_id: u64 = 9999;

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };

        // 鼠标左键模拟 Started/Moved/Ended（桌面调试）。
        // ⚠️ 写侧是「引擎外模拟」：直接改 `win.input.touches`，引擎内部只从
        // winit 的 `WindowEvent::Touch` 写这张表，没有公开的注入 API。
        let left = win.mouse_left();
        let (mx, my) = win.mouse_pos().logical();
        let (mx, my) = (mx as f32, my as f32);
        {
            let mut touches = win.input.touches.lock();
            if left && !mouse_touch_active {
                mouse_touch_active = true;
                touches.insert(sim_id, (mx, my, Some(0.5)));
                let mut g = log.lock().unwrap();
                g.count += 1;
                g.last = Some(TouchEvent {
                    id: sim_id,
                    phase: TouchPhase::Started,
                    x: mx,
                    y: my,
                    force: Some(0.5),
                });
            } else if left && mouse_touch_active {
                touches.insert(sim_id, (mx, my, Some(0.5)));
                let mut g = log.lock().unwrap();
                g.last = Some(TouchEvent {
                    id: sim_id,
                    phase: TouchPhase::Moved,
                    x: mx,
                    y: my,
                    force: Some(0.5),
                });
            } else if !left && mouse_touch_active {
                mouse_touch_active = false;
                touches.remove(&sim_id);
                let mut g = log.lock().unwrap();
                g.count += 1;
                g.last = Some(TouchEvent {
                    id: sim_id,
                    phase: TouchPhase::Ended,
                    x: mx,
                    y: my,
                    force: None,
                });
            }
        }

        let mut batch = DrawBatch::new();
        batch.set_sdf_feather(Some(1.0));

        draw_text(
            &mut batch.texts,
            "Touch: on_touch + touches()  |  LMB = simulate finger",
            Pos::new(16.0, 14.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(0.75, 0.8, 0.9, 1.0)),
        );

        // 读侧：公开轮询 API。坐标用 `.logical()`（绘制坐标系，与 `Pos` 同空间）。
        let touches = win.touches();

        for tp in &touches {
            let id = tp.id;
            let (x, y) = tp.pos.logical();
            let (x, y) = (x as f32, y as f32);
            let force = tp.force;
            let r = 28.0 + force.unwrap_or(0.3) as f32 * 24.0;
            let hue = (id as f32 * 0.17) % 1.0;
            let col = Color::new(0.3 + hue * 0.5, 0.6, 1.0 - hue * 0.4, 0.85);
            draw_circle(&mut batch, Pos::new(x, y), r, Some(col));
            draw_circle_outline(&mut batch, Pos::new(x, y), r, 2.0, Some(WHITE), 32);
            draw_text(
                &mut batch.texts,
                &format!("id={id}"),
                Pos::new(x - 20.0, y - 8.0),
                TextDef::default().font_size(12.0),
                TextOverride::from_color(WHITE),
            );
        }

        draw_rectangle(
            &mut batch,
            Pos::new(0.0, 480.0),
            800.0,
            80.0,
            Some(Color::new(0.08, 0.09, 0.12, 1.0)),
        );
        // 先把 log 锁的作用域收窄：拷贝所需字段后立即释放。
        // 历史上 `win.draw()` 会等待 winit owner 线程完成 present；若期间仍持有 lock，
        // 而 owner 线程的 `on_touch` 回调也锁同一 Mutex，就会形成死锁。当前完整 surface
        // 帧循环已移到渲染线程，但仍应避免让输入回调与绘制逻辑跨长操作争用同一把锁。
        let (count, last_s) = {
            let g = log.lock().unwrap();
            let last_s = match &g.last {
                Some(e) => format!(
                    "last: id={} phase={:?} ({:.0},{:.0}) force={:?}",
                    e.id, e.phase, e.x, e.y, e.force
                ),
                None => "last: (none — touch or hold LMB)".into(),
            };
            (g.count, last_s)
        };
        draw_text(
            &mut batch.texts,
            &format!("active: {}  events: {}\n{last_s}", touches.len(), count),
            Pos::new(16.0, 492.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.7, 0.75, 0.85, 1.0)),
        );

        win.draw(Color::new(0.05, 0.06, 0.09, 1.0), &[&batch]);
        true
    })
    .await
    .unwrap();
}
