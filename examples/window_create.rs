//! 窗口创建示例：`WindowDesc` 全部 builder + 多窗口 + 创建后查询
//!
//! 创建窗口只需两步：`App::window(desc, on_close)` 返回 [`WindowIndex`]，
//! 之后在 `app.run` 回调里用 `app.window_ref(&idx)` 拿到 `&VireoWindow` 绘制。
//! 三个窗口用不同的 `WindowDesc` 配置演示创建期选项：
//!
//! - **W1**：默认逻辑像素尺寸 + `dpi_override(Some(1.0))`（vireo 全自持像素，
//!   逻辑 = 物理）。`min_size`/`max_size`/`resize_increments` 用裸数（= 逻辑像素）。
//! - **W2**：`size(Px, Px)` 显式物理像素 + `position` 定位 +
//!   `FrameStyle::HiddenTitlebar` 无标题栏但保留系统 resize 边框
//!   （= Electron `titleBarStyle:'hidden'`；**保留边框仅 Windows 生效，其它平台
//!   整体去装饰**）。**自定义装饰**：
//!   顶部自绘标题栏拖动窗口（`drag_window`）+ 更宽的边/角缩放手势
//!   （`drag_resize_window`），跟原生窗口一样有系统吸附/Aero Snap/拖动阴影。
//!   顶部无系统 resize 热区（`WM_NCCALCSIZE` top inset=0，避免 DWM 顶部白条），
//!   顶部缩放交给自绘 `drag_resize_window(North*)` 手势。
//! - **W3**：`present_mode` / `frame_latency` / `anti_aliasing` / `theme` 等
//!   GPU 与外观选项 + `maximized`。
//!
//! 创建后各窗口 HUD 打印 `metrics()`（逻辑/物理宽高、scale_factor），
//! 演示「构造期声明的像素意图」如何落到实际窗口。
//!
//! ```bash
//! cargo run --example window_create
//! ```
//!
//! 说明：
//! - `icon_from_path("logo.png")` 需要工作目录下有图片文件，缺省自动跳过（返回 None）。
//! - 关闭窗口时触发 `on_close` 回调；三个窗口全部关闭后进程退出。
//! - 键盘 `T`：切换 W1 与 W2 的可见性（`set_visible`）。
//! - W2 无标题栏的拖动/缩放为 Windows 演示（`drag_window`/`drag_resize_window` macOS 不支持）。

use vireo::prelude::*;
use vireo::window::Cursor;
use winit::window::CursorIcon;

// W2 无边框窗口的「自定义装饰」热区尺寸（逻辑像素）：
// - 顶部 0..TITLE_H：标题栏，拖动窗口
// - 四边 EDGE_HIT 厚、四角 CORNER_HIT×CORNER_HIT：缩放（优先级：角 > 边）
// - 其余内容区：无手势
//
// 关键：命中区（EDGE_HIT/CORNER_HIT）比画出来的边框（EDGE_VISUAL/CORNER_VISUAL）
// 宽 —— 这就是原生窗口那种「离内容几像素也能抓」的隐形抓取带：外沿部分
// 看不见、摸得着。
const TITLE_H: f32 = 32.0;
const EDGE_HIT: f32 = 8.0;
const CORNER_HIT: f32 = 16.0;
const EDGE_VISUAL: f32 = 1.0;
const CORNER_VISUAL: f32 = 6.0;

/// 命中测试：把光标位置映射为无边框窗口的缩放手势（None = 内容区/标题栏）。
/// 角判定用 `CORNER_HIT`，边判定用 `EDGE_HIT`（都比视觉边框宽）。
fn w2_resize_direction(w: f32, h: f32, mx: f32, my: f32) -> Option<ResizeDirection> {
    let left = mx <= EDGE_HIT;
    let right = mx >= w - EDGE_HIT;
    let top = my <= EDGE_HIT;
    let bottom = my >= h - EDGE_HIT;
    let nw = mx <= CORNER_HIT && my <= CORNER_HIT;
    let ne = mx >= w - CORNER_HIT && my <= CORNER_HIT;
    let sw = mx <= CORNER_HIT && my >= h - CORNER_HIT;
    let se = mx >= w - CORNER_HIT && my >= h - CORNER_HIT;
    match (nw, ne, sw, se) {
        (true, _, _, _) => Some(ResizeDirection::NorthWest),
        (_, true, _, _) => Some(ResizeDirection::NorthEast),
        (_, _, true, _) => Some(ResizeDirection::SouthWest),
        (_, _, _, true) => Some(ResizeDirection::SouthEast),
        _ => match (left, right, top, bottom) {
            (true, _, _, _) => Some(ResizeDirection::West),
            (_, true, _, _) => Some(ResizeDirection::East),
            (_, _, true, _) => Some(ResizeDirection::North),
            (_, _, _, true) => Some(ResizeDirection::South),
            _ => None,
        },
    }
}

/// 无边框窗口的「自定义装饰」处理：返回按下时应调用的手势，并设置缩放光标。
fn handle_custom_decoration(
    win: &vireo::window::VireoWindow,
    lb_pressed: bool,
    last_cursor: &mut Option<Cursor>,
) -> Option<ResizeDirection> {
    let m = win.metrics();
    let w = m.width as f32;
    let h = m.height as f32;
    let (mx, my) = win.mouse_pos();

    // 先判四边/四角缩放（角 > 边 > 标题栏，与原生一致）
    let dir = w2_resize_direction(w, h, mx, my);
    if let Some(dir) = dir {
        if lb_pressed {
            let _ = win.drag_resize_window(dir);
        }
        let cursor = Some(Cursor::Icon(CursorIcon::from(dir)));
        if cursor != *last_cursor {
            *last_cursor = cursor.clone();
            win.set_cursor(cursor.unwrap());
        }
        return Some(dir);
    }

    // 顶部标题栏：拖动窗口
    if my >= 0.0 && my < TITLE_H && mx >= 0.0 && mx < w {
        if lb_pressed {
            let _ = win.drag_window();
        }
        if last_cursor.is_some() {
            *last_cursor = None;
            win.set_cursor(Cursor::default());
        }
        return None;
    }

    // 内容区：恢复默认光标
    if last_cursor.is_some() {
        *last_cursor = None;
        win.set_cursor(Cursor::default());
    }
    None
}

fn main() {
    let mut app = App::new();

    // ---- W1：默认逻辑尺寸 + vireo 全自持像素 ----
    let w1 = app.window(
        WindowDesc::new("Vireo Window 1 — 逻辑像素 + dpi_override", 640, 480)
            .dpi_override(Some(1.0))
            .min_size(320, 240)
            .max_size(1600, 1200)
            .resize_increments(4, 4)
            .icon_from_path("logo.png"),
        Some(|| println!("W1 已关闭")),
    );

    // ---- W2：显式物理像素 + 定位 + 无标题栏但保留系统边框（HiddenTitlebar）----
    // `px(...)` 是物理像素意图：物理尺寸固定，逻辑 = 物理 ÷ OS DPI。
    // 高 DPI（如 200%）下逻辑会变小，故物理尺寸要比 W1 大不少才看着相当。
    // `FrameStyle::HiddenTitlebar` = Electron `titleBarStyle:'hidden'`：
    // 系统 resize 边框（≈8px）由 Windows 自动接管缩放，无需手写命中测试；
    // 下方自定义装饰仅演示「顶部自绘标题栏拖动 + 更宽的边/角缩放热区」。
    let w2 = app.window(
        WindowDesc::new("Vireo Window 2 — 物理像素 + 无边框", 480, 320)
            .size(px(1024.0), px(640.0)) // 物理像素意图；W1/W3 用裸数 = 逻辑像素
            .position(px(80.0), px(60.0))
            .resizable(true)
            .frame_style(FrameStyle::HiddenTitlebar), // 无标题栏 + 保留系统 resize 边框
        Some(|| println!("W2 已关闭")),
    );

    // ---- W3：GPU/外观选项 + 最大化 ----
    let w3 = app.window(
        WindowDesc::new("Vireo Window 3 — present_mode + MSAA + theme", 480, 300)
            .present_mode(PresentMode::AutoVsync)
            .frame_latency(2)
            .anti_aliasing(AntiAliasing::Msaa { samples: 4, alpha_to_coverage: false })
            .theme(Theme::Dark)
            .maximized(true),
        Some(|| println!("W3 已关闭")),
    );

    let mut visible = true;
    let mut t_was_down = false;
    let mut w2_lb_was_down = false;
    let mut w2_last_cursor: Option<Cursor> = None;

    app.run(move |app| {
        let (win1, win2, win3) = match (
            app.window_ref(&w1),
            app.window_ref(&w2),
            app.window_ref(&w3),
        ) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => return false,
        };

        // `T` 切换 W1/W2 可见性（下降沿：仅在按下瞬间翻转一次）
        let t_down = win1.key_down(KeyCode::KeyT);
        if t_down && !t_was_down {
            visible = !visible;
            win1.set_visible(visible);
            win2.set_visible(visible);
        }
        t_was_down = t_down;

        // W2 无边框：自定义装饰——顶部标题栏拖动、四边四角缩放。
        // 手势只在「左键按下沿」调一次；按住期间每帧调会让 winit 重发
        // WM_NCLBUTTONDOWN，把窗口吸附到鼠标。
        if win2.frame_style() != FrameStyle::Normal {
            let lb_down = win2.mouse_left();
            let lb_pressed = lb_down && !w2_lb_was_down;
            handle_custom_decoration(win2, lb_pressed, &mut w2_last_cursor);
            w2_lb_was_down = lb_down;
        }

        draw_window(win1, 1, "W1", "dpi_override(Some(1.0)) · min/max · 裸数=逻辑");
        draw_window(win2, 2, "W2", "size(Px) · position · HiddenTitlebar · 保留系统边框");
        draw_window(win3, 3, "W3", "AutoVsync · Msaa · Dark · maximized");
        true
    });
}

fn draw_window(win: &vireo::window::VireoWindow, id: u8, name: &str, cfg: &str) {
    let m = win.metrics();
    let w = m.width as f32;
    let h = m.height as f32;

    // 无边框窗口（W2）自绘了 32px 标题栏，内容区要向下让出
    let content_top = if win.frame_style() != FrameStyle::Normal { 32.0 } else { 0.0 };

    let mut b = DrawBatch::new();

    // 无边框窗口（W2）顶部画一条「标题栏」示意可拖动区域（拖动逻辑在 main 闭包）
    if win.frame_style() != FrameStyle::Normal {
        b.set_color(Color::new(0.18, 0.2, 0.28, 1.0));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), w, TITLE_H, None);
        draw_text(
            &mut b.texts,
            "无标题栏 · 拖这里移动 · 边/角可缩放（系统边框+自定义热区）",
            Pos::new(12.0, 8.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.85, 0.9, 1.0, 1.0)),
        );

        // 缩放热区视觉提示：只画 1px 细边框线 + 小角块。
        // 命中区（EDGE_HIT/CORNER_HIT）比这宽得多 —— 外沿「看不见但能抓」。
        let (mx, my) = win.mouse_pos();
        let hover = w2_resize_direction(w, h, mx, my).is_some();
        let edge_c = if hover {
            Color::new(0.9, 0.7, 0.2, 0.8)
        } else {
            Color::new(0.5, 0.6, 0.7, 0.4)
        };
        b.set_color(edge_c);
        // 边：1px 细线（贴着最外沿）
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), w, EDGE_VISUAL, None);
        draw_rectangle(&mut b, Pos::new(0.0, h - EDGE_VISUAL), w, EDGE_VISUAL, None);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), EDGE_VISUAL, h, None);
        draw_rectangle(&mut b, Pos::new(w - EDGE_VISUAL, 0.0), EDGE_VISUAL, h, None);
        // 角：小方块（提示可对角缩放）
        b.set_color(Color::new(0.75, 0.55, 0.15, 0.6));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), CORNER_VISUAL, CORNER_VISUAL, None);
        draw_rectangle(&mut b, Pos::new(w - CORNER_VISUAL, 0.0), CORNER_VISUAL, CORNER_VISUAL, None);
        draw_rectangle(&mut b, Pos::new(0.0, h - CORNER_VISUAL), CORNER_VISUAL, CORNER_VISUAL, None);
        draw_rectangle(&mut b, Pos::new(w - CORNER_VISUAL, h - CORNER_VISUAL), CORNER_VISUAL, CORNER_VISUAL, None);
    }

    // ---- 文本区：标题栏下依次三行，行距充足 ----
    let mut ty = content_top + 8.0;
    draw_text(
        &mut b.texts,
        &format!("{name} — 拖动窗口边缘/移动窗口观察"),
        Pos::new(16.0, ty),
        TextDef::default().font_size(16.0),
        TextOverride::from_color(Color::new(0.9, 0.95, 1.0, 1.0)),
    );
    ty += 26.0;
    draw_text(
        &mut b.texts,
        cfg,
        Pos::new(16.0, ty),
        TextDef::default().font_size(13.0),
        TextOverride::from_color(Color::new(0.6, 0.7, 0.8, 1.0)),
    );
    ty += 22.0;
    draw_text(
        &mut b.texts,
        &format!(
            "metrics: logical {}x{}  physical {}x{}  sf {:.2}",
            m.width, m.height, m.physical_width, m.physical_height, m.scale_factor
        ),
        Pos::new(16.0, ty),
        TextDef::default().font_size(13.0),
        TextOverride::from_color(Color::new(0.7, 0.8, 0.9, 1.0)),
    );

    // ---- 色块：填充文本区下方的剩余空间，不盖文字 ----
    let c = match id {
        1 => Color::new(0.15, 0.35, 0.55, 1.0),
        2 => Color::new(0.55, 0.35, 0.15, 1.0),
        _ => Color::new(0.25, 0.5, 0.25, 1.0),
    };
    let band_top = ty + 14.0;
    let band_h = (h - band_top - 8.0).max(0.0);
    if band_h > 0.0 {
        draw_rectangle(&mut b, Pos::new(w * 0.2, band_top), w * 0.6, band_h, Some(c));
    }

    win.draw(Color::new(0.06, 0.07, 0.1, 1.0), &[&b]);
}
