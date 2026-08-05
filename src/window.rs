use std::sync::{Arc, Mutex, mpsc};
use std::cell::RefCell;
use rustc_hash::FxHashMap;

use crate::context::Renderer;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{WindowId, WindowAttributes},
};

use crate::context::DrawBatch;
use crate::offscreen::OffscreenCanvas;
use crate::texture::Texture;
use crate::gpu::GpuContext;
use crate::input::InputState;

pub use winit::dpi::LogicalSize;
pub use winit::window::Fullscreen;
pub use winit::window::Icon;
pub use winit::window::WindowLevel;

use winit::window::Cursor;

/// 滑动窗口帧数：约 0.5s@60Hz，平滑 FPS，避免「满 1 秒整段重置」导致 55↔60 乱跳。
const FPS_SAMPLE_CAP: usize = 30;
/// resize 去抖默认值：尺寸**稳定**满此时间才 `surface.configure`。
/// 连续拖动时每帧尺寸都变、去抖永不触发 → 全程不 configure，按旧尺寸持续
/// present（DXGI SCALING_STRETCH 实时拉伸），帧流保持满速、无 27-68ms 卡顿
/// （wgpu-hal DX12 configure 每次都会 `wait_for_present_queue_idle` 等 present
/// queue 排空，DWM 停消费时无限等 → 旧实现拖动即冻屏）。
/// 用户可经 `VireoWindow::set_resize_debounce` 覆盖。
const DEFAULT_RESIZE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(100);

/// 拖动窗口时的 resize 尺寸刷新策略（`VireoWindow::set_resize_refresh_policy`）。
/// 目标是把「拖动中是否实时跟踪尺寸」的选择权交给用户：每帧/周期刷新在 wgpu-hal
/// DX12 上每次 `surface.configure` 都要阻塞等 present queue 排空（实测 ~50-80ms），
/// 会明显掉帧，但能实时看到新布局；`OnRelease` 全程不卡但内容拉伸到松手。
/// 「松手 snap」的去抖时长由 `VireoWindow::set_resize_debounce` 配置（默认 100ms）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeRefreshPolicy {
    /// 拖动全程不更新（按旧尺寸拉伸 present，帧流满速），松手尺寸稳定满
    /// 去抖时长后一次性 `surface.configure`（snap）。默认。
    OnRelease,
    /// 拖动中**每帧**都 `surface.configure` 实时跟踪尺寸。每次 configure
    /// 阻塞 ~50-80ms，帧率骤降、一顿一顿——给需要拖动时看到真实布局的用户。
    EveryFrame,
    /// 拖动中每满 `interval` 强制 configure 一次（折中；每次同样 ~50ms 级停顿）。
    Periodic(std::time::Duration),
}

/// resize 刷新决策（`VireoWindow::draw` 尺寸同步用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeRefresh {
    /// 尺寸未变，或当前策略下不需要 configure。
    None,
    /// 尺寸已**稳定**满 debounce（松手 snap）。
    Stable,
    /// 拖动中按策略实时刷新（每帧或周期性）。
    Live,
}

/// 尺寸是否需要在 draw 阶段 configure：
/// - 未变化 → `None`；
/// - 稳定满 `debounce` → `Stable`（拖动结束后的 snap，任何策略下都生效）；
/// - `EveryFrame` → 尺寸变化即 `Live`；`Periodic(iv)` → 距上次 configure 满 iv
///   → `Live`；`OnRelease` → 无实时刷新。
fn resize_refresh(
    size_changed: bool,
    stable_since: Option<std::time::Instant>,
    now: std::time::Instant,
    debounce: std::time::Duration,
    policy: ResizeRefreshPolicy,
    last_configure: std::time::Instant,
) -> ResizeRefresh {
    if !size_changed {
        return ResizeRefresh::None;
    }
    if let Some(t) = stable_since {
        if now.saturating_duration_since(t) >= debounce {
            return ResizeRefresh::Stable;
        }
    }
    match policy {
        ResizeRefreshPolicy::OnRelease => {}
        ResizeRefreshPolicy::EveryFrame => return ResizeRefresh::Live,
        ResizeRefreshPolicy::Periodic(iv) => {
            if now.saturating_duration_since(last_configure) >= iv {
                return ResizeRefresh::Live;
            }
        }
    }
    ResizeRefresh::None
}

/// 窗口描述符（用于配置待创建窗口）
/// 抗锯齿模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AntiAliasing {
    None,
    /// 多重采样：per-pixel 着色，硬件解析采样点覆盖。
    Msaa { samples: u32, alpha_to_coverage: bool },
    /// 超采样：per-sample 着色（`@interpolate(linear, sample)`），每个采样点独立计算 SDF。
    Ssaa { samples: u32, alpha_to_coverage: bool },
}

impl AntiAliasing {
    pub fn sample_count(&self) -> u32 {
        match self {
            AntiAliasing::None => 1,
            AntiAliasing::Msaa { samples, .. } | AntiAliasing::Ssaa { samples, .. } => *samples,
        }
    }

    pub fn alpha_to_coverage(&self) -> bool {
        match self {
            AntiAliasing::None => false,
            AntiAliasing::Msaa { alpha_to_coverage, .. } | AntiAliasing::Ssaa { alpha_to_coverage, .. } => *alpha_to_coverage,
        }
    }

    pub fn is_ssaa(&self) -> bool {
        matches!(self, AntiAliasing::Ssaa { .. })
    }
}

/// 把 AA 的 sample_count snap 到 `supported` 中 ≤ 请求的最大项。
/// 不可用 `min(req, max)`：列表可能是 `[1,4]`（无 2/8），硬截到 8 仍会在 pipeline 创建时 panic。
pub(crate) fn clamp_aa(aa: AntiAliasing, supported: &[u32]) -> AntiAliasing {
    let snap = |req: u32| -> u32 {
        let req = req.max(1);
        supported
            .iter()
            .copied()
            .filter(|&c| c <= req)
            .max()
            .unwrap_or(1)
    };
    match aa {
        AntiAliasing::None => AntiAliasing::None,
        AntiAliasing::Msaa { samples, alpha_to_coverage } => AntiAliasing::Msaa {
            samples: snap(samples),
            alpha_to_coverage,
        },
        AntiAliasing::Ssaa { samples, alpha_to_coverage } => AntiAliasing::Ssaa {
            samples: snap(samples),
            alpha_to_coverage,
        },
    }
}

pub struct WindowDesc {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub scale_factor_override: Option<f64>,
    pub min_size: Option<(u32, u32)>,
    pub max_size: Option<(u32, u32)>,
    pub position: Option<(i32, i32)>,
    pub resizable: bool,
    pub fullscreen: Option<Fullscreen>,
    pub maximized: bool,
    pub visible: bool,
    pub transparent: bool,
    pub decorations: bool,
    pub window_level: WindowLevel,
    pub window_icon: Option<Icon>,
    pub theme: Option<winit::window::Theme>,
    pub resize_increments: Option<(u32, u32)>,
    pub content_protected: bool,
    pub active: bool,
    pub cursor: Cursor,
    pub enabled_buttons: winit::window::WindowButtons,
    pub blur: bool,
    pub present_mode: wgpu::PresentMode,
    pub anti_aliasing: AntiAliasing,
}

impl WindowDesc {
    pub fn new(title: &str, width: u32, height: u32) -> Self {
        Self {
            title: title.to_string(),
            width,
            height,
            scale_factor_override: None,
            min_size: None,
            max_size: None,
            position: None,
            resizable: true,
            fullscreen: None,
            maximized: false,
            visible: true,
            transparent: false,
            decorations: true,
            window_level: WindowLevel::default(),
            window_icon: None,
            theme: None,
            resize_increments: None,
            content_protected: false,
            active: true,
            cursor: Cursor::default(),
            enabled_buttons: winit::window::WindowButtons::all(),
            blur: false,
            present_mode: wgpu::PresentMode::AutoVsync,
            anti_aliasing: AntiAliasing::None,
        }
    }

    /// 启用 high_dpi 模式：逻辑像素 = 物理像素（scale_factor = 1.0）
    pub fn high_dpi(mut self, enabled: bool) -> Self {
        self.scale_factor_override = if enabled { Some(1.0) } else { None };
        self
    }

    pub fn min_size(mut self, w: u32, h: u32) -> Self {
        self.min_size = Some((w, h));
        self
    }

    pub fn max_size(mut self, w: u32, h: u32) -> Self {
        self.max_size = Some((w, h));
        self
    }

    pub fn position(mut self, x: i32, y: i32) -> Self {
        self.position = Some((x, y));
        self
    }

    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    pub fn fullscreen(mut self, fullscreen: Fullscreen) -> Self {
        self.fullscreen = Some(fullscreen);
        self
    }

    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn transparent(mut self, transparent: bool) -> Self {
        self.transparent = transparent;
        self
    }

    pub fn decorations(mut self, decorations: bool) -> Self {
        self.decorations = decorations;
        self
    }

    pub fn window_level(mut self, level: WindowLevel) -> Self {
        self.window_level = level;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.window_icon = Some(icon);
        self
    }

    /// 从图片文件加载窗口图标（PNG/JPG/BMP）
    pub fn icon_from_path(mut self, path: impl AsRef<std::path::Path>) -> Self {
        if let Ok(data) = std::fs::read(path.as_ref()) {
            if let Ok(img) = image::load_from_memory(&data) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if let Ok(icon) = Icon::from_rgba(rgba.into_raw(), w, h) {
                    self.window_icon = Some(icon);
                }
            }
        }
        self
    }

    pub fn theme(mut self, theme: winit::window::Theme) -> Self {
        self.theme = Some(theme);
        self
    }

    pub fn resize_increments(mut self, w: u32, h: u32) -> Self {
        self.resize_increments = Some((w, h));
        self
    }

    pub fn content_protected(mut self, protected: bool) -> Self {
        self.content_protected = protected;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn cursor(mut self, cursor: Cursor) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn enabled_buttons(mut self, buttons: winit::window::WindowButtons) -> Self {
        self.enabled_buttons = buttons;
        self
    }

    pub fn blur(mut self, blur: bool) -> Self {
        self.blur = blur;
        self
    }

    pub fn present_mode(mut self, mode: wgpu::PresentMode) -> Self {
        self.present_mode = mode;
        self
    }

    pub fn anti_aliasing(mut self, aa: AntiAliasing) -> Self {
        self.anti_aliasing = aa;
        self
    }
}

/// 分段耗时（秒），由 [`VireoWindow::draw`] 回传。
///
/// 新流程（render thread 独占 surface 帧循环）：
/// - `configure_secs`：本帧 `surface.configure`（未 configure 时为 0）
/// - `acquire_secs`：`get_current_texture`（不含此前的尺寸同步与 `surface.configure`）
/// - `encode_secs`：Renderer 编码 + `queue.submit`
/// - `present_secs`：`queue.present`
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawTimings {
    /// `surface.configure` 同步耗时；本帧未 configure 时为 0。
    pub configure_secs: f64,
    /// `get_current_texture`：可能阻塞等待 swapchain 空位；不含尺寸同步与
    /// `surface.configure`，后两者发生在此计时区间之前。
    pub acquire_secs: f64,
    /// 编码 + `queue.submit`（不含 present）。
    pub encode_secs: f64,
    /// `queue.present`（不含 GPU 执行）。
    pub present_secs: f64,
    /// 上一份已完成提交的 GPU queue latency（不含 CPU 构图）。
    /// 该值包含驱动排队/GPU 竞争，不等于纯 shader 执行时间。
    pub gpu_secs: Option<f64>,
}

/// 窗口尺寸/缩放只读快照（逻辑坐标 = 用户坐标系）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowMetrics {
    /// 逻辑宽（用户坐标系宽度）
    pub width: u32,
    /// 逻辑高（用户坐标系高度）
    pub height: u32,
    /// 逻辑像素 → 物理像素 缩放因子（`high_dpi` 窗口为 1.0）
    pub scale_factor: f64,
}

/// 一次 [`VireoWindow::draw`] 的结果。
#[derive(Clone, Copy, Debug)]
pub struct DrawReport {
    /// 本帧结局（presented / skipped / failed）
    pub outcome: DrawOutcome,
    /// 分段耗时
    pub timings: DrawTimings,
}

/// 一次 [`VireoWindow::draw`] 的结局。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawOutcome {
    /// 已 acquire → submit → present 完成一帧。
    /// `suboptimal` 原样报告 wgpu 的 surface acquire 状态；它只表示当前 surface
    /// 对 swapchain 而言不是最优状态，不是规范的 resize/拉伸/拖动状态信号。
    Presented { suboptimal: bool },
    /// 本帧被跳过（未 acquire / 未 present）。
    Skipped(DrawSkipReason),
    /// GPU 设备丢失，应用应终止。
    Failed(DrawFailure),
}

/// 本帧被跳过的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawSkipReason {
    /// 窗口为 0×0（如最小化）——本次 draw 不 acquire。
    /// 这只跳过当前帧，不负责限制调用方渲染循环的 CPU 频率。
    ZeroSized,
    /// `get_current_texture` 超时，稍后重试。
    Timeout,
    /// 窗口被遮挡/最小化，重开后再画。
    Occluded,
    /// surface 过期（`Outdated`），已重配，本帧跳过。
    SurfaceReconfigured,
    /// 窗口正在关闭，不绘制。
    Closing,
}

/// 不可恢复的帧失败。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawFailure {
    /// GPU 设备丢失，surface 与全部 GPU 资源失效。
    DeviceLost,
}

/// 从 winit 线程发往渲染线程的事件（全是 Send-safe 的自定义类型）。
enum WinitEvent {
    WindowCreated {
        handle: usize,
        window: Arc<winit::window::Window>,
        surface: wgpu::Surface<'static>,
        surface_config: wgpu::SurfaceConfiguration,
        renderer: crate::context::Renderer,
        logical_width: u32,
        logical_height: u32,
        scale: f32,
        dpi_scale: f32,
        high_dpi: bool,
        init_duration: f64,
    },
    Resized { handle: usize, width: u32, height: u32 },
    ScaleFactorChanged { handle: usize, scale: f64 },
    CursorMoved { handle: usize, x: f64, y: f64 },
    KeyboardInput { handle: usize, event: crate::input::KeyEvent },
    MouseInput { handle: usize, button: winit::event::MouseButton, pressed: bool },
    MouseWheel { handle: usize, delta: crate::input::ScrollDelta },
    ModifiersChanged { handle: usize, modifiers: crate::input::Modifiers },
    Focused { handle: usize, focused: bool },
    CursorEntered { handle: usize },
    CursorLeft { handle: usize },
    Touch { handle: usize, event: crate::input::TouchEvent },
    CloseRequested { handle: usize },
    SetTitle { handle: usize, title: String },
    SetSize { handle: usize, width: u32, height: u32 },
    SetMinSize { handle: usize, width: Option<u32>, height: Option<u32> },
    SetMaxSize { handle: usize, width: Option<u32>, height: Option<u32> },
    SetFullscreen { handle: usize, fullscreen: Option<Fullscreen> },
    SetMaximized { handle: usize, maximized: bool },
    SetMinimized { handle: usize, minimized: bool },
    SetVisible { handle: usize, visible: bool },
    FocusWindow { handle: usize },
    SetWindowLevel { handle: usize, level: WindowLevel },
    SetDecorations { handle: usize, decorations: bool },
    SetIcon { handle: usize, icon: Icon },
    SetCursor { handle: usize, cursor: winit::window::Cursor },
}

/// 物理尺寸 → 逻辑尺寸（`high_dpi` 窗口逻辑 = 物理）。
fn logical_size(width: u32, height: u32, high_dpi: bool, scale_factor: f64) -> (u32, u32) {
    if high_dpi || scale_factor <= 0.0 {
        (width, height)
    } else {
        (
            (width as f64 / scale_factor) as u32,
            (height as f64 / scale_factor) as u32,
        )
    }
}

/// 构造「本帧跳过」的 [`DrawReport`]（保留 gpu_secs）。
fn skip_report(gpu_secs: Option<f64>, reason: DrawSkipReason) -> DrawReport {
    DrawReport {
        outcome: DrawOutcome::Skipped(reason),
        timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
    }
}

/// 窗口实例 —— 渲染线程独占，持有 surface/renderer/input 与完整帧循环。
///
/// **关键架构（第五十一轮）**：`SurfaceTexture` 从 acquire 到 present 全程是
/// `draw()` 内的局部值，不进入 Mutex/channel/winit 线程。同一 surface 最多一个
/// outstanding texture，且满足 wgpu-hal 的同线程 acquire→present 约束。
///
/// 所有公开 API 坐标系为逻辑像素（用户友好），GPU 内部使用物理像素。
pub struct VireoWindow {
    pub(crate) surface: std::cell::RefCell<wgpu::Surface<'static>>,
    instance: wgpu::Instance,
    surface_config: std::cell::RefCell<wgpu::SurfaceConfiguration>,
    renderer: std::cell::RefCell<crate::context::Renderer>,
    pub inner: Arc<winit::window::Window>,
    pub gpu: Arc<GpuContext>,
    pub mouse_pos: (f32, f32),
    logical_width: std::cell::Cell<u32>,
    logical_height: std::cell::Cell<u32>,
    high_dpi: bool,
    scale: std::cell::Cell<f32>,
    dpi_scale: std::cell::Cell<f32>,
    /// Last layout committed by `surface.configure`. FollowLayout may temporarily
    /// move the live camera away from this snapshot while the surface keeps its size.
    configured_layout: std::cell::Cell<(u32, u32, f32, f32)>,
    pub input: InputState,
    /// 该窗口初始化耗时（秒）：app.window() 内的 AA 管线预热。
    pub init_duration: f64,
    /// 用于向 winit 线程发送窗口操作事件
    event_tx: mpsc::Sender<WinitEvent>,
    /// 向 winit 线程注册输入回调
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    /// 待应用的 present mode（在 draw 开头应用）
    pending_mode: std::cell::Cell<Option<wgpu::PresentMode>>,
    /// 真正 configure 到 surface 的 present mode（仅 configure 时更新）
    applied_present_mode: std::cell::Cell<wgpu::PresentMode>,
    /// 上次 `surface.configure` 的时刻（resize 实时刷新间隔用）
    last_configure: std::cell::Cell<std::time::Instant>,
    /// 最近一次「尺寸仍与已配置值不同」的帧时刻（resize 去抖用）
    pending_resize_at: std::cell::Cell<Option<std::time::Instant>>,
    /// 上一帧观测到的窗口状态 (phys_w, phys_h, log_w, log_h, scale)——
    /// 用于判断尺寸是否仍在**移动**（相对上一帧变化才算移动，松手即停）。
    last_observed: std::cell::Cell<(u32, u32, u32, u32, f32)>,
    /// 拖动中的 resize 尺寸刷新策略。见 [`ResizeRefreshPolicy`]。
    resize_policy: std::cell::Cell<ResizeRefreshPolicy>,
    /// resize 去抖时长：尺寸稳定满此时间才一次性 configure（松手 snap）。默认
    /// [`DEFAULT_RESIZE_DEBOUNCE`]（100ms），可经 `set_resize_debounce` 覆盖。
    resize_debounce: std::cell::Cell<std::time::Duration>,
    /// 布局跟随开关（独立于 `ResizeRefreshPolicy`，默认开）：窗口尺寸已变但 surface
    /// 未重配时，每帧把 camera/逻辑尺寸更新到新窗口（`Renderer::update_layout`），
    /// 内容**实时重排**而非停在旧布局——DXGI 把旧 surface 拉伸到新窗口时正好抵消
    /// 缩放：几何和文字都按 x/y 两轴的新尺寸映射，不因宽高比变化产生额外近似。
    /// 可见误差来自窗口尺寸采样时序、整数舍入和 DPI 转换，而非单轴补偿。
    /// 关闭 = 旧行为：拖动中内容停旧逻辑布局（纯拉伸）。
    layout_follow: std::cell::Cell<bool>,
    /// 待应用的 AA 模式（在 draw 开头应用）
    pending_aa: std::cell::Cell<Option<AntiAliasing>>,
    /// 窗口 handle（在 App.windows 中的索引）
    handle: usize,
    /// 关窗事件已到达（关闭中，draw 跳过）
    closing: std::cell::Cell<bool>,
    /// 是否启用 queue completion 计时（`DrawTimings::gpu_secs`）
    gpu_timing_enabled: std::sync::atomic::AtomicBool,
    /// 上一份已完成提交的 GPU queue latency（由 `on_submitted_work_done` 写回）
    last_gpu_secs: Arc<Mutex<Option<f64>>>,
    pending_gpu_starts: Arc<Mutex<std::collections::VecDeque<std::time::Instant>>>,
    /// Outcome recorded by this window's draw call in the current update iteration.
    last_draw_outcome: std::cell::Cell<Option<DrawOutcome>>,
    presented_frames: std::cell::Cell<u64>,
    skipped_frames: std::cell::Cell<u64>,
}

impl VireoWindow {
    fn new(
        inner: Arc<winit::window::Window>,
        gpu: Arc<GpuContext>,
        surface: wgpu::Surface<'static>,
        instance: wgpu::Instance,
        surface_config: wgpu::SurfaceConfiguration,
        renderer: crate::context::Renderer,
        logical_width: u32,
        logical_height: u32,
        scale: f32,
        dpi_scale: f32,
        high_dpi: bool,
        init_duration: f64,
        event_tx: mpsc::Sender<WinitEvent>,
        cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
        handle: usize,
    ) -> Self {
        let initial_present_mode = surface_config.present_mode;
        let initial_phys = (surface_config.width, surface_config.height);
        Self {
            surface: std::cell::RefCell::new(surface),
            instance,
            surface_config: std::cell::RefCell::new(surface_config),
            renderer: std::cell::RefCell::new(renderer),
            inner,
            gpu,
            mouse_pos: (-1.0, -1.0),
            logical_width: std::cell::Cell::new(logical_width),
            logical_height: std::cell::Cell::new(logical_height),
            high_dpi,
            scale: std::cell::Cell::new(scale),
            dpi_scale: std::cell::Cell::new(dpi_scale),
            configured_layout: std::cell::Cell::new((
                logical_width,
                logical_height,
                scale,
                dpi_scale,
            )),
            input: InputState::default(),
            init_duration,
            event_tx,
            cb_tx,
            pending_mode: std::cell::Cell::new(None),
            applied_present_mode: std::cell::Cell::new(initial_present_mode),
            last_configure: std::cell::Cell::new(std::time::Instant::now()),
            pending_resize_at: std::cell::Cell::new(None),
            last_observed: std::cell::Cell::new((
                initial_phys.0,
                initial_phys.1,
                logical_width,
                logical_height,
                scale,
            )),
            resize_policy: std::cell::Cell::new(ResizeRefreshPolicy::OnRelease),
            resize_debounce: std::cell::Cell::new(DEFAULT_RESIZE_DEBOUNCE),
            layout_follow: std::cell::Cell::new(true),
            pending_aa: std::cell::Cell::new(None),
            handle,
            closing: std::cell::Cell::new(false),
            gpu_timing_enabled: std::sync::atomic::AtomicBool::new(false),
            last_gpu_secs: Arc::new(Mutex::new(None)),
            pending_gpu_starts: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            last_draw_outcome: std::cell::Cell::new(None),
            presented_frames: std::cell::Cell::new(0),
            skipped_frames: std::cell::Cell::new(0),
        }
    }

    /// Configure the surface and synchronise every size-dependent renderer state.
    /// The caller must ensure no `SurfaceTexture` is outstanding.
    fn configure_surface(&self, size: winit::dpi::PhysicalSize<u32>, now: std::time::Instant) {
        debug_assert!(size.width > 0 && size.height > 0);
        let sf = self.inner.scale_factor();
        let scale = if self.high_dpi { 1.0 } else { sf as f32 };
        let dpi_scale = sf as f32;
        let (logical_w, logical_h) = logical_size(size.width, size.height, self.high_dpi, sf);

        let mut config = self.surface_config.borrow().clone();
        config.width = size.width;
        config.height = size.height;
        self.surface.borrow().configure(&self.gpu.device, &config);

        self.applied_present_mode.set(config.present_mode);
        *self.surface_config.borrow_mut() = config;
        self.logical_width.set(logical_w);
        self.logical_height.set(logical_h);
        self.scale.set(scale);
        self.dpi_scale.set(dpi_scale);
        self.configured_layout.set((logical_w, logical_h, scale, dpi_scale));
        self.last_configure.set(now);
        self.pending_resize_at.set(None);
        self.last_observed.set((size.width, size.height, logical_w, logical_h, scale));
        self.renderer.borrow_mut().resize(
            logical_w,
            logical_h,
            size.width,
            size.height,
            scale,
            dpi_scale,
        );
    }

    /// 绘制一帧（render thread 独占 surface 帧循环）。
    ///
    /// 流程：
    /// 1. 应用 pending present mode / AA（在 acquire 前，configure 时无 outstanding st）
    /// 2. 轮询 `Window::inner_size()` / `scale_factor()`（模态循环期间尺寸事件滞后，
    ///    逐帧主动同步是可靠兜底），必要时 `surface.configure`
    /// 3. `get_current_texture` → 编码 CommandBuffer → `queue.submit` → `queue.present`
    ///
    /// `SurfaceTexture` 从 acquire 到 present 都是本函数局部值，不跨线程、不同时
    /// 存在两份，因此不违反 wgpu-hal 同线程 acquire→present 约束，也不会在 close/
    /// resize 时残留 semaphore 引用。
    ///
    /// 返回 [`DrawReport`]：`outcome` 描述本帧结局，`timings` 提供分段耗时。
    pub fn draw(
        &self,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) -> DrawReport {
        let report = self.draw_frame(clear_color, batches);
        self.last_draw_outcome.set(Some(report.outcome));
        match report.outcome {
            DrawOutcome::Presented { .. } => {
                self.presented_frames.set(self.presented_frames.get().saturating_add(1));
            }
            DrawOutcome::Skipped(_) => {
                self.skipped_frames.set(self.skipped_frames.get().saturating_add(1));
            }
            DrawOutcome::Failed(_) => {}
        }
        report
    }

    fn draw_frame(
        &self,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) -> DrawReport {
        let gpu_secs = self.last_gpu_secs.lock().unwrap().take();
        if self.closing.get() {
            return DrawReport {
                outcome: DrawOutcome::Skipped(DrawSkipReason::Closing),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
            };
        }
        let trace = std::env::var_os("VIREO_DRAW_TRACE").is_some();
        let t_trace = std::time::Instant::now();
        let mut configure_secs = 0.0;

        // 1) 应用 pending present mode（改 config 即可；尺寸同步在下方统一 configure）
        if let Some(mode) = self.pending_mode.take() {
            let caps = self.surface.borrow().get_capabilities(&self.gpu.adapter);
            let actual = Self::resolve_present_mode(mode, &caps.present_modes);
            self.surface_config.borrow_mut().present_mode = actual;
        }
        // 应用 pending AA 变化（不触碰 surface；重建 msaa/ds 纹理）
        if let Some(aa) = self.pending_aa.take() {
            let sc = aa.sample_count();
            let atc = aa.alpha_to_coverage();
            let ssaa = aa.is_ssaa();
            let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
            let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
            self.renderer.borrow_mut().update_aa(aa);
        }

        // 2) 逐帧轮询实际尺寸 + 缩放（模态循环期间最可靠）。
        //    resize 刷新策略（`ResizeRefreshPolicy`）：任何策略下，尺寸停止变化满
        //    去抖时长（`set_resize_debounce`，默认 100ms）都会一次性 configure
        //    （松手 snap）；`EveryFrame`/`Periodic` 在尺寸持续变化时额外实时
        //    configure。configure 阻塞在 wgpu-hal DX12 present queue 排空（~50-80ms），
        //    实时刷新会掉帧——这是用户显式选择。
        //    present mode 变化不进去抖，下一帧立即 configure。
        let size = self.inner.inner_size();
        if size.width == 0 || size.height == 0 {
            return DrawReport {
                outcome: DrawOutcome::Skipped(DrawSkipReason::ZeroSized),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
            };
        }
        let sf = self.inner.scale_factor();
        let new_scale = if self.high_dpi { 1.0 } else { sf as f32 };
        let dpi_scale = sf as f32;
        let (logical_w, logical_h) = logical_size(size.width, size.height, self.high_dpi, sf);
        let mut configured_this_frame = false;
        let mut follow_pending = false;
        {
            let sc = self.surface_config.borrow();
            let size_drifted = sc.width != size.width
                || sc.height != size.height
                || logical_w != self.logical_width.get()
                || logical_h != self.logical_height.get()
                || new_scale != self.scale.get();
            let mode_drifted = sc.present_mode != self.applied_present_mode.get();
            drop(sc);
            let now = std::time::Instant::now();
            // 「移动」= 相对上一帧观测值有变化。只有移动才刷新去抖计时；松手后
            // 尺寸不变 → 计时器不再刷新 → 开始老化，满去抖时长即 snap。
            let moved = size_drifted
                && self.last_observed.get() != (size.width, size.height, logical_w, logical_h, new_scale);
            self.last_observed.set((size.width, size.height, logical_w, logical_h, new_scale));
            if moved {
                self.pending_resize_at.set(Some(now));
            }
            let refresh = resize_refresh(
                size_drifted,
                self.pending_resize_at.get(),
                now,
                self.resize_debounce.get(),
                self.resize_policy.get(),
                self.last_configure.get(),
            );
            let need_configure = size_drifted && refresh != ResizeRefresh::None
                || mode_drifted;
            if need_configure {
                configured_this_frame = true;
                let t_conf = std::time::Instant::now();
                if trace {
                    let label = match refresh {
                        ResizeRefresh::Stable => "stable",
                        ResizeRefresh::Live => "live",
                        ResizeRefresh::None => "mode",
                    };
                    eprintln!("[draw] conf-start size={}x{} ({} {:?})", size.width, size.height,
                        label, now);
                }
                // wgpu 30 configure 返回 ()，错误经全局 error handler 上报。
                self.configure_surface(size, now);
                configure_secs = t_conf.elapsed().as_secs_f64();
                if trace {
                    eprintln!("[draw] conf-end {:?}us", t_conf.elapsed().as_micros());
                }
            } else if size_drifted && self.layout_follow.get() {
                // layout_follow（独立开关，默认开）：窗口已变但 surface 未重配——
                // 内容要实时重排而非停在旧布局。真正更新 camera 推迟到 acquire 之后
                // （见下方 `follow-layout` 段）：acquire 可能等待 swapchain 空位，因此返回后
                // re-poll 通常能取得更接近本帧 present 时刻的尺寸，但不保证前帧已上屏。
                // 配合 frame_latency=1 降低 camera 的采样时差，不能保证消除拖动跳动。
                // 这里只登记尺寸漂移状态 + 置 follow_pending 标记。
                // DXGI 把旧 surface（S）拉伸到新窗口（W）：camera 用新逻辑尺寸 →
                // 复合映射 uniform dpi、几何零畸变。
                // 文字不重光栅化（保持 scale=dpi → 图集 cache key 稳定），而是把
                // 文字 shader 的 screen_resolution 覆盖为「虚拟新物理尺寸」
                // （新逻辑 × dpi）：NDC = 2*px/(L*dpi) - 1 与几何相机 2x/L - 1
                // 对齐。x/y 分别使用新宽/高，宽高比变化时也逐轴映射；残余误差来自
                // 尺寸采样时序、逻辑/物理整数舍入及 DPI 转换。
                // 不重置 pending_resize_at：计时继续老化，松手满 debounce 触发
                // 上方 Stable 分支一次性 configure（snap）。
                if trace {
                    eprintln!("[draw] follow-layout(drift) {}x{}", logical_w, logical_h);
                }
                self.logical_width.set(logical_w);
                self.logical_height.set(logical_h);
                self.scale.set(new_scale);
                self.dpi_scale.set(dpi_scale);
                follow_pending = true;
            } else {
                // 不跟随 / 尺寸未漂移：清掉可能残留的虚拟 viewport（配置/稳定路径已由
                // `Renderer::resize` 清，这里兜底防 follow 中途关闭后残留）。
                self.renderer.borrow_mut().set_text_viewport_override(None);
            }
        }

        // 3) acquire
        let t1 = std::time::Instant::now();
        if trace {
            eprintln!("[draw] acq-start");
        }
        let acquired = self.surface.borrow().get_current_texture();
        let (st, suboptimal) = match acquired {
            wgpu::CurrentSurfaceTexture::Success(st) => (st, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(st) => (st, true),
            wgpu::CurrentSurfaceTexture::Outdated => {
                // 重配后再试；本帧跳过
                let size = self.inner.inner_size();
                if size.width == 0 || size.height == 0 {
                    return DrawReport {
                        outcome: DrawOutcome::Skipped(DrawSkipReason::ZeroSized),
                        timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
                    };
                }
                let t_conf = std::time::Instant::now();
                self.configure_surface(size, t_conf);
                return DrawReport {
                    outcome: DrawOutcome::Skipped(DrawSkipReason::SurfaceReconfigured),
                    timings: DrawTimings {
                        configure_secs: t_conf.elapsed().as_secs_f64(),
                        gpu_secs,
                        ..DrawTimings::default()
                    },
                };
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                return skip_report(gpu_secs, DrawSkipReason::Timeout);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return skip_report(gpu_secs, DrawSkipReason::Occluded);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // surface 丢失：用现有 instance+window 重建 surface 并重配，本帧跳过
                let size = self.inner.inner_size();
                if size.width == 0 || size.height == 0 {
                    return DrawReport {
                        outcome: DrawOutcome::Skipped(DrawSkipReason::ZeroSized),
                        timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
                    };
                }
                if let Some(new_surface) = self.recreate_surface() {
                    *self.surface.borrow_mut() = new_surface;
                    let t_conf = std::time::Instant::now();
                    self.configure_surface(size, t_conf);
                    configure_secs = t_conf.elapsed().as_secs_f64();
                }
                return DrawReport {
                    outcome: DrawOutcome::Skipped(DrawSkipReason::SurfaceReconfigured),
                    timings: DrawTimings { configure_secs, gpu_secs, ..DrawTimings::default() },
                };
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return skip_report(gpu_secs, DrawSkipReason::SurfaceReconfigured);
            }
        };
        if trace {
            eprintln!("[draw] acq-end {:?}us", t1.elapsed().as_micros());
        }
        let acquire_secs = t1.elapsed().as_secs_f64();

        // 4a) follow 布局跟随（实际执行）：acquire 可能等待 swapchain 空位；返回后
        //     re-poll inner_size 通常比 acquire 前的样本更接近本帧 present 时刻，但 acquire
        //     不保证前帧已上屏。配合 frame_latency=1 降低 camera 时差与拖动跳动，仍可能
        //     留下约一个刷新周期内的采样差异。
        //     只在 follow_pending（step 2 登记的漂移）且确实仍漂移时更新；否则清残留
        //     的虚拟 viewport。
        if follow_pending {
            let size = self.inner.inner_size();
            let sf = self.inner.scale_factor();
            let new_scale = if self.high_dpi { 1.0 } else { sf as f32 };
            let dpi_scale = sf as f32;
            let (logical_w, logical_h) = logical_size(size.width, size.height, self.high_dpi, sf);
            let still_drifted = {
                let sc = self.surface_config.borrow();
                sc.width != size.width
                    || sc.height != size.height
                    || logical_w != self.logical_width.get()
                    || logical_h != self.logical_height.get()
                    || new_scale != self.scale.get()
            };
            if still_drifted && size.width != 0 && size.height != 0 {
                if trace {
                    eprintln!("[draw] follow-layout(acq) {}x{}", logical_w, logical_h);
                }
                self.logical_width.set(logical_w);
                self.logical_height.set(logical_h);
                self.scale.set(new_scale);
                self.dpi_scale.set(dpi_scale);
                let vw = (logical_w as f32 * new_scale).round().max(1.0) as u32;
                let vh = (logical_h as f32 * new_scale).round().max(1.0) as u32;
                self.renderer.borrow_mut().update_layout(
                    logical_w, logical_h, new_scale, dpi_scale,
                );
                self.renderer.borrow_mut().set_text_viewport_override(Some((vw, vh)));
            } else {
                // 松手尺寸回稳但尚未 snap（debounce 未满）：不再重排，清虚拟 viewport，
                // 内容停在当前布局，等 Stable 分支一次性 configure。
                self.renderer.borrow_mut().set_text_viewport_override(None);
            }
        }

        // 4b) 编码
        let view = st.texture.create_view(&Default::default());
        let target = crate::context::RenderTarget::from_texture_view(view);
        let batch_refs: Vec<&DrawBatch> = batches.iter().copied().collect();
        let t2 = std::time::Instant::now();
        let cmd_buf = self.renderer.borrow().draw(&target, clear_color, &batch_refs);
        // 5) submit + 提交完成计时
        let timing_enabled = self.gpu_timing_enabled.load(std::sync::atomic::Ordering::Acquire);
        if timing_enabled {
            self.pending_gpu_starts.lock().unwrap().push_back(std::time::Instant::now());
        }
        self.gpu.queue.submit([cmd_buf]);
        if timing_enabled {
            let last_gpu_secs = self.last_gpu_secs.clone();
            let pending_gpu_starts = self.pending_gpu_starts.clone();
            self.gpu.queue.on_submitted_work_done(move || {
                let start = pending_gpu_starts.lock().unwrap().pop_front();
                if let Some(start) = start {
                    *last_gpu_secs.lock().unwrap() = Some(start.elapsed().as_secs_f64());
                }
            });
        }
        let encode_secs = t2.elapsed().as_secs_f64();

        // 6) present
        let t3 = std::time::Instant::now();
        self.gpu.queue.present(st);
        let present_secs = t3.elapsed().as_secs_f64();

        if trace {
            eprintln!("[draw] total={:?}us conf={} acq={:?}us enc+sub={:?}us pres={:?}us gpu={:?} suboptimal={}",
                t_trace.elapsed().as_micros(),
                configured_this_frame,
                (acquire_secs * 1e6) as u64,
                (encode_secs * 1e6) as u64,
                (present_secs * 1e6) as u64,
                gpu_secs.map(|v| v * 1e6),
                suboptimal);
        }

        DrawReport {
            outcome: DrawOutcome::Presented { suboptimal },
            timings: DrawTimings {
                configure_secs,
                acquire_secs,
                encode_secs,
                present_secs,
                gpu_secs,
            },
        }
    }

    /// 解析请求的 present mode：
    /// - 后端能力包含则直接用；
    /// - `AutoVsync` 是 wgpu 别名（DX12/Vulkan 下映射到 `Fifo`），`get_capabilities`
    ///   只返回后端具体模式、永不列出别名本身，直接接受；
    /// - 其余不支持时回退 `AutoVsync` 并告警。
    fn resolve_present_mode(
        requested: wgpu::PresentMode,
        supported: &[wgpu::PresentMode],
    ) -> wgpu::PresentMode {
        if supported.contains(&requested)
            || matches!(requested, wgpu::PresentMode::AutoVsync)
        {
            requested
        } else {
            eprintln!("vireo: PresentMode {requested:?} not supported, falling back to AutoVsync");
            wgpu::PresentMode::AutoVsync
        }
    }

    /// 用保留的 Instance + Window 重建 surface（`CurrentSurfaceTexture::Lost`）。
    fn recreate_surface(&self) -> Option<wgpu::Surface<'static>> {
        self.instance.create_surface(self.inner.clone()).ok()
    }

    /// 启用 queue completion 计时，用于诊断 GPU 竞争和提交排队。
    /// 结果通过下一帧的 [`DrawTimings::gpu_secs`] 返回。
    pub fn set_gpu_timing(&self, enabled: bool) {
        self.gpu_timing_enabled.store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数（渲染器真实统计）。
    /// `preserve_order=false` 重排合并后此值下降（如 bench 场景 3 混合 1000→2）。
    pub fn last_draw_calls(&self) -> u32 {
        self.renderer.borrow().last_draw_calls()
    }

    /// 强制 GPU 端 PSO 编译（DX12 懒编译需要）。
    /// 新流程下 draw 自带尺寸同步 + acquire + present，首帧 PSO 编译卡一次可接受。
    /// 保留此函数为 no-op 以维持 API 兼容。
    pub fn preheat(&self, _clear_color: crate::color::Color) {
        // no-op
    }

    /// 调整窗口大小（size 为物理像素）。
    ///
    /// 仅同步逻辑尺寸（用户代码当帧即可读到新 `metrics()`）。真正的
    /// `surface.configure` / renderer 视图更新由 `draw` 的逐帧尺寸同步完成——
    /// 拖动/模态循环期间 Resized 事件可能滞后，逐帧轮询 `inner_size` 才是可靠兜底。
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 { return; }
        let sf = self.inner.scale_factor();
        let mut logical_w = width;
        let mut logical_h = height;
        if !self.high_dpi && sf > 0.0 {
            logical_w = (width as f64 / sf) as u32;
            logical_h = (height as f64 / sf) as u32;
        }
        self.logical_width.set(logical_w);
        self.logical_height.set(logical_h);
        self.scale.set(if self.high_dpi { 1.0 } else { sf as f32 });
        self.dpi_scale.set(sf as f32);
    }

    /// 当前逻辑尺寸/缩放只读快照（用户坐标系）。
    pub fn metrics(&self) -> WindowMetrics {
        WindowMetrics {
            width: self.logical_width.get(),
            height: self.logical_height.get(),
            scale_factor: self.scale.get() as f64,
        }
    }

    /// Whether the observed window metrics differ from the configured surface/layout.
    pub fn resize_pending(&self) -> bool {
        let size = self.inner.inner_size();
        let sf = self.inner.scale_factor();
        let scale = if self.high_dpi { 1.0 } else { sf as f32 };
        let (logical_w, logical_h) = logical_size(size.width, size.height, self.high_dpi, sf);
        let config = self.surface_config.borrow();
        config.width != size.width
            || config.height != size.height
            || self.configured_layout.get() != (logical_w, logical_h, scale, sf as f32)
    }

    /// Number of successful `queue.present` calls made by this window.
    pub fn presented_frames(&self) -> u64 {
        self.presented_frames.get()
    }

    /// Number of draw attempts skipped before present.
    pub fn skipped_frames(&self) -> u64 {
        self.skipped_frames.get()
    }

    /// 获取当前鼠标位置（窗口用户坐标系，即 WindowDesc 传入的宽高范围）
    pub fn mouse_pos(&self) -> (f32, f32) {
        self.mouse_pos
    }

    /// 获取当前投影矩阵（逻辑像素）
    pub fn projection(&self) -> glam::Mat4 {
        glam::camera::rh::proj::opengl::orthographic(
            0.0,
            self.logical_width.get() as f32,
            self.logical_height.get() as f32,
            0.0,
            -1.0,
            1.0,
        )
    }

    /// 获取共享 GPU 上下文
    pub fn gpu(&self) -> &Arc<GpuContext> {
        &self.gpu
    }

    // ------ 输入状态轮询 API ------

    pub fn key_down(&self, key: crate::input::KeyCode) -> bool {
        self.input.keys_down.borrow().contains(&key)
    }

    pub fn any_key_down(&self) -> bool {
        !self.input.keys_down.borrow().is_empty()
    }

    pub fn mouse_down(&self, button: crate::input::MouseButton) -> bool {
        self.input.mouse_buttons_down.borrow().contains(&button)
    }

    pub fn mouse_left(&self) -> bool {
        self.mouse_down(crate::input::MouseButton::Left)
    }

    pub fn mouse_right(&self) -> bool {
        self.mouse_down(crate::input::MouseButton::Right)
    }

    pub fn modifiers(&self) -> crate::input::Modifiers {
        *self.input.modifiers.borrow()
    }

    pub fn ctrl_down(&self) -> bool {
        self.input.modifiers.borrow().ctrl()
    }

    pub fn shift_down(&self) -> bool {
        self.input.modifiers.borrow().shift()
    }

    pub fn alt_down(&self) -> bool {
        self.input.modifiers.borrow().alt()
    }

    pub fn take_scroll(&self) -> (f32, f32) {
        let mut delta = self.input.scroll_delta.borrow_mut();
        let result = delta.line;
        delta.line = (0.0, 0.0);
        result
    }

    pub fn take_scroll_pixel(&self) -> (f32, f32) {
        let mut delta = self.input.scroll_delta.borrow_mut();
        let result = delta.pixel;
        delta.pixel = (0.0, 0.0);
        result
    }

    pub fn focused(&self) -> bool {
        *self.input.focused.borrow()
    }

    pub fn cursor_inside(&self) -> bool {
        *self.input.cursor_inside.borrow()
    }

}

/// 应用管理器 —— 管理 GPU 上下文、窗口、纹理。
/// 构造后在 `run()` 之前配置窗口/纹理/离屏画布，`run()` 将 `self` 移动到渲染线程。
pub struct App {
    pub window_descs: Vec<WindowDesc>,
    /// `Vec<Option<VireoWindow>>`，以 handle 为索引。关闭的窗口为 `None`。
    pub windows: Vec<Option<VireoWindow>>,
    pub gpu: Arc<GpuContext>,
    instance: Option<wgpu::Instance>,
    /// 设备丢失标志（预留：wgpu 30 的 `get_current_texture` 无 `DeviceLost` 变体，
    /// 当前无代码路径置位；未来接 `Device::set_device_lost_callback` 时使用）。
    /// 渲染循环每帧读它，置位则干净终止。
    device_lost: Arc<std::sync::atomic::AtomicBool>,
    /// 稳定 handle → winit WindowId。handle 由 `App::window()` 分配，
    /// 在 run() 中被取出给 winit 线程用。
    handle_to_id: FxHashMap<u64, WindowId>,
    /// 下一个待分配的 handle（单调递增；`App::window()` 自增）。
    next_handle: u64,
    close_hooks: FxHashMap<u64, Option<Box<dyn FnOnce() + Send>>>,
    /// 输入事件回调集合（按 handle 索引，run() 后迁移到 winit 线程）
    callbacks: FxHashMap<u64, crate::input::InputCallbacks>,
    default_icon: Option<Icon>,
    textures: Vec<Texture>,
    offscreens: Vec<OffscreenCanvas>,
    pub frame_count: u64,
    /// 相邻两次 update/`on_frame` 调用的间隔（秒），瞬时值。第一帧为 0。
    /// 它不是相邻 displayed frame 的呈现间隔。
    pub frame_time: f64,
    /// update/`on_frame` 调用频率的滑动窗口平均值。它不是 displayed FPS；surface acquire、
    /// present mode、跳帧或合成器可能使实际显示频率不同。第一帧为 0。
    pub fps: f64,
    /// App::new 内部耗时（秒）：GPU 设备、shader 模块、bind group layout 构造。
    pub init_duration: f64,
    /// 各 `app.window()` 调用的 init_duration（秒），按调用顺序入队。
    /// run() 中按序出队给 winit 线程用。
    window_init_durations: Vec<f64>,
    /// 最近若干帧的间隔，用于平滑 FPS。
    fps_samples: Vec<f64>,
    last_frame: std::time::Instant,
    deferred_tasks: RefCell<Vec<DeferredTask>>,
}

/// 延迟执行的任务，由 [`App::after_frames`] / [`App::after_secs`] 注册。
pub struct DeferredTask {
    kind: DeferredTaskKind,
    pub(crate) f: Box<dyn FnOnce() + Send>,
}

/// 在 `App::after_frames` / `App::after_secs` 内部使用。
/// 用户不直接构造。
#[doc(hidden)]
pub struct DeferredTaskGuard;

impl DeferredTask {
    fn is_ready(&self, frame_count: u64) -> bool {
        match self.kind {
            DeferredTaskKind::AfterFrames(target) => frame_count >= target,
            DeferredTaskKind::AfterSecs(wakeup) => std::time::Instant::now() >= wakeup,
        }
    }
}

enum DeferredTaskKind {
    AfterFrames(u64),
    AfterSecs(std::time::Instant),
}

impl App {
    /// 创建 App。构造时即初始化 GPU 设备，可在 run() 之前加载纹理等资源。
    pub fn new() -> Self {
        let init_start = std::time::Instant::now();
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        );
        let gpu = Arc::new(GpuContext::new(&instance));
        let default_icon = std::fs::read("logo.png")
            .ok()
            .and_then(|data| image::load_from_memory(&data).ok())
            .map(|img| {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Icon::from_rgba(rgba.into_raw(), w, h).ok()
            })
            .flatten();
        let init_duration = init_start.elapsed().as_secs_f64();
        Self {
            window_descs: Vec::new(),
            windows: Vec::new(),
            gpu,
            instance: Some(instance),
            device_lost: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            handle_to_id: FxHashMap::default(),
            next_handle: 0,
            close_hooks: FxHashMap::default(),
            callbacks: FxHashMap::default(),
            default_icon,
            textures: Vec::new(),
            offscreens: Vec::new(),
            frame_count: 0,
            frame_time: 0.0,
            fps: 0.0,
            init_duration,
            window_init_durations: Vec::new(),
            fps_samples: Vec::with_capacity(FPS_SAMPLE_CAP),
            last_frame: std::time::Instant::now(),
            deferred_tasks: RefCell::new(Vec::new()),
        }
    }

    /// 创建离屏画布。与 window() 对称，可在 run() 之前调用。
    /// 同步预热 AA 对应的 SDF + geo 管线，构造耗时由 `OffscreenCanvas::init_duration()` 暴露。
    pub fn offscreen(&mut self, width: u32, height: u32, aa: AntiAliasing) -> OffscreenIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(aa, self.gpu.supported_sample_counts());
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        let idx = self.offscreens.len();
        let mut offscreen = OffscreenCanvas::with_aa(&self.gpu, width, height, aa, init_duration);
        offscreen.index = OffscreenIndex(idx);
        self.offscreens.push(offscreen);
        OffscreenIndex(idx)
    }

    /// 根据索引获取离屏画布引用
    pub fn offscreen_ref(&self, idx: &OffscreenIndex) -> Option<&OffscreenCanvas> {
        self.offscreens.get(idx.0)
    }

    /// 从文件加载纹理（存储在 App 中管理生命周期），返回纹理索引。
    /// 读取或解码失败时会打印错误并返回一个“missing”棋盘纹理（不返回 Err）。
    pub fn load_texture(&mut self, path: impl AsRef<std::path::Path>) -> usize {
        let tex = Texture::from_file(path, &self.gpu);
        let idx = self.textures.len();
        self.textures.push(tex);
        idx
    }

    /// 根据索引获取已加载的纹理
    pub fn texture(&self, index: usize) -> Option<&Texture> {
        self.textures.get(index)
    }

    /// 配置一个待创建的窗口。可选 on_close 钩子在窗口被关闭时调用。必须在 run() 之前调用。
    /// 同步预热窗口 AA 对应的 SDF + geo 管线，并把 AA clamp 到硬件上限（避免 wgpu panic）。
    /// 构造耗时在 `App::run` 创建窗口后由 `VireoWindow::init_duration()` 暴露。
    pub fn window(&mut self, mut desc: WindowDesc, on_close: Option<impl FnOnce() + Send + 'static>) -> WindowIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(desc.anti_aliasing, self.gpu.supported_sample_counts());
        desc.anti_aliasing = aa;
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        self.window_init_durations.push(init_duration);
        let handle = self.next_handle;
        self.next_handle += 1;
        self.window_descs.push(desc);
        self.close_hooks.insert(handle, on_close.map(|f| Box::new(f) as Box<dyn FnOnce() + Send>));
        WindowIndex::new(handle)
    }

    // ------ 输入事件回调注册（winit 线程 invoke，无需 +Send）------

    pub fn on_key_down(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_key_down.push(Box::new(callback));
        self
    }

    pub fn on_key_up(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_key_up.push(Box::new(callback));
        self
    }

    pub fn on_mouse_down(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_mouse_down.push(Box::new(callback));
        self
    }

    pub fn on_mouse_up(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_mouse_up.push(Box::new(callback));
        self
    }

    pub fn on_scroll(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::MouseScrollEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_scroll.push(Box::new(callback));
        self
    }

    pub fn on_cursor_entered(&mut self, handle: WindowIndex, callback: impl FnOnce() + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_cursor_entered.push(Box::new(callback));
        self
    }

    pub fn on_cursor_left(&mut self, handle: WindowIndex, callback: impl FnOnce() + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_cursor_left.push(Box::new(callback));
        self
    }

    pub fn on_touch(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::TouchEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_touch.push(Box::new(callback));
        self
    }

    pub fn on_focus_gained(&mut self, handle: WindowIndex, callback: impl FnOnce() + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_focus_gained.push(Box::new(callback));
        self
    }

    pub fn on_focus_lost(&mut self, handle: WindowIndex, callback: impl FnOnce() + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_focus_lost.push(Box::new(callback));
        self
    }

    pub fn on_modifiers_changed(&mut self, handle: WindowIndex, callback: impl FnMut(crate::input::Modifiers) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_modifiers_changed.push(Box::new(callback));
        self
    }

    /// 注册一个延迟 `frames` 帧后执行的闭包。
    /// frame 计数以 `render_on_frame` 循环的帧为单位，首次调用 `on_frame` 时 `frame_count` 为 1。
    pub fn after_frames<F: FnOnce() + Send + 'static>(&self, frames: u64, f: F) {
        let target = self.frame_count + frames;
        self.deferred_tasks.borrow_mut().push(DeferredTask {
            kind: DeferredTaskKind::AfterFrames(target),
            f: Box::new(f),
        });
    }

    /// 注册一个延迟 `secs` 秒后执行的闭包（墙钟时间）。
    pub fn after_secs<F: FnOnce() + Send + 'static>(&self, secs: f64, f: F) {
        self.deferred_tasks.borrow_mut().push(DeferredTask {
            kind: DeferredTaskKind::AfterSecs(
                std::time::Instant::now() + std::time::Duration::from_secs_f64(secs),
            ),
            f: Box::new(f),
        });
    }

    /// 启动事件循环 + 渲染线程。
    /// winit 线程只负责任何操作，渲染线程持有 `App` + `on_frame` 独立运行。
    /// 闭包签名: FnMut(&App) -> bool，返回 true 继续循环，false 退出。
    pub fn run<F: FnMut(&App) -> bool + Send + 'static>(mut self, on_frame: F) {
        let event_loop = EventLoop::new().unwrap();

        let window_init_durations = std::mem::take(&mut self.window_init_durations);
        let window_descs: Vec<_> = self.window_descs.drain(..).collect();
        let close_hooks = std::mem::take(&mut self.close_hooks);
        let default_icon = self.default_icon.take();
        // 保留 self.instance（渲染线程重建 surface 需要）；Runner 拿 clone。
        let instance = self.instance.clone().expect("instance already taken");
        self.handle_to_id.clear();
        self.windows.clear();

        // 提取回调集合：从 App 取出后移入 winit 线程 Runner。
        let window_callbacks: Vec<crate::input::InputCallbacks> = {
            let n = window_descs.len();
            let mut cbs: Vec<crate::input::InputCallbacks> = Vec::with_capacity(n);
            for i in 0..n {
                cbs.push(self.callbacks.remove(&(i as u64)).unwrap_or_default());
            }
            cbs
        };

        let (event_tx, event_rx) = mpsc::channel::<WinitEvent>();
        let render_event_tx = event_tx.clone();
        let (cb_tx, cb_rx) = mpsc::channel::<(usize, crate::input::InputCallbacks)>();
        // 渲染线程 → winit 线程的终止请求（on_frame 返回 false / 设备丢失）。
        let (exit_tx, exit_rx) = mpsc::channel::<()>();

        // 渲染线程：持有 App + on_frame，处理事件 + 用户代码 + 渲染。
        let expected_windows = window_descs.len();
        // Clone GpuContext Arc for Runner（winit 线程只在创建窗口时用 device/queue 初始化 surface）
        let gpu_for_runner = self.gpu.clone();
        let device_lost = self.device_lost.clone();
        let render_thread = std::thread::Builder::new()
            .name("vireo-render".into())
            .spawn(move || {
                render_on_frame(
                    self,
                    on_frame,
                    render_event_tx,
                    event_rx,
                    cb_tx,
                    exit_tx,
                    device_lost,
                    expected_windows,
                );
            })
            .expect("failed to spawn render thread");

        // Winit 线程：仅创建窗口和转发事件。
        struct Runner {
    event_tx: mpsc::Sender<WinitEvent>,
    /// 接收渲染线程发来的输入回调注册
    cb_rx: mpsc::Receiver<(usize, crate::input::InputCallbacks)>,
    /// 接收渲染线程发来的终止请求
    exit_rx: mpsc::Receiver<()>,
            window_descs: Vec<WindowDesc>,
            id_to_handle: FxHashMap<WindowId, usize>,
            close_hooks: FxHashMap<u64, Option<Box<dyn FnOnce() + Send>>>,
            window_callbacks: Vec<crate::input::InputCallbacks>,
            default_icon: Option<Icon>,
            window_init_durations: Vec<f64>,
            instance: wgpu::Instance,
            /// 用于 winit 线程创建/初始化 surface（后续帧循环全在渲染线程）
            gpu: Arc<GpuContext>,
            created: bool,
            alive_handles: usize,
        }

        // 辅助：从 Runner 获取 handle（panic-safe）
        impl Runner {
            fn handle_for(&self, window_id: WindowId) -> Option<usize> {
                self.id_to_handle.get(&window_id).copied()
            }

            fn send(&self, event: WinitEvent) {
                let _ = self.event_tx.send(event);
            }

            fn create_attrs(desc: &WindowDesc, default_icon: &Option<Icon>) -> WindowAttributes {
                let size: winit::dpi::Size = match desc.scale_factor_override {
                    Some(_) => winit::dpi::PhysicalSize::new(desc.width, desc.height).into(),
                    None => winit::dpi::LogicalSize::new(desc.width, desc.height).into(),
                };
                let mut attrs = WindowAttributes::default()
                    .with_title(&desc.title)
                    .with_inner_size(size)
                    .with_resizable(desc.resizable)
                    .with_maximized(desc.maximized)
                    .with_visible(desc.visible)
                    .with_transparent(desc.transparent)
                    .with_decorations(desc.decorations)
                    .with_window_level(desc.window_level)
                    .with_content_protected(desc.content_protected)
                    .with_active(desc.active)
                    .with_blur(desc.blur)
                    .with_cursor(desc.cursor.clone())
                    .with_enabled_buttons(desc.enabled_buttons);
                if let Some((w, h)) = desc.min_size {
                    attrs = attrs.with_min_inner_size(LogicalSize::new(w, h));
                }
                if let Some((w, h)) = desc.max_size {
                    attrs = attrs.with_max_inner_size(LogicalSize::new(w, h));
                }
                if let Some((x, y)) = desc.position {
                    attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
                }
                if let Some(ref fs) = desc.fullscreen {
                    attrs = attrs.with_fullscreen(Some(fs.clone()));
                }
                let icon = desc.window_icon.as_ref().or(default_icon.as_ref());
                if let Some(icon) = icon {
                    attrs = attrs.with_window_icon(Some(icon.clone()));
                }
                if let Some(theme) = desc.theme {
                    attrs = attrs.with_theme(Some(theme));
                }
                if let Some((w, h)) = desc.resize_increments {
                    attrs = attrs.with_resize_increments(LogicalSize::new(w, h));
                }
                attrs
            }
        }

        impl ApplicationHandler for Runner {
            fn resumed(&mut self, event_loop: &ActiveEventLoop) {
                if self.created {
                    return;
                }
                self.created = true;

                for (handle, desc) in self.window_descs.iter().enumerate() {
                    let attrs = Self::create_attrs(desc, &self.default_icon);
                    let window = Arc::new(
                        event_loop.create_window(attrs).unwrap(),
                    );
                    let surface = self.instance.create_surface(window.clone()).unwrap();
                    let window_id = window.id();

                    // ---- 在 winit 线程上创建 surface + 初始 configure ----
                    // surface 的 create_surface 必须紧跟窗口创建；首次 configure 在此建立
                    // 合法 swapchain。后续尺寸同步/重新 configure 由渲染线程 draw() 完成。
                    // `SurfaceTexture` 从不跨线程：acquire→present 全在渲染线程 draw() 内，
                    // 满足 wgpu-hal 同线程约束（第三十三/三十四轮的 handoff 失败不重演）。
                    let scale = if desc.scale_factor_override.is_some() {
                        1.0
                    } else {
                        window.scale_factor() as f32
                    };
                    let dpi = window.scale_factor() as f32;
                    let renderer = Renderer::new(
                        self.gpu.clone(),
                        desc.width,
                        desc.height,
                        window.inner_size().width,
                        window.inner_size().height,
                        scale,
                        desc.anti_aliasing,
                        dpi,
                    );

                    let caps = surface.get_capabilities(&self.gpu.adapter);
                    let alpha_mode = if desc.transparent {
                        if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PostMultiplied) {
                            wgpu::CompositeAlphaMode::PostMultiplied
                        } else if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                            wgpu::CompositeAlphaMode::PreMultiplied
                        } else {
                            wgpu::CompositeAlphaMode::Auto
                        }
                    } else {
                        wgpu::CompositeAlphaMode::Auto
                    };
                    let fmt = if caps.formats.contains(&self.gpu.surface_format) {
                        self.gpu.surface_format
                    } else {
                        caps.formats[0]
                    };
                    let surface_config = wgpu::SurfaceConfiguration {
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        format: fmt,
                        width: window.inner_size().width.max(1),
                        height: window.inner_size().height.max(1),
                        present_mode: desc.present_mode,
                        alpha_mode,
                        view_formats: vec![],
                        // 在途帧封顶到 1：follow 拖动态降低 camera 尺寸样本与 present
                        // 之间的时差，减轻 vsync 下的布局跳动，但不保证完全消除。
                        desired_maximum_frame_latency: 1,
                        color_space: wgpu::SurfaceColorSpace::Auto,
                    };
                    surface.configure(&self.gpu.device, &surface_config);

                    let init_duration = if handle < self.window_init_durations.len() {
                        self.window_init_durations[handle]
                    } else {
                        0.0
                    };

                    self.id_to_handle.insert(window_id, handle);
                    self.alive_handles += 1;

                    self.send(WinitEvent::WindowCreated {
                        handle,
                        window,
                        surface,
                        surface_config,
                        renderer,
                        logical_width: desc.width,
                        logical_height: desc.height,
                        scale,
                        dpi_scale: dpi,
                        high_dpi: desc.scale_factor_override.is_some(),
                        init_duration,
                    });
                }
            }

            fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
                // 渲染线程请求终止（on_frame 返回 false / 设备丢失）
                while self.exit_rx.try_recv().is_ok() {
                    event_loop.exit();
                }
                // Drain callback registrations sent from render thread
                while let Ok((handle, mut reg)) = self.cb_rx.try_recv() {
                    if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                        cbs.on_key_down.extend(reg.on_key_down.drain(..));
                        cbs.on_key_up.extend(reg.on_key_up.drain(..));
                        cbs.on_mouse_down.extend(reg.on_mouse_down.drain(..));
                        cbs.on_mouse_up.extend(reg.on_mouse_up.drain(..));
                        cbs.on_scroll.extend(reg.on_scroll.drain(..));
                        cbs.on_cursor_entered.extend(reg.on_cursor_entered.drain(..));
                        cbs.on_cursor_left.extend(reg.on_cursor_left.drain(..));
                        cbs.on_touch.extend(reg.on_touch.drain(..));
                        cbs.on_focus_gained.extend(reg.on_focus_gained.drain(..));
                        cbs.on_focus_lost.extend(reg.on_focus_lost.drain(..));
                        cbs.on_modifiers_changed.extend(reg.on_modifiers_changed.drain(..));
                    }
                }
                event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
            }

            fn window_event(
                &mut self,
                _event_loop: &ActiveEventLoop,
                window_id: WindowId,
                event: WindowEvent,
            ) {
                let Some(handle) = self.handle_for(window_id) else { return };

                match event {
                    WindowEvent::CloseRequested => {
                        if let Some(hook_opt) = self.close_hooks.get_mut(&(handle as u64)) {
                            if let Some(h) = hook_opt.take() { h(); }
                        }
                        // SurfaceTexture 全部由渲染线程在 draw() 内 acquire→present。
                        // owner 只发送关闭请求；最后一个 VireoWindow 由渲染线程 drop 后，
                        // 渲染线程会通过 exit_tx 确认退出。此处不能先退出 event loop 再
                        // join，否则 owner 可能无期限等待仍在同步 wgpu 调用中的渲染线程。
                        self.send(WinitEvent::CloseRequested { handle });
                        self.alive_handles -= 1;
                    }
                    WindowEvent::Resized(size) => {
                        // 事件驱动路径：仅同步逻辑尺寸。真正的 surface.configure /
                        // renderer 视图更新由渲染线程 draw() 的逐帧尺寸同步完成
                        // （模态循环期间 Resized 事件可能滞后，逐帧轮询 inner_size 兜底）。
                        self.send(WinitEvent::Resized {
                            handle,
                            width: size.width,
                            height: size.height,
                        });
                    }
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        self.send(WinitEvent::ScaleFactorChanged {
                            handle,
                            scale: scale_factor,
                        });
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        self.send(WinitEvent::CursorMoved {
                            handle,
                            x: position.x,
                            y: position.y,
                        });
                    }
                    WindowEvent::KeyboardInput { event: key_event, .. } => {
                        if let Some(mapped) = crate::input::map_key_event(&key_event) {
                            if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                                if mapped.state.is_pressed() {
                                    for cb in &mut cbs.on_key_down { cb(&mapped); }
                                } else {
                                    for cb in &mut cbs.on_key_up { cb(&mapped); }
                                }
                            }
                            self.send(WinitEvent::KeyboardInput { handle, event: mapped });
                        }
                    }
                    WindowEvent::MouseInput { state, button, .. } => {
                        let pressed = state == winit::event::ElementState::Pressed;
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            let evt = crate::input::MouseButtonEvent { button, state };
                            if pressed {
                                for cb in &mut cbs.on_mouse_down { cb(&evt); }
                            } else {
                                for cb in &mut cbs.on_mouse_up { cb(&evt); }
                            }
                        }
                        self.send(WinitEvent::MouseInput { handle, button, pressed });
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        let delta = crate::input::map_scroll_delta(delta);
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            let evt = crate::input::MouseScrollEvent { delta };
                            for cb in &mut cbs.on_scroll { cb(&evt); }
                        }
                        self.send(WinitEvent::MouseWheel { handle, delta });
                    }
                    WindowEvent::ModifiersChanged(state) => {
                        let modifiers = crate::input::map_modifiers(&state.state());
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_modifiers_changed { cb(modifiers); }
                        }
                        self.send(WinitEvent::ModifiersChanged { handle, modifiers });
                    }
                    WindowEvent::Focused(focused) => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            if focused {
                                for c in cbs.on_focus_gained.drain(..) { c(); }
                            } else {
                                for c in cbs.on_focus_lost.drain(..) { c(); }
                            }
                        }
                        self.send(WinitEvent::Focused { handle, focused });
                    }
                    WindowEvent::CursorEntered { .. } => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for c in cbs.on_cursor_entered.drain(..) { c(); }
                        }
                        self.send(WinitEvent::CursorEntered { handle });
                    }
                    WindowEvent::CursorLeft { .. } => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for c in cbs.on_cursor_left.drain(..) { c(); }
                        }
                        self.send(WinitEvent::CursorLeft { handle });
                    }
                    WindowEvent::Touch(touch) => {
                        let mapped = crate::input::map_touch_event(&touch, 1.0);
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_touch { cb(&mapped); }
                        }
                        self.send(WinitEvent::Touch { handle, event: mapped });
                    }
                    // 帧循环全在渲染线程（draw 内 acquire→submit→present），
                    // winit 线程不需要响应 RedrawRequested。
                    WindowEvent::RedrawRequested => {}
                    _ => {}
                }
            }
        }

        event_loop.run_app(&mut Runner {
            event_tx,
            cb_rx,
            exit_rx,
            window_descs,
            id_to_handle: FxHashMap::default(),
            close_hooks,
            window_callbacks,
            default_icon,
            window_init_durations,
            instance,
            gpu: gpu_for_runner,
            created: false,
            alive_handles: 0,
        }).unwrap();

        // Winit loop 结束后等待渲染线程退出。
        let _ = render_thread.join();
    }
}

/// 渲染线程主循环：处理 winit 事件 → 调用用户 on_frame → 重复。
fn render_on_frame<F>(
    mut app: App,
    mut on_frame: F,
    event_tx: mpsc::Sender<WinitEvent>,
    rx: mpsc::Receiver<WinitEvent>,
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    exit_tx: mpsc::Sender<()>,
    device_lost: Arc<std::sync::atomic::AtomicBool>,
    expected_windows: usize,
)
where F: FnMut(&App) -> bool + Send + 'static
{
    let mut created_windows = 0usize;
    let request_exit = || {
        let _ = exit_tx.send(());
    };
    loop {
        // 处理所有待处理事件
        loop {
            match rx.try_recv() {
                Ok(WinitEvent::WindowCreated {
                    handle, window, surface, surface_config, renderer,
                    logical_width, logical_height, scale, dpi_scale, high_dpi, init_duration,
                }) => {
                    let vw = VireoWindow::new(
                        window,
                        app.gpu.clone(),
                        surface,
                        app.instance.clone().expect("instance available"),
                        surface_config,
                        renderer,
                        logical_width,
                        logical_height,
                        scale,
                        dpi_scale,
                        high_dpi,
                        init_duration,
                        event_tx.clone(),
                        cb_tx.clone(),
                        handle,
                    );
                    while app.windows.len() <= handle {
                        app.windows.push(None);
                    }
                    app.windows[handle] = Some(vw);
                    created_windows += 1;
                }

                Ok(WinitEvent::Resized { handle, width, height }) => {
                    if let Some(Some(win)) = app.windows.get_mut(handle) {
                        win.resize(width, height);
                    }
                }

                Ok(WinitEvent::ScaleFactorChanged { handle, scale: _scale }) => {
                    if let Some(Some(win)) = app.windows.get_mut(handle) {
                        let size = win.inner.inner_size();
                        win.resize(size.width, size.height);
                    }
                }

                Ok(WinitEvent::CursorMoved { handle, x, y }) => {
                    if let Some(Some(win)) = app.windows.get_mut(handle) {
                        let sf = if win.high_dpi { 1.0_f64 } else { win.inner.scale_factor() };
                        win.mouse_pos = ((x / sf) as f32, (y / sf) as f32);
                    }
                }

                Ok(WinitEvent::KeyboardInput { handle, event }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let is_pressed = event.state.is_pressed();
                        let repeat = event.repeat;
                        if is_pressed && !repeat {
                            win.input.keys_down.borrow_mut().insert(event.key);
                        } else if !is_pressed {
                            win.input.keys_down.borrow_mut().remove(&event.key);
                        }
                    }
                }

                Ok(WinitEvent::MouseInput { handle, button, pressed }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        if pressed {
                            win.input.mouse_buttons_down.borrow_mut().insert(button);
                        } else {
                            win.input.mouse_buttons_down.borrow_mut().remove(&button);
                        }
                    }
                }

                Ok(WinitEvent::MouseWheel { handle, delta }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let mut acc = win.input.scroll_delta.borrow_mut();
                        match &delta {
                            crate::input::ScrollDelta::Line { x, y } => {
                                acc.line.0 += x; acc.line.1 += y;
                            }
                            crate::input::ScrollDelta::Pixel { x, y } => {
                                acc.pixel.0 += x; acc.pixel.1 += y;
                            }
                        }
                    }
                }

                Ok(WinitEvent::ModifiersChanged { handle, modifiers }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        *win.input.modifiers.borrow_mut() = modifiers;
                    }
                }

                Ok(WinitEvent::Focused { handle, focused }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let was_focused = std::mem::replace(&mut *win.input.focused.borrow_mut(), focused);
                        if !focused && was_focused {
                            win.input.keys_down.borrow_mut().clear();
                            win.input.mouse_buttons_down.borrow_mut().clear();
                        }
                    }
                }

                Ok(WinitEvent::CursorEntered { handle }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        *win.input.cursor_inside.borrow_mut() = true;
                    }
                }

                Ok(WinitEvent::CursorLeft { handle }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        *win.input.cursor_inside.borrow_mut() = false;
                    }
                }

                Ok(WinitEvent::Touch { handle, event }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let sf = if win.high_dpi { 1.0_f64 } else { win.inner.scale_factor() };
                        let tx = (event.x as f64 / sf) as f32;
                        let ty = (event.y as f64 / sf) as f32;
                        match event.phase {
                            crate::input::TouchPhase::Started | crate::input::TouchPhase::Moved => {
                                win.input.touches.borrow_mut().insert(event.id, (tx, ty, event.force));
                            }
                            _ => {
                                win.input.touches.borrow_mut().remove(&event.id);
                            }
                        }
                    }
                }

                Ok(WinitEvent::CloseRequested { handle, .. }) => {
                    if let Some(Some(win)) = app.windows.get_mut(handle) {
                        win.closing.set(true);
                        // 置 closing 后再 drop：此时无 outstanding SurfaceTexture
                        // （draw 内的 st 在 present 后已释放），drop surface 安全。
                    }
                    if let Some(w) = app.windows.get_mut(handle) {
                        *w = None;
                    }
                    if app.window_count() == 0 {
                        request_exit();
                        return;
                    }
                }

                // Winit 窗口操作：转发到正确的窗口
                Ok(WinitEvent::SetTitle { handle, title }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_title(&title);
                    }
                }
                Ok(WinitEvent::SetSize { handle, width, height }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let _ = win.inner.request_inner_size(winit::dpi::LogicalSize::new(width, height));
                    }
                }
                Ok(WinitEvent::SetMinSize { handle, width, height }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let size = match (width, height) {
                            (Some(w), Some(h)) => Some(winit::dpi::LogicalSize::new(w, h)),
                            _ => None,
                        };
                        win.inner.set_min_inner_size(size);
                    }
                }
                Ok(WinitEvent::SetMaxSize { handle, width, height }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let size = match (width, height) {
                            (Some(w), Some(h)) => Some(winit::dpi::LogicalSize::new(w, h)),
                            _ => None,
                        };
                        win.inner.set_max_inner_size(size);
                    }
                }
                Ok(WinitEvent::SetFullscreen { handle, fullscreen }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_fullscreen(fullscreen);
                    }
                }
                Ok(WinitEvent::SetMaximized { handle, maximized }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_maximized(maximized);
                    }
                }
                Ok(WinitEvent::SetMinimized { handle, minimized }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_minimized(minimized);
                    }
                }
                Ok(WinitEvent::SetVisible { handle, visible }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_visible(visible);
                    }
                }
                Ok(WinitEvent::FocusWindow { handle }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.focus_window();
                    }
                }
                Ok(WinitEvent::SetWindowLevel { handle, level }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_window_level(level);
                    }
                }
                Ok(WinitEvent::SetDecorations { handle, decorations }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_decorations(decorations);
                    }
                }
                Ok(WinitEvent::SetIcon { handle, icon }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_window_icon(Some(icon));
                    }
                }
                Ok(WinitEvent::SetCursor { handle, cursor }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_cursor(cursor);
                    }
                }

                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }

        // The winit thread may have already exited after the final close
        // event. Do not enter user code or block in another frame wait.
        if created_windows >= expected_windows && app.window_count() == 0 {
            return;
        }

        // FPS 统计
        let now = std::time::Instant::now();
        app.frame_count += 1;
        if app.frame_count == 1 {
            app.last_frame = now;
        } else {
            let dt = now.duration_since(app.last_frame).as_secs_f64();
            app.last_frame = now;
            if dt > 0.0 && dt < 0.5 {
                app.frame_time = dt;
                app.fps_samples.push(dt);
                if app.fps_samples.len() > FPS_SAMPLE_CAP {
                    app.fps_samples.remove(0);
                }
                let sum: f64 = app.fps_samples.iter().sum();
                if sum > 0.0 {
                    app.fps = app.fps_samples.len() as f64 / sum;
                }
            }
        }

        // 执行所有到期的延迟任务
        {
            let ready = {
                let mut tasks = app.deferred_tasks.borrow_mut();
                let mut ready = Vec::new();
                let mut i = 0;
                while i < tasks.len() {
                    if tasks[i].is_ready(app.frame_count) {
                        ready.push(tasks.swap_remove(i));
                    } else {
                        i += 1;
                    }
                }
                ready
            };
            for task in ready {
                (task.f)();
            }
        }

        // 等所有窗口创建完才开始调用用户代码
        if created_windows >= expected_windows {
            for win in app.windows.iter().flatten() {
                win.last_draw_outcome.set(None);
            }
            if !(on_frame)(&app) {
                // 用户请求退出：通知 winit 线程 exit，本线程返回。
                request_exit();
                break;
            }
            if should_backoff_after_draws(
                app.windows.iter().flatten().map(|win| win.last_draw_outcome.get()),
            ) {
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            if device_lost.load(std::sync::atomic::Ordering::Acquire) {
                eprintln!("vireo: GPU device lost — terminating");
                request_exit();
                break;
            }
        } else {
            std::thread::yield_now();
        }
    }
}

fn should_backoff_after_draws(
    outcomes: impl IntoIterator<Item = Option<DrawOutcome>>,
) -> bool {
    let mut any = false;
    for outcome in outcomes {
        any = true;
        if !matches!(
            outcome,
            Some(DrawOutcome::Skipped(
                DrawSkipReason::ZeroSized
                    | DrawSkipReason::Timeout
                    | DrawSkipReason::Occluded
                    | DrawSkipReason::Closing
            ))
        ) {
            return false;
        }
    }

    any
}

/// 窗口索引 —— 用于在 run() 闭包中引用窗口。稳定 handle：关窗后该索引失效（`window_ref` 返回 None），
/// 不会因其他窗口关闭而重指向新窗口。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WindowIndex(pub(crate) u64);

impl WindowIndex {
    fn new(handle: u64) -> Self {
        Self(handle)
    }
}

impl App {
    /// 创建无 group 3 的材质（纯 shader，无额外 buffer/纹理绑定）。
    /// pipeline layout 仅含 groups 0–2。
    pub fn material(&self, source: &str) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material(source)
    }

    /// 创建无 group 3 的材质 + 自定义 shape 顶点着色器。
    pub fn material_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu
            .create_material_with_vertex_shader(source, vertex_source)
    }

    /// 带 group 3 资源的材质。引擎按描述符自动生成 BGL + 注入 WGSL + AutoDefaults。
    pub fn material_with_resources(
        &self,
        source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_with_resources(source, resources)
    }

    /// 带 group 3 资源的材质 + 自定义 shape 顶点着色器。
    pub fn material_with_resources_and_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_with_resources_and_vertex_shader(source, vertex_source, resources)
    }

    /// 自定义 BGL 材质。用户自建 BGL + buffer + 每帧 `set_bind_group_provider`。
    /// 见 `examples/custom_material_manual`。
    pub fn material_manual(
        &self,
        source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_manual(source, bgl)
    }

    /// 自定义 BGL 材质 + 自定义 shape 顶点着色器。
    pub fn material_manual_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_manual_with_vertex_shader(source, vertex_source, bgl)
    }

    /// 根据索引获取窗口引用。返回 None 表示窗口已关闭或索引无效。
    pub fn window_ref(&self, idx: &WindowIndex) -> Option<&VireoWindow> {
        self.windows.get(idx.0 as usize).and_then(|w| w.as_ref())
    }

    /// 存活窗口数量
    pub fn window_count(&self) -> usize {
        self.windows.iter().filter(|w| w.is_some()).count()
    }

    /// App::new 内部耗时（秒）：GPU 设备、shader 模块、bind group layout 构造。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    /// 所有存活窗口引用
    pub fn windows(&self) -> Vec<&VireoWindow> {
        self.windows.iter().filter_map(|w| w.as_ref()).collect()
    }

    /// 所有存活窗口索引（与 `window_ref` 配合使用）。
    /// handle 是稳定 id；同一 handle 跨关窗事件不变（关窗后 `window_ref` 返回 None）。
    pub fn window_indices(&self) -> Vec<WindowIndex> {
        self.windows.iter().enumerate()
            .filter(|(_, w)| w.is_some())
            .map(|(i, _)| WindowIndex::new(i as u64))
            .collect()
    }
}

impl VireoWindow {
    /// 该窗口初始化耗时（秒）：app.window() 内的 AA 管线预热。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    /// 获取窗口标题
    pub fn title(&self) -> String {
        self.inner.title()
    }

    /// 设置窗口标题（通过 winit 线程异步操作）
    pub fn set_title(&self, title: &str) {
        let _ = self.event_tx.send(WinitEvent::SetTitle {
            handle: self.handle(),
            title: title.to_string(),
        });
    }

    /// 设置窗口大小（逻辑像素，通过 winit 线程异步操作）
    pub fn set_size(&self, width: u32, height: u32) {
        let _ = self.event_tx.send(WinitEvent::SetSize {
            handle: self.handle(),
            width,
            height,
        });
    }

    /// 设置最小窗口大小（逻辑像素，通过 winit 线程异步操作）
    pub fn set_min_size(&self, width: Option<u32>, height: Option<u32>) {
        let _ = self.event_tx.send(WinitEvent::SetMinSize {
            handle: self.handle(),
            width,
            height,
        });
    }

    /// 设置最大窗口大小（逻辑像素，通过 winit 线程异步操作）
    pub fn set_max_size(&self, width: Option<u32>, height: Option<u32>) {
        let _ = self.event_tx.send(WinitEvent::SetMaxSize {
            handle: self.handle(),
            width,
            height,
        });
    }

    /// 切换全屏模式（通过 winit 线程异步操作）
    pub fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        let _ = self.event_tx.send(WinitEvent::SetFullscreen {
            handle: self.handle(),
            fullscreen,
        });
    }

    /// 最大化窗口（通过 winit 线程异步操作）
    pub fn set_maximized(&self, maximized: bool) {
        let _ = self.event_tx.send(WinitEvent::SetMaximized {
            handle: self.handle(),
            maximized,
        });
    }

    /// 最小化窗口（通过 winit 线程异步操作）
    pub fn set_minimized(&self, minimized: bool) {
        let _ = self.event_tx.send(WinitEvent::SetMinimized {
            handle: self.handle(),
            minimized,
        });
    }

    /// 显示/隐藏窗口（通过 winit 线程异步操作）
    pub fn set_visible(&self, visible: bool) {
        let _ = self.event_tx.send(WinitEvent::SetVisible {
            handle: self.handle(),
            visible,
        });
    }

    /// 获取焦点（通过 winit 线程异步操作）
    pub fn focus(&self) {
        let _ = self.event_tx.send(WinitEvent::FocusWindow {
            handle: self.handle(),
        });
    }

    /// 设置窗口层级（通过 winit 线程异步操作）
    pub fn set_window_level(&self, level: WindowLevel) {
        let _ = self.event_tx.send(WinitEvent::SetWindowLevel {
            handle: self.handle(),
            level,
        });
    }

    /// 设置窗口装饰（标题栏边框，通过 winit 线程异步操作）
    pub fn set_decorations(&self, decorations: bool) {
        let _ = self.event_tx.send(WinitEvent::SetDecorations {
            handle: self.handle(),
            decorations,
        });
    }

    // ------ 事件订阅 API（通过 cb_tx 异步发送到 winit 线程，无需 +Send）------

    pub fn on_key_down(&self, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_key_down.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_key_up(&self, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_key_up.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_mouse_down(&self, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_mouse_down.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_mouse_up(&self, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_mouse_up.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_scroll(&self, callback: impl FnMut(&crate::input::MouseScrollEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_scroll.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_cursor_entered(&self, callback: impl FnOnce() + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_cursor_entered.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_cursor_left(&self, callback: impl FnOnce() + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_cursor_left.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_touch(&self, callback: impl FnMut(&crate::input::TouchEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_touch.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_focus_gained(&self, callback: impl FnOnce() + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_focus_gained.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_focus_lost(&self, callback: impl FnOnce() + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_focus_lost.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_modifiers_changed(&self, callback: impl FnMut(crate::input::Modifiers) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_modifiers_changed.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    /// 运行时切换 present mode（会 reconfigure surface）。
    /// 下次 `draw` 前应用。
    pub fn set_present_mode(&self, mode: wgpu::PresentMode) {
        self.pending_mode.set(Some(mode));
    }

    /// 当前真正生效（已 configure 到 surface）的 present mode；pending 尚未应用。
    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.applied_present_mode.get()
    }

    /// 设置拖动窗口时的 resize 尺寸刷新策略（[`ResizeRefreshPolicy`]）。
    /// 默认 `OnRelease`：拖动全程不更新（拉伸显示、帧流满速），松手 snap。
    /// `EveryFrame` / `Periodic(interval)` 会在拖动中实时 `surface.configure`，
    /// 每次阻塞 ~50-80ms（wgpu-hal DX12 present queue 排空），掉帧是预期代价。
    pub fn set_resize_refresh_policy(&self, policy: ResizeRefreshPolicy) {
        self.resize_policy.set(policy);
    }

    /// 当前拖动中的 resize 尺寸刷新策略（默认 [`ResizeRefreshPolicy::OnRelease`]）。
    pub fn resize_refresh_policy(&self) -> ResizeRefreshPolicy {
        self.resize_policy.get()
    }

    /// 设置 resize 去抖时长：拖动中尺寸**稳定**满此时间后才一次性
    /// `surface.configure`（松手 snap）。默认 100ms。
    ///
    /// 调小 → 松手 snap 更快，但「按住但暂停一下」的拖动间隙更容易误触发
    /// configure 卡顿；调大 → 松手 snap 更慢、更不容易被暂停误触发。
    pub fn set_resize_debounce(&self, debounce: std::time::Duration) {
        self.resize_debounce.set(debounce);
    }

    /// 当前 resize 去抖时长（默认 100ms）。见 [`VireoWindow::set_resize_debounce`]。
    pub fn resize_debounce(&self) -> std::time::Duration {
        self.resize_debounce.get()
    }

    /// 布局跟随开关（独立于 [`ResizeRefreshPolicy`]，默认开）。
    ///
    /// 窗口尺寸已变但 surface 未重配时（拖动中），每帧把 camera/逻辑尺寸更新到
    /// 新窗口（`Renderer::update_layout`），让内容**实时重排**：DXGI 把旧 surface
    /// 拉伸到新窗口时，几何和文字都按 x/y 两轴的新尺寸映射；宽高比变化本身不会
    /// 造成单轴近似。可见误差来自尺寸采样时序、逻辑/物理整数舍入和 DPI 转换。
    /// 关闭 = 旧行为：拖动中内容停在旧逻辑布局（纯拉伸，松手才 snap）。
    ///
    /// 与 [`VireoWindow::set_resize_refresh_policy`] 正交：策略决定**何时 configure
    /// surface**，本开关决定**configure 之前布局是否跟随**。关闭后 configure 前
    /// 内容完全停旧布局。
    pub fn set_layout_follow(&self, enabled: bool) {
        let was_enabled = self.layout_follow.replace(enabled);
        if was_enabled && !enabled {
            let (logical_w, logical_h, scale, dpi_scale) = self.configured_layout.get();
            self.logical_width.set(logical_w);
            self.logical_height.set(logical_h);
            self.scale.set(scale);
            self.dpi_scale.set(dpi_scale);
            let mut renderer = self.renderer.borrow_mut();
            renderer.update_layout(logical_w, logical_h, scale, dpi_scale);
            renderer.set_text_viewport_override(None);
        }
    }

    /// 当前布局跟随开关（默认开）。见 [`VireoWindow::set_layout_follow`]。
    pub fn layout_follow(&self) -> bool {
        self.layout_follow.get()
    }

    /// 运行时切换抗锯齿（运行时重建 msaa 纹理）。
    /// 下次 `draw` 前应用。
    pub fn set_anti_aliasing(&self, aa: AntiAliasing) {
        let aa = crate::window::clamp_aa(aa, self.gpu.supported_sample_counts());
        self.pending_aa.set(Some(aa));
    }

    /// 窗口 handle（在 App.windows 中的索引）
    pub fn handle(&self) -> usize {
        self.handle
    }

    /// 返回 `WindowIndex`（与 `app.on_key_down` 等注册方法共用）
    pub fn index(&self) -> WindowIndex {
        WindowIndex(self.handle as u64)
    }

    /// 设置窗口图标（通过 winit 线程异步操作）
    pub fn set_icon(&self, icon: Icon) {
        let _ = self.event_tx.send(WinitEvent::SetIcon {
            handle: self.handle(),
            icon,
        });
    }

    /// 设置光标样式（通过 winit 线程异步操作）
    pub fn set_cursor(&self, cursor: winit::window::Cursor) {
        let _ = self.event_tx.send(WinitEvent::SetCursor {
            handle: self.handle(),
            cursor,
        });
    }
}


/// 离屏画布索引
/// 离屏画布索引。handle 是稳定 id（构造顺序，不受其他 canvas 关闭影响）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct OffscreenIndex(pub usize);

#[cfg(test)]
mod present_state_tests {
    //! 独立并发练习：`pending_view` / `pending_cmd_buf` 双槽状态机在多线程下的
    //! 原子转移语义。第五十一轮起 vireo 删除 present-proxy（`SharedGPUState` 已移除，
    //! render thread 独占 acquire→present，不再跨线程移交 SurfaceTexture），
    //! 本模块作为通用并发 sanity 测试保留，不代表当前窗口架构。

    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    /// 通用状态机：`pending_view: Mutex<Option<V>>` + `pending_cmd_buf: Mutex<Option<C>>`。
    /// 用 `u32` 等值类型替身模拟 wgpu 的 TextureView/CommandBuffer。
    struct StateMachine<V, C> {
        pending_view: Mutex<Option<V>>,
        pending_cmd_buf: Mutex<Option<C>>,
    }

    impl<V, C> StateMachine<V, C> {
        fn new() -> Self {
            Self { pending_view: Mutex::new(None), pending_cmd_buf: Mutex::new(None) }
        }

        /// winit 线程调：acquire 后把 view 放进来。
        /// 返回 `true` 表示成功（之前是 Idle），`false` 表示状态错乱（已经有 view）。
        fn put_view(&self, v: V) -> bool {
            let mut slot = self.pending_view.lock().unwrap();
            if slot.is_some() { return false; }
            *slot = Some(v);
            true
        }

        /// 逻辑线程调：take view 用于编码。
        /// 返回 `Some(v)` 表示 Idle/Acquired 状态，`None` 表示 Encoded（view 已被消费）。
        fn take_view(&self) -> Option<V> {
            self.pending_view.lock().unwrap().take()
        }

        /// 逻辑线程调：编码完成后放 cmd_buf。
        /// 返回 `true` 表示成功（Acquired 状态），`false` 表示状态错乱（cmd_buf 已存在）。
        fn put_cmd_buf(&self, c: C) -> bool {
            let mut slot = self.pending_cmd_buf.lock().unwrap();
            if slot.is_some() { return false; }
            *slot = Some(c);
            true
        }

        /// winit 线程调：take cmd_buf 用于 submit+present。
        /// 返回 `Some(c)` 表示 Encoded 状态，`None` 表示 Idle/Acquired。
        fn take_cmd_buf(&self) -> Option<C> {
            self.pending_cmd_buf.lock().unwrap().take()
        }

        /// resize/close 路径：清空所有字段。
        fn drain(&self) {
            *self.pending_view.lock().unwrap() = None;
            *self.pending_cmd_buf.lock().unwrap() = None;
        }
    }

    #[test]
    fn idle_state_has_no_view_no_cmd_buf() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
        assert!(sm.pending_view.lock().unwrap().is_none());
        assert!(sm.pending_cmd_buf.lock().unwrap().is_none());
    }

    #[test]
    fn idle_to_acquired_puts_view() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        assert!(sm.put_view(42));
        assert!(matches!(*sm.pending_view.lock().unwrap(), Some(42)));
        assert!(sm.pending_cmd_buf.lock().unwrap().is_none());
    }

    #[test]
    fn acquired_take_view_returns_view() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(42);
        assert!(matches!(sm.take_view(), Some(42)));
        // Acquired → Idle 转移
        assert!(sm.take_view().is_none());
    }

    #[test]
    fn acquired_to_encoded_take_view_then_put_cmd_buf() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(42);
        let v = sm.take_view();
        assert!(matches!(v, Some(42)));
        // 现在 view 已被消费（即便字段可能仍 Some），模拟逻辑线程完成编码
        assert!(sm.put_cmd_buf(100));
        assert!(matches!(*sm.pending_cmd_buf.lock().unwrap(), Some(100)));
    }

    #[test]
    fn encoded_take_cmd_buf_returns_cmd_buf() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(42);
        sm.take_view();
        sm.put_cmd_buf(100);
        assert!(matches!(sm.take_cmd_buf(), Some(100)));
        // Encoded → Idle
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn full_cycle_idle_acquired_encoded_idle() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        // Idle
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
        // Idle → Acquired
        sm.put_view(1);
        // Acquired → Encoded
        sm.take_view();
        sm.put_cmd_buf(2);
        // Encoded → Idle
        sm.take_cmd_buf();
        // 回到 Idle
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn put_view_when_already_acquired_returns_false() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        // 状态错乱：重复 acquire
        assert!(!sm.put_view(2));
        // 仍是第一个 view
        assert!(matches!(*sm.pending_view.lock().unwrap(), Some(1)));
    }

    #[test]
    fn put_cmd_buf_when_already_encoded_returns_false() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        sm.take_view();
        sm.put_cmd_buf(10);
        // 状态错乱：重复编码
        assert!(!sm.put_cmd_buf(20));
        assert!(matches!(*sm.pending_cmd_buf.lock().unwrap(), Some(10)));
    }

    #[test]
    fn drain_clears_acquired_state() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn drain_clears_encoded_state() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        sm.take_view();
        sm.put_cmd_buf(2);
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn drain_on_idle_is_noop() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn double_drain_safe() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        sm.take_view();
        sm.put_cmd_buf(2);
        sm.drain();
        sm.drain();
        // 不 panic，字段仍为 None
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    /// 并发安全：N 线程随机执行状态机操作，验证不 panic + 最终可达成 Idle。
    /// 模拟旧 present-proxy 双线程 race 的通用并发练习（当前架构已不适用）。
    #[test]
    fn concurrent_operations_safe() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let mut handles = Vec::new();
        for tid in 0..8 {
            let sm = Arc::clone(&sm);
            handles.push(thread::spawn(move || {
                for i in 0..1000 {
                    match i % 4 {
                        0 => { sm.put_view((tid * 1000 + i) as u32); }
                        1 => { sm.take_view(); }
                        2 => { sm.put_cmd_buf((tid * 1000 + i) as u32); }
                        3 => { sm.take_cmd_buf(); }
                        _ => unreachable!(),
                    }
                    // 防止某线程饿死
                    thread::yield_now();
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        // 不验证最终状态（race 下不保证 Idle），只验证不 panic
    }

    /// drain 路径并发：winit 线程调 drain 时，逻辑线程可能正在 take_view。
    /// 验证 drain 完成后所有 take 返回 None（不返回陈旧 view/cmd_buf）。
    #[test]
    fn drain_during_concurrent_access() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let mut handles = Vec::new();
        // 逻辑线程：持续 put_view + take_view 模拟编码循环
        for _ in 0..4 {
            let sm = Arc::clone(&sm);
            handles.push(thread::spawn(move || {
                for i in 0..500 {
                    sm.put_view(i);
                    sm.take_view();
                    sm.put_cmd_buf(i);
                    thread::yield_now();
                }
            }));
        }
        // winit 线程：持续 drain + acquire 模拟 resize/close
        for _ in 0..2 {
            let sm = Arc::clone(&sm);
            handles.push(thread::spawn(move || {
                for _ in 0..500 {
                    sm.drain();
                    thread::sleep(Duration::from_micros(10));
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        // 最终 drain
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    /// put_view 并发竞争：N 线程同时 put_view，恰好一个成功，其余 false。
    /// 这是 winit 线程 + 逻辑线程的 race 关键路径。
    #[test]
    fn put_view_is_atomic_only_one_succeeds() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let mut handles = Vec::new();
        let success_count = Arc::new(Mutex::new(0u32));
        for tid in 0..16 {
            let sm = Arc::clone(&sm);
            let success_count = Arc::clone(&success_count);
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    if sm.put_view((tid * 1000 + i) as u32) {
                        *success_count.lock().unwrap() += 1;
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // 仅当 pending_view 为 None 时 put 成功
        // 多次成功是允许的（drain 之后又能 put）
        // 但每个 put 时刻只有 1 个线程能 put
        assert!(*success_count.lock().unwrap() > 0);
    }

    /// 模拟完整帧循环：winit 线程跑 "acquire + present" 循环，逻辑线程跑
    /// "take_view + encode + put_cmd_buf" 循环。验证 N 帧后能稳定运行不 panic。
    #[test]
    fn winit_and_logic_thread_full_frame_loop() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let sm_winit = Arc::clone(&sm);
        let sm_logic = Arc::clone(&sm);

        // winit 线程：Idle → Acquire → ... → wait → ... → Idle
        let winit = thread::spawn(move || {
            for i in 0..500 {
                // 模拟 acquire：Idle 时 put_view（成功）
                if !sm_winit.put_view(i) {
                    // 状态错乱（不应发生）
                    panic!("winit: put_view failed at frame {}", i);
                }
                // 模拟等逻辑线程编码
                while sm_winit.take_cmd_buf().is_none() {
                    thread::yield_now();
                }
                // 模拟 submit+present：Encoded → Idle
                // 提交后回到 Idle（drain 模拟）
                sm_winit.drain();
            }
        });

        // 逻辑线程：等 view → 编码 → 放 cmd_buf
        let logic = thread::spawn(move || {
            for _ in 0..500 {
                // 等 view
                let v = loop {
                    if let Some(v) = sm_logic.take_view() {
                        break v;
                    }
                    thread::yield_now();
                };
                // 编码（用 view 值作为 cmd_buf 内容）
                assert!(sm_logic.put_cmd_buf(v + 1000));
            }
        });

        winit.join().expect("winit thread panicked");
        logic.join().expect("logic thread panicked");
        // 最终 Idle
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    /// Resize 路径模拟：winit 线程在 resize 期间调 drain，必须与逻辑线程正在
    /// 进行的 put_view/take_view 兼容。验证 drain 后状态为 Idle。
    #[test]
    fn resize_drain_concurrent_with_logic() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let mut handles = Vec::new();

        // 逻辑线程：持续 put_view + take_view
        for tid in 0..3 {
            let sm = Arc::clone(&sm);
            handles.push(thread::spawn(move || {
                for i in 0..500u32 {
                    let v = (tid as u32) * 10000 + i;
                    let _ = sm.put_view(v);
                    // take_view 可能返回 None（如果 winit 同时 drain）
                    let _ = sm.take_view();
                    let _ = sm.put_cmd_buf(v + 1000);
                    let _ = sm.take_cmd_buf();
                    thread::yield_now();
                }
            }));
        }

        // winit 线程：resize 触发 drain
        let sm_winit = Arc::clone(&sm);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                sm_winit.drain();
                thread::sleep(Duration::from_micros(50));
            }
        }));

        for h in handles {
            h.join().unwrap();
        }
        // 最终 drain
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }
}

#[cfg(test)]
mod metrics_tests {
    //! 第五十一轮纯函数测试：逻辑/物理尺寸换算与「跳过帧」report 构造。

    use super::{logical_size, skip_report, DrawOutcome, DrawSkipReason};

    #[test]
    fn draw_backoff_requires_every_active_window_to_skip() {
        assert!(super::should_backoff_after_draws([
            Some(DrawOutcome::Skipped(DrawSkipReason::ZeroSized)),
            Some(DrawOutcome::Skipped(DrawSkipReason::Occluded)),
        ]));
        assert!(!super::should_backoff_after_draws([
            Some(DrawOutcome::Skipped(DrawSkipReason::ZeroSized)),
            Some(DrawOutcome::Presented { suboptimal: false }),
        ]));
    }

    #[test]
    fn draw_backoff_does_not_throttle_undrawn_or_empty_windows() {
        assert!(!super::should_backoff_after_draws([None]));
        assert!(!super::should_backoff_after_draws(std::iter::empty()));
        assert!(!super::should_backoff_after_draws([
            Some(DrawOutcome::Skipped(DrawSkipReason::SurfaceReconfigured)),
        ]));
    }

    #[test]
    fn logical_size_scales_physical_by_scale_factor() {
        // 非 high_dpi：逻辑 = 物理 / scale_factor
        assert_eq!(logical_size(1920, 1080, false, 1.5), (1280, 720));
        assert_eq!(logical_size(1000, 500, false, 2.0), (500, 250));
    }

    #[test]
    fn logical_size_high_dpi_is_physical() {
        // high_dpi：逻辑 = 物理（用户坐标即物理像素）
        assert_eq!(logical_size(1920, 1080, true, 1.5), (1920, 1080));
        assert_eq!(logical_size(1000, 500, true, 2.0), (1000, 500));
    }

    #[test]
    fn logical_size_invalid_scale_factor_falls_back_to_physical() {
        assert_eq!(logical_size(800, 600, false, 0.0), (800, 600));
    }

    #[test]
    fn skip_report_preserves_gpu_secs_and_reason() {
        let r = skip_report(Some(0.123), DrawSkipReason::ZeroSized);
        assert!(matches!(r.outcome, DrawOutcome::Skipped(DrawSkipReason::ZeroSized)));
        assert_eq!(r.timings.gpu_secs, Some(0.123));
    }

    #[test]
    fn skip_report_default_timings_are_zero() {
        let r = skip_report(None, DrawSkipReason::Timeout);
        let t = r.timings;
        assert_eq!((t.acquire_secs, t.encode_secs, t.present_secs), (0.0, 0.0, 0.0));
        assert!(t.gpu_secs.is_none());
    }

    #[test]
    fn window_metrics_matches_constructor_values() {
        let m = super::WindowMetrics { width: 800, height: 600, scale_factor: 1.5 };
        assert_eq!((m.width, m.height), (800, 600));
        assert_eq!(m.scale_factor, 1.5);
    }

    #[test]
    fn resolve_present_mode_auto_vsync_alias_always_accepted() {
        // AutoVsync 是 wgpu 别名，get_capabilities 永不列出别名本身（只列 Fifo 等具体模式）
        let supported = [wgpu::PresentMode::Fifo];
        assert_eq!(super::VireoWindow::resolve_present_mode(wgpu::PresentMode::AutoVsync, &supported), wgpu::PresentMode::AutoVsync);
        assert_eq!(super::VireoWindow::resolve_present_mode(wgpu::PresentMode::AutoVsync, &[]), wgpu::PresentMode::AutoVsync);
    }

    #[test]
    fn resolve_present_mode_supported_mode_passes_through() {
        let supported = [wgpu::PresentMode::Fifo, wgpu::PresentMode::Immediate];
        assert_eq!(super::VireoWindow::resolve_present_mode(wgpu::PresentMode::Immediate, &supported), wgpu::PresentMode::Immediate);
    }

    #[test]
    fn resolve_present_mode_unsupported_mode_falls_back_to_auto_vsync() {
        let supported = [wgpu::PresentMode::Fifo];
        assert_eq!(super::VireoWindow::resolve_present_mode(wgpu::PresentMode::Immediate, &supported), wgpu::PresentMode::AutoVsync);
    }

    fn base() -> std::time::Instant {
        std::time::Instant::now()
    }

    #[test]
    fn resize_refresh_default_no_live_only_stable_snaps() {
        // 默认（OnRelease）：拖动中尺寸持续变化 → 永不 configure；松手稳定满 debounce → Stable。
        let t0 = base();
        let debounce = std::time::Duration::from_millis(100);
        // 刚变化（stable_since=now）→ 还不稳，OnRelease → None
        assert_eq!(
            super::resize_refresh(true, Some(t0), t0, debounce,
                super::ResizeRefreshPolicy::OnRelease, t0),
            super::ResizeRefresh::None,
        );
        // 已稳定 200ms → Stable
        let stable = t0 + std::time::Duration::from_millis(200);
        assert_eq!(
            super::resize_refresh(true, Some(t0), stable, debounce,
                super::ResizeRefreshPolicy::OnRelease, t0),
            super::ResizeRefresh::Stable,
        );
    }

    #[test]
    fn resize_refresh_every_frame_tracks_during_change() {
        let t0 = base();
        let debounce = std::time::Duration::from_millis(100);
        // 拖动中（stable_since=now，未稳）→ EveryFrame 立即 Live
        assert_eq!(
            super::resize_refresh(true, Some(t0), t0, debounce,
                super::ResizeRefreshPolicy::EveryFrame, t0),
            super::ResizeRefresh::Live,
        );
        // 尺寸稳定满 debounce 时 Stable 优先于 EveryFrame 的 Live
        let stable = t0 + std::time::Duration::from_millis(200);
        assert_eq!(
            super::resize_refresh(true, Some(t0), stable, debounce,
                super::ResizeRefreshPolicy::EveryFrame, t0),
            super::ResizeRefresh::Stable,
        );
    }

    #[test]
    fn resize_refresh_periodic_triggers_on_interval() {
        let t0 = base();
        let debounce = std::time::Duration::from_millis(100);
        let iv = std::time::Duration::from_millis(400);
        // 拖动中：距上次 configure 200ms < iv → None
        let mid = t0 + std::time::Duration::from_millis(200);
        assert_eq!(
            super::resize_refresh(true, Some(mid), mid, debounce,
                super::ResizeRefreshPolicy::Periodic(iv), t0),
            super::ResizeRefresh::None,
        );
        // 距上次 configure 已满 iv → Live（拖动中周期性实时刷新）
        let late = t0 + std::time::Duration::from_millis(450);
        assert_eq!(
            super::resize_refresh(true, Some(late), late, debounce,
                super::ResizeRefreshPolicy::Periodic(iv), t0),
            super::ResizeRefresh::Live,
        );
    }

    #[test]
    fn resize_refresh_size_unchanged_never_configure() {
        let t0 = base();
        let debounce = std::time::Duration::from_millis(100);
        assert_eq!(
            super::resize_refresh(false, Some(t0), t0 + std::time::Duration::from_millis(500),
                debounce, super::ResizeRefreshPolicy::EveryFrame, t0),
            super::ResizeRefresh::None,
        );
    }

    #[test]
    fn resize_refresh_honors_custom_debounce() {
        // 自定义去抖被纯函数尊重：短 debounce 更快 snap，长 debounce 在旧默认点不 snap。
        let t0 = base();
        let short = std::time::Duration::from_millis(16);
        let long = std::time::Duration::from_millis(500);
        let at_50ms = t0 + std::time::Duration::from_millis(50);
        let at_200ms = t0 + std::time::Duration::from_millis(200);
        // 短去抖（16ms）：稳定 50ms 已 snap
        assert_eq!(
            super::resize_refresh(true, Some(t0), at_50ms, short,
                super::ResizeRefreshPolicy::OnRelease, t0),
            super::ResizeRefresh::Stable,
        );
        // 长去抖（500ms）：稳定 200ms 仍不 snap（旧默认 100ms 点也不 snap）
        assert_eq!(
            super::resize_refresh(true, Some(t0), at_200ms, long,
                super::ResizeRefreshPolicy::OnRelease, t0),
            super::ResizeRefresh::None,
        );
    }

    #[test]
    fn default_resize_debounce_is_100ms() {
        assert_eq!(super::DEFAULT_RESIZE_DEBOUNCE, std::time::Duration::from_millis(100));
    }
}
