//! 输入事件示例：展示状态轮询 + 事件订阅两种模式
//!
//! - 用方向键移动一个彩色方块
//! - 用鼠标滚轮改变方块大小
//! - 鼠标左键点击随机换色
//! - 按下 ESC 退出
//! - 左侧面板显示当前输入状态

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Vireo Input Demo", 800, 600), None::<fn()>);

    // 方块状态
    let mut x: f32 = 350.0;
    let mut y: f32 = 250.0;
    let mut size: f32 = 60.0;
    let mut color = BLUE;
    let mut click_count: u32 = 0;
    let mut scroll_log: f32 = 0.0;

    // 防止重复点击（仅在第1帧标识一次mouse_left按下才算一次点击）
    let mut mouse_was_down = false;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        // ------ 状态轮询 ------

        // WASD/方向键移动方块
        if win.key_down(KeyCode::ArrowLeft) || win.key_down(KeyCode::KeyA) {
            x -= 3.0;
        }
        if win.key_down(KeyCode::ArrowRight) || win.key_down(KeyCode::KeyD) {
            x += 3.0;
        }
        if win.key_down(KeyCode::ArrowUp) || win.key_down(KeyCode::KeyW) {
            y -= 3.0;
        }
        if win.key_down(KeyCode::ArrowDown) || win.key_down(KeyCode::KeyS) {
            y += 3.0;
        }

        // 滚轮改变大小
        let (_sx, sy) = win.take_scroll();
        scroll_log += sy;
        size = (size + sy * 2.0).clamp(10.0, 300.0);

        // 鼠标点击（边缘检测，每帧只算一次）
        let mouse_left = win.mouse_left();
        if mouse_left && !mouse_was_down {
            let (mx, my) = win.mouse_pos();
            if mx >= x && mx <= x + size && my >= y && my <= y + size {
                click_count += 1;
                // 随机换色
                color = match click_count % 5 {
                    0 => RED,
                    1 => GREEN,
                    2 => BLUE,
                    3 => Color::new(1.0, 1.0, 0.0, 1.0), // 黄
                    _ => Color::new(1.0, 0.0, 1.0, 1.0), // 紫
                };
            }
        }
        mouse_was_down = mouse_left;

        // Ctrl+Q 或 ESC 退出
        if win.key_down(KeyCode::Escape) || (win.ctrl_down() && win.key_down(KeyCode::KeyQ)) {
            return false;
        }

        // 边界裁剪
        x = x.clamp(0.0, 800.0 - size);
        y = y.clamp(0.0, 600.0 - size);

        // ------ 渲染 ------
        let mut batch = DrawBatch::new();

        // 面板背景
        draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 200.0, 600.0, Some(Color::new(0.1, 0.1, 0.15, 1.0)));

        // 方块
        draw_rectangle(&mut batch, Pos::new(x, y), size, size, Some(color));

        // 面板文字
        let info = format!(
            "Position: ({:.0}, {:.0})\nSize: {:.0}\nScroll: {:.1}\nClicks: {}\n\nKeys:\n  W=up S=down\n  A=left D=right\n  Arrows also work\n\n  Left click → color\n  Scroll → size\n  Ctrl+Q / ESC → exit\n\nCtrl: {}\nShift: {}\nAlt: {}",
            x, y,
            size,
            scroll_log,
            click_count,
            if win.ctrl_down() { "YES" } else { "no" },
            if win.shift_down() { "YES" } else { "no" },
            if win.alt_down() { "YES" } else { "no" },
        );

        draw_text(
            &mut batch.texts,
            &info,
            Pos::new(10.0, 10.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.8, 0.8, 0.9, 1.0)),
        );

        // 焦点状态
        let focus_text = if win.focused() {
            "Window focused"
        } else {
            "Window NOT focused"
        };
        let focus_color = if win.focused() {
            Color::new(0.0, 0.8, 0.0, 1.0)
        } else {
            Color::new(0.8, 0.0, 0.0, 1.0)
        };
        draw_text(
            &mut batch.texts,
            focus_text,
            Pos::new(10.0, 560.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(focus_color),
        );

        win.draw(BLACK, &[&batch]);
        true
    });
}
