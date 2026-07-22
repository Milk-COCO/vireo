//! 文本测量验证：画 X 验证 bounding box 准确性
//!
//! 文本严格居中，背景框与文本尺寸完全匹配。
//! 对角线验证测量结果。

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Vireo Measure - Bounding Box", 800, 600), None::<fn()>);

    let text = "Measure Me!";
    let font_size = 48.0;

    // 预先测量一次（文本不变，尺寸不变）
    let opts = TextDef::default().font_size(font_size);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let gpu = win.gpu();
        let mut batch = DrawBatch::new();

        let (tw, th) = gpu.measure_text(text, &opts);

        // 屏幕中心（使用实际窗口逻辑尺寸）
        let cx = win.logical_width as f32 * 0.5;
        let cy = win.logical_height as f32 * 0.5;

        // 文本左上角（居中放置）
        let tx = cx - tw * 0.5;
        let ty = cy - th * 0.5;

        // 背景框
        draw_rectangle(&mut batch, Pos::new(tx, ty), tw, th, Some(Color::new(0.12, 0.12, 0.22, 1.0)));

        // 对角线 X（黄色）
        let yellow = Color::new(1.0, 1.0, 0.0, 0.6);
        draw_line(&mut batch, tx, ty, tx + tw, ty + th, 1.5, Some(yellow));        // 左上→右下
        draw_line(&mut batch, tx + tw, ty, tx, ty + th, 1.5, Some(yellow));        // 右上→左下

        // 边框
        draw_line(&mut batch, tx, ty, tx + tw, ty, 1.0, Some(Color::new(0.3, 0.3, 0.5, 1.0))); // top
        draw_line(&mut batch, tx, ty + th, tx + tw, ty + th, 1.0, Some(Color::new(0.3, 0.3, 0.5, 1.0))); // bottom
        draw_line(&mut batch, tx, ty, tx, ty + th, 1.0, Some(Color::new(0.3, 0.3, 0.5, 1.0))); // left
        draw_line(&mut batch, tx + tw, ty, tx + tw, ty + th, 1.0, Some(Color::new(0.3, 0.3, 0.5, 1.0))); // right

        // 屏幕中心十字
        draw_line(&mut batch, cx - 20.0, cy, cx + 20.0, cy, 1.0, Some(Color::new(1.0, 0.0, 0.0, 0.5)));
        draw_line(&mut batch, cx, cy - 20.0, cx, cy + 20.0, 1.0, Some(Color::new(1.0, 0.0, 0.0, 0.5)));

        // 中心点
        draw_circle(&mut batch, Pos::new(cx, cy), 3.0, Some(RED));

        // 四角标记
        draw_circle(&mut batch, Pos::new(tx, ty), 2.5, Some(Color::new(0.0, 1.0, 0.0, 0.7)));
        draw_circle(&mut batch, Pos::new(tx + tw, ty), 2.5, Some(Color::new(0.0, 1.0, 0.0, 0.7)));
        draw_circle(&mut batch, Pos::new(tx, ty + th), 2.5, Some(Color::new(0.0, 1.0, 0.0, 0.7)));
        draw_circle(&mut batch, Pos::new(tx + tw, ty + th), 2.5, Some(Color::new(0.0, 1.0, 0.0, 0.7)));

        // 文本
        draw_text(&mut batch.texts, text, Pos::new(tx, ty), TextDef::default().font_size(font_size), TextOverride::from_color(WHITE));

        // 信息
        let info = format!(
            "Text: \"{}\"\nFont: {}px\nMeasured: {:.0}x{:.0}\nBox center: ({:.0}, {:.0}) = screen center\nDiagonals should cross at center dot",
            text,
            font_size,
            tw, th,
            cx, cy,
        );
        draw_text(
            &mut batch.texts,
            &info,
            Pos::new(20.0, 20.0), TextDef::default().font_size(13.0), TextOverride::from_color(Color::new(0.5, 0.5, 0.6, 1.0)),
        );

        win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        true
    });
}
