//! 文本裁剪与对齐：`TextOverride::clip` + Justified / End / max_width

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Text Clip & Align", 720, 420).high_dpi(true), None::<fn()>);

    let mut t: f32 = 0.0;
    let long = "The quick brown fox jumps over the lazy dog. 裁剪区外不可见 — clip(left,top,right,bottom).";

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.8;
        let mut batch = DrawBatch::new();

        draw_text(
            &mut batch.texts,
            "clip + TextAlign (Left / Center / Right / End / Justified)",
            Pos::new(20.0, 16.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(Color::new(0.7, 0.75, 0.85, 1.0)),
        );

        // 裁剪框 + 横向滚动文字
        let clip = (40i32, 60, 360, 120);
        draw_rect_outline(&mut batch, Pos::new(clip.0 as f32, clip.1 as f32), (clip.2 - clip.0) as f32, (clip.3 - clip.1) as f32, 1.5, Some(Color::new(0.4, 0.5, 0.7, 1.0)));
        let scroll_x = 40.0 - (t % 400.0);
        draw_text(
            &mut batch.texts,
            long,
            Pos::new(scroll_x, 78.0),
            TextDef::default().font_size(18.0),
            TextOverride::from_color(WHITE).clip(clip.0, clip.1, clip.2, clip.3),
        );
        draw_text(
            &mut batch.texts,
            "clip marquee",
            Pos::new(40.0, 128.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(Color::new(0.5, 0.55, 0.65, 1.0)),
        );

        // 对齐演示
        let box_x = 40.0;
        let box_w = 640.0;
        let sample = "Align demo — 对齐测试 max_width";
        let aligns = [
            (TextAlign::Left, "Left", 170.0),
            (TextAlign::Center, "Center", 220.0),
            (TextAlign::Right, "Right", 270.0),
            (TextAlign::End, "End", 320.0),
            (TextAlign::Justified, "Justified (needs wrap width)", 370.0),
        ];
        draw_rect_outline(&mut batch, Pos::new(box_x, 160.0), box_w, 230.0, 1.0, Some(Color::new(0.3, 0.35, 0.45, 1.0)));

        for (align, label, y) in aligns {
            draw_text(
                &mut batch.texts,
                label,
                Pos::new(box_x + 8.0, y - 18.0),
                TextDef::default().font_size(11.0),
                TextOverride::from_color(GOLD),
            );
            draw_text(
                &mut batch.texts,
                sample,
                Pos::new(box_x, y),
                TextDef::default()
                    .font_size(16.0)
                    .max_width(box_w)
                    .align(align),
                TextOverride::from_color(WHITE),
            );
        }

        win.draw(Color::new(0.06, 0.07, 0.1, 1.0), &[&batch]);
        true
    });
}
