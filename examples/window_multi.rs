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
        let ma = win_a.metrics();
        let mb = win_b.metrics();
        let w = ma.width as f32;
        let h = ma.height as f32;
        let cx = mouse.0 * w / mb.width as f32;
        let cy = mouse.1 * h / mb.height as f32;
        if has_mouse {
            draw_circle(&mut batch, Pos::new(cx, cy), 20.0, Some(RED));
            draw_line(&mut batch, cx, 0.0, cx, h, 1.0, Some(Color::new(0.25, 0.25, 0.35, 0.4)));
            draw_line(&mut batch, 0.0, cy, w, cy, 1.0, Some(Color::new(0.25, 0.25, 0.35, 0.4)));

            // 文本显示坐标
            draw_text(
                &mut batch.texts,
                &format!("({:.0}, {:.0})", cx, cy),
                Pos::new(cx + 24.0, cy - 20.0), TextDef::default().font_size(14.0),
                TextOverride::default(),
            );
        }
        win_a.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&batch]);

        // B
        let mut batch = DrawBatch::new();
        if has_mouse {
            draw_rectangle(&mut batch, Pos::new(mouse.0 - 16.0, mouse.1 - 1.0), 32.0, 2.0, Some(WHITE));
            draw_rectangle(&mut batch, Pos::new(mouse.0 - 1.0, mouse.1 - 16.0), 2.0, 32.0, Some(WHITE));
            draw_circle(&mut batch, Pos::new(mouse.0, mouse.1), 8.0, Some(RED));
        }

        // 混合中英文示例
        draw_text(
            &mut batch.texts,
            "Vireo 文本渲染! Hello World!",
            Pos::new(10.0, 10.0), TextDef::default().font_size(20.0),
            TextOverride::from_color(Color::new(0.9, 0.9, 1.0, 1.0)),
        );

        if has_mouse {
            draw_text(
                &mut batch.texts,
                &format!("鼠标: ({:.0}, {:.0})", mouse.0, mouse.1),
                Pos::new(10.0, 40.0), TextDef::default().font_size(14.0),
                TextOverride::from_color(Color::new(0.7, 0.7, 0.7, 1.0)),
            );
        } else {
            draw_text(
                &mut batch.texts,
                "移动鼠标到本窗口...",
                Pos::new(10.0, 40.0), TextDef::default().font_size(14.0),
                TextOverride::from_color(Color::new(0.5, 0.5, 0.5, 1.0)),
            );
        }

        win_b.draw(Some(Color::new(0.12, 0.12, 0.18, 1.0)), &[&batch]);

        true
    });
}
