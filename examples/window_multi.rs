/// 演示：high_dpi 模式、多窗口、鼠标跟随、关闭钩子、文本渲染
use vireo::prelude::*;

fn main() {
    let mut app = App::new();

    let idx_a = app.window(
        WindowDesc::new("A - high_dpi mouse follower", 800, 600).high_dpi(true),
        Some(|| println!("窗口 A 已关闭")),
    );
    let idx_b = app.window(
        WindowDesc::new("B - mouse capture + text", 400, 400),
        Some(|| println!("窗口 B 已关闭")),
    );

    app.run(move |app| {
        let win_a = match app.window_ref(&idx_a) {
            Some(w) => w,
            None => return true,
        };
        let win_b = match app.window_ref(&idx_b) {
            Some(w) => w,
            None => return true,
        };

        let mouse = win_b.mouse_pos();
        let has_mouse = mouse.0 >= 0.0 && mouse.1 >= 0.0;

        // A
        let mut batch = DrawBatch::new();
        let w = win_a.logical_width as f32;
        let h = win_a.logical_height as f32;
        let cx = mouse.0 * w / win_b.logical_width as f32;
        let cy = mouse.1 * h / win_b.logical_height as f32;
        if has_mouse {
            draw_circle(&mut batch, cx, cy, 20.0, Some(RED));
            draw_line(&mut batch, cx, 0.0, cx, h, 1.0, Some(Color::new(0.25, 0.25, 0.35, 0.4)));
            draw_line(&mut batch, 0.0, cy, w, cy, 1.0, Some(Color::new(0.25, 0.25, 0.35, 0.4)));

            // 文本显示坐标
            draw_text(
                &mut batch.texts,
                &format!("({:.0}, {:.0})", cx, cy),
                TextOptions {
                    x: cx + 24.0,
                    y: cy - 20.0,
                    font_size: 14.0,
                    ..TextOptions::default()
                },
            );
        }
        win_a.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&batch]);

        // B
        let mut batch = DrawBatch::new();
        if has_mouse {
            draw_rectangle(&mut batch, mouse.0 - 16.0, mouse.1 - 1.0, 32.0, 2.0, Some(WHITE));
            draw_rectangle(&mut batch, mouse.0 - 1.0, mouse.1 - 16.0, 2.0, 32.0, Some(WHITE));
            draw_circle(&mut batch, mouse.0, mouse.1, 8.0, Some(RED));
        }

        // 混合中英文示例
        draw_text(
            &mut batch.texts,
            "Vireo 文本渲染! Hello World!",
            TextOptions {
                x: 10.0,
                y: 10.0,
                font_size: 20.0,
                color: Color::new(0.9, 0.9, 1.0, 1.0),
                ..TextOptions::default()
            },
        );

        if has_mouse {
            draw_text(
                &mut batch.texts,
                &format!("鼠标: ({:.0}, {:.0})", mouse.0, mouse.1),
                TextOptions {
                    x: 10.0,
                    y: 40.0,
                    font_size: 14.0,
                    color: Color::new(0.7, 0.7, 0.7, 1.0),
                    ..TextOptions::default()
                },
            );
        } else {
            draw_text(
                &mut batch.texts,
                "移动鼠标到本窗口...",
                TextOptions {
                    x: 10.0,
                    y: 40.0,
                    font_size: 14.0,
                    color: Color::new(0.5, 0.5, 0.5, 1.0),
                    ..TextOptions::default()
                },
            );
        }

        win_b.draw(Some(Color::new(0.12, 0.12, 0.18, 1.0)), &[&batch]);

        true
    });
}
