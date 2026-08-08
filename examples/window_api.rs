//! 窗口控制 API 演示：1:1 封装 winit 的窗口状态命令/查询
//!
//! 键位：
//! - `D`：切换系统装饰（set_decorations）；无边框后可拖顶部「标题栏」区域拖动窗口
//! - `1`：切换可调大小（set_resizable）
//! - `2`：切换光标可见（set_cursor_visible）
//! - `3`：切换光标抓取 Locked（set_cursor_grab）
//! - `4`：切换标题栏按钮 = 仅关闭 / 全部（set_enabled_buttons）
//! - `5`：请求用户注意 Critical（request_user_attention）
//! - `6`：循环主题 None / Dark / Light（set_theme）
//! - `F`：切换全屏 Borderless
//! - `G`：光标移动到窗口中心（set_cursor_position）
//! - `R`：重置外层位置到 (80, 80)（set_outer_position）
//! - `T`：循环窗口标题（set_title）
//! - `M` / `N`：最大化 / 最小化（set_maximized / set_minimized）
//! - `H`：显示 / 隐藏（set_visible）
//! - `O`：切换 vireo 封装 high_dpi（逻辑 = 物理，set_high_dpi）
//! - `7` / `8` / `9`：预设大小 300×200 / 600×400 / 900×600（set_size，逻辑像素）
//!
//! ```bash
//! cargo run --example window_api
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use vireo::prelude::*;

struct WinState {
    moved: Option<(i32, i32)>,
    theme: Option<Theme>,
}

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Window Control", 960, 540), None::<fn()>);

    let st = Arc::new(Mutex::new(WinState {
        moved: None,
        theme: None,
    }));

    let mut registered = false;
    let mut was_down: HashMap<KeyCode, bool> = HashMap::new();
    let mut resizable = true;
    let mut cursor_visible = true;
    let mut grab = false;
    let mut buttons_all = true;
    let mut theme_mode: u8 = 0; // 0=None 1=Dark 2=Light
    let mut decorated = true;
    let mut fullsc = false;
    let mut visible = true;
    let mut high_dpi = false;
    let titles = ["Window API", "标题已换!", "Vireo Window"];
    let mut title_i = 0usize;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        if !registered {
            registered = true;
            let st_m = Arc::clone(&st);
            win.on_moved(move |pos| {
                st_m.lock().unwrap().moved = Some((pos.x, pos.y));
            });
            let st_t = Arc::clone(&st);
            win.on_theme_changed(move |theme| {
                st_t.lock().unwrap().theme = Some(theme);
            });
        }

        // 按键下降沿辅助
        let mut edge = |key: KeyCode| -> bool {
            let cur = win.key_down(key);
            let prev = was_down.get(&key).copied().unwrap_or(false);
            was_down.insert(key, cur);
            cur && !prev
        };

        if edge(KeyCode::KeyD) {
            decorated = !decorated;
            win.set_decorations(decorated);
        }
        if edge(KeyCode::Digit1) {
            resizable = !resizable;
            win.set_resizable(resizable);
        }
        if edge(KeyCode::Digit2) {
            cursor_visible = !cursor_visible;
            win.set_cursor_visible(cursor_visible);
        }
        if edge(KeyCode::Digit3) {
            grab = !grab;
            let _ = win.set_cursor_grab(if grab { CursorGrabMode::Locked } else { CursorGrabMode::None });
        }
        if edge(KeyCode::Digit4) {
            buttons_all = !buttons_all;
            win.set_enabled_buttons(if buttons_all { WindowButtons::all() } else { WindowButtons::CLOSE });
        }
        if edge(KeyCode::Digit5) {
            win.request_user_attention(Some(UserAttentionType::Critical));
        }
        if edge(KeyCode::Digit6) {
            theme_mode = (theme_mode + 1) % 3;
            let t = match theme_mode {
                1 => Some(Theme::Dark),
                2 => Some(Theme::Light),
                _ => None,
            };
            win.set_theme(t);
        }
        if edge(KeyCode::KeyF) {
            fullsc = !fullsc;
            let _ = win.set_fullscreen(if fullsc {
                Some(Fullscreen::Borderless(None))
            } else {
                None
            });
        }
        if edge(KeyCode::KeyG) {
            let _ = win.set_cursor_position(winit::dpi::LogicalPosition::new(480.0, 270.0));
        }
        if edge(KeyCode::KeyR) {
            win.set_outer_position(winit::dpi::LogicalPosition::new(80.0, 80.0));
        }
        if edge(KeyCode::KeyT) {
            title_i = (title_i + 1) % titles.len();
            win.set_title(titles[title_i]);
        }
        if edge(KeyCode::KeyM) {
            win.set_maximized(!win.is_maximized());
        }
        if edge(KeyCode::KeyN) {
            win.set_minimized(true);
        }
        if edge(KeyCode::KeyH) {
            visible = !visible;
            win.set_visible(visible);
        }
        if edge(KeyCode::KeyO) {
            high_dpi = !high_dpi;
            win.set_high_dpi(high_dpi);
        }
        if edge(KeyCode::Digit7) {
            win.set_size(300, 200);
        }
        if edge(KeyCode::Digit8) {
            win.set_size(600, 400);
        }
        if edge(KeyCode::Digit9) {
            win.set_size(900, 600);
        }

        // 拖「标题栏」区域（顶部 36px）拖动窗口
        let (_, my) = win.mouse_pos();
        let lb_down = win.mouse_left();
        if lb_down && my < 36.0 {
            let _ = win.drag_window();
        }

        // ---- 查询（直接转发 winit）----
        let is_min = win.is_minimized().unwrap_or(false);
        let is_max = win.is_maximized();
        let is_vis = win.is_visible().unwrap_or(true);
        let is_dec = win.is_decorated();
        let full = win.fullscreen().is_some();
        let outer_size = win.outer_size();
        let inner_pos = win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let outer_pos = win.outer_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let cur_theme = win.theme();

        let st_guard = st.lock().unwrap();
        let (moved_x, moved_y) = st_guard.moved.unwrap_or((0, 0));
        let evt_theme = st_guard.theme;
        drop(st_guard);

        // ---- 绘制 ----
        let mut b = DrawBatch::new();

        // 模拟标题栏
        b.set_color(Color::new(0.16, 0.18, 0.24, 1.0));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 960.0, 36.0, None);
        draw_text(
            &mut b.texts,
            "拖这里拖动窗口（drag_window）",
            Pos::new(12.0, 10.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.85, 0.9, 1.0, 1.0)),
        );

        let mut lines = Vec::new();
        lines.push(format!("decorated={}  resizable={}  cursor_visible={}  grab={}", is_dec, resizable, cursor_visible, grab));
        lines.push(format!("enabled_buttons={}  fullscreen={}", if buttons_all { "all" } else { "close-only" }, full));
        lines.push(format!("theme(set)={:?}  theme(query)={:?}", match theme_mode { 1 => Some(Theme::Dark), 2 => Some(Theme::Light), _ => None }, cur_theme));
        lines.push(format!("minimized={}  maximized={}  visible={}", is_min, is_max, is_vis));
        lines.push(format!("inner_pos={:?}  outer_pos={:?}  outer_size={}x{}", inner_pos, outer_pos, outer_size.width, outer_size.height));
        lines.push(format!("on_moved=({}, {})  on_theme_changed={:?}", moved_x, moved_y, evt_theme));
        lines.push(format!(
            "high_dpi={}  metrics logical={}x{} physical={}x{} sf={:.2}",
            high_dpi,
            win.metrics().width,
            win.metrics().height,
            win.metrics().physical_width,
            win.metrics().physical_height,
            win.metrics().scale_factor,
        ));

        let mut y = 60.0f32;
        for line in lines {
            draw_text(
                &mut b.texts,
                &line,
                Pos::new(20.0, y),
                TextDef::default().font_size(16.0),
                TextOverride::from_color(Color::new(0.9, 0.95, 1.0, 1.0)),
            );
            y += 28.0;
        }

        draw_text(
            &mut b.texts,
            "D 装饰 · 1 可调 · 2 光标 · 3 抓取 · 4 按钮 · 5 注意 · 6 主题 · F 全屏 · G 光标中心 · R 位置 · T 标题 · M/N 最大/最小 · H 显隐 · O high_dpi · 7/8/9 尺寸",
            Pos::new(20.0, 500.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.6, 0.7, 0.8, 1.0)),
        );

        win.draw(Color::new(0.07, 0.08, 0.12, 1.0), &[&b]);
        true
    });
}
