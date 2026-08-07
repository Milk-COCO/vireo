//! 窗口控制示例：标题、全屏、最大化、游标等控制
//!
//! 按键盘切换窗口控制：
//!   F = 全屏  M = 最大化  N = 最小化  H = 隐藏/显示
//!   D = 无装饰  T = 标题切换  C = 游标切换
//!   数字 1-3 = 预设窗口大小

use vireo::prelude::*;
use vireo::input::KeyCode;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Window Controls", 600, 400), None::<fn()>);

    let titles = vec!["Window Controls", "Title Changed!", "Vireo Window"];
    let mut title_idx = 0usize;
    let mut decorations = true;
    let mut cursor_idx = 0usize;
    let cursor_names = ["default", "crosshair", "pointer"];

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        // 键盘控制（keys_down 当前按下的键）
        for keycode in win.input.keys_down.borrow().iter() {
            match keycode {
                KeyCode::KeyF => win.set_fullscreen(
                    if win.inner.fullscreen().is_some() { None }
                    else { Some(Fullscreen::Borderless(None)) }
                ),
                KeyCode::KeyM => win.set_maximized(!win.inner.is_maximized()),
                KeyCode::KeyN => win.set_minimized(true),
                KeyCode::KeyH => win.set_visible(!win.inner.is_visible().unwrap_or(true)),
                KeyCode::KeyD => {
                    decorations = !decorations;
                    win.set_decorations(decorations);
                }
                KeyCode::KeyT => {
                    title_idx = (title_idx + 1) % titles.len();
                    win.set_title(titles[title_idx]);
                }
                KeyCode::KeyC => {
                    cursor_idx = (cursor_idx + 1) % cursor_names.len();
                    win.set_cursor(winit::window::Cursor::Icon(winit::window::CursorIcon::Crosshair));
                }
                KeyCode::Digit1 => win.set_size(300, 200),
                KeyCode::Digit2 => win.set_size(600, 400),
                KeyCode::Digit3 => win.set_size(900, 600),
                _ => {}
            }
        }

        // 渲染帮助文本
        let mut batch = DrawBatch::new();
        let lines = [
            "F: 全屏  M: 最大化  N: 最小化  H: 显示/隐藏",
            "D: 装饰  T: 标题  C: 游标",
            "1-3: 预设大小",
            &format!("游标: {}", cursor_names[cursor_idx]),
        ];
        for (i, line) in lines.iter().enumerate() {
            draw_text(&mut batch.texts, line,
                      Pos::new(20.0, 20.0 + i as f32 * 28.0), TextDef::default().font_size(18.0), TextOverride::from_color(WHITE));
        }

        win.draw(Color::new(0.06, 0.08, 0.12, 1.0), &[&batch]);
        true
    });
}
