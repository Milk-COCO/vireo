//! 触摸：`on_touch` 回调 + `input.touches` 轮询
//!
//! 桌面无触摸屏时：用鼠标左键模拟（按住拖动 = 单指）。
//! 触屏设备可多点；显示 id / phase / force。

use std::sync::{Arc, Mutex};
use wgpu::PresentMode::Immediate;
use vireo::prelude::*;

struct TouchLog {
    last: Option<TouchEvent>,
    count: u32,
}

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Input Touch", 800, 560).present_mode(Immediate), None::<fn()>);

    let log = Arc::new(Mutex::new(TouchLog {
        last: None,
        count: 0,
    }));
    let mut registered = false;
    let mut mouse_touch_active = false;
    let sim_id: u64 = 9999;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        if !registered {
            registered = true;
            let log_cb = Arc::clone(&log);
            win.on_touch(move |ev| {
                let mut g = log_cb.lock().unwrap();
                g.count += 1;
                g.last = Some(ev.clone());
            });
        }

        // 鼠标左键模拟 Started/Moved/Ended（桌面调试）
        let left = win.mouse_left();
        let (mx, my) = win.mouse_pos();
        {
            let mut touches = win.input.touches.borrow_mut();
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
            "Touch: on_touch + input.touches  |  LMB = simulate finger",
            Pos::new(16.0, 14.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(0.75, 0.8, 0.9, 1.0)),
        );

        let touches: Vec<(u64, f32, f32, Option<f64>)> = win
            .input
            .touches
            .borrow()
            .iter()
            .map(|(&id, &(x, y, f))| (id, x, y, f))
            .collect();

        for (id, x, y, force) in &touches {
            let r = 28.0 + force.unwrap_or(0.3) as f32 * 24.0;
            let hue = (*id as f32 * 0.17) % 1.0;
            let col = Color::new(0.3 + hue * 0.5, 0.6, 1.0 - hue * 0.4, 0.85);
            draw_circle(&mut batch, Pos::new(*x, *y), r, Some(col));
            draw_circle_outline(&mut batch, Pos::new(*x, *y), r, 2.0, Some(WHITE), 32);
            draw_text(
                &mut batch.texts,
                &format!("id={id}"),
                Pos::new(*x - 20.0, *y - 8.0),
                TextDef::default().font_size(12.0),
                TextOverride::from_color(WHITE),
            );
        }

        draw_rectangle(&mut batch, Pos::new(0.0, 480.0), 800.0, 80.0, Some(Color::new(0.08, 0.09, 0.12, 1.0)));
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

        win.draw(Some(Color::new(0.05, 0.06, 0.09, 1.0)), &[&batch]);
        true
    });
}
