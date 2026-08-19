//! 窗口创建示例：`WindowDesc` 全部 builder + 多窗口 + 创建后查询
//!
//! 创建窗口只需两步：`App::window(desc, on_close)` 返回 [`WindowIndex`]，
//! 之后在 `app.run` 回调里用 `app.window_ref(&idx)` 拿到 `&VireoWindow` 绘制。
//! 三个窗口用不同的 `WindowDesc` 配置演示创建期选项：
//!
//! - **W1**：默认逻辑像素尺寸 + `dpi_override(Some(1.0))`（vireo 全自持像素，
//!   逻辑 = 物理）。`min_size`/`max_size`/`resize_increments` 用裸数（= 逻辑像素）。
//! - **W2**：`FrameStyle::HiddenTitlebar` 无标题栏但保留系统 resize 边框。
//!   （按钮画在客户端，`WM_NCHITTEST` 返回 `HT*` 让
//!   Windows 自动接管交互）：
//!   - `frame_subclass` 保留左/右/下 8px 系统边框（Win11 圆角 + resize 热区）。
//!   - 顶部 32px 客户端标题栏：`set_non_client_regions` 声明整条 Caption +
//!     右上角 3 个按钮（Close / Max / Min）。
//!   - 点击按钮由 NC 子类拦截 `WM_NCLBUTTONDOWN` 自管（阻止 DefWindowProc
//!     渲染经典按下按钮），自己发 `WM_SYSCOMMAND` 完成最小化/最大化/关闭。
//!   - 顶部 32px 其余区域 = Caption region → Windows 自动接管拖动 /snap layout/
//!     双击最大化/右键系统菜单。
//!   - 8px 系统边框由 `WM_NCHITTEST` 手动命中热区（HTTOP/HTLEFT/...）接管 resize。
//!   - 按钮位置随窗口宽度变化（`metrics().width` 变化时重设）。
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
//! - W2 NC 视觉全由客户端 DrawBatch 自绘（标题栏背景 + Win10/11 风格按钮）；
//!   交互由 `WM_NCHITTEST` 返回 `HT*` + 按钮点击自管让 Windows 自动接管。
//!   按钮位置逻辑：`metrics().width` 变化时重设，缓存上一帧宽度避免每帧
//!   spam `nc_tx`。hover 由 `mouse_pos()` 驱动（普通按钮淡灰、关闭按钮红）。

use vireo::prelude::*;
#[cfg(target_os = "windows")]
use vireo::platform::windows::WindowExtWindows;

#[cfg(target_os = "windows")]
const TITLE_H: f32 = 32.0;
#[cfg(target_os = "windows")]
const BTN_W: f32 = 46.0;

fn main() {
    let app = App::new();

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

    // ---- W2：物理像素 + 定位 + 自管标题栏（HiddenTitlebar + §7.6 NC API）----
    // `px(...)` 是物理像素意图：物理尺寸固定，逻辑 = 物理 ÷ OS DPI。
    // 高 DPI（如 200%）下逻辑会变小，故物理尺寸要比 W1 大不少才看着相当。
    // `FrameStyle::HiddenTitlebar` = Electron `titleBarStyle:'hidden'`：
    // 系统 resize 边框（≈8px）由 `frame_subclass` 保留 + `WM_NCHITTEST`
    // 手动命中热区（HTTOP/HTLEFT/...）接管 resize。
    // §7.6 NC API 在 on_frame 闭包里设置（需 hwnd 已就绪）：
    //   set_non_client_regions([Caption 全条 + 3 buttons]) → 拖动/snap + 按钮命中
    //   客户端标题栏 + 按钮外观由 DrawBatch 自绘；点击按钮自管发 WM_SYSCOMMAND
    let w2 = app.window(
        WindowDesc::new("Vireo Window 2 — 物理像素 + 自管标题栏 + 3 按钮", 480, 320)
            .size(px(1024.0), px(640.0))
            .position(px(80.0), px(60.0))
            .resizable(true)
            .frame_style(FrameStyle::HiddenTitlebar),
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
    // W2 NC 状态：regions 随宽度变化重设（按钮位置跟随窗口宽度）。
    #[cfg(target_os = "windows")]
    let mut w2_last_width: u32 = 0;
    // W2 客户端标题栏高度：仅 Windows（自绘按钮 + WM_NCHITTEST 接管交互）；
    // macOS `HiddenTitlebar` 由系统自管红绿灯、不自绘，内容从顶部开始。
    #[cfg(target_os = "windows")]
    let w2_top = TITLE_H;
    #[cfg(not(target_os = "windows"))]
    let w2_top = 0.0;

    app.run(move |app| {
        let (win1, win2, win3) = match (
            app.window_ref(&w1),
            app.window_ref(&w2),
            app.window_ref(&w3),
        ) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => return false,
        };

        // W2 NC hit-test regions：按钮外观画在客户端，WM_NCHITTEST 返回 HT* 让
        // Windows 自动接管交互（snap layout / 双击 / 右键菜单 / 按钮点击）。
        #[cfg(target_os = "windows")]
        {
            let m2 = win2.metrics();
            if m2.width != w2_last_width {
                w2_last_width = m2.width;
                let w = m2.width as f32;
                let close_x = w - BTN_W;
                let max_x = w - 2.0 * BTN_W;
                let min_x = w - 3.0 * BTN_W;
                win2.set_non_client_regions(&[
                    NonClientRegion {
                        rect: Rect::new(0.0, 0.0, w, TITLE_H),
                        hit_test: NonClientHit::Caption,
                    },
                    NonClientRegion {
                        rect: Rect::new(close_x, 0.0, BTN_W, TITLE_H),
                        hit_test: NonClientHit::Close,
                    },
                    NonClientRegion {
                        rect: Rect::new(max_x, 0.0, BTN_W, TITLE_H),
                        hit_test: NonClientHit::MaxButton,
                    },
                    NonClientRegion {
                        rect: Rect::new(min_x, 0.0, BTN_W, TITLE_H),
                        hit_test: NonClientHit::MinButton,
                    },
                ]);
            }
        }

        // `T` 切换 W1/W2 可见性（下降沿：仅在按下瞬间翻转一次）
        let t_down = win1.key_down(KeyCode::KeyT);
        if t_down && !t_was_down {
            visible = !visible;
            win1.set_visible(visible);
            win2.set_visible(visible);
        }
        t_was_down = t_down;

        // W2 标题栏画在客户端 y=0..32（DrawBatch 自绘），WM_NCHITTEST 返回 HT* 让
        // Windows 自动接管交互（拖动 / 双击 / 右键 / snap layout / 按钮点击）。
        draw_window(win1, 1, "W1", "dpi_override(Some(1.0)) · min/max · 裸数=逻辑", 0.0);
        draw_window(
            win2,
            2,
            "W2",
            "Px · HiddenTitlebar · 客户端标题栏 · HT* 自动交互",
            w2_top,
        );
        draw_window(win3, 3, "W3", "AutoVsync · Msaa · Dark · maximized", 0.0);
        true
    });
}

fn draw_window(
    win: &vireo::window::VireoWindow,
    id: u8,
    name: &str,
    cfg: &str,
    content_top: f32,
) {
    let m = win.metrics();
    let w = m.width as f32;
    let h = m.height as f32;

    let mut b = DrawBatch::new();

    // W2 客户端标题栏：画背景 + 3 按钮（Win10/11 系统风格，几何绘制）。
    // WM_NCHITTEST 返回 HT* 让 Windows 自动处理点击行为。
    // （仅 Windows：macOS `HiddenTitlebar` 红绿灯由系统自管，不自绘按钮。）
    #[cfg(target_os = "windows")]
    if content_top > 0.0 && id == 2 {
        let bg = Color::new(0.12, 0.12, 0.14, 1.0);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), w, TITLE_H, Some(bg));
        let close_x = w - BTN_W;
        let max_x = w - 2.0 * BTN_W;
        let min_x = w - 3.0 * BTN_W;
        // 鼠标位置（客户端逻辑像素）驱动 hover 背景
        let (mx, my) = win.mouse_pos();
        let maxed = win.is_maximized();
        let hover = |bx: f32| my >= 0.0 && my < TITLE_H && mx >= bx && mx < bx + BTN_W;
        // hover 背景（Win11 风格：普通按钮淡灰、关闭按钮红）
        let normal_hover = Color::new(0.22, 0.22, 0.25, 1.0);
        let close_hover = Color::new(0.70, 0.11, 0.11, 1.0);
        if hover(min_x) {
            draw_rectangle(&mut b, Pos::new(min_x, 0.0), BTN_W, TITLE_H, Some(normal_hover));
        }
        if hover(max_x) {
            draw_rectangle(&mut b, Pos::new(max_x, 0.0), BTN_W, TITLE_H, Some(normal_hover));
        }
        if hover(close_x) {
            draw_rectangle(&mut b, Pos::new(close_x, 0.0), BTN_W, TITLE_H, Some(close_hover));
        }
        // 符号颜色：hover 关闭按钮时用白色，其余用浅灰
        let sym_white = Color::new(1.0, 1.0, 1.0, 1.0);
        let sym_gray = Color::new(0.82, 0.82, 0.84, 1.0);
        let close_col = if hover(close_x) { sym_white } else { sym_gray };
        let max_col = if hover(max_x) { sym_white } else { sym_gray };
        let min_col = if hover(min_x) { sym_white } else { sym_gray };
        // 几何符号（模仿 Win11 系统标题栏按钮，居中在 BTN_W×TITLE_H 内）
        // 最小化：一条短横线
        let cy = TITLE_H / 2.0;
        let cx = |bx: f32| bx + BTN_W / 2.0;
        draw_line(&mut b, cx(min_x) - 5.0, cy, cx(min_x) + 5.0, cy, 1.2, Some(min_col));
        // 最大化/还原：空心方框（还原 = 前后两个错位方框）
        let box_sz = 10.0;
        if maxed {
            let bx = cx(max_x) - box_sz / 2.0 + 2.0;
            let by = cy - box_sz / 2.0 - 1.0;
            draw_rect_outline(&mut b, Pos::new(bx, by), box_sz, box_sz, 1.0, Some(max_col));
            draw_rect_outline(&mut b, Pos::new(bx - 2.0, by + 2.0), box_sz, box_sz, 1.0, Some(max_col));
        } else {
            let bx = cx(max_x) - box_sz / 2.0;
            let by = cy - box_sz / 2.0;
            draw_rect_outline(&mut b, Pos::new(bx, by), box_sz, box_sz, 1.0, Some(max_col));
        }
        // 关闭：两条交叉斜线（X）
        let d = 5.0;
        draw_line(&mut b, cx(close_x) - d, cy - d, cx(close_x) + d, cy + d, 1.2, Some(close_col));
        draw_line(&mut b, cx(close_x) + d, cy - d, cx(close_x) - d, cy + d, 1.2, Some(close_col));
    }

    // ---- 文本区：依次三行 ----
    let mut ty = content_top + 8.0;
    draw_text(
        &mut b.texts,
        name,
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

    // ---- 色块 ----
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
