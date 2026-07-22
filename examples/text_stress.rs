/// 文本压力测试 —— 覆盖各种极端情况
use vireo::prelude::*;

fn main() {
    let mut app = App::new();

    let idx = app.window(
        WindowDesc::new("Text Stress Test", 800, 600),
        None::<fn()>,
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let w = win.logical_width as f32;
        let h = win.logical_height as f32;

        let mut batch = DrawBatch::new();

        // 1. 原点坐标文字
        draw_text(
            &mut batch.texts,
            "坐标原点 (0, 0)",
            Pos::new(0.0, 0.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(1.0, 1.0, 1.0, 1.0)),
        );

        // 2. 右下角底边对齐
        draw_text(
            &mut batch.texts,
            "右下角底边对齐",
            Pos::new(w - 160.0, h - 2.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(1.0, 1.0, 1.0, 1.0)),
        );

        // 3. 超大字体
        draw_text(
            &mut batch.texts,
            "BIG!",
            Pos::new(w * 0.5 - 60.0, h * 0.15),
            TextDef::default().font_size(96.0),
            TextOverride::from_color(Color::new(1.0, 0.8, 0.0, 1.0)),
        );

        // 4. 中英混排
        draw_text(
            &mut batch.texts,
            "Vireo 文本渲染 Hello 世界!",
            Pos::new(20.0, 180.0),
            TextDef::default().font_size(24.0),
            TextOverride::from_color(Color::new(0.8, 0.9, 1.0, 1.0)),
        );

        // 5. 纯中文
        draw_text(
            &mut batch.texts,
            "中文测试：你好世界！",
            Pos::new(20.0, 220.0),
            TextDef::default().font_size(20.0),
            TextOverride::from_color(Color::new(0.8, 1.0, 0.8, 1.0)),
        );

        // 6. 窗口尺寸提示
        draw_text(
            &mut batch.texts,
            &format!("窗口: {} x {} (逻辑)", w as u32, h as u32),
            Pos::new(w - 220.0, 20.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.5, 0.5, 0.7, 1.0)),
        );

        // 7. 极窄换行
        draw_text(
            &mut batch.texts,
            "Narrow wrapping text box test — long words should wrap nicely here!",
            Pos::new(20.0, 270.0),
            TextDef::default().font_size(14.0).max_width(120.0),
            TextOverride::from_color(Color::new(1.0, 0.9, 0.5, 1.0)),
        );

        // 8. 微字体
        draw_text(
            &mut batch.texts,
            "tiny text 微字 (8px)",
            Pos::new(20.0, 400.0),
            TextDef::default().font_size(8.0),
            TextOverride::from_color(Color::new(0.7, 0.7, 0.7, 1.0)),
        );

        // 9. 中心绘制
        draw_text(
            &mut batch.texts,
            "Center Text 居中区域",
            Pos::new(w * 0.5 - 100.0, h * 0.55),
            TextDef::default().font_size(22.0),
            TextOverride::from_color(Color::new(1.0, 0.6, 0.8, 1.0)),
        );

        // 10. 半透明
        draw_text(
            &mut batch.texts,
            "alpha 50% (0.5) — 半透明测试",
            Pos::new(20.0, 460.0),
            TextDef::default().font_size(18.0),
            TextOverride::from_color(Color::new(1.0, 1.0, 1.0, 0.5)),
        );

        // 11. 多色文字（红绿蓝）
        draw_text(
            &mut batch.texts,
            "RED LINE 红色",
            Pos::new(500.0, 380.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(1.0, 0.2, 0.2, 1.0)),
        );
        draw_text(
            &mut batch.texts,
            "GREEN LINE 绿色",
            Pos::new(500.0, 405.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(0.2, 1.0, 0.3, 1.0)),
        );
        draw_text(
            &mut batch.texts,
            "BLUE LINE 蓝色",
            Pos::new(500.0, 430.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(0.3, 0.4, 1.0, 1.0)),
        );

        // 12. 对齐测试 —— 三个对齐框并排，用竖线标记边界
        let align_y = 530.0;
        let align_w = 200.0;
        let gap = 50.0;
        let x_left = 40.0;
        let x_center = x_left + align_w + gap;
        let x_right = x_center + align_w + gap;

        draw_text(&mut batch.texts, "Left", Pos::new(x_left + 80.0, align_y - 18.0),
                  TextDef::default().font_size(13.0),
                  TextOverride::from_color(Color::new(1.0, 0.4, 0.4, 1.0)));
        draw_text(&mut batch.texts, "Hello world!", Pos::new(x_left, align_y),
                  TextDef::default().font_size(16.0).max_width(align_w).align(TextAlign::Left),
                  TextOverride::from_color(Color::new(1.0, 1.0, 1.0, 1.0)));

        draw_text(&mut batch.texts, "Center", Pos::new(x_center + 70.0, align_y - 18.0),
                  TextDef::default().font_size(13.0),
                  TextOverride::from_color(Color::new(0.4, 1.0, 0.4, 1.0)));
        draw_text(&mut batch.texts, "Hello world!", Pos::new(x_center, align_y),
                  TextDef::default().font_size(16.0).max_width(align_w).align(TextAlign::Center),
                  TextOverride::from_color(Color::new(1.0, 1.0, 1.0, 1.0)));

        draw_text(&mut batch.texts, "Right", Pos::new(x_right + 80.0, align_y - 18.0),
                  TextDef::default().font_size(13.0),
                  TextOverride::from_color(Color::new(0.4, 0.4, 1.0, 1.0)));
        draw_text(&mut batch.texts, "Hello world!", Pos::new(x_right, align_y),
                  TextDef::default().font_size(16.0).max_width(align_w).align(TextAlign::Right),
                  TextOverride::from_color(Color::new(1.0, 1.0, 1.0, 1.0)));

        // 用竖线标记每个框的左右边界
        for (bx, color) in [
            (x_left, Color::new(1.0, 0.3, 0.3, 0.5)),
            (x_center, Color::new(0.3, 1.0, 0.3, 0.5)),
            (x_right, Color::new(0.3, 0.3, 1.0, 0.5)),
        ] {
            draw_line(&mut batch, bx, align_y - 5.0, bx, align_y + 22.0, 1.0, Some(color));
            draw_line(&mut batch, bx + align_w, align_y - 5.0, bx + align_w, align_y + 22.0, 1.0, Some(color));
        }

        // 背景参考线（帮助判断文字位置）
        draw_line(&mut batch, 0.0, 0.0, w, 0.0, 1.0, Some(Color::new(0.3, 0.3, 0.3, 1.0)));
        draw_line(&mut batch, 0.0, 0.0, 0.0, h, 1.0, Some(Color::new(0.3, 0.3, 0.3, 1.0)));
        draw_line(&mut batch, 0.0, h, w, h, 1.0, Some(Color::new(0.3, 0.3, 0.3, 1.0)));
        draw_line(&mut batch, w, 0.0, w, h, 1.0, Some(Color::new(0.3, 0.3, 0.3, 1.0)));

        win.draw(Some(Color::new(0.08, 0.1, 0.14, 1.0)), &[&batch]);

        true
    });
}
