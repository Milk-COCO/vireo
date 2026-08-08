//! IME 输入法 + 文件拖放演示
//!
//! `on_ime` 回调（1:1 封装 winit `Ime`）+ `set_ime_allowed` / `set_ime_cursor_area`
//! 命令。拖放演示 `on_file_dropped` / `on_file_hovered` / `on_file_hover_cancelled`。
//!
//! 操作：
//! - `E` 键：切换 IME 开关
//! - 点击窗口放置文本光标（`set_ime_cursor_area` 候选窗跟随）
//! - 中文/日文输入法可见 preedit 组词过程；无 IME 时 Commit 单字直接上屏
//! - 从系统拖文件进窗口：显示 hovered / dropped / cancelled 事件
//!
//! ```bash
//! cargo run --example ime_input
//! ```

use std::sync::{Arc, Mutex};
use vireo::prelude::*;

struct ImeState {
    committed: String,
    preedit: String,
    cursor: Option<(usize, usize)>,
    enabled: bool,
    hovered: Option<String>,
    dropped: Vec<String>,
}

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("IME Input + Drag Drop", 800, 560), None::<fn()>);

    let st = Arc::new(Mutex::new(ImeState {
        committed: String::new(),
        preedit: String::new(),
        cursor: None,
        enabled: false,
        hovered: None,
        dropped: Vec::new(),
    }));

    let mut registered = false;
    let mut last_e = false;
    let mut last_mouse_down = false;
    let mut text_cursor = (400.0f32, 260.0f32);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        if !registered {
            registered = true;
            // 首次：开 IME + 设置候选窗跟随区域（1:1 转发 winit，线程安全）
            win.set_ime_allowed(true);
            st.lock().unwrap().enabled = true;
            win.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(text_cursor.0 as f64, text_cursor.1 as f64),
                winit::dpi::LogicalSize::new(2.0, 26.0),
            );

            let st_cb = Arc::clone(&st);
            win.on_ime(move |ev| {
                let mut g = st_cb.lock().unwrap();
                match ev {
                    Ime::Preedit(text, cursor) => {
                        g.preedit = text.clone();
                        g.cursor = *cursor;
                    }
                    Ime::Commit(text) => {
                        g.committed.push_str(text);
                        g.preedit.clear();
                        g.cursor = None;
                    }
                    Ime::Enabled => g.enabled = true,
                    Ime::Disabled => g.enabled = false,
                }
            });

            let st_drop = Arc::clone(&st);
            win.on_file_dropped(move |path| {
                let mut g = st_drop.lock().unwrap();
                g.dropped.push(path.display().to_string());
                if g.dropped.len() > 20 {
                    g.dropped.remove(0);
                }
            });
            let st_hov = Arc::clone(&st);
            win.on_file_hovered(move |path| {
                st_hov.lock().unwrap().hovered = Some(path.display().to_string());
            });
            let st_hov_c = Arc::clone(&st);
            win.on_file_hover_cancelled(move || {
                st_hov_c.lock().unwrap().hovered = None;
            });
        }

        // E 键下降沿切换 IME 开关
        let e_down = win.key_down(KeyCode::KeyE);
        if e_down && !last_e {
            let mut g = st.lock().unwrap();
            g.enabled = !g.enabled;
            win.set_ime_allowed(g.enabled);
        }
        last_e = e_down;

        // 点击放置文本光标 → 候选窗跟随（仅按下瞬间更新）
        let mouse_down = win.mouse_left();
        if mouse_down && !last_mouse_down {
            let (mx, my) = win.mouse_pos();
            text_cursor = (mx, my);
            win.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(text_cursor.0 as f64, text_cursor.1 as f64),
                winit::dpi::LogicalSize::new(2.0, 26.0),
            );
        }
        last_mouse_down = mouse_down;

        // ---- 绘制 ----
        let mut b = DrawBatch::new();
        let g = st.lock().unwrap();

        draw_text(
            &mut b.texts,
            "已输入（Commit）",
            Pos::new(40.0, 180.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(Color::new(0.7, 0.75, 0.85, 1.0)),
        );
        let committed_disp = if g.committed.is_empty() {
            "（空，点击窗口后用输入法打字试试）".to_string()
        } else {
            g.committed.clone()
        };
        draw_text(
            &mut b.texts,
            &committed_disp,
            Pos::new(40.0, 208.0),
            TextDef::default().font_size(22.0),
            TextOverride::from_color(Color::new(0.95, 0.95, 1.0, 1.0)),
        );

        draw_text(
            &mut b.texts,
            "组词（Preedit）",
            Pos::new(40.0, 250.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(Color::new(0.7, 0.75, 0.85, 1.0)),
        );
        let preedit_disp = if g.preedit.is_empty() {
            "（无）".to_string()
        } else {
            g.preedit.clone()
        };
        draw_text(
            &mut b.texts,
            &preedit_disp,
            Pos::new(40.0, 278.0),
            TextDef::default().font_size(22.0),
            TextOverride::from_color(Color::new(1.0, 0.88, 0.4, 1.0)),
        );
        let cursor_txt = format!("preedit 光标（byte 区间）: {:?}", g.cursor);
        draw_text(
            &mut b.texts,
            &cursor_txt,
            Pos::new(40.0, 312.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.6, 0.65, 0.75, 1.0)),
        );

        draw_text(
            &mut b.texts,
            "拖放",
            Pos::new(40.0, 360.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(Color::new(0.7, 0.75, 0.85, 1.0)),
        );
        if let Some(h) = &g.hovered {
            draw_text(
                &mut b.texts,
                &format!("悬停: {}", h),
                Pos::new(40.0, 388.0),
                TextDef::default().font_size(14.0),
                TextOverride::from_color(Color::new(0.6, 0.85, 0.6, 1.0)),
            );
        }
        if let Some(last) = g.dropped.last() {
            draw_text(
                &mut b.texts,
                &format!("放下: {}", last),
                Pos::new(40.0, 414.0),
                TextDef::default().font_size(14.0),
                TextOverride::from_color(Color::new(0.6, 0.75, 0.9, 1.0)),
            );
        }

        let enabled = g.enabled;
        drop(g);

        // 文本光标竖线
        b.set_color(Color::new(0.9, 0.95, 1.0, 0.9));
        draw_line(
            &mut b,
            text_cursor.0,
            text_cursor.1 - 13.0,
            text_cursor.0,
            text_cursor.1 + 13.0,
            2.0,
            None,
        );

        // HUD
        let mut ui = DrawBatch::new();
        let ime_state = if enabled { "IME: ON" } else { "IME: OFF" };
        let hud = format!("{ime_state} · E 键切换 · 点击窗口放置光标 · 拖文件进窗口");
        draw_text(
            &mut ui.texts,
            &hud,
            Pos::new(16.0, 12.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.9, 0.95, 1.0, 1.0)),
        );

        win.draw(Color::new(0.06, 0.07, 0.1, 1.0), &[&ui, &b]);
        true
    });
}
