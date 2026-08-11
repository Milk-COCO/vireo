//! 窗口控制 API 演示：1:1 封装 winit 的窗口状态命令/查询
//!
//! 键位：
//! - `D`：循环窗口边框样式 Normal → HiddenTitlebar → Frameless（set_frame_style）；
//!   无标题栏后可拖顶部「标题栏」区域拖动窗口
//! - `1`：切换可调大小（set_resizable）
//! - `2`：切换光标可见（set_cursor_visible）
//! - `3`：切换光标抓取 Locked（set_cursor_grab）
//! - `4`：切换标题栏按钮 = 仅关闭 / 全部（set_enabled_buttons）
//! - `5`：请求用户注意 Critical（request_user_attention）
//! - `6`：循环主题 None / Dark / Light（set_theme）
//! - `F`：切换全屏 Borderless
//! - `G`：光标移动到窗口中心（set_cursor_position，裸数 = vireo 逻辑像素）
//! - `R`：重置外层位置到 (80, 80)（set_outer_position，裸数 = vireo 逻辑像素）
//! - `T`：循环窗口标题（set_title）
//! - `M` / `N`：最大化 / 最小化（set_maximized / set_minimized）
//! - `H`：显示 / 隐藏（set_visible）
//! - `O`：循环 vireo 自定义 dpi 覆盖 None / 1.0 / 1.5（set_dpi_override）。
//!   `None` = vireo 逻辑即 winit 逻辑（OS 缩放参与）；`Some(v)` = vireo 全自持像素，
//!   物理 = 逻辑 × v（忽略 OS 缩放）。切换保持 vireo 逻辑尺寸、调整物理窗口大小。
//! - `7` / `8` / `9`：预设大小 300×200 / 600×400 / 900×600（set_size，vireo 逻辑像素）
//! - `I`：请求把当前逻辑尺寸缩到 0.66×（request_resize，请求尺寸并回告是否当场生效）
//! - `[` / `]`：循环 resize_increments 无 / 8px / 32px（set_resize_increments，Px 意图）
//! - `Q`：切换光标穿透（set_cursor_hittest）
//! - `C`：切换内容保护 / 模糊（set_content_protected / set_blur，仅 macOS / Wayland 生效，
//!   其它平台 no-op）
//! - `B`：刷新显示器查询（current / primary / available 数量）
//! - `K`：重置键盘 dead key 状态（reset_dead_keys，macOS/Windows）
//! - `Z`：窗口在当前显示器居中（center，vireo 自实现，跨平台）
//! - `Y` / `U`：窗口置顶 / 提到 z 序顶层（move_top / move_above，Windows
//!   `vireo::platform::windows::WindowExtWindows` 扩展方法，vireo 自实现）
//! - `P`：循环任务栏进度 None → Normal 50% → Indeterminate → Paused 30% → Error 70%
//!   （set_progress_bar）
//! - `;`：切换任务栏缩略图按钮（set_thumbar_buttons，3 个演示按钮）；
//!   点击按钮触发 `on_thumb_button(id)` 回调，HUD 显示最近点击的按钮 id
//! - `,`：切换任务栏 overlay 图标（set_overlay_icon，8×8 半透明箭头）
//! - `.`：切换任务栏 AppUserModelID（set_app_user_model_id）
//!
//! HUD 中 `PixelSize` / `PixelPos` 同时给出物理（`px`）与 vireo 逻辑（`dp`）双视图，
//! 展示统一像素 API：getter 返回双表示快照，裸数值写入默认按逻辑像素。
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
    #[cfg(target_os = "windows")]
    thumb: Option<u32>,
}

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Window Control", 960, 540), None::<fn()>);

    let st = Arc::new(Mutex::new(WinState {
        moved: None,
        theme: None,
        #[cfg(target_os = "windows")]
        thumb: None,
    }));

    let mut registered = false;
    let mut was_down: HashMap<KeyCode, bool> = HashMap::new();
    let mut resizable = true;
    let mut cursor_visible = true;
    let mut grab = false;
    let mut buttons_all = true;
    let mut theme_mode: u8 = 0; // 0=None 1=Dark 2=Light
    let mut frame_style = FrameStyle::Normal;
    let mut fullsc = false;
    let mut lb_was_down = false;
    let mut visible = true;
    let mut dpi_override: Option<f64> = None;
    let titles = ["Window API", "标题已换!", "Vireo Window"];
    let mut title_i = 0usize;
    let mut hittest = true;
    let mut inc_mode: u8 = 0; // 0=None 1=8px 2=32px
    let mut protect = false;
    let mut reqsize: Option<(u32, u32)> = None;
    let mut opacity: f64 = 1.0;
    #[cfg(target_os = "windows")]
    let mut win_enable = true;
    #[cfg(target_os = "windows")]
    let mut win_skip_taskbar = false;
    #[cfg(target_os = "windows")]
    let mut win_taskbar_icon = false;
    #[cfg(target_os = "windows")]
    let mut backdrop_mode: u8 = 0; // 0=None 1=Mica 2=Acrylic 3=Tabbed
    #[cfg(target_os = "windows")]
    let mut border_color_mode: u8 = 0; // 0=None 1=红 2=绿
    #[cfg(target_os = "windows")]
    let mut title_bg_mode: u8 = 0; // 0=None 1=深灰 2=浅灰
    #[cfg(target_os = "windows")]
    let mut title_text_mode: u8 = 0; // 0=系统 1=白 2=黑
    #[cfg(target_os = "windows")]
    let mut corner_mode: u8 = 0; // 0=Default 1=Round 2=RoundSmall 3=DoNotRound
    #[cfg(target_os = "windows")]
    let mut progress_mode: u8 = 0; // 0=None 1=Normal 2=Indeterminate 3=Paused 4=Error
    #[cfg(target_os = "windows")]
    let mut thumbar_on = false;
    #[cfg(target_os = "windows")]
    let mut overlay_on = false;
    #[cfg(target_os = "windows")]
    let mut appid_on = false;
    #[cfg(target_os = "macos")]
    let mut mac_fullscreen = false;
    #[cfg(target_os = "macos")]
    let mut mac_shadow = true;
    #[cfg(target_os = "macos")]
    let mut mac_edited = false;
    #[cfg(target_os = "macos")]
    let mut mac_game = false;
    #[cfg(target_os = "macos")]
    let mut mac_alt: u8 = 0; // 0=Both 1=OnlyLeft 2=OnlyRight

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
            #[cfg(target_os = "windows")]
            {
                let st_th = Arc::clone(&st);
                win.on_thumb_button(move |id| {
                    st_th.lock().unwrap().thumb = Some(id);
                });
            }
        }

        // 按键下降沿辅助
        let mut edge = |key: KeyCode| -> bool {
            let cur = win.key_down(key);
            let prev = was_down.get(&key).copied().unwrap_or(false);
            was_down.insert(key, cur);
            cur && !prev
        };

        if edge(KeyCode::KeyD) {
            frame_style = match frame_style {
                FrameStyle::Normal => FrameStyle::HiddenTitlebar,
                FrameStyle::HiddenTitlebar => FrameStyle::Frameless,
                FrameStyle::Frameless => FrameStyle::Normal,
            };
            win.set_frame_style(frame_style);
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
            let _ = win.set_cursor_position(480.0f64, 270.0f64);
        }
        if edge(KeyCode::KeyR) {
            win.set_outer_position(80.0f64, 80.0f64);
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
            dpi_override = match dpi_override {
                None => Some(1.0),
                Some(1.0) => Some(1.5),
                _ => None,
            };
            win.set_dpi_override(dpi_override);
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
        if edge(KeyCode::KeyI) {
            let m = win.metrics();
            reqsize = win.request_resize(
                (m.width as f64 * 0.66).max(1.0),
                (m.height as f64 * 0.66).max(1.0),
            ).map(|s| (s.width, s.height));
        }
        if edge(KeyCode::BracketLeft) || edge(KeyCode::BracketRight) {
            inc_mode = (inc_mode + 1) % 3;
            match inc_mode {
                0 => win.set_resize_increments::<f64, f64>(None),
                1 => win.set_resize_increments(Some((8.0f64, 8.0f64))),
                _ => win.set_resize_increments(Some((px(32.0), px(32.0)))),
            }
        }
        if edge(KeyCode::KeyQ) {
            hittest = !hittest;
            let _ = win.set_cursor_hittest(hittest);
        }
        if edge(KeyCode::KeyC) {
            protect = !protect;
            win.set_content_protected(protect);
            win.set_blur(protect);
        }
        if edge(KeyCode::KeyK) {
            win.reset_dead_keys();
        }
        if edge(KeyCode::KeyZ) {
            win.center();
        }
        if edge(KeyCode::KeyB) {
            opacity = match opacity {
                1.0 => 0.5,
                0.5 => 0.0,
                _ => 1.0,
            };
            win.set_opacity(opacity);
        }
        #[cfg(target_os = "windows")]
        {
            use vireo::platform::windows::{Color as WinColor, WindowExtWindows};
            if edge(KeyCode::KeyY) {
                win.move_top();
            }
            if edge(KeyCode::KeyU) {
                win.move_above();
            }
            if edge(KeyCode::KeyE) {
                win_enable = !win_enable;
                win.set_enable(win_enable);
            }
            if edge(KeyCode::KeyS) {
                win_skip_taskbar = !win_skip_taskbar;
                win.set_skip_taskbar(win_skip_taskbar);
            }
            if edge(KeyCode::KeyL) {
                win_taskbar_icon = !win_taskbar_icon;
                if win_taskbar_icon {
                    // 8×8 纯红方块，演示 set_taskbar_icon
                    let rgba = vec![255u8, 0, 0, 255].repeat(8 * 8);
                    if let Ok(icon) = winit::window::Icon::from_rgba(rgba, 8, 8) {
                        win.set_taskbar_icon(Some(icon));
                    }
                } else {
                    win.set_taskbar_icon(None);
                }
            }
            if edge(KeyCode::KeyA) {
                backdrop_mode = (backdrop_mode + 1) % 4;
                let bt = match backdrop_mode {
                    0 => vireo::platform::windows::BackdropType::None,
                    1 => vireo::platform::windows::BackdropType::MainWindow,
                    2 => vireo::platform::windows::BackdropType::TransientWindow,
                    _ => vireo::platform::windows::BackdropType::TabbedWindow,
                };
                win.set_system_backdrop(bt);
            }
            if edge(KeyCode::KeyW) {
                border_color_mode = (border_color_mode + 1) % 3;
                let c = match border_color_mode {
                    0 => None,
                    1 => Some(WinColor::from_rgb(220, 40, 40)),
                    _ => Some(WinColor::from_rgb(40, 200, 90)),
                };
                win.set_border_color(c);
            }
            if edge(KeyCode::KeyX) {
                title_bg_mode = (title_bg_mode + 1) % 3;
                let c = match title_bg_mode {
                    0 => None,
                    1 => Some(WinColor::from_rgb(30, 40, 60)),
                    _ => Some(WinColor::from_rgb(60, 40, 30)),
                };
                win.set_title_background_color(c);
            }
            if edge(KeyCode::KeyV) {
                title_text_mode = (title_text_mode + 1) % 3;
                let c = match title_text_mode {
                    0 => WinColor::SYSTEM_DEFAULT,
                    1 => WinColor::from_rgb(255, 255, 255),
                    _ => WinColor::from_rgb(10, 10, 10),
                };
                win.set_title_text_color(c);
            }
            if edge(KeyCode::KeyJ) {
                corner_mode = (corner_mode + 1) % 4;
                let cp = match corner_mode {
                    0 => vireo::platform::windows::CornerPreference::Default,
                    1 => vireo::platform::windows::CornerPreference::Round,
                    2 => vireo::platform::windows::CornerPreference::RoundSmall,
                    _ => vireo::platform::windows::CornerPreference::DoNotRound,
                };
                win.set_corner_preference(cp);
            }
            if edge(KeyCode::KeyP) {
                progress_mode = (progress_mode + 1) % 5;
                let st = match progress_mode {
                    1 => vireo::platform::windows::TaskbarProgress::Normal(0.5),
                    2 => vireo::platform::windows::TaskbarProgress::Indeterminate,
                    3 => vireo::platform::windows::TaskbarProgress::Paused(0.3),
                    4 => vireo::platform::windows::TaskbarProgress::Error(0.7),
                    _ => vireo::platform::windows::TaskbarProgress::None,
                };
                win.set_progress_bar(st);
            }
            if edge(KeyCode::Semicolon) {
                thumbar_on = !thumbar_on;
                if thumbar_on {
                    use vireo::platform::windows::{TaskbarIcon, ThumbarButton};
                    let make_icon = |r: u8, g: u8, b: u8| TaskbarIcon {
                        rgba: vec![r, g, b, 255].repeat(32 * 32),
                        width: 32,
                        height: 32,
                    };
                    let buttons = vec![
                        ThumbarButton {
                            id: 1,
                            icon: Some(make_icon(90, 200, 120)),
                            tooltip: Some("播放 (id=1)".to_string()),
                            dismiss_on_click: false,
                            disabled: false,
                            hidden: false,
                            no_background: false,
                            non_interactive: false,
                        },
                        ThumbarButton {
                            id: 2,
                            icon: Some(make_icon(220, 150, 60)),
                            tooltip: Some("暂停 (id=2)".to_string()),
                            dismiss_on_click: true,
                            disabled: false,
                            hidden: false,
                            no_background: false,
                            non_interactive: false,
                        },
                        ThumbarButton {
                            id: 3,
                            icon: Some(make_icon(200, 90, 90)),
                            tooltip: Some("停止 (id=3)".to_string()),
                            dismiss_on_click: false,
                            disabled: false,
                            hidden: false,
                            no_background: false,
                            non_interactive: false,
                        },
                    ];
                    win.set_thumbar_buttons(Some(&buttons));
                } else {
                    win.set_thumbar_buttons(None);
                }
            }
            if edge(KeyCode::Comma) {
                overlay_on = !overlay_on;
                if overlay_on {
                    use vireo::platform::windows::{TaskbarIcon, TaskbarOverlay};
                    // 8×8 半透明绿色箭头（RGBA 手动填充下三角）。
                    let mut rgba = vec![0u8; 8 * 8 * 4];
                    for y in 0..8u32 {
                        for x in 0..8u32 {
                            let idx = ((y * 8 + x) * 4) as usize;
                            let in_tri = x >= y && x < 8 - y;
                            rgba[idx] = if in_tri { 40 } else { 0 };
                            rgba[idx + 1] = if in_tri { 220 } else { 0 };
                            rgba[idx + 2] = if in_tri { 90 } else { 0 };
                            rgba[idx + 3] = if in_tri { 200 } else { 0 };
                        }
                    }
                    win.set_overlay_icon(Some(TaskbarOverlay {
                        icon: TaskbarIcon {
                            rgba,
                            width: 8,
                            height: 8,
                        },
                        description: "下载进行中".to_string(),
                    }));
                } else {
                    win.set_overlay_icon(None);
                }
            }
            if edge(KeyCode::Period) {
                appid_on = !appid_on;
                win.set_app_user_model_id(if appid_on {
                    Some("com.example.vireo.window-api")
                } else {
                    None
                });
            }
        }
        #[cfg(target_os = "macos")]
        {
            use vireo::platform::macos::{OptionAsAlt, WindowExtMacOS};
            if edge(KeyCode::KeyA) {
                mac_fullscreen = !mac_fullscreen;
                win.set_simple_fullscreen(mac_fullscreen);
            }
            if edge(KeyCode::KeyS) {
                mac_shadow = !mac_shadow;
                win.set_has_shadow(mac_shadow);
            }
            if edge(KeyCode::KeyE) {
                mac_edited = !mac_edited;
                win.set_document_edited(mac_edited);
            }
            if edge(KeyCode::KeyV) {
                mac_game = !mac_game;
                win.set_borderless_game(mac_game);
            }
            if edge(KeyCode::KeyP) {
                mac_alt = (mac_alt + 1) % 3;
                let a = match mac_alt {
                    0 => OptionAsAlt::Both,
                    1 => OptionAsAlt::OnlyLeft,
                    _ => OptionAsAlt::OnlyRight,
                };
                win.set_option_as_alt(a);
            }
        }

        // 拖「标题栏」区域（顶部 36px）拖动窗口。
        // 只能在「左键按下沿」且按下点在区域内时调用一次 drag_window()；
        // 若每帧 while 按住就调用，winit 会以当时光标位置重发 WM_NCLBUTTONDOWN，
        // 从窗口别处按住再移入区域内会瞬间把窗口吸附到鼠标。
        let (_, my) = win.mouse_pos();
        let lb_down = win.mouse_left();
        let lb_pressed = lb_down && !lb_was_down;
        if lb_pressed && my < 36.0 {
            let _ = win.drag_window();
        }
        lb_was_down = lb_down;

        // ---- 查询（直接转发 winit）----
        let is_min = win.is_minimized().unwrap_or(false);
        let is_max = win.is_maximized();
        let is_vis = win.is_visible().unwrap_or(true);
        let full = win.fullscreen().is_some();
        let outer_size = win.outer_size();
        let inner_pos = win.inner_position().map(|p| p.logical()).unwrap_or((0.0, 0.0));
        let inner_pos_px = win.inner_position().map(|p| p.physical()).unwrap_or((0.0, 0.0));
        let outer_pos = win.outer_position().map(|p| p.logical()).unwrap_or((0.0, 0.0));
        let cur_theme = win.theme();
        let cur_mon = win.current_monitor();
        let prim_mon = win.primary_monitor();
        let avail_n = win.available_monitors().count();
        let inc_q = win.resize_increments();

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
        lines.push(format!("frame={:?}  resizable={}  cursor_visible={}  grab={}", win.frame_style(), resizable, cursor_visible, grab));
        lines.push(format!("enabled_buttons={}  fullscreen={}", if buttons_all { "all" } else { "close-only" }, full));
        lines.push(format!("theme(set)={:?}  theme(query)={:?}", match theme_mode { 1 => Some(Theme::Dark), 2 => Some(Theme::Light), _ => None }, cur_theme));
        lines.push(format!("minimized={}  maximized={}  visible={}", is_min, is_max, is_vis));
        lines.push(format!(
            "inner_pos px={:.0},{:.0} dp={:.0},{:.0}  outer_pos(dp)={:?}  outer_size px={:.0}x{:.0} dp(logical)={:.0}x{:.0}",
            inner_pos_px.0, inner_pos_px.1, inner_pos.0, inner_pos.1,
            outer_pos,
            outer_size.physical().0, outer_size.physical().1,
            outer_size.logical().0, outer_size.logical().1,
        ));
        lines.push(format!("hittest={}  reqsize={:?}  resize_increments={:?}", hittest, reqsize, inc_q.map(|p| p.logical())));
        lines.push(format!("content_protected={}  monitor_current={:?}  primary={:?}  available={}", protect, cur_mon.is_some(), prim_mon.is_some(), avail_n));
        lines.push(format!("on_moved=({}, {})  on_theme_changed={:?}", moved_x, moved_y, evt_theme));
        lines.push(format!(
            "dpi_override={:?}  opacity={:.2}  metrics logical={}x{} physical={}x{} sf={:.2}",
            dpi_override,
            opacity,
            win.metrics().width,
            win.metrics().height,
            win.metrics().physical_width,
            win.metrics().physical_height,
            win.metrics().scale_factor,
        ));
        #[cfg(target_os = "windows")]
        lines.push(format!(
            "win: enable={} skip_taskbar={} taskbar_icon={} backdrop={:?} border={:?} title_bg={:?} title_text={:?} corner={:?}",
            win_enable, win_skip_taskbar, win_taskbar_icon,
            match backdrop_mode { 1 => "Mica", 2 => "Acrylic", 3 => "Tabbed", _ => "None" },
            match border_color_mode { 1 => "red", 2 => "green", _ => "None" },
            match title_bg_mode { 1 => "dark", 2 => "light", _ => "None" },
            match title_text_mode { 1 => "white", 2 => "black", _ => "system" },
            match corner_mode { 1 => "Round", 2 => "RoundSmall", 3 => "DoNotRound", _ => "Default" },
        ));
        #[cfg(target_os = "windows")]
        lines.push(format!(
            "taskbar: progress={} thumbar={} overlay={} appid={} click={:?}",
            match progress_mode { 1 => "Normal50", 2 => "Indeterminate", 3 => "Paused30", 4 => "Error70", _ => "None" },
            thumbar_on,
            overlay_on,
            appid_on,
            st.lock().unwrap().thumb,
        ));
        #[cfg(target_os = "macos")]
        lines.push(format!(
            "mac: simple_fullscreen={} shadow={} edited={} game={} alt={:?}",
            mac_fullscreen, mac_shadow, mac_edited, mac_game,
            match mac_alt { 1 => "OnlyLeft", 2 => "OnlyRight", _ => "Both" },
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
            "D 装饰 · 1 可调 · 2 光标 · 3 抓取 · 4 按钮 · 5 注意 · 6 主题 · F 全屏 · G 中心 · R 位置 · T 标题 · M/N 最大/最小 · H 显隐 · O dpi覆盖 · 7/8/9 尺寸",
            Pos::new(20.0, 438.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.6, 0.7, 0.8, 1.0)),
        );
        draw_text(
            &mut b.texts,
            "I 异步改尺寸 · [ ] resize增量 · Q 光标穿透 · C 内容保护 · B 透明度 · K dead-key重置 · Z 居中",
            Pos::new(20.0, 462.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.55, 0.65, 0.75, 1.0)),
        );
        #[cfg(target_os = "windows")]
        draw_text(
            &mut b.texts,
            "Y 置顶 · U 顶层 · E 启用 · S 跳过任务栏 · L 任务栏图标 · A 背景 · W 边框色 · X 标题栏底色 · V 标题文字色 · J 圆角 · P 进度 · ; 缩略图按钮 · , overlay · . AppID [Windows]",
            Pos::new(20.0, 486.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.55, 0.65, 0.75, 1.0)),
        );
        #[cfg(target_os = "macos")]
        draw_text(
            &mut b.texts,
            "A 简单全屏 · S 阴影 · E 已编辑 · V 无边框游戏 · P Option键 [macOS]",
            Pos::new(20.0, 486.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.55, 0.65, 0.75, 1.0)),
        );

        win.draw(Color::new(0.07, 0.08, 0.12, 1.0), &[&b]);
        true
    });
}
