//! 窗口控制 API 演示：1:1 封装 winit 的窗口状态命令/查询
//!
//! HUD 按类别分组展示当前窗口状态，布局随窗口尺寸自适应（不再硬编码坐标）。
//! 各类可编辑属性均有对应按键，键位在底部提示行按类别排列。
//!
//! ## 通用键位
//! 状态：`D` 装饰 · `1` 可调 · `2` 光标可见 · `3` 抓取 · `4` 标题栏按钮 ·
//! `5` 注意 · `6` 主题 · `F` 全屏 · `M`/`N` 最大/最小 · `H` 显隐 · `T` 标题
//! 尺寸：`G` 光标到中心 · `R` 外层位置 · `7`/`8`/`9` 预设尺寸 · `I` 异步改尺寸 ·
//! `O` dpi 覆盖 · `-` 最小尺寸 · `=` 最大尺寸 · `[`/`]` resize 增量 · `Z` 居中
//! 其它：`Q` 光标穿透 · `C` 内容保护/模糊 · `B` 透明度 · `K` dead-key 重置 ·
//! `0` 窗口层级 · `\` 透明 · `'` 运行期图标 · `Enter` 拖边缩放 ·
//! `F1` 可聚焦 · `F2` 宽高比
//!
//! ## 平台专属键位
//! Windows（`vireo::platform::windows::WindowExtWindows`）：`Y` 置顶 · `U` 顶层 ·
//! `E` 启用 · `S` 跳过任务栏 · `L` 任务栏图标 · `A` 背景 · `W` 边框色 ·
//! `X` 标题栏底色 · `V` 标题文字色 · `J` 圆角 · `P` 任务栏进度 ·
//! `;` 缩略图按钮 · `,` overlay 图标 · `.` AppUserModelID · `` ` `` 无边框阴影
//! macOS（`vireo::platform::macos::WindowExtMacOS`）：`A` 简单全屏 · `S` 阴影 ·
//! `E` 已编辑 · `V` 无边框游戏 · `P` Option 键
//!
//! ## 渲染/性能属性（有专门示例，本示例不重复）
//! - present mode / frame latency / max fps / MSAA → [`frame_stats`](crate::frame_stats)（另有 `window_present` / `window_aa` / `msaa_clamp`）
//! - resize 刷新策略 / debounce / layout follow / smoothing → [`window_resize`](crate::window_resize) / [`layout_follow`](crate::layout_follow)
//! - IME → [`input_ime`](crate::input_ime)；自定义光标 → [`window_create`](crate::window_create)
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

const ROW_H: f32 = 20.0;
const GROUP_H: f32 = 26.0;
const GROUP_GAP: f32 = 8.0;
const NAME_W: f32 = 158.0;

fn group(b: &mut DrawBatch, x: f32, y: f32, in_view: bool, name: &str) -> f32 {
    if in_view {
        draw_text(
            &mut b.texts,
            name,
            Pos::new(x, y),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.42, 0.82, 0.95, 1.0)),
        );
    }
    GROUP_H
}

fn row(b: &mut DrawBatch, x: f32, y: f32, in_view: bool, name: &str, value: &str) -> f32 {
    if in_view {
        draw_text(
            &mut b.texts,
            name,
            Pos::new(x, y),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.55, 0.62, 0.72, 1.0)),
        );
        draw_text(
            &mut b.texts,
            value,
            Pos::new(x + NAME_W, y),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.92, 0.95, 1.0, 1.0)),
        );
    }
    ROW_H
}

fn hint(b: &mut DrawBatch, x: f32, y: f32, text: &str) {
    draw_text(
        &mut b.texts,
        text,
        Pos::new(x, y),
        TextDef::default().font_size(12.0),
        TextOverride::from_color(Color::new(0.55, 0.65, 0.75, 1.0)),
    );
}

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Window Control", 960, 600), None::<fn()>);

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
    let mut focusable_enabled = true;
    let mut aspect_ratio: Option<f64> = None;
    let mut grab = false;
    let mut buttons_all = true;
    let mut theme_mode: u8 = 0; // 0=None 1=Dark 2=Light
    let mut frame_style = FrameStyle::Normal;
    let mut fullsc = false;
    let mut lb_was_down = false;
    let mut visible = true;
    let mut hidden_at: Option<std::time::Instant> = None;
    let mut dpi_override: Option<f64> = None;
    let titles = ["Window API", "标题已换!", "Vireo Window"];
    let mut title_i = 0usize;
    let mut hittest = true;
    let mut inc_mode: u8 = 0; // 0=None 1=8px 2=32px
    let mut protect = false;
    let mut reqsize: Option<(u32, u32)> = None;
    let mut opacity: f64 = 1.0;
    let mut min_mode: u8 = 0; // 0=None 1=400x300 2=800x600
    let mut max_mode: u8 = 0; // 0=None 1=1280x800 2=1920x1080
    let mut level_mode: u8 = 0; // 0=Normal 1=AlwaysOnTop 2=AlwaysOnBottom
    let mut transparent = false;
    let mut icon_mode: u8 = 0; // 0=红 1=绿
    let mut drag_dir: u8 = 0; // 0=East 1=South 2=West 3=North
    let mut last_attention = false;
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
        if edge(KeyCode::F1) {
            focusable_enabled = !focusable_enabled;
            win.set_focusable(focusable_enabled);
        }
        if edge(KeyCode::F2) {
            aspect_ratio = match aspect_ratio {
                None => Some(16.0 / 9.0),
                Some(v) if (v - 16.0 / 9.0).abs() < 0.01 => Some(4.0 / 3.0),
                Some(v) if (v - 4.0 / 3.0).abs() < 0.01 => Some(1.0),
                _ => None,
            };
            win.set_aspect_ratio(aspect_ratio);
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
            last_attention = true;
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
            if visible {
                visible = false;
                win.set_visible(false);
                hidden_at = Some(std::time::Instant::now());
            }
        }
        if let Some(h) = hidden_at {
            if h.elapsed().as_secs_f64() >= 3.0 {
                hidden_at = None;
                visible = true;
                win.set_visible(true);
            }
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
        // ---- 新增：尺寸约束 / 层级 / 透明 / 图标 / 拖边 ----
        if edge(KeyCode::Minus) {
            min_mode = (min_mode + 1) % 3;
            match min_mode {
                0 => win.set_min_size::<f64, f64>(None, None),
                1 => win.set_min_size(Some(400.0f64), Some(300.0f64)),
                _ => win.set_min_size(Some(800.0f64), Some(600.0f64)),
            }
        }
        if edge(KeyCode::Equal) {
            max_mode = (max_mode + 1) % 3;
            match max_mode {
                0 => win.set_max_size::<f64, f64>(None, None),
                1 => win.set_max_size(Some(1280.0f64), Some(800.0f64)),
                _ => win.set_max_size(Some(1920.0f64), Some(1080.0f64)),
            }
        }
        if edge(KeyCode::Digit0) {
            level_mode = (level_mode + 1) % 3;
            let lvl = match level_mode {
                0 => WindowLevel::Normal,
                1 => WindowLevel::AlwaysOnTop,
                _ => WindowLevel::AlwaysOnBottom,
            };
            win.set_window_level(lvl);
        }
        if edge(KeyCode::Backslash) {
            transparent = !transparent;
            win.set_transparent(transparent);
        }
        if edge(KeyCode::Quote) {
            icon_mode = (icon_mode + 1) % 2;
            let (r, g, bl) = match icon_mode {
                0 => (230, 60, 60),
                _ => (60, 200, 110),
            };
            let rgba = vec![r, g, bl, 255].repeat(16 * 16);
            if let Ok(icon) = winit::window::Icon::from_rgba(rgba, 16, 16) {
                win.set_icon(icon);
            }
        }
        if edge(KeyCode::Enter) {
            drag_dir = (drag_dir + 1) % 4;
            let dir = match drag_dir {
                0 => ResizeDirection::East,
                1 => ResizeDirection::South,
                2 => ResizeDirection::West,
                _ => ResizeDirection::North,
            };
            let _ = win.drag_resize_window(dir);
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

        // ---- 绘制：分类分组 + 自适应布局 ----
        let m = win.metrics();
        let win_w = m.width as f32;
        let win_h = m.height as f32;
        let title_h = 36.0;
        let hint_h = 5.0 * 18.0 + 8.0;
        let area_bottom = (win_h - hint_h - 10.0).max(title_h + 12.0);
        let col_w = (win_w - 32.0 - 24.0) / 2.0;
        let left_x = 16.0;
        let right_x = left_x + col_w + 24.0;
        let in_view = |y: f32| y < area_bottom;

        let mut b = DrawBatch::new();

        // 模拟标题栏
        b.set_color(Color::new(0.16, 0.18, 0.24, 1.0));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), win_w, title_h, None);
        draw_text(
            &mut b.texts,
            "拖这里拖动窗口（drag_window）",
            Pos::new(12.0, 10.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.85, 0.9, 1.0, 1.0)),
        );

        // 左栏：外观·状态 / 尺寸·位置
        let mut ly = title_h + 12.0;
        ly += group(&mut b, left_x, ly, in_view(ly), "外观 · 状态") + GROUP_GAP;
        ly += row(&mut b, left_x, ly, in_view(ly), "frame_style", &format!("{:?}", win.frame_style()));
        ly += row(&mut b, left_x, ly, in_view(ly), "enabled_buttons", if buttons_all { "all" } else { "close-only" });
        ly += row(&mut b, left_x, ly, in_view(ly), "theme", &format!("set={:?} query={:?}", match theme_mode { 1 => Some(Theme::Dark), 2 => Some(Theme::Light), _ => None }, cur_theme));
        ly += row(&mut b, left_x, ly, in_view(ly), "fullscreen", &full.to_string());
        let vis_desc = if let Some(h) = hidden_at {
            format!("hidden, {:.0}s auto-restore", (3.0 - h.elapsed().as_secs_f64()).max(0.0))
        } else if visible {
            "visible".to_string()
        } else {
            "hidden".to_string()
        };
        ly += row(&mut b, left_x, ly, in_view(ly), "min/max/visible", &format!("{}/{}/{}", is_min, is_max, vis_desc));
        ly += row(&mut b, left_x, ly, in_view(ly), "window_level", match level_mode { 0 => "Normal", 1 => "AlwaysOnTop", _ => "AlwaysOnBottom" });
        ly += row(&mut b, left_x, ly, in_view(ly), "transparent", &format!("{}", transparent));
        ly += row(&mut b, left_x, ly, in_view(ly), "opacity", &format!("{:.2}", opacity));
        ly += row(&mut b, left_x, ly, in_view(ly), "title", titles[title_i]);

        ly += GROUP_GAP;
        ly += group(&mut b, left_x, ly, in_view(ly), "尺寸 · 位置") + GROUP_GAP;
        ly += row(&mut b, left_x, ly, in_view(ly), "inner_pos", &format!("px={:.0},{:.0} dp={:.0},{:.0}", inner_pos_px.0, inner_pos_px.1, inner_pos.0, inner_pos.1));
        ly += row(&mut b, left_x, ly, in_view(ly), "outer", &format!("pos={:?} size px={:.0}x{:.0}", outer_pos, outer_size.physical().0, outer_size.physical().1));
        ly += row(&mut b, left_x, ly, in_view(ly), "metrics", &format!("logical={}x{} physical={}x{} sf={:.2}", m.width, m.height, m.physical_width, m.physical_height, m.scale_factor));
        ly += row(&mut b, left_x, ly, in_view(ly), "dpi_override", &format!("{:?}", dpi_override));
        ly += row(&mut b, left_x, ly, in_view(ly), "min/max_size", &format!("{}/{}", match min_mode { 1 => "400x300", 2 => "800x600", _ => "None" }, match max_mode { 1 => "1280x800", 2 => "1920x1080", _ => "None" }));
        ly += row(&mut b, left_x, ly, in_view(ly), "reqsize", &format!("{:?}", reqsize));
        ly += row(&mut b, left_x, ly, in_view(ly), "resize_incr/hittest", &format!("{:?} / {}", inc_q.map(|p| p.logical()), hittest));
        ly += row(&mut b, left_x, ly, in_view(ly), "aspect_ratio", &format!("{:?}", aspect_ratio));
        row(&mut b, left_x, ly, in_view(ly), "monitors", &format!("cur={} prim={} avail={}", cur_mon.is_some(), prim_mon.is_some(), avail_n));

        // 右栏：光标·输入 / 平台
        let mut ry = title_h + 12.0;
        ry += group(&mut b, right_x, ry, in_view(ry), "光标 · 输入") + GROUP_GAP;
        ry += row(&mut b, right_x, ry, in_view(ry), "cursor/grab", &format!("visible={} grab={}", cursor_visible, grab));
        ry += row(&mut b, right_x, ry, in_view(ry), "attention", &last_attention.to_string());
        ry += row(&mut b, right_x, ry, in_view(ry), "protect/blur", &protect.to_string());
        ry += row(&mut b, right_x, ry, in_view(ry), "focusable/focused", &format!("{}/{}", win.is_focusable(), win.focused()));
        ry += row(&mut b, right_x, ry, in_view(ry), "moved/theme", &format!("({}, {}) / {:?}", moved_x, moved_y, evt_theme));

        ry += GROUP_GAP;
        #[cfg(target_os = "windows")]
        {
            ry += group(&mut b, right_x, ry, in_view(ry), "Windows 平台") + GROUP_GAP;
            ry += row(&mut b, right_x, ry, in_view(ry), "enable/skip/icon", &format!("{}/{}/{}", win_enable, win_skip_taskbar, win_taskbar_icon));
            ry += row(&mut b, right_x, ry, in_view(ry), "backdrop", match backdrop_mode { 1 => "Mica", 2 => "Acrylic", 3 => "Tabbed", _ => "None" });
            ry += row(&mut b, right_x, ry, in_view(ry), "border/title", &format!("{} {} {}", match border_color_mode { 1 => "red", 2 => "green", _ => "None" }, match title_bg_mode { 1 => "dark", 2 => "light", _ => "None" }, match title_text_mode { 1 => "white", 2 => "black", _ => "system" }));
            ry += row(&mut b, right_x, ry, in_view(ry), "corner", match corner_mode { 1 => "Round", 2 => "RoundSmall", 3 => "DoNotRound", _ => "Default" });
            ry += row(&mut b, right_x, ry, in_view(ry), "progress", match progress_mode { 1 => "Normal50", 2 => "Indeterminate", 3 => "Paused30", 4 => "Error70", _ => "None" });
            row(&mut b, right_x, ry, in_view(ry), "thumb/overlay/appid", &format!("{}/{}/{} click={:?}", thumbar_on, overlay_on, appid_on, st.lock().unwrap().thumb));
        }
        #[cfg(target_os = "macos")]
        {
            ry += group(&mut b, right_x, ry, in_view(ry), "macOS 平台") + GROUP_GAP;
            ry += row(&mut b, right_x, ry, in_view(ry), "fullscreen/shadow", &format!("{}/{}", mac_fullscreen, mac_shadow));
            row(&mut b, right_x, ry, in_view(ry), "edited/game/alt", &format!("{}/{}/{}", mac_edited, mac_game, match mac_alt { 1 => "OnlyLeft", 2 => "OnlyRight", _ => "Both" }));
        }

        // 底部键位提示（自适应，按类别分行）
        let hy = area_bottom + 10.0;
        hint(&mut b, 16.0, hy, "通用·状态: D 装饰 · 1 可调 · 2 光标 · 3 抓取 · 4 按钮 · 5 注意 · 6 主题 · F 全屏 · M/N 最大/最小 · H 显隐 · T 标题");
        hint(&mut b, 16.0, hy + 18.0, "通用·尺寸: G 中心 · R 位置 · 7/8/9 尺寸 · I 异步 · O dpi覆盖 · - 最小 · = 最大 · [ ] 增量 · Z 居中");
        hint(&mut b, 16.0, hy + 36.0, "通用·其它: Q 穿透 · C 保护 · B 透明度 · K dead键 · 0 层级 · \\ 透明 · ' 图标 · Enter 拖边 · F1 可聚焦 · F2 宽高比");
        #[cfg(target_os = "windows")]
        hint(&mut b, 16.0, hy + 54.0, "Windows: Y 置顶 · U 顶层 · E 启用 · S 跳过任务栏 · L 图标 · A 背景 · W 边框 · X 标题栏底 · V 标题文字 · J 圆角 · P 进度 · ; 缩略图 · , overlay · . AppID · 注: 无边框下 J圆角 不生效(DWM 无法圆角)，会记住偏好·切回有边框恢复");
        #[cfg(target_os = "macos")]
        hint(&mut b, 16.0, hy + 54.0, "macOS: A 简单全屏 · S 阴影 · E 已编辑 · V 无边框游戏 · P Option 键");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        hint(&mut b, 16.0, hy + 54.0, "本平台无额外专属属性");
        hint(&mut b, 16.0, hy + 72.0, "渲染/性能(AA/present/latency/cap)→ frame_stats · resize 策略/follow → window_resize/layout_follow · IME → input_ime · 自定义光标 → window_create");

        win.draw(Color::new(0.07, 0.08, 0.12, if transparent { 0.55 } else { 1.0 }), &[&b]);
        true
    });
}
