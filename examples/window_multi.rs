/// 演示：dpi_override、多窗口、鼠标跟随、关闭钩子、文本渲染
use vireo::prelude::*;

fn main() {
    let app = App::new();

    let idx_a = app.window(
        WindowDesc::new("A - high_dpi mouse follower", 800, 600).dpi_override(Some(1.0)),
        Some(|| println!("窗口 A 已关闭")),
    );
    let idx_b = app.window(
        WindowDesc::new("B - mouse capture + text", 400, 400),
        Some(|| println!("窗口 B 已关闭")),
    );

    app.run(move |app| {
        let win_a = match app.window_ref(&idx_a) {
            Ok(w) => w,
            Err(_) => return true,
        };
        let win_b = match app.window_ref(&idx_b) {
            Ok(w) => w,
            Err(_) => return true,
        };

        let mouse = win_b.mouse_pos().logical();
        let (mx, my) = (mouse.0 as f32, mouse.1 as f32);
        let has_mouse = mx >= 0.0 && my >= 0.0;

        // A
        let mut batch = DrawBatch::new();
        let (wa, ha) = win_a.layout_size().logical();
        let (wb, hb) = win_b.layout_size().logical();
        let w = wa as f32;
        let h = ha as f32;
        let cx = mx * w / wb as f32;
        let cy = my * h / hb as f32;
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
        win_a.draw(Color::new(0.06, 0.08, 0.12, 1.0), &[&batch]);

        // B
        let mut batch = DrawBatch::new();
        if has_mouse {
            draw_rectangle(&mut batch, Pos::new(mx - 16.0, my - 1.0), 32.0, 2.0, Some(WHITE));
            draw_rectangle(&mut batch, Pos::new(mx - 1.0, my - 16.0), 2.0, 32.0, Some(WHITE));
            draw_circle(&mut batch, Pos::new(mx, my), 8.0, Some(RED));
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
                &format!("鼠标: ({:.0}, {:.0})", mx, my),
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

        win_b.draw(Color::new(0.12, 0.12, 0.18, 1.0), &[&batch]);

        true
    }).unwrap();
}
