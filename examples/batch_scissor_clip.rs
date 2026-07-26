//! scissor scissor 裁剪演示
//!
//! ```bash
//! cargo run --example scissor_demo
//! ```
//!
//! 关键点：
//! - 父 batch 自身内容**不**被裁
//! - 子 batch 被裁（GPU scissor）
//! - 对比组：无 scissor，子内容越界可见

use vireo::prelude::*;

fn label(batch: &mut DrawBatch, text: &str, x: f32, y: f32) {
    draw_text(
        &mut batch.texts,
        text,
        Pos::new(x, y),
        TextDef::default().font_size(14.0),
        TextOverride::from_color(Color::new(0.65, 0.68, 0.78, 1.0)),
    );
}

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("scissor Scissor Demo", 900, 540).high_dpi(true),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.02;

        let mut ui = DrawBatch::new();

        // ===== 演示组：scissor =====
        let mut scissor_batch = DrawBatch::new();
        scissor_batch.clips_children = false;
        scissor_batch.scissor = Some(Rect::new(50.0, 50.0, 300.0, 220.0));

        // 父 batch 自身内容：画一个贯穿 scissor 的大矩形（应不被裁）
        draw_rectangle(
            &mut scissor_batch,
            Pos::new(0.0, 0.0), 400.0, 320.0,
            Some(Color::new(0.15, 0.16, 0.22, 0.3)),
        );
        label(
            &mut scissor_batch,
            "父内容不被裁（此文字超出 scissor 仍可见）",
            10.0, 290.0,
        );

        // 子 batch：色块在 scissor 内外摆动
        let mut child = DrawBatch::new();
        let n = 20;
        for i in 0..n {
            let phase = t * 1.5 + i as f32 * 0.4;
            let ox = phase.sin() * 120.0;
            let oy = (phase * 0.8 + 1.0).cos() * 100.0;
            let x = 100.0 + (i % 5) as f32 * 50.0 + ox;
            let y = 80.0 + (i / 5) as f32 * 40.0 + oy;
            let hue = (i as f32 / n as f32 + t * 0.05) % 1.0;

            // 越界部分应被裁掉
            draw_rectangle(
                &mut child, Pos::new(x, y), 30.0, 22.0,
                Some(Color::new(
                    0.3 + hue * 0.6,
                    0.4 + (1.0 - hue) * 0.4,
                    0.5 + hue * 0.3,
                    0.9,
                )),
            );
        }
        scissor_batch.push_child(child);

        // scissor 外框标注
        draw_rect_outline(
            &mut ui, Pos::new(50.0, 50.0), 300.0, 220.0, 1.5,
            Some(Color::new(0.3, 0.7, 1.0, 0.6)),
        );
        label(&mut ui, "scissor (scissor 范围)", 50.0, 36.0);

        // ===== 对比组：无 scissor =====
        let mut no_clip = DrawBatch::new();
        no_clip.clips_children = false;

        draw_rectangle(
            &mut no_clip,
            Pos::new(500.0, 50.0), 300.0, 220.0,
            Some(Color::new(0.15, 0.16, 0.22, 0.3)),
        );
        label(&mut no_clip, "无 scissor（子内容越界可见）", 510.0, 290.0);

        let mut child2 = DrawBatch::new();
        for i in 0..n {
            let phase = t * 1.5 + i as f32 * 0.4 + 0.5;
            let ox = phase.sin() * 120.0;
            let oy = (phase * 0.8 + 1.0).cos() * 100.0;
            let x = 550.0 + (i % 5) as f32 * 50.0 + ox;
            let y = 80.0 + (i / 5) as f32 * 40.0 + oy;
            let hue = (i as f32 / n as f32 + t * 0.05 + 0.3) % 1.0;

            draw_rounded_rect(
                &mut child2, Pos::new(x, y), 30.0, 22.0, 4.0,
                Some(Color::new(
                    0.5 + hue * 0.3,
                    0.3 + (1.0 - hue) * 0.5,
                    0.4 + hue * 0.4,
                    0.9,
                )),
            );
        }
        no_clip.push_child(child2);

        // ===== 说明文字 =====
        draw_text(
            &mut ui.texts,
            "左：子 batch 色块超出 scissor 的部分被裁断    右：无 scissor，越界完全可见",
            Pos::new(20.0, 510.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.5, 0.55, 0.65, 1.0)),
        );

        win.draw(
            Some(Color::new(0.06, 0.07, 0.1, 1.0)),
            &[&ui, &scissor_batch, &no_clip],
        );
        true
    });
}
