/// 文本压力测试 —— 覆盖各种极端情况
///
/// 预期效果（逐项对照）：
///
/// 1. 左上角 "坐标原点 (0,0)"         — 紧贴窗口左上角，文字内容即为 "(0, 0)"
/// 2. 右下角 "右下角底边对齐"          — 文字基线贴近窗口底边，不超出窗口
/// 3. 超大字体 "BIG!" (96px)          — 巨大字符占据中部，不裁剪，完整显示
/// 4. 中英混排 "Vireo 文本渲染 Hello 世界!" (24px) — 英文和中文切换流畅，字号一致不跳动
/// 5. 纯中文 "中文测试：你好世界！" (20px)          — 汉字清晰，间距均匀
/// 6. 窗口右下角 "(逻辑宽度, 逻辑高度)"  — 显示当前窗口的逻辑尺寸
/// 7. 极窄换行 "Narrow wrapping text box" (max_width=120) — 文字在窄框内自动换行，不溢出
/// 8. 微字体 "tiny text 微字" (8px)    — 极小但清晰可辨
/// 9. 中心绘制 "Center Text 居中区域"  — 在屏幕大致中心位置显示
/// 10. 半透明 "alpha 50% (0.5)"       — 白色文字但明显半透明，能看到后面内容透出
/// 11. 多色文字 — 红绿蓝三行          — 三行分别是红、绿、蓝
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
            TextOptions {
                x: 0.0,
                y: 0.0,
                font_size: 14.0,
                color: Color::new(1.0, 1.0, 1.0, 1.0),
                ..TextOptions::default()
            },
        );

        // 2. 右下角底边对齐
        draw_text(
            &mut batch.texts,
            "右下角底边对齐",
            TextOptions {
                x: w - 160.0,
                y: h - 2.0,
                font_size: 16.0,
                color: Color::new(1.0, 1.0, 1.0, 1.0),
                ..TextOptions::default()
            },
        );

        // 3. 超大字体
        draw_text(
            &mut batch.texts,
            "BIG!",
            TextOptions {
                x: w * 0.5 - 60.0,
                y: h * 0.15,
                font_size: 96.0,
                color: Color::new(1.0, 0.8, 0.0, 1.0),
                ..TextOptions::default()
            },
        );

        // 4. 中英混排
        draw_text(
            &mut batch.texts,
            "Vireo 文本渲染 Hello 世界!",
            TextOptions {
                x: 20.0,
                y: 180.0,
                font_size: 24.0,
                color: Color::new(0.8, 0.9, 1.0, 1.0),
                ..TextOptions::default()
            },
        );

        // 5. 纯中文
        draw_text(
            &mut batch.texts,
            "中文测试：你好世界！",
            TextOptions {
                x: 20.0,
                y: 220.0,
                font_size: 20.0,
                color: Color::new(0.8, 1.0, 0.8, 1.0),
                ..TextOptions::default()
            },
        );

        // 6. 窗口尺寸提示
        draw_text(
            &mut batch.texts,
            &format!("窗口: {} x {} (逻辑)", w as u32, h as u32),
            TextOptions {
                x: w - 220.0,
                y: 20.0,
                font_size: 13.0,
                color: Color::new(0.5, 0.5, 0.7, 1.0),
                ..TextOptions::default()
            },
        );

        // 7. 极窄换行
        draw_text(
            &mut batch.texts,
            "Narrow wrapping text box test — long words should wrap nicely here!",
            TextOptions {
                x: 20.0,
                y: 270.0,
                font_size: 14.0,
                color: Color::new(1.0, 0.9, 0.5, 1.0),
                max_width: Some(120.0),
                ..TextOptions::default()
            },
        );

        // 8. 微字体
        draw_text(
            &mut batch.texts,
            "tiny text 微字 (8px)",
            TextOptions {
                x: 20.0,
                y: 400.0,
                font_size: 8.0,
                color: Color::new(0.7, 0.7, 0.7, 1.0),
                ..TextOptions::default()
            },
        );

        // 9. 中心绘制
        draw_text(
            &mut batch.texts,
            "Center Text 居中区域",
            TextOptions {
                x: w * 0.5 - 100.0,
                y: h * 0.55,
                font_size: 22.0,
                color: Color::new(1.0, 0.6, 0.8, 1.0),
                ..TextOptions::default()
            },
        );

        // 10. 半透明
        draw_text(
            &mut batch.texts,
            "alpha 50% (0.5) — 半透明测试",
            TextOptions {
                x: 20.0,
                y: 460.0,
                font_size: 18.0,
                color: Color::new(1.0, 1.0, 1.0, 0.5),
                ..TextOptions::default()
            },
        );

        // 11. 多色文字（红绿蓝）
        draw_text(
            &mut batch.texts,
            "RED LINE 红色",
            TextOptions {
                x: 500.0,
                y: 380.0,
                font_size: 16.0,
                color: Color::new(1.0, 0.2, 0.2, 1.0),
                ..TextOptions::default()
            },
        );
        draw_text(
            &mut batch.texts,
            "GREEN LINE 绿色",
            TextOptions {
                x: 500.0,
                y: 405.0,
                font_size: 16.0,
                color: Color::new(0.2, 1.0, 0.3, 1.0),
                ..TextOptions::default()
            },
        );
        draw_text(
            &mut batch.texts,
            "BLUE LINE 蓝色",
            TextOptions {
                x: 500.0,
                y: 430.0,
                font_size: 16.0,
                color: Color::new(0.3, 0.4, 1.0, 1.0),
                ..TextOptions::default()
            },
        );

        // 12. 对齐测试 —— 三个对齐框并排，用竖线标记边界
        let align_y = 530.0;
        let align_w = 200.0;
        let gap = 50.0;
        let x_left = 40.0;
        let x_center = x_left + align_w + gap;
        let x_right = x_center + align_w + gap;

        draw_text(&mut batch.texts, "Left", TextOptions {
            x: x_left + 80.0, y: align_y - 18.0, font_size: 13.0,
            color: Color::new(1.0, 0.4, 0.4, 1.0),
            ..TextOptions::default()
        });
        draw_text(&mut batch.texts, "Hello world!", TextOptions {
            x: x_left, y: align_y, font_size: 16.0,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
            max_width: Some(align_w), align: TextAlign::Left,
            ..TextOptions::default()
        });

        draw_text(&mut batch.texts, "Center", TextOptions {
            x: x_center + 70.0, y: align_y - 18.0, font_size: 13.0,
            color: Color::new(0.4, 1.0, 0.4, 1.0),
            ..TextOptions::default()
        });
        draw_text(&mut batch.texts, "Hello world!", TextOptions {
            x: x_center, y: align_y, font_size: 16.0,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
            max_width: Some(align_w), align: TextAlign::Center,
            ..TextOptions::default()
        });

        draw_text(&mut batch.texts, "Right", TextOptions {
            x: x_right + 80.0, y: align_y - 18.0, font_size: 13.0,
            color: Color::new(0.4, 0.4, 1.0, 1.0),
            ..TextOptions::default()
        });
        draw_text(&mut batch.texts, "Hello world!", TextOptions {
            x: x_right, y: align_y, font_size: 16.0,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
            max_width: Some(align_w), align: TextAlign::Right,
            ..TextOptions::default()
        });

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
