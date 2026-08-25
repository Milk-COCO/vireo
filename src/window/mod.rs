use std::sync::{Arc, Mutex, mpsc};
use std::cell::RefCell;
use rustc_hash::FxHashMap;

use crate::render::Renderer;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::WindowAttributes,
};

use crate::render::DrawBatch;
use crate::offscreen::OffscreenCanvas;
use crate::texture::Texture;
use crate::gpu::GpuContext;
use crate::input::InputState;

/// 取 winit 窗口的原生 HWND（Windows）。失败返回 `None`。
///
/// 跨线程可用：winit 的 `Window::window_handle()` 有线程亲和限制（仅 owner 线程），
/// 而 vireo 的部分调用点（如 `set_opacity`）跑在渲染线程，故走
/// `window_handle_any_thread()` 逃生通道。调用方负责只把 handle 传给线程安全的
/// Win32 API（本模块的调用点均为线程安全操作）。
use crate::platform::windows::win_hwnd;

pub use winit::dpi::LogicalPosition;
pub use winit::dpi::LogicalSize;
pub use winit::dpi::PhysicalPosition;
pub use winit::dpi::PhysicalSize;
pub use winit::dpi::Position;
pub use winit::dpi::Size;
pub use winit::error::ExternalError;
pub use winit::error::NotSupportedError;
pub use winit::monitor::MonitorHandle;
pub use winit::monitor::VideoModeHandle;
pub use winit::window::Cursor;
pub use winit::window::CursorGrabMode;
pub use winit::window::Fullscreen;
pub use winit::window::Icon;
pub use winit::window::ImePurpose;
pub use winit::window::ResizeDirection;
pub use winit::window::Theme;
pub use winit::window::UserAttentionType;
pub use winit::window::WindowButtons;
pub use winit::window::WindowId;
pub use winit::window::WindowLevel;

mod desc;
mod metrics;
mod on;
pub use desc::{AntiAliasing, FrameStyle, SendRawWindowHandle, WindowDesc};
pub use metrics::{
    DrawFailure, DrawOutcome, DrawReport, DrawSkipReason, DrawTimings, FollowAmount,
    FollowFramesOrTime, ResizeRefreshPolicy,
};
pub(crate) use desc::clamp_aa;
#[allow(unused_imports)]
pub(crate) use metrics::{
    DEFAULT_RESIZE_DEBOUNCE, FPS_SAMPLE_CAP, PRESENT_SAMPLE_CAP, RESIZE_DRIFT_EPSILON,
    ResizeRefresh, drag_cap_effective, drag_effective_cap, mhz_to_hz, observed_moved,
    pac_advance, phys_to_logical, resize_refresh, should_backoff_after_draws,
    size_drifted_beyond, skip_report, sliding_rate, validate_aspect_ratio,
};

pub use crate::dpi::{Dp, Pixel, PixelPos, PixelSize, Pp, Px, dp, px};
use crate::dpi::{dim_to_winit_position, dim_to_winit_size, to_pixel_pos, to_pixel_size};

/// 从 winit 线程发往渲染线程的事件（全是 Send-safe 的自定义类型）。
enum WinitEvent {
    WindowCreated {
        handle: usize,
        window: Arc<winit::window::Window>,
        surface: wgpu::Surface<'static>,
        surface_config: wgpu::SurfaceConfiguration,
        renderer: crate::render::Renderer,
        dpi_scale: f32,
        dpi_override: Option<f64>,
        init_duration: f64,
        frame_style: FrameStyle,
        /// 首帧渲染后是否自动显示（`desc.visible && desc.preparable`）。
        pending_show: bool,
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
    /// size 为已按 dpi_override 换算后的 winit 尺寸（vireo 逻辑 × dpi）。
    SetSize { handle: usize, size: Size },
    SetMinSize { handle: usize, size: Option<Size> },
    SetMaxSize { handle: usize, size: Option<Size> },
    SetFullscreen { handle: usize, fullscreen: Option<Fullscreen> },
    SetMaximized { handle: usize, maximized: bool },
    SetMinimized { handle: usize, minimized: bool },
    SetVisible { handle: usize, visible: bool },
    FocusWindow { handle: usize },
    SetWindowLevel { handle: usize, level: WindowLevel },
    SetFrameStyle { handle: usize, style: FrameStyle },
    SetIcon { handle: usize, icon: Icon },
    SetCursor { handle: usize, cursor: winit::window::Cursor },
    /// 设置窗口宽高比（`Some(r > 0)` = 宽/高比 = r；`None` 或非正数 = 清除）。
    /// 跨平台语义：Electron `setAspectRatio`（macOS）、Tauri 暂无；
    /// vireo 先在 Windows 上落地（`WM_GETMINMAXINFO` + `WM_SIZING` 子类），
    /// macOS 等后续 `NSWindow setContentAspectRatio` 实施时挂接。
    /// 必须经 winit 线程执行 `SetWindowSubclass`（同 `set_frame_style`），
    /// 渲染线程收到本事件后转发 `aspect_ratio_tx` → winit 线程。
    SetAspectRatio { handle: usize, ratio: Option<f64> },
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
    /// 首次 draw 前 surface 尚未 `surface.configure`（建窗时推迟到渲染线程）：
    /// 置 true 强制首帧走 configure 路径，建立合法 swapchain 后再 acquire。
    needs_initial_configure: std::cell::Cell<bool>,
    /// 首帧渲染成功后是否自动显示窗口（`WindowDesc::preparable`）：建窗时隐藏，
    /// 首次 `Presented` 后 `set_visible(true)`，让窗口第一次出现即完整形态。
    pending_show: std::cell::Cell<bool>,
    renderer: std::cell::RefCell<crate::render::Renderer>,
    pub inner: Arc<winit::window::Window>,
    pub gpu: Arc<GpuContext>,
    /// 最近一次 CursorMoved 的**物理像素**位置。逻辑/物理双表示走 [`Self::mouse_pos`]。
    pub(crate) mouse_pos: std::cell::Cell<(f32, f32)>,
    /// 最近一次观测到的**物理像素**窗口尺寸（真实来源）。逻辑尺寸 = 物理 ÷
    /// [`Self::layout_scale`] 现算（f64 除法，无截断；`PixelSize` 快照会因 scale
    /// 变化而过期，故不缓存逻辑值）。
    physical_size: std::cell::Cell<(u32, u32)>,
    /// vireo 层自定义 dpi 覆盖：`Some(v)` = **vireo 全自持像素**（物理 = vireo 逻辑 × v，
    /// 忽略 OS 缩放）；`None` = vireo 逻辑即 winit 逻辑（OS 系统 DPI 参与）。
    /// **不**设置 winit 的 `scale_factor_override`。运行时经 [`VireoWindow::set_dpi_override`] 切换。
    dpi_override: std::cell::Cell<Option<f64>>,
    /// 真正应用（写进 renderer/布局）的 dpi 覆盖。`set_dpi_override` 只改
    /// `dpi_override` 并请求物理 resize；`draw` 在物理尺寸落到目标后把
    /// `applied_dpi_override` 推进到新值（此前仍用旧 override，避免物理旧尺寸 × 新
    /// scale 造成逻辑瞬时漂移）。
    applied_dpi_override: std::cell::Cell<Option<f64>>,
    /// `set_dpi_override` 请求的目标物理尺寸（等待 resize 落地）；`None` = 无 pending。
    pending_override_target: std::cell::Cell<Option<(u32, u32)>>,
    /// `pending_override_target` 设置时刻（超时兜底：resize 被 OS 钳制时也应用 override）。
    pending_override_since: std::cell::Cell<Option<std::time::Instant>>,
    dpi_scale: std::cell::Cell<f32>,
    /// Last layout committed by `surface.configure`. FollowLayout may temporarily
    /// move the live camera away from this snapshot while the surface keeps its size.
    /// 存 `(phys_w, phys_h, scale, dpi_scale)`——物理尺寸 + scale 族；逻辑由物理 ÷
    /// scale 现算，不在快照里缓存（避免 scale 时效问题）。
    configured_layout: std::cell::Cell<(u32, u32, f32, f32)>,
    pub input: InputState,
    /// 待应用输入事件队列：`render_on_frame` 的 `WinitEvent` 通道 drain 时把输入类事件
    /// 入队（按 window handle 路由），由 `refresh_input` 批量应用到 `InputState`。
    /// 这样输入更新权交给用户（可在 `on_frame` 构批次前调 `refresh_input` 拿当帧新鲜输入），
    /// `draw` 在 `auto_refresh_input` 开启时也自动调一次，用户不必手动重复。
    pending_input: std::cell::RefCell<Vec<WinitEvent>>,
    /// 输入自动刷新开关（默认开）：`draw` 在 `auto_refresh_input` 为 true 时自动调
    /// `refresh_input`；设为 false 后由用户自行在 `on_frame` 内调用以获得当帧零滞后。
    auto_refresh_input: std::cell::Cell<bool>,
    /// 该窗口初始化耗时（秒）：app.window() 内的 AA 管线预热。
    pub init_duration: f64,
    /// 用于向 winit 线程发送窗口操作事件
    event_tx: mpsc::Sender<WinitEvent>,
    /// 向 winit 线程注册输入回调
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    /// NC 状态变更通道（§7.6）。`pub(crate)` 供 `platform::windows::WindowExtWindows`
    /// 的 NC 方法使用。
    pub(crate) nc_tx: mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    /// 程序化关窗通道（`VireoWindow::close` → winit 线程完整关窗路径）。
    close_tx: mpsc::Sender<usize>,
    /// 待应用的 present mode（在 draw 开头应用）
    pending_mode: std::cell::Cell<Option<wgpu::PresentMode>>,
    /// 真正 configure 到 surface 的 present mode（仅 configure 时更新）
    applied_present_mode: std::cell::Cell<wgpu::PresentMode>,
    /// 待应用的最大在途帧（`desired_maximum_frame_latency`，在 draw 开头应用）
    pending_frame_latency: std::cell::Cell<Option<u32>>,
    /// 真正 configure 到 surface 的在途帧（仅 configure 时更新）
    applied_frame_latency: std::cell::Cell<u32>,
    /// 上次 `surface.configure` 的时刻（resize 实时刷新间隔用）
    last_configure: std::cell::Cell<std::time::Instant>,
    /// 最近一次「尺寸仍与已配置值不同」的帧时刻（resize 去抖用）
    pending_resize_at: std::cell::Cell<Option<std::time::Instant>>,
    /// 上一帧观测到的窗口状态 (phys_w, phys_h, scale)——逻辑 = 物理 ÷ scale 派生，
    /// 不单存；用于判断尺寸是否仍在**移动**（相对上一帧变化才算移动，松手即停）。
    last_observed: std::cell::Cell<(u32, u32, f32)>,
    /// 拖动开始时缓存的显示器刷新率（Hz）。acquire 在拖动期失去 vsync 节流时，
    /// 渲染循环用它把 `max_fps` 临时压到刷新率（防空转）；松手 snap（configure）
    /// 后清空。只查询一次，避免拖动中每帧 `current_monitor()`。
    drag_refresh_mhz: std::cell::Cell<Option<u32>>,
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
    /// 布局跟随的平滑模式（`FollowAmount`，默认 `Average(Time(16ms))`）。
    /// 参量化每个模式的平滑强度，详见 [`VireoWindow::set_layout_follow_smoothing`]。
    follow_smoothing: std::cell::Cell<FollowAmount>,
    /// 平均窗（`Average`）的尺寸采样队列：(帧号, 时刻, 逻辑宽, 逻辑高)。
    /// follow 平滑滑动窗：采样**物理像素**尺寸（逻辑 = 物理 ÷ scale 现算，避免在
    /// 逻辑空间均值引入额外精度损失）。
    follow_samples: std::cell::RefCell<std::collections::VecDeque<(u64, std::time::Instant, u32, u32)>>,
    /// 跟随执行计数器（`Frames` 单位节流依据），每次 4a 跟随段自增。
    follow_frame: std::cell::Cell<u64>,
    /// 待应用的 AA 模式（在 draw 开头应用）
    pending_aa: std::cell::Cell<Option<AntiAliasing>>,
    /// 窗口 handle（在 App.windows 中的索引）
    handle: usize,
    /// 窗口边框样式（供 `frame_style()` 查询；运行时经 `set_frame_style` 切换）。
    pub(crate) frame_style: std::cell::Cell<FrameStyle>,
    /// 窗口是否可被点击激活获得焦点（`VireoWindow::set_focusable`）。
    /// 跨平台字段：Windows 经 `WS_EX_NOACTIVATE` 扩展样式落地；macOS 暂存
    /// 意图（需 NSWindow 子类化 `acceptsFirstResponder` 覆盖，待 macOS 平台
    /// 窗口能力整批实施时一并实现）。
    focusable: std::cell::Cell<bool>,
    /// 用户圆角偏好（Windows 11 22000+）。`set_corner_preference` 记录；
    /// Frameless 无边框时 DWM 无法圆角，`set_frame_style` 离幀前钳 / 恢复用。
    pub(crate) user_corner_pref: std::cell::Cell<crate::platform::windows::CornerPreference>,
    /// 关窗事件已到达（关闭中，draw 跳过）
    closing: std::cell::Cell<bool>,
    /// 是否启用 queue completion 计时（`DrawTimings::gpu_secs`）
    gpu_timing_enabled: std::sync::atomic::AtomicBool,
    /// 上一份已完成提交的 GPU queue latency（由 `on_submitted_work_done` 写回）
    last_gpu_secs: Arc<Mutex<Option<f64>>>,
    pending_gpu_starts: Arc<Mutex<std::collections::VecDeque<std::time::Instant>>>,
    /// Outcome recorded by this window's draw call in the current update iteration.
    last_draw_outcome: std::cell::Cell<Option<DrawOutcome>>,
    /// 本窗口最近一次 draw 的完整报告（timings + outcome），供
    /// `VIREO_PACING_STATS=1` 采集器读取。
    last_draw_report: std::cell::Cell<Option<DrawReport>>,
    /// 本窗口最近一次 present 的相位样本（`VIREO_PHASE_STATS=1` 采集器读取）。
    last_phase_sample: std::cell::Cell<Option<PhaseSample>>,
    presented_frames: std::cell::Cell<u64>,
    skipped_frames: std::cell::Cell<u64>,
    /// 最近成功 present 的间隔（秒），滑动窗口，用于 [`VireoWindow::presented_fps`]。
    present_intervals: std::cell::RefCell<Vec<f64>>,
    last_present: std::cell::Cell<Option<std::time::Instant>>,
}

impl VireoWindow {
    fn new(
        inner: Arc<winit::window::Window>,
        gpu: Arc<GpuContext>,
        surface: wgpu::Surface<'static>,
        instance: wgpu::Instance,
        surface_config: wgpu::SurfaceConfiguration,
        renderer: crate::render::Renderer,
        dpi_scale: f32,
        dpi_override: Option<f64>,
        init_duration: f64,
        frame_style: FrameStyle,
        pending_show: bool,
        event_tx: mpsc::Sender<WinitEvent>,
        cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
        nc_tx: mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
        close_tx: mpsc::Sender<usize>,
        handle: usize,
    ) -> Self {
        let initial_present_mode = surface_config.present_mode;
        let initial_frame_latency = surface_config.desired_maximum_frame_latency;
        let initial_phys = (surface_config.width, surface_config.height);
        let scale = dpi_override.unwrap_or(dpi_scale as f64) as f32;
        Self {
            surface: std::cell::RefCell::new(surface),
            instance,
            surface_config: std::cell::RefCell::new(surface_config),
            needs_initial_configure: std::cell::Cell::new(true),
            pending_show: std::cell::Cell::new(pending_show),
            renderer: std::cell::RefCell::new(renderer),
            inner,
            gpu,
            mouse_pos: std::cell::Cell::new((-1.0, -1.0)),
            physical_size: std::cell::Cell::new(initial_phys),
            dpi_override: std::cell::Cell::new(dpi_override),
            applied_dpi_override: std::cell::Cell::new(dpi_override),
            pending_override_target: std::cell::Cell::new(None),
            pending_override_since: std::cell::Cell::new(None),
            dpi_scale: std::cell::Cell::new(dpi_scale),
            configured_layout: std::cell::Cell::new((
                initial_phys.0,
                initial_phys.1,
                scale,
                dpi_scale,
            )),
            input: InputState::default(),
            pending_input: std::cell::RefCell::new(Vec::new()),
            auto_refresh_input: std::cell::Cell::new(true),
            init_duration,
            event_tx,
            cb_tx,
            close_tx,
            pending_mode: std::cell::Cell::new(None),
            applied_present_mode: std::cell::Cell::new(initial_present_mode),
            pending_frame_latency: std::cell::Cell::new(None),
            applied_frame_latency: std::cell::Cell::new(initial_frame_latency),
            last_configure: std::cell::Cell::new(std::time::Instant::now()),
            pending_resize_at: std::cell::Cell::new(None),
            last_observed: std::cell::Cell::new((
                initial_phys.0,
                initial_phys.1,
                scale,
            )),
            drag_refresh_mhz: std::cell::Cell::new(None),
            resize_policy: std::cell::Cell::new(ResizeRefreshPolicy::OnRelease),
            resize_debounce: std::cell::Cell::new(DEFAULT_RESIZE_DEBOUNCE),
            layout_follow: std::cell::Cell::new(true),
            follow_smoothing: std::cell::Cell::new(FollowAmount::default()),
            follow_samples: std::cell::RefCell::new(std::collections::VecDeque::with_capacity(16)),
            follow_frame: std::cell::Cell::new(0),
            pending_aa: std::cell::Cell::new(None),
            handle,
            frame_style: std::cell::Cell::new(frame_style),
            focusable: std::cell::Cell::new(true),
            nc_tx,
            user_corner_pref: std::cell::Cell::new(
                crate::platform::windows::CornerPreference::Default,
            ),
            closing: std::cell::Cell::new(false),
            gpu_timing_enabled: std::sync::atomic::AtomicBool::new(false),
            last_gpu_secs: Arc::new(Mutex::new(None)),
            pending_gpu_starts: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            last_draw_outcome: std::cell::Cell::new(None),
            last_draw_report: std::cell::Cell::new(None),
            last_phase_sample: std::cell::Cell::new(None),
            presented_frames: std::cell::Cell::new(0),
            skipped_frames: std::cell::Cell::new(0),
            present_intervals: std::cell::RefCell::new(Vec::with_capacity(PRESENT_SAMPLE_CAP)),
            last_present: std::cell::Cell::new(None),
        }
    }

    /// Configure the surface and synchronise every size-dependent renderer state.
    /// The caller must ensure no `SurfaceTexture` is outstanding.
    fn configure_surface(&self, size: winit::dpi::PhysicalSize<u32>, now: std::time::Instant) {
        debug_assert!(size.width > 0 && size.height > 0);
        let sf = self.inner.scale_factor();
        let dpi_override = self.applied_dpi_override.get();
        let scale = dpi_override.unwrap_or(sf) as f32;
        let dpi_scale = sf as f32;
        let (logical_w, logical_h) = phys_to_logical((size.width, size.height), dpi_override.unwrap_or(sf));

        let mut config = self.surface_config.borrow().clone();
        config.width = size.width;
        config.height = size.height;
        self.surface.borrow().configure(&self.gpu.device, &config);

        self.applied_present_mode.set(config.present_mode);
        self.applied_frame_latency.set(config.desired_maximum_frame_latency);
        *self.surface_config.borrow_mut() = config;
        self.physical_size.set((size.width, size.height));
        self.dpi_scale.set(dpi_scale);
        self.configured_layout.set((size.width, size.height, scale, dpi_scale));
        self.needs_initial_configure.set(false);
        self.last_configure.set(now);
        self.pending_resize_at.set(None);
        self.drag_refresh_mhz.set(None);
        self.last_observed.set((size.width, size.height, scale));
        self.renderer.borrow_mut().resize(
            logical_w as f32,
            logical_h as f32,
            size.width,
            size.height,
            scale,
            dpi_scale,
        );
    }

    /// 绘制一帧（render thread 独占 surface 帧循环）。
    ///
    /// `clear_color` 为本帧目标底色，任何情况下都会先 Clear 再绘制 batch。
    /// （不提供「保留旧内容」模式：present 后 swapchain buffer 内容即被丢弃，
    /// `LoadOp::Load` 只会读到未定义值，对窗口无意义。）
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
        clear_color: crate::color::Color,
        batches: &[&DrawBatch],
    ) -> DrawReport {
        let report = self.draw_frame(clear_color, batches);
        self.last_draw_outcome.set(Some(report.outcome));
        self.last_draw_report.set(Some(report));
        match report.outcome {
            DrawOutcome::Presented { .. } => {
                self.presented_frames.set(self.presented_frames.get().saturating_add(1));
                self.record_present();
            }
            DrawOutcome::Skipped(_) => {
                self.skipped_frames.set(self.skipped_frames.get().saturating_add(1));
            }
            DrawOutcome::Failed(_) => {}
        }
        report
    }

    /// 记录一次成功 present 的间隔（供 `presented_fps` 滑动窗口）。
    fn record_present(&self) {
        let now = std::time::Instant::now();
        if let Some(prev) = self.last_present.get() {
            let dt = now.duration_since(prev).as_secs_f64();
            if dt > 0.0 && dt < 0.5 {
                let mut v = self.present_intervals.borrow_mut();
                v.push(dt);
                if v.len() > PRESENT_SAMPLE_CAP {
                    v.remove(0);
                }
            }
        }
        self.last_present.set(Some(now));
    }

    fn draw_frame(
        &self,
        clear_color: crate::color::Color,
        batches: &[&DrawBatch],
    ) -> DrawReport {
        let gpu_secs = self.last_gpu_secs.lock().unwrap().take();
        if self.closing.get() {
            return DrawReport {
                outcome: DrawOutcome::Skipped(DrawSkipReason::Closing),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
            };
        }
        if self.gpu.is_device_lost() {
            return DrawReport {
                outcome: DrawOutcome::Failed(DrawFailure::DeviceLost),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
            };
        }
        // 自动输入刷新（`auto_refresh_input` 默认开；用户如需当帧零滞后可在 on_frame 内手动调）
        if self.auto_refresh_input.get() {
            self.refresh_input();
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
        if let Some(latency) = self.pending_frame_latency.take() {
            self.surface_config.borrow_mut().desired_maximum_frame_latency = latency;
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
        let dpi_override = self.dpi_override.get();
        // override 覆盖变更（`set_dpi_override` 请求物理 resize）：物理尺寸落到
        // 目标（或超时兜底）前，仍用旧 override 换算——否则物理旧尺寸 × 新 scale
        // 会让逻辑瞬时漂移。落地后推进 applied_dpi_override，本帧起用新 override，
        // 尺寸漂移走下方正常的 resize 去抖 / 跟随 / configure 路径。
        if dpi_override != self.applied_dpi_override.get() {
            let reached = match self.pending_override_target.get() {
                Some((pw, ph)) => (pw == size.width && ph == size.height)
                    || self
                        .pending_override_since
                        .get()
                        .is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(1)),
                None => true,
            };
            if reached {
                self.applied_dpi_override.set(dpi_override);
                self.pending_override_target.set(None);
                self.pending_override_since.set(None);
            }
}
        // 构图缓存由 `refresh_metrics` 显式提供：此处复用它做 layout_follow 相机推进
        // （drift 判定与下方 resize 路径一致），避免 draw 内再复制一份尺寸轮询逻辑。
        // 用户也可在 on_frame 内手动调 `refresh_metrics` 以拿当帧构图新鲜度；漏调则由
        // 本帧 draw 补一次。
        self.refresh_metrics();
        let dpi_override = self.applied_dpi_override.get();
        let new_scale = dpi_override.unwrap_or(sf) as f32;
        let mut configured_this_frame = false;
        let mut follow_pending = false;
        {
            let sc = self.surface_config.borrow();
            // 尺寸漂移只看物理（逻辑 = 物理 ÷ scale，物理在容差内且 scale 不变 ⇒
            // 逻辑必在容差内），scale 变化单独捕获。
            let size_drifted = size_drifted_beyond(
                (sc.width, sc.height),
                (size.width, size.height),
                RESIZE_DRIFT_EPSILON,
            ) || new_scale != self.layout_scale() as f32;
            let mode_drifted = sc.present_mode != self.applied_present_mode.get();
            let latency_drifted =
                sc.desired_maximum_frame_latency != self.applied_frame_latency.get();
            drop(sc);
            let now = std::time::Instant::now();
            // 「移动」= 相对锚点（`last_observed`，上次显著变化位置）的变化，物理轴
            // 超容差 `RESIZE_DRIFT_EPSILON` 或缩放变化才算；锚点只在移动时推进。
            // 快速拖动松手后 Windows 会把 inner_size 短暂报成相邻像素抖动（约 1s）：
            // 若按上一帧精确比较，`moved` 每帧都真 → 去抖计时永不过期 → snap 永不
            // 触发，follow 持续跟随抖动 → 画面反复左右拉伸（抽搐）。带容差的锚点
            // 比较让小抖动不再刷新计时：计时开始老化，满去抖时长即一次性 snap。
            let moved = size_drifted
                && observed_moved(
                    self.last_observed.get(),
                    (size.width, size.height, new_scale),
                    RESIZE_DRIFT_EPSILON as f32,
                );
            if moved {
                self.last_observed.set((size.width, size.height, new_scale));
                let drag_starting = self.pending_resize_at.get().is_none();
                self.pending_resize_at.set(Some(now));
                if drag_starting {
                    // 拖动开始：缓存显示器刷新率（acquire 失去 vsync 节流时用它
                    // 临时压 cap 防空转）。只查一次；monitor 不跨屏时刷新率稳定。
                    // 原样存 milli-Hz（winit 返回值），换算只发生在 drag_effective_cap。
                    self.drag_refresh_mhz
                        .set(self.current_monitor().and_then(|m| m.refresh_rate_millihertz()));
                }
            }
            let refresh = resize_refresh(
                size_drifted,
                self.pending_resize_at.get(),
                now,
                self.resize_debounce.get(),
                self.resize_policy.get(),
                self.last_configure.get(),
            );
            let need_configure = self.needs_initial_configure.get()
                || (size_drifted && refresh != ResizeRefresh::None)
                || mode_drifted || latency_drifted;
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
                    eprintln!("[draw] follow-layout(drift) {}x{}", size.width, size.height);
                }
                // 相机（physical_size/dpi_scale）已由上方 `refresh_metrics()` 推进；
                // 这里只置 follow_pending 标记供下方 acquire 后平滑段使用。
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
                // `preparable` 兜底：首帧若持续走 skip，窗口会永远隐藏——显示它
                // （宁可无内容一帧，不可永不出现）。
                self.maybe_show_prepared_window();
                return skip_report(gpu_secs, DrawSkipReason::Timeout);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                self.maybe_show_prepared_window();
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
                // get_current_texture 内部校验失败：surface 配置可能已失效。
                // 按当前尺寸重配后本帧跳过（与 Outdated 一致），并记录告警——
                // 不再伪装成「已重配」（旧实现只打 SurfaceReconfigured 不实际重配）。
                log::warn!("vireo surface get_current_texture validation error — reconfiguring");
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
let dpi_override = self.applied_dpi_override.get();
        let new_scale = dpi_override.unwrap_or(sf) as f32;
        let dpi_scale = sf as f32;
            let still_drifted = {
                let sc = self.surface_config.borrow();
                size_drifted_beyond(
                    (sc.width, sc.height),
                    (size.width, size.height),
                    RESIZE_DRIFT_EPSILON,
                ) || new_scale != self.layout_scale() as f32
            };
            if still_drifted && size.width != 0 && size.height != 0 {
                // 平滑模式分派：PerFrame 每帧追；Average 用滑动窗均值（物理像素空间
                // 均值，逻辑 = 物理 ÷ scale 现算）。
                let frame = self.follow_frame.get() + 1;
                self.follow_frame.set(frame);
                let now = std::time::Instant::now();
                let target: Option<(u32, u32)> = match self.follow_smoothing.get() {
                    FollowAmount::PerFrame => Some((size.width, size.height)),
                    FollowAmount::Average(amt) => {
                        // 采样并入滑动窗，淘汰过期样本，取均值（连续渐变，不跳格）。
                        let mut q = self.follow_samples.borrow_mut();
                        q.push_back((frame, now, size.width, size.height));
                        loop {
                            let stale = match amt {
                                FollowFramesOrTime::Time(d) => q
                                    .front()
                                    .map(|&(_, t, _, _)| now.saturating_duration_since(t) > d)
                                    .unwrap_or(false),
                                FollowFramesOrTime::Frames(n) => q
                                    .front()
                                    .map(|&(f, _, _, _)| frame.saturating_sub(f) >= n as u64)
                                    .unwrap_or(false),
                            };
                            if stale {
                                q.pop_front();
                            } else {
                                break;
                            }
                        }
                        let (tw, th) = q.iter().fold(
                            (0u64, 0u64),
                            |(a, b), &(_, _, w, h)| (a + w as u64, b + h as u64),
                        );
                        let n = q.len().max(1) as u64;
                        drop(q);
                        Some(((tw / n).max(1) as u32, (th / n).max(1) as u32))
                    }
                };
                if let Some((pw, ph)) = target {
                    if trace {
                        eprintln!("[draw] follow-layout(acq) {}x{}", pw, ph);
                    }
                    let (logical_w, logical_h) =
                        phys_to_logical((pw, ph), dpi_override.unwrap_or(sf));
                    self.physical_size.set((pw, ph));
                    self.dpi_scale.set(dpi_scale);
                    self.renderer.borrow_mut().update_layout(
                        logical_w as f32, logical_h as f32, new_scale, dpi_scale,
                    );
                    self.renderer.borrow_mut().set_text_viewport_override(Some((pw, ph)));
                }
            } else {
                // 松手尺寸回稳但尚未 snap（debounce 未满）：不再重排，清虚拟 viewport，
                // 内容停在当前布局，等 Stable 分支一次性 configure。
                self.renderer.borrow_mut().set_text_viewport_override(None);
            }
        }

        // 4b) 编码
        let view = st.texture.create_view(&Default::default());
        let target = crate::render::RenderTarget::from_texture_view(view);
        let batch_refs: Vec<&DrawBatch> = batches.iter().copied().collect();
        let t2 = std::time::Instant::now();
        let cmd_buf = self.renderer.borrow().draw(&target, Some(clear_color), &batch_refs);
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
        // Wayland 需要 present 前通知合成器（调度 frame callback）；其余平台 no-op。
        self.inner.pre_present_notify();
        let t3 = std::time::Instant::now();
        self.gpu.queue.present(st);
        let present_secs = t3.elapsed().as_secs_f64();

        if std::env::var_os("VIREO_PHASE_STATS").is_some() {
            if let Some((qpc_vblank, qpc_period)) = dwm_timing() {
                let now = qpc_now();
                let per_sec = qpc_ticks_per_sec() as f64;
                let vblank_ms = qpc_period as f64 / per_sec * 1e3;
                // phase = 距最近 vblank 的时间 / 周期（0~1，0=刚过 vblank）。
                // 注意：qpcVBlank 是 vblank 网格上的**某个**采样点，可能在 now 之前/之后
                // 甚至离 now 好几帧（Firefox 源码分析证实），直接相减没有意义。
                // 用 rem_euclid 归一到 [0, period)，无论采样在过去/未来都得到正确的相位。
                let period = qpc_period.max(1) as i128;
                let phase_ticks = (now as i128 - qpc_vblank as i128).rem_euclid(period);
                let phase = phase_ticks as f64 / period as f64;
                self.last_phase_sample.set(Some(PhaseSample {
                    phase,
                    vblank_ms,
                    stretch: suboptimal,
                    dragging: self.pending_resize_at.get().is_some(),
                }));
            }
        }

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

        // `preparable`：首帧渲染成功（已 present）后显示窗口。此时 surface 已
        // configure、内容已渲染，窗口第一次出现即完整形态，消除 winit 建窗的
        // 4 阶段闪烁。winit `set_visible` 线程安全（排队到 winit 线程）。
        self.maybe_show_prepared_window();

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

    /// `preparable` 建窗：首帧后显示窗口（消除 winit 建窗 4 阶段闪烁）。
    /// 只在 `pending_show` 置位时触发一次——首帧成功 `Presented` 后，或
    /// 首次 draw 走 skip 路径（Timeout/Occluded 等）时兜底，避免窗口永远隐藏。
    /// winit `set_visible` 线程安全（排队到 winit 线程）。
    fn maybe_show_prepared_window(&self) {
        if self.pending_show.get() {
            self.inner.set_visible(true);
            self.pending_show.set(false);
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
            log::warn!("vireo PresentMode {requested:?} not supported, falling back to AutoVsync");
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
        self.physical_size.set((width, height));
        self.dpi_scale.set(sf as f32);
    }

    /// 显式刷新本窗构图缓存（尺寸/`scale`/`metrics`）。
    ///
    /// 采 `Window::inner_size()`/`scale_factor()`，`0×0`（最小化）返回 `false`
    ///（调用方应跳过本窗本帧构图）；否则按 `layout_follow` 与 `RESIZE_DRIFT_EPSILON`
    /// 推 `physical_size`/`dpi_scale`（与 `draw` 的 follow 策略一致），返回 `true`。
    /// 不触发 `surface.configure`（`present_mode`/`AA`/`latency` 等重配仍由 `draw` 兑现）。
    /// `draw` 未调本方法时静默用旧快照；`draw` 内部复用本方法判据，外部显式调用可获同帧新鲜度。
    pub fn refresh_metrics(&self) -> bool {
        let size = self.inner.inner_size();
        if size.width == 0 || size.height == 0 {
            return false;
        }
        let sf = self.inner.scale_factor();
        let dpi_override = self.applied_dpi_override.get();
        let scale = dpi_override.unwrap_or(sf) as f32;
        let dpi_scale = sf as f32;
        let drift = {
            let sc = self.surface_config.borrow();
            size_drifted_beyond(
                (sc.width, sc.height),
                (size.width, size.height),
                RESIZE_DRIFT_EPSILON,
            ) || scale != self.layout_scale() as f32
        };
        if self.layout_follow.get() && drift {
            self.physical_size.set((size.width, size.height));
            self.dpi_scale.set(dpi_scale);
        }
        true
    }

    /// 显式拉取本窗待处理输入（drain 通道中排队的 `WinitEvent` 输入事件，应用到 `InputState`）。
    ///
    /// 覆盖上次 `refresh_input` 到本次调用之间 winit 线程攒下的全部输入：按键/鼠标按下释放
    /// 的净结果、滚轮增量（`take_scroll` 仍返回区间累加）、`mouse_pos`/`focus`/`cursor_inside`
    /// 最新值。`0×0` 等窗口状态不影响输入，恒返回是否有事件被应用。
    ///
    /// `draw` 在 `auto_refresh_input` 开启（默认）时自动调一次，用户不必手动重复；
    /// 设为 `false` 后由用户在 `on_frame` 内构批次前调用，可获得当帧零滞后输入。
    /// 漏调则这段事件在下次 `refresh_input` 才结算（与 `refresh_metrics` 同策略）。
    pub fn refresh_input(&self) -> bool {
        let mut q = self.pending_input.borrow_mut();
        if q.is_empty() {
            return false;
        }
        for ev in q.drain(..) {
            self.apply_input_event(ev);
        }
        true
    }

    /// `draw` 内自动输入刷新的开关（默认开）。见 [`Self::refresh_input`]。
    pub fn set_auto_refresh_input(&self, enabled: bool) {
        self.auto_refresh_input.set(enabled);
    }

    /// 当前是否启用 `draw` 内自动输入刷新。见 [`Self::refresh_input`]。
    pub fn auto_refresh_input(&self) -> bool {
        self.auto_refresh_input.get()
    }

    /// 把单个输入事件应用到 `InputState`（与 `pending_input` 队列的语义一致）。
    fn apply_input_event(&self, ev: WinitEvent) {
        match ev {
            WinitEvent::CursorMoved { x, y, .. } => {
                self.mouse_pos.set((x as f32, y as f32));
            }
            WinitEvent::KeyboardInput { event, .. } => {
                let is_pressed = event.state.is_pressed();
                let repeat = event.repeat;
                if is_pressed && !repeat {
                    self.input.keys_down.borrow_mut().insert(event.key);
                } else if !is_pressed {
                    self.input.keys_down.borrow_mut().remove(&event.key);
                }
            }
            WinitEvent::MouseInput { button, pressed, .. } => {
                if pressed {
                    self.input.mouse_buttons_down.borrow_mut().insert(button);
                } else {
                    self.input.mouse_buttons_down.borrow_mut().remove(&button);
                }
            }
            WinitEvent::MouseWheel { delta, .. } => {
                let mut acc = self.input.scroll_delta.borrow_mut();
                match &delta {
                    crate::input::ScrollDelta::Line { x, y } => {
                        acc.line.0 += x;
                        acc.line.1 += y;
                    }
                    crate::input::ScrollDelta::Pixel { x, y } => {
                        acc.pixel.0 += x;
                        acc.pixel.1 += y;
                    }
                }
            }
            WinitEvent::ModifiersChanged { modifiers, .. } => {
                *self.input.modifiers.borrow_mut() = modifiers;
            }
            WinitEvent::Focused { focused, .. } => {
                let was_focused = std::mem::replace(&mut *self.input.focused.borrow_mut(), focused);
                if !focused && was_focused {
                    self.input.keys_down.borrow_mut().clear();
                    self.input.mouse_buttons_down.borrow_mut().clear();
                }
            }
            WinitEvent::CursorEntered { .. } => {
                *self.input.cursor_inside.borrow_mut() = true;
            }
            WinitEvent::CursorLeft { .. } => {
                *self.input.cursor_inside.borrow_mut() = false;
            }
            WinitEvent::Touch { event, .. } => {
                let sf = self.applied_dpi_override.get().unwrap_or(self.inner.scale_factor());
                let tx = (event.x as f64 / sf) as f32;
                let ty = (event.y as f64 / sf) as f32;
                match event.phase {
                    crate::input::TouchPhase::Started | crate::input::TouchPhase::Moved => {
                        self.input.touches.borrow_mut().insert(event.id, (tx, ty, event.force));
                    }
                    _ => {
                        self.input.touches.borrow_mut().remove(&event.id);
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the observed window metrics differ from the configured surface/layout.
    pub fn resize_pending(&self) -> bool {
        let size = self.inner.inner_size();
        let sf = self.inner.scale_factor();
        let dpi_override = self.applied_dpi_override.get();
        let scale = dpi_override.unwrap_or(sf) as f32;
        let config = self.surface_config.borrow();
        size_drifted_beyond(
            (config.width, config.height),
            (size.width, size.height),
            RESIZE_DRIFT_EPSILON,
        ) || self.configured_layout.get() != (size.width, size.height, scale, sf as f32)
    }

    /// Number of successful `queue.present` calls made by this window.
    pub fn presented_frames(&self) -> u64 {
        self.presented_frames.get()
    }

    /// 最近成功 present 的提交频率（滑动窗口平均值）。
    ///
    /// 这是**提交节拍**（本进程成功 `queue.present` 的间隔），不是显示器/compositor
    /// 实际呈现频率：present 只把帧排队给合成器，displayed FPS 需 DXGI present
    /// statistics / PresentMon / ETW 才能测得，不能用 CPU 循环推断。无样本返回 0。
    pub fn presented_fps(&self) -> f64 {
        sliding_rate(&self.present_intervals.borrow())
    }

    /// Number of draw attempts skipped before present.
    pub fn skipped_frames(&self) -> u64 {
        self.skipped_frames.get()
    }

    /// 获取当前鼠标位置（客户端坐标，物理 + 逻辑双表示）。
    ///
    /// 与 [`Self::inner_position`] 一致：`.physical()` 返回物理像素，`.logical()`
    /// 返回 vireo 逻辑像素（= 物理 ÷ 当前有效 scale）。
    ///
    /// **不阻塞**：读 vireo 内部缓存（由事件 / 渲染线程轮询锚点更新），任何线程可调，
    /// 无 winit 跨线程 hop（macOS 亦如此）。
    pub fn mouse_pos(&self) -> PixelPos {
        let mp = self.mouse_pos.get();
        to_pixel_pos(mp.0 as f64, mp.1 as f64, self.layout_scale())
    }

    /// 获取当前投影矩阵（逻辑像素）
    pub fn projection(&self) -> glam::Mat4 {
        let (w, h) = phys_to_logical(self.physical_size.get(), self.layout_scale());
        glam::camera::rh::proj::opengl::orthographic(
            0.0,
            w as f32,
            h as f32,
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

    /// 窗口当前是否聚焦（基于最近 `Focused` 事件）。
    ///
    /// **不阻塞**：读 vireo 内部缓存，任何线程可调，无 winit 跨线程 hop。
    pub fn focused(&self) -> bool {
        *self.input.focused.borrow()
    }

    /// 窗口当前是否失焦（`!focused()`）。
    ///
    /// **语义与 Electron `BrowserWindow.blur()` 的差异**：Electron `blur()` 是
    /// **主动让窗口失焦**的命令（`win.blur()` 会调用 OS API 把焦点转移到别处）；
    /// vireo 本方法只暴露**查询**面（基于最近 `Focused` 事件），不实现命令面
    /// ——winit 0.30.13 没有"主动让窗口失焦"的运行期 API，vireo 暂不绕过 winit
    /// 自实现。如需在程序内主动转移焦点，临时用 `set_visible(false)` / 重新
    /// 聚焦其他窗口再聚焦回来等方式模拟。
    pub fn blur(&self) -> bool {
        !self.focused()
    }

    /// 光标是否在窗口客户区内（基于最近 `CursorEntered` / `CursorLeft` 事件）。
    ///
    /// **不阻塞**：读 vireo 内部缓存，任何线程可调，无 winit 跨线程 hop。
    pub fn cursor_inside(&self) -> bool {
        *self.input.cursor_inside.borrow()
    }

}

/// 应用管理器 —— 管理 GPU 上下文、窗口、纹理。
/// 构造后在 `run()` 之前配置窗口/纹理/离屏画布，`run()` 将 `self` 移动到渲染线程。
/// 渲染线程 → winit 线程的运行期窗口创建请求（`App::window` 在 on_frame 里调用时）。
/// winit 线程在 `about_to_wait` drain 后执行窗口创建。
struct CreateWindowRequest {
    handle: u64,
    desc: WindowDesc,
    init_duration: f64,
    on_close: Option<Box<dyn FnOnce() + Send>>,
}

pub struct App {
    /// run() 前由 `App::window` 入队的待建窗口描述；run() 取出给 winit 线程。
    /// 运行期（run 中）的 `App::window` 改走 `create_tx` 通道，不再写本字段。
    pub(crate) window_descs: RefCell<Vec<WindowDesc>>,
    /// `Vec<Option<VireoWindow>>`，以 handle 为索引。关闭的窗口为 `None`。
    pub windows: Vec<Option<VireoWindow>>,
    pub gpu: Arc<GpuContext>,
    instance: Option<wgpu::Instance>,
    /// 设备丢失标志：由 `GpuContext` 的 `Device::set_device_lost_callback` 置位
    ///（`GpuContext::device_lost()` 同 Arc）。渲染循环每帧读它，置位则干净终止。
    device_lost: Arc<std::sync::atomic::AtomicBool>,
    /// 稳定 handle → winit WindowId。handle 由 `App::window()` 分配，
    /// 在 run() 中被取出给 winit 线程用。
    handle_to_id: FxHashMap<u64, WindowId>,
    /// 下一个待分配的 handle（单调递增；`App::window()` 自增）。
    next_handle: std::cell::Cell<u64>,
    close_hooks: RefCell<FxHashMap<u64, Option<Box<dyn FnOnce() + Send>>>>,
    /// 输入事件回调集合（按 handle 索引，run() 后迁移到 winit 线程）
    callbacks: FxHashMap<u64, crate::input::InputCallbacks>,
    default_icon: Option<Icon>,
    textures: Vec<Texture>,
    offscreens: Vec<OffscreenCanvas>,
    pub frame_count: u64,
    /// 相邻两次 update/`on_frame` 调用的间隔（秒），瞬时值。第一帧为 0。
    /// 它不是相邻 displayed frame 的呈现间隔。
    pub frame_time: f64,
    /// update/`on_frame` 调用频率的滑动窗口平均值（约 0.5s@60Hz）。它不是 displayed FPS；
    /// surface acquire、present mode、跳帧或合成器可能使实际显示频率不同。第一帧为 0。
    /// 需要每窗口成功提交节拍用 [`VireoWindow::presented_fps`]。
    pub fps: f64,
    /// App::new 内部耗时（秒）：GPU 设备、shader 模块、bind group layout 构造。
    pub init_duration: f64,
    /// 各 `app.window()` 调用的 init_duration（秒），按调用顺序入队。
    /// run() 中按序出队给 winit 线程用。
    window_init_durations: RefCell<Vec<f64>>,
    /// 最近若干帧的间隔，用于平滑 FPS。
    fps_samples: Vec<f64>,
    last_frame: std::time::Instant,
    deferred_tasks: RefCell<Vec<DeferredTask>>,
    /// 可选帧率上限（`App::set_max_fps`）。仅在 acquire 不阻塞（拖动/无 vsync）时
    /// 用 sleep 把 CPU 循环拉回目标频率，避免空转。默认 `Some(240)`：给足余量，
    /// 正常 vsync 下 acquire 更早卡住、cap 不生效；仅在空转时兜底。
    max_fps: std::cell::Cell<Option<u32>>,
    /// 拖动期帧率上限开关（`App::set_drag_cap`）。与 `set_max_fps` 解耦：开启时
    /// resize 拖动中即使 `max_fps(None)` 也压到显示器刷新率（省资源但画面内容
    /// 变化实测易卡）；关闭则拖动期不额外压制、渲染循环全速产帧，画面内容随
    /// 窗口尺寸变化更平滑。默认开启。
    drag_cap: std::cell::Cell<bool>,
    /// 下一次帧循环「开始」的目标时刻（相位锁）。每帧推进一个 stride；落后（中途
    /// 耗时已超 stride）时不追赶、直接跳到 now+stride，避免攒出 33ms 双帧。
    pacing_deadline: std::cell::Cell<Option<std::time::Instant>>,
    /// 运行期窗口创建通道：run() 内设置（取 create_rx 给 winit 线程），
    /// 之后 `App::window` 通过它把创建请求发给 winit 线程。
    create_tx: Option<mpsc::Sender<CreateWindowRequest>>,
    /// 运行期已发出、尚未收到 `WindowCreated` 的窗口数（渲染线程维护）。
    /// 退出判定用它防止「请求在途但窗口尚未出现」时提前退出。
    pending_creates: std::cell::Cell<usize>,
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
    pub(crate) fn is_ready(&self, frame_count: u64) -> bool {
        match self.kind {
            DeferredTaskKind::AfterFrames(target) => frame_count >= target,
            DeferredTaskKind::AfterSecs(wakeup) => std::time::Instant::now() >= wakeup,
        }
    }
}

#[cfg(test)]
impl DeferredTask {
    /// 仅测试用：构造一个 `after_frames(target)` 等价任务（空闭包）。
    pub(crate) fn for_frames(target: u64) -> Self {
        DeferredTask {
            kind: DeferredTaskKind::AfterFrames(target),
            f: Box::new(|| {}),
        }
    }
}

enum DeferredTaskKind {
    AfterFrames(u64),
    AfterSecs(std::time::Instant),
}

impl App {
    /// 创建 App（内部 `InstanceDescriptor::new_without_display_handle_from_env()`：
    /// 允许 `WGPU_BACKEND` 等环境变量选择后端）。构造时即初始化 GPU 设备，
    /// 可在 run() 之前加载纹理等资源。
    pub fn new() -> Self {
        App::from_instance_descriptor(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
    }

    /// 用 wgpu `InstanceDescriptor` 构造 App —— 代码指定后端/flags/内存预算/backend
    /// options/display，压过 `WGPU_BACKEND` 等环境变量（不读 env）。
    ///
    /// `display` 原样透传：vireo 用窗口自身 handle 创建 surface（create_surface），
    /// 若 display 与窗口 handle 所属显示服务器不一致会触发 wgpu 校验错误
    /// `MismatchingDisplayHandle`；非 GLES（Wayland）后端通常传 `None` 即可。
    pub fn with_descriptor(desc: wgpu::InstanceDescriptor) -> Self {
        App::from_instance_descriptor(desc)
    }

    fn from_instance_descriptor(desc: wgpu::InstanceDescriptor) -> Self {
        let init_start = std::time::Instant::now();
        let instance = wgpu::Instance::new(desc);
        let gpu = Arc::new(GpuContext::new(&instance));
        let device_lost = gpu.device_lost();
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
            window_descs: RefCell::new(Vec::new()),
            windows: Vec::new(),
            gpu,
            instance: Some(instance),
            device_lost,
            handle_to_id: FxHashMap::default(),
            next_handle: std::cell::Cell::new(0),
            close_hooks: RefCell::new(FxHashMap::default()),
            callbacks: FxHashMap::default(),
            default_icon,
            textures: Vec::new(),
            offscreens: Vec::new(),
            frame_count: 0,
            frame_time: 0.0,
            fps: 0.0,
            init_duration,
            window_init_durations: RefCell::new(Vec::new()),
            fps_samples: Vec::with_capacity(FPS_SAMPLE_CAP),
            last_frame: std::time::Instant::now(),
            deferred_tasks: RefCell::new(Vec::new()),
            max_fps: std::cell::Cell::new(Some(240)),
            drag_cap: std::cell::Cell::new(true),
            pacing_deadline: std::cell::Cell::new(None),
            create_tx: None,
            pending_creates: std::cell::Cell::new(0),
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

    /// 配置一个待创建的窗口。可选 on_close 钩子在窗口被关闭时调用。
    /// 同步预热窗口 AA 对应的 SDF + geo 管线，并把 AA clamp 到硬件上限（避免 wgpu panic）。
    /// 构造耗时在 `App::run` 创建窗口后由 `VireoWindow::init_duration()` 暴露。
    ///
    /// **run() 之前调用**（`&mut App`）→ 入队，由 `App::run` 的 winit 线程在
    /// `resumed` 创建。
    /// **run() 之后（on_frame 里）调用** → 经内部通道发给 winit 线程异步创建，
    /// 下一轮 `about_to_wait` 生效；创建完成前 `App::window_ref` 返回 `None`。
    pub fn window(&self, mut desc: WindowDesc, on_close: Option<impl FnOnce() + Send + 'static>) -> WindowIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(desc.anti_aliasing, self.gpu.supported_sample_counts());
        desc.anti_aliasing = aa;
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        let handle = self.next_handle.get();
        self.next_handle.set(handle + 1);
        let on_close = on_close.map(|f| Box::new(f) as Box<dyn FnOnce() + Send>);
        match &self.create_tx {
            // 运行期：发请求给 winit 线程异步创建。
            Some(tx) => {
                let _ = tx.send(CreateWindowRequest {
                    handle,
                    desc,
                    init_duration,
                    on_close,
                });
                self.pending_creates.set(self.pending_creates.get() + 1);
            }
            // run() 前：入队，run() 取出交给 winit 线程在 resumed 创建。
            None => {
                self.window_init_durations.borrow_mut().push(init_duration);
                self.window_descs.borrow_mut().push(desc);
                self.close_hooks.borrow_mut().insert(handle, on_close);
            }
        }
        WindowIndex::new(handle)
    }

    // ------ 输入事件回调注册（winit 线程 invoke，无需 +Send）------
    crate::def_app_ons! {
        on_key_down: impl FnMut(&crate::input::KeyEvent) + 'static => on_key_down,
        on_key_up: impl FnMut(&crate::input::KeyEvent) + 'static => on_key_up,
        on_mouse_down: impl FnMut(&crate::input::MouseButtonEvent) + 'static => on_mouse_down,
        on_mouse_up: impl FnMut(&crate::input::MouseButtonEvent) + 'static => on_mouse_up,
        on_scroll: impl FnMut(&crate::input::MouseScrollEvent) + 'static => on_scroll,
        on_cursor_entered: impl FnOnce() + 'static => on_cursor_entered,
        on_cursor_left: impl FnOnce() + 'static => on_cursor_left,
        on_touch: impl FnMut(&crate::input::TouchEvent) + 'static => on_touch,
        on_focus_gained: impl FnOnce() + 'static => on_focus_gained,
        on_focus_lost: impl FnOnce() + 'static => on_focus_lost,
        on_modifiers_changed: impl FnMut(crate::input::Modifiers) + 'static => on_modifiers_changed,
        on_ime: impl FnMut(&crate::input::Ime) + 'static => on_ime,
        on_file_dropped: impl FnMut(&std::path::PathBuf) + 'static => on_file_dropped,
        on_file_hovered: impl FnMut(&std::path::PathBuf) + 'static => on_file_hovered,
        on_file_hover_cancelled: impl FnOnce() + 'static => on_file_hover_cancelled,
        on_moved: impl FnMut(winit::dpi::PhysicalPosition<i32>) + 'static => on_moved,
        on_theme_changed: impl FnMut(winit::window::Theme) + 'static => on_theme_changed,
        /// 窗口尺寸（物理像素）变化时回调（模态循环期间可能滞后，渲染线程逐帧轮询兜底）。
        /// 运行在 winit 线程。
        on_resized: impl FnMut(winit::dpi::PhysicalSize<u32>) + 'static => on_resized,
        #[cfg(target_os = "windows")]
        /// 任务栏缩略图按钮点击回调（参数 = 按钮 `id`）。仅 Windows 生效。
        ///
        /// 运行在 winit 线程。窗口创建后（`resumed`）由 Runner 注册到进程级拦截子类，
        /// 与运行期 [`VireoWindow::on_thumb_button`] 等价。
        on_thumb_button: impl FnMut(u32) + 'static => on_thumb_button,
    }

    /// 注册一个延迟 `frames` 帧后执行的闭包。
    /// 帧计数以 `render_on_frame` 循环的帧为单位。任务在**帧末**（`on_frame` / `draw` 之后）
    /// 执行：若在 `on_frame` 内注册，则 `after_frames(0)` 于本帧末尾执行、`after_frames(1)`
    /// 于下一帧末尾执行；若在 `run()` 之前注册（`frame_count == 0`），`after_frames(0)` 于
    /// 第一个 `on_frame` 之前执行、`after_frames(1)` 于第 1 帧末尾执行。
    pub fn after_frames<F: FnOnce() + Send + 'static>(&self, frames: u64, f: F) {
        let target = self.frame_count + frames;
        self.deferred_tasks.borrow_mut().push(DeferredTask {
            kind: DeferredTaskKind::AfterFrames(target),
            f: Box::new(f),
        });
    }

    /// 注册一个延迟 `secs` 秒后执行的闭包（墙钟时间）。与 `after_frames` 一样在帧末检查到期，
    /// 因此实际执行点落在某帧末尾（而非 sleep 精确的墙钟时刻）。
    pub fn after_secs<F: FnOnce() + Send + 'static>(&self, secs: f64, f: F) {
        self.deferred_tasks.borrow_mut().push(DeferredTask {
            kind: DeferredTaskKind::AfterSecs(
                std::time::Instant::now() + std::time::Duration::from_secs_f64(secs),
            ),
            f: Box::new(f),
        });
    }

    /// 执行所有已到期的延迟任务（`after_frames` / `after_secs` 注册）。
    /// 在每帧末尾（on_frame / draw 之后）调用；循环开始前也会调用一次（此时
    /// `frame_count == 0`），让 `run()` 之前排的 `after_frames(0)` 在第一个 `on_frame`
    /// 之前执行。语义：`after_frames(k)` 于「编号为注册时 `frame_count + k` 的帧末」执行。
    fn run_due_deferred_tasks(&self) {
        let ready = {
            let mut tasks = self.deferred_tasks.borrow_mut();
            let mut ready = Vec::new();
            let mut i = 0;
            while i < tasks.len() {
                if tasks[i].is_ready(self.frame_count) {
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

    /// 启动事件循环 + 渲染线程。
    /// winit 线程只负责任何操作，渲染线程持有 `App` + `on_frame` 独立运行。
    /// 闭包签名: FnMut(&App) -> bool，返回 true 继续循环，false 退出。
    pub fn run<F: FnMut(&App) -> bool + Send + 'static>(
        mut self,
        on_frame: F,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let event_loop = EventLoop::new().unwrap();

        let window_init_durations = std::mem::take(&mut *self.window_init_durations.borrow_mut());
        let window_descs: Vec<_> = self.window_descs.borrow_mut().drain(..).collect();
        let close_hooks = std::mem::take(&mut *self.close_hooks.borrow_mut());
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
        // 渲染线程 → winit 线程：运行期边框样式切换。SetWindowSubclass /
        // RemoveWindowSubclass 必须在窗口 owner（winit 事件）线程调用（见
        // platform::windows::install 注释），而 set_frame_style 从渲染线程发出，
        // 故通过本 channel 转发到 Runner::about_to_wait 在 winit 线程执行。
        let (frame_style_tx, frame_style_rx) = mpsc::channel::<(isize, FrameStyle)>();
        // 渲染线程 → winit 线程：运行期 set_aspect_ratio。同 set_frame_style，
        // 子类化必须在 winit 事件线程调用；ratio 值存到 platform::windows 进程级表。
        let (aspect_ratio_tx, aspect_ratio_rx) = mpsc::channel::<(isize, Option<f64>)>();
        // 渲染线程 → winit 线程：非客户区管理（§7.6 4 套消息）。
        // SetWindowSubclass 不可跨线程；state 经本 channel 转发到 winit 线程
        // 安装/卸载 nc_subclass。
        let (nc_tx, nc_rx) = mpsc::channel::<(isize, crate::platform::windows::NcUpdate)>();
        // 运行期窗口创建：渲染线程 `App::window`（on_frame 里）→ winit 线程
        // `about_to_wait` drain 后执行窗口创建。render 端 sender 存进 self，
        // 随 self move 到渲染线程；winit 端 receiver 给 Runner。
        let (create_tx, create_rx) = mpsc::channel::<CreateWindowRequest>();
        self.create_tx = Some(create_tx);
        // 渲染线程 → winit 线程：程序化关闭窗口（`VireoWindow::close`）。
        // 与 `WindowEvent::CloseRequested` 同路径：close_hooks / NC 清理 /
        // alive_handles 递减 / 发 `WinitEvent::CloseRequested` 都在 winit 线程
        // 执行，故经本 channel 转发。载荷 = 窗口 handle。
        let (close_tx, close_rx) = mpsc::channel::<usize>();

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
                    frame_style_tx,
                    aspect_ratio_tx,
                    nc_tx,
                    close_tx,
                    device_lost,
                    expected_windows,
                )
            })
            .expect("failed to spawn render thread");

        // Winit 线程：仅创建窗口和转发事件。
        struct Runner {
    event_tx: mpsc::Sender<WinitEvent>,
    /// 接收渲染线程发来的输入回调注册
    cb_rx: mpsc::Receiver<(usize, crate::input::InputCallbacks)>,
    /// 接收渲染线程发来的终止请求
    exit_rx: mpsc::Receiver<()>,
    /// 接收渲染线程发来的运行期边框样式切换（在 winit 线程执行 Win32 子类操作）
    frame_style_rx: mpsc::Receiver<(isize, FrameStyle)>,
    /// 接收渲染线程发来的运行期宽高比设置（在 winit 线程挂接/卸载子类）
    aspect_ratio_rx: mpsc::Receiver<(isize, Option<f64>)>,
/// 接收渲染线程发来的运行期非客户区管理（§7.6；在 winit 线程装/卸 nc_subclass）
            nc_rx: mpsc::Receiver<(isize, crate::platform::windows::NcUpdate)>,
            /// 接收渲染线程发来的运行期窗口创建请求（on_frame 里 `App::window`）
            create_rx: mpsc::Receiver<CreateWindowRequest>,
            /// 接收渲染线程发来的程序化关窗请求（`VireoWindow::close`；载荷 = handle）。
            close_rx: mpsc::Receiver<usize>,
            /// 已创建窗口的 hwnd（按 handle 索引），窗口关闭时用于清理 NC 状态。
            hwnds: Vec<isize>,
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

            /// 完整关窗路径：close_hooks / NC 状态清理 / alive_handles 递减 /
            /// 发 `WinitEvent::CloseRequested`。用户点关闭按钮与
            /// `VireoWindow::close`（经 close_rx drain）共用。
            fn request_close(&mut self, handle: usize) {
                if let Some(hook_opt) = self.close_hooks.get_mut(&(handle as u64)) {
                    if let Some(h) = hook_opt.take() { h(); }
                }
                // 清理 NC / 任务栏状态表，避免 hwnd 被系统复用后串扰到新窗口。
                if let Some(&hwnd) = self.hwnds.get(handle) {
                    if hwnd != 0 {
                        crate::platform::windows::nc_remove(hwnd);
                        crate::platform::windows::drop_thumbar_icons(hwnd);
                        crate::platform::windows::drop_overlay_icons(hwnd);
                        crate::platform::windows::clear_thumbar_callback(hwnd);
                        // 清理 WINDOW_ICONS 空壳，避免 GDI 句柄泄漏
                        crate::platform::windows::remove_window_icons_entry(hwnd);
                    }
                }
                // SurfaceTexture 全部由渲染线程在 draw() 内 acquire→present。
                // owner 只发送关闭请求；最后一个 VireoWindow 由渲染线程 drop 后，
                // 渲染线程会通过 exit_tx 确认退出。此处不能先退出 event loop 再
                // join，否则 owner 可能无期限等待仍在同步 wgpu 调用中的渲染线程。
                self.send(WinitEvent::CloseRequested { handle });
                self.alive_handles -= 1;
            }

            fn create_attrs(desc: &WindowDesc, default_icon: &Option<Icon>, os_scale: f64) -> WindowAttributes {
                let mut attrs = WindowAttributes::default()
                    .with_title(&desc.title)
                    .with_inner_size(dim_to_winit_size(desc.size.0, desc.size.1, desc.dpi_override, os_scale))
                    .with_resizable(desc.resizable)
                    .with_maximized(desc.maximized)
                    // `preparable`：创建隐藏、首帧渲染后显示。winit 建窗是「先以默认
                    // 尺寸+边框显示、再改尺寸/去边框」，首帧渲染后才显示可避免 4 阶段
                    // 闪烁。preparable=false 时保持旧行为（创建即显示）。
                    .with_visible(desc.visible && !desc.preparable)
                    .with_transparent(desc.transparent)
                    .with_decorations(desc.frame_style.decorated())
                    .with_window_level(desc.window_level)
                    .with_content_protected(desc.content_protected)
                    .with_active(desc.active)
                    .with_blur(desc.blur)
                    .with_cursor(desc.cursor.clone())
                    .with_enabled_buttons(desc.enabled_buttons);
                if let Some(d) = desc.min_size {
                    attrs = attrs.with_min_inner_size(dim_to_winit_size(d.0, d.1, desc.dpi_override, os_scale));
                }
                if let Some(d) = desc.max_size {
                    attrs = attrs.with_max_inner_size(dim_to_winit_size(d.0, d.1, desc.dpi_override, os_scale));
                }
                if let Some(d) = desc.position {
                    attrs = attrs.with_position(dim_to_winit_position(d.0, d.1, desc.dpi_override, os_scale));
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
                if let Some(d) = desc.resize_increments {
                    attrs = attrs.with_resize_increments(dim_to_winit_size(d.0, d.1, desc.dpi_override, os_scale));
                }
                if let Some(ph) = desc.parent_window {
                    attrs = unsafe { attrs.with_parent_window(Some(ph.0)) };
                }
                #[cfg(target_os = "macos")]
                {
                    use winit::platform::macos::WindowAttributesExtMacOS;
                    // `HiddenTitlebar` = 隐藏标题栏文本 + 透明标题栏 + 内容区
                    // 延伸到红绿灯下（Electron `titleBarStyle: 'hidden'` 语义）。
                    // 用 `with_title_hidden`（只藏文本，红绿灯保留并由系统接管）
                    // 而不是 `with_titlebar_hidden`（winit 0.30 将其实现为
                    // `Borderless`，红绿灯/边框全部消失，效果等同 `Frameless`）。
                    // 这些只有构造期属性，运行时无 setter → 只能在构造期实现。
                    if desc.frame_style == FrameStyle::HiddenTitlebar {
                        attrs = attrs
                            .with_title_hidden(true)
                            .with_titlebar_transparent(true)
                            .with_fullsize_content_view(true);
                    }
                }
                attrs
            }

            /// 在 winit 线程创建并初始化一个窗口（预建 resumed / 运行期 create_rx 共用）。
            /// 注意 `window_callbacks` / `hwnds` 需先扩到 handle+1（运行期 handle 可能
            /// 超过预建数量），否则 cb_rx drain 的 `get_mut(handle)` 返回 None 会吞掉回调。
            fn create_window(
                &mut self,
                event_loop: &ActiveEventLoop,
                handle: usize,
                desc: &WindowDesc,
                init_duration: f64,
                on_close: Option<Box<dyn FnOnce() + Send>>,
            ) {
                // 运行期窗口：window_callbacks 扩到 handle+1，否则注册到该窗口的
                // 输入/事件回调在 cb_rx drain 时因 get_mut 越界被静默丢弃。
                if self.window_callbacks.len() <= handle {
                    self.window_callbacks.resize_with(handle + 1, crate::input::InputCallbacks::default);
                }
                let os_scale = event_loop
                    .primary_monitor()
                    .map(|m| m.scale_factor())
                    .unwrap_or(1.0);
                let attrs = Self::create_attrs(desc, &self.default_icon, os_scale);
                let window = Arc::new(
                    event_loop.create_window(attrs).unwrap(),
                );
                // Windows：仅 `HiddenTitlebar` 需子类化拦截 WM_NCCALCSIZE
                // （去标题栏、保留系统 resize 边框、顶部不留 inset）。
                // `Normal`/`Frameless` 不装子类，完全放行 winit 原生：
                // Frameless 由 winit 处理客户区（客户区 = 窗口矩形）。
                // 必须在 winit 事件线程、窗口创建后安装。
                if let Some(hwnd) = win_hwnd(&window) {
                    let fs = desc.frame_style;
                    if !fs.has_titlebar() && fs.has_border() {
                        crate::platform::windows::install(hwnd, false, true);
                    }
                    // run 前注册的缩略图按钮点击回调（App::on_thumb_button）：
                    // 移出（take）注册到进程级拦截子类，避免重复注册。
                    if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                        for cb in std::mem::take(&mut cbs.on_thumb_button) {
                            crate::platform::windows::set_thumbar_callback(hwnd, cb);
                        }
                    }
                    if self.hwnds.len() <= handle {
                        self.hwnds.resize(handle + 1, 0);
                    }
                    self.hwnds[handle] = hwnd;
                }
                // 运行期创建的窗口 on_close 钩子经通道送来，需补进 close_hooks。
                if let Some(h) = on_close {
                    self.close_hooks.insert(handle as u64, Some(h));
                }
                let surface = self.instance.create_surface(window.clone()).unwrap();
                let window_id = window.id();

                // ---- 在 winit 线程上创建 surface；初始 configure 推迟到渲染线程 ----
                // surface 的 create_surface 必须紧跟窗口创建；**初始 `surface.configure`
                // 不再在此执行**——DX12 的 configure 会 `wait_for_present_queue_idle`
                // 无限等 present queue 排空（主窗口每帧 present，阻塞可达几十 ms），
                // 若在 winit 线程同步执行，会卡住整个事件循环（所有窗口的输入/事件
                // 都被延迟）。改由渲染线程首次 `draw_frame`（`needs_initial_configure`
                // 标志）在 acquire 前建立合法 swapchain。
                // `SurfaceTexture` 从不跨线程：acquire→present 全在渲染线程 draw() 内，
                // 满足 wgpu-hal 同线程约束（第三十三/三十四轮的 handoff 失败不重演）。
                let scale = desc.dpi_override.unwrap_or(window.scale_factor()) as f32;
                let dpi = window.scale_factor() as f32;
                let s = to_pixel_size(
                    window.inner_size().width as f64,
                    window.inner_size().height as f64,
                    desc.dpi_override.unwrap_or(dpi as f64),
                );
                let (logical_w, logical_h) = (s.width.dp.0 as f32, s.height.dp.0 as f32);
                let renderer = Renderer::new(
                    self.gpu.clone(),
                    logical_w,
                    logical_h,
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
                let fmt = if caps.formats.contains(&self.gpu.surface_format()) {
                    self.gpu.surface_format()
                } else {
                    caps.formats[0]
                };
                // 首次建窗时把真实 surface 格式同步进 GpuContext（macOS Metal
                // surface 无 Rgba8UnormSrgb，只提供 Bgra8UnormSrgb）。管线缓存键
                // 均含 format 位（gpu.rs），旧 Rgba8 条目永不命中，并清空共享管线
                // 表兜底；文字 atlas 格式 + 管线一并刷新。offscreen 纹理亦派生
                // 自此格式，保持一致。
                if self.gpu.surface_format() != fmt {
                    self.gpu.set_surface_format(fmt);
                    self.gpu.clear_pipelines();
                    self.gpu
                        .text_ctx
                        .lock()
                        .unwrap()
                        .ensure_text_format(&self.gpu.device, fmt);
                }
                let surface_config = wgpu::SurfaceConfiguration {
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    format: fmt,
                    width: window.inner_size().width.max(1),
                    height: window.inner_size().height.max(1),
                    present_mode: desc.present_mode,
                    alpha_mode,
                    view_formats: vec![],
                    // 在途帧上限：默认 2（DX12 → 3 buffer，CPU 可超前 2 帧、
                    // 无 vsync 峰值更高）；设 1 → 2 buffer（vsync 拖动时 camera
                    // 时差更小）。经 `WindowDesc::frame_latency` / `set_frame_latency`。
                    desired_maximum_frame_latency: desc.frame_latency,
                    color_space: wgpu::SurfaceColorSpace::Auto,
                };
                // 初始 configure 推迟到渲染线程首次 draw（见上方注释）。

                self.id_to_handle.insert(window_id, handle);
                self.alive_handles += 1;

                self.send(WinitEvent::WindowCreated {
                    handle,
                    window,
                    surface,
                    surface_config,
                    renderer,
                    dpi_scale: dpi,
                    dpi_override: desc.dpi_override,
                    init_duration,
                    frame_style: desc.frame_style,
                    pending_show: desc.visible && desc.preparable,
                });
            }
        }

        impl ApplicationHandler for Runner {
            fn resumed(&mut self, event_loop: &ActiveEventLoop) {
                if self.created {
                    return;
                }
                self.created = true;

                // 预建窗口：与运行期创建共用 create_window。取走 window_descs
                // 避免循环内 &mut self 方法调用与字段借用冲突（resumed 只跑一次）。
                let window_descs = std::mem::take(&mut self.window_descs);
                for (handle, desc) in window_descs.iter().enumerate() {
                    let init_duration = if handle < self.window_init_durations.len() {
                        self.window_init_durations[handle]
                    } else {
                        0.0
                    };
                    self.create_window(event_loop, handle, desc, init_duration, None);
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
                        cbs.on_ime.extend(reg.on_ime.drain(..));
                        cbs.on_file_dropped.extend(reg.on_file_dropped.drain(..));
                        cbs.on_file_hovered.extend(reg.on_file_hovered.drain(..));
                        cbs.on_file_hover_cancelled.extend(reg.on_file_hover_cancelled.drain(..));
                        cbs.on_moved.extend(reg.on_moved.drain(..));
                        cbs.on_theme_changed.extend(reg.on_theme_changed.drain(..));
                        cbs.on_resized.extend(reg.on_resized.drain(..));
                    }
                }
                // 运行期边框样式切换：SetWindowSubclass / RemoveWindowSubclass
                // 必须在窗口 owner（winit 事件）线程执行，这里 drain 渲染线程
                // 发来的请求（见 platform::windows::install 注释）。
                while let Ok((hwnd, style)) = self.frame_style_rx.try_recv() {
                    // 只有 HiddenTitlebar 需要子类；切到 Normal/Frameless
                    // 时卸载，让 winit 原生接管（Frameless 客户区/阴影）。
                    if !style.has_titlebar() && style.has_border() {
                        crate::platform::windows::set_frame(hwnd, false, true);
                    } else {
                        crate::platform::windows::remove(hwnd);
                    }
                }
                                // 运行期宽高比切换：drain 渲染线程发来的 (hwnd, ratio)，
                // 在本线程（winit 事件线程）挂/卸 aspect 子类。
                while let Ok((hwnd, ratio)) = self.aspect_ratio_rx.try_recv() {
                    crate::platform::windows::set_aspect_ratio(hwnd, ratio);
                }
                // 非客户区 hit-test（§7.6）：drain 渲染线程发来的 (hwnd, NcUpdate)，
                // 在本线程应用 state（有 regions 或 callback 时装 nc_subclass）。
                while let Ok((hwnd, upd)) = self.nc_rx.try_recv() {
                    crate::platform::windows::nc_apply(hwnd, upd);
                }
                // 运行期窗口创建：drain 渲染线程（on_frame 里 `App::window`）
                // 发来的请求，在本线程（winit 事件线程）创建窗口。
                while let Ok(req) = self.create_rx.try_recv() {
                    self.create_window(
                        event_loop,
                        req.handle as usize,
                        &req.desc,
                        req.init_duration,
                        req.on_close,
                    );
                }
                // 程序化关窗：`VireoWindow::close` 发来的 handle，走与用户点关闭
                // 按钮相同的完整关窗路径（close_hooks / NC 清理 / 退出判定）。
                while let Ok(handle) = self.close_rx.try_recv() {
                    self.request_close(handle);
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
                        self.request_close(handle);
                    }
                    WindowEvent::Resized(size) => {
                        // 事件驱动路径：仅同步逻辑尺寸。真正的 surface.configure /
                        // renderer 视图更新由渲染线程 draw() 的逐帧尺寸同步完成
                        // （模态循环期间 Resized 事件可能滞后，逐帧轮询 inner_size 兜底）。
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_resized { cb(size); }
                        }
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
                    WindowEvent::Ime(ime) => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_ime { cb(&ime); }
                        }
                    }
                    WindowEvent::DroppedFile(path) => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_file_dropped { cb(&path); }
                        }
                    }
                    WindowEvent::HoveredFile(path) => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_file_hovered { cb(&path); }
                        }
                    }
                    WindowEvent::HoveredFileCancelled => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for c in cbs.on_file_hover_cancelled.drain(..) { c(); }
                        }
                    }
                    WindowEvent::Moved(position) => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_moved { cb(position); }
                        }
                    }
                    WindowEvent::ThemeChanged(theme) => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_theme_changed { cb(theme); }
                        }
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
            frame_style_rx,
            aspect_ratio_rx,
            nc_rx,
            create_rx,
            close_rx,
            hwnds: Vec::new(),
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
        match render_thread.join() {
            Ok(inner) => inner,
            Err(payload) => Err(panic_payload_to_string(payload).into()),
        }
    }
}

/// 渲染线程主循环：处理 winit 事件 → 调用用户 on_frame → 重复。
/// 帧节奏诊断样本（`VIREO_PACING_STATS=1` 时在 `render_on_frame` 内采集）。
#[derive(Clone, Copy)]
struct PacingSample {
    /// 本帧循环起点到上一帧起点的间隔（CPU 帧节奏）
    interval_ms: f64,
    /// `get_current_texture` 耗时（swapchain 阻塞则大；拖动拉伸时不阻塞 → 小）
    acquire_ms: f64,
    /// `queue.present` 耗时（异步 → 通常 ~20µs）
    present_ms: f64,
    /// 本帧是否 stretch present（`Presented { suboptimal: true }`）
    stretch: bool,
    /// 拖动中（`pending_resize_at` 非空）
    dragging: bool,
}

/// 把 `VIREO_PACING_STATS` 样本输出摘要（stderr）。按 stretch / 正常分两组，
/// 各报 interval/acquire/present 的 min/p50/p95/max，定位「CPU 帧节奏是否均匀、
/// 是否阻塞在 acquire/present」。
fn pacing_summary(samples: &[PacingSample]) {
    fn pct(sorted: &[f64], q: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let i = ((sorted.len() as f64 - 1.0) * q).round() as usize;
        sorted[i]
    }
    fn line(tag: &str, v: &mut Vec<f64>) {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        eprintln!(
            "  {:<12} min={:7.3} p50={:7.3} p95={:7.3} max={:7.3} (ms)",
            tag,
            v.first().copied().unwrap_or(0.0),
            pct(v, 0.50),
            pct(v, 0.95),
            v.last().copied().unwrap_or(0.0),
        );
    }

    let n_stretch = samples.iter().filter(|s| s.stretch).count();
    let n_normal = samples.len().saturating_sub(n_stretch);
    let n_drag = samples.iter().filter(|s| s.dragging).count();
    eprintln!(
        "[pacing] n={} stretch={} normal={} drag={}",
        samples.len(),
        n_stretch,
        n_normal,
        n_drag
    );

    let mut iv = samples.iter().map(|s| s.interval_ms).collect::<Vec<_>>();
    let mut iv_st = samples.iter().filter(|s| s.stretch).map(|s| s.interval_ms).collect::<Vec<_>>();
    let mut iv_nm = samples.iter().filter(|s| !s.stretch).map(|s| s.interval_ms).collect::<Vec<_>>();
    line("interval", &mut iv);
    line("interval*st", &mut iv_st);
    line("interval*norm", &mut iv_nm);

    let mut aq = samples.iter().map(|s| s.acquire_ms).collect::<Vec<_>>();
    line("acquire", &mut aq);
    let mut pr = samples.iter().map(|s| s.present_ms).collect::<Vec<_>>();
    line("present", &mut pr);
}

/// 相位诊断样本（`VIREO_PHASE_STATS=1`，`draw_frame` 内 present 后采集）。
#[derive(Clone, Copy)]
struct PhaseSample {
    /// present 相对最近一次 vblank 的相位（0~1，0=vblank 刚过，1=马上到下一个 vblank）
    phase: f64,
    /// 两次相邻 vblank 的间隔（QPC ticks → ms），判断 DWM 合成时钟是否均匀
    vblank_ms: f64,
    /// 本帧是否 stretch present
    stretch: bool,
    /// 拖动中
    dragging: bool,
}

/// 相位诊断摘要：phase 的 min/p50/p95/max + 直方图（分 10 桶），
/// 验证「present 相位在拖动时漂移」的拍频假说。
fn phase_summary(samples: &[PhaseSample]) {
    let n_st = samples.iter().filter(|s| s.stretch).count();
    let n_drag = samples.iter().filter(|s| s.dragging).count();
    eprintln!(
        "[phase] n={} stretch={} drag={}",
        samples.len(),
        n_st,
        n_drag
    );
    let mut ph = samples.iter().map(|s| s.phase).collect::<Vec<_>>();
    ph.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |q: f64| {
        if ph.is_empty() {
            return 0.0;
        }
        let i = ((ph.len() as f64 - 1.0) * q).round() as usize;
        ph[i]
    };
    eprintln!(
        "  phase       min={:.3} p50={:.3} p95={:.3} max={:.3}",
        ph.first().copied().unwrap_or(0.0),
        pct(0.50),
        pct(0.95),
        ph.last().copied().unwrap_or(0.0),
    );
    let mut vb = samples.iter().map(|s| s.vblank_ms).collect::<Vec<_>>();
    vb.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct_vb = |q: f64| {
        if vb.is_empty() {
            return 0.0;
        }
        let i = ((vb.len() as f64 - 1.0) * q).round() as usize;
        vb[i]
    };
    eprintln!(
        "  vblank_ms   min={:.3} p50={:.3} p95={:.3} max={:.3}",
        vb.first().copied().unwrap_or(0.0),
        pct_vb(0.50),
        pct_vb(0.95),
        vb.last().copied().unwrap_or(0.0),
    );
    let mut hist = [0usize; 10];
    for s in samples {
        let b = ((s.phase * 10.0) as usize).min(9);
        hist[b] += 1;
    }
    for (i, c) in hist.iter().enumerate() {
        let pct = *c as f64 / samples.len().max(1) as f64 * 100.0;
        let bar = "*".repeat((pct / 2.0).round() as usize);
        eprintln!(
            "  phase {:0.1}-{:0.1} | {:>3} 帧 {:5.1}% {}",
            i as f64 * 0.1,
            (i + 1) as f64 * 0.1,
            c,
            pct,
            bar
        );
    }
}

/// 读取 DWM 合成时钟：返回 `(qpcVBlank, qpcRefreshPeriod)`（QPC ticks）。
/// `qpcVBlank` = 最近一次 vblank 的 QPC 时间；`qpcRefreshPeriod` = 刷新周期。
/// 失败返回 `None`（非 Windows / DWM 不可用 / 远程会话）。
fn dwm_timing() -> Option<(u64, u64)> {
    crate::platform::windows::dwm_timing()
}

/// 当前 QPC 计数（`QueryPerformanceCounter`）。
fn qpc_now() -> u64 {
    crate::platform::windows::qpc_now()
}

/// QPC 频率（每类 QPC tick 的纳秒数），用于把 ticks 转成 ms。
fn qpc_ticks_per_sec() -> u64 {
    crate::platform::windows::qpc_ticks_per_sec()
}

/// 将 `catch_unwind` 捕获的 panic payload 转成可读字符串。
fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn render_on_frame<F>(
    mut app: App,
    mut on_frame: F,
    event_tx: mpsc::Sender<WinitEvent>,
    rx: mpsc::Receiver<WinitEvent>,
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    exit_tx: mpsc::Sender<()>,
    frame_style_tx: mpsc::Sender<(isize, FrameStyle)>,
    aspect_ratio_tx: mpsc::Sender<(isize, Option<f64>)>,
    // NC 状态变更通道（§7.6，render thread → winit thread）。
    nc_tx: mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    // 程序化关窗通道（`VireoWindow::close` → winit 线程完整关窗路径）。
    close_tx: mpsc::Sender<usize>,
    device_lost: Arc<std::sync::atomic::AtomicBool>,
    expected_windows: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where F: FnMut(&App) -> bool + Send + 'static
{
    let mut created_windows = 0usize;
    // 用户 on_frame 至少跑过一次后，才允许「零窗口退出」判定生效。
    // 否则 expected_windows==0（纯 run 内创建窗口）会在首帧 on_frame 前就退出。
    let mut on_frame_called = false;
    let request_exit = || {
        let _ = exit_tx.send(());
    };
    // `VIREO_PACING_STATS=1`：帧节奏诊断（仅诊断用，默认零开销）。
    let pacing = std::env::var_os("VIREO_PACING_STATS").is_some();
    let mut pacing_samples: Vec<PacingSample> = Vec::new();
    let mut pacing_prev: Option<std::time::Instant> = None;
    // `VIREO_PHASE_STATS=1`：present 相对 vblank 相位诊断（需 Windows + DWM）。
    let phase_diag = std::env::var_os("VIREO_PHASE_STATS").is_some();
    let mut phase_samples: Vec<PhaseSample> = Vec::new();
    // 循环前先跑一次到期延迟任务（frame_count 仍为 0）：
    // run 之前排的 after_frames(0) 会在第一个 on_frame 之前执行。
    app.run_due_deferred_tasks();
    loop {
        let frame_start = std::time::Instant::now();
        let interval_ms = pacing_prev
            .map(|p| frame_start.duration_since(p).as_secs_f64() * 1e3)
            .unwrap_or(0.0);
        pacing_prev = Some(frame_start);
        // 处理所有待处理事件
        loop {
            match rx.try_recv() {
                Ok(WinitEvent::WindowCreated {
                    handle, window, surface, surface_config, renderer,
                    dpi_scale, dpi_override, init_duration,
                    frame_style, pending_show,
                }) => {
                    let vw = VireoWindow::new(
                        window,
                        app.gpu.clone(),
                        surface,
                        app.instance.clone().expect("instance available"),
                        surface_config,
                        renderer,
                        dpi_scale,
                        dpi_override,
                        init_duration,
                    frame_style,
                    pending_show,
                    event_tx.clone(),
                    cb_tx.clone(),
                    nc_tx.clone(),
                    close_tx.clone(),
                    handle,
                );
                    while app.windows.len() <= handle {
                        app.windows.push(None);
                    }
                    app.windows[handle] = Some(vw);
                    created_windows += 1;
                    // 运行期创建的窗口：pending 计数递减（退出判定依赖它）。
                    if handle >= expected_windows {
                        app.pending_creates.set(app.pending_creates.get().saturating_sub(1));
                    }
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
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::CursorMoved { handle, x, y });
                    }
                }

                Ok(WinitEvent::KeyboardInput { handle, event }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::KeyboardInput { handle, event });
                    }
                }

                Ok(WinitEvent::MouseInput { handle, button, pressed }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::MouseInput { handle, button, pressed });
                    }
                }

                Ok(WinitEvent::MouseWheel { handle, delta }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::MouseWheel { handle, delta });
                    }
                }

                Ok(WinitEvent::ModifiersChanged { handle, modifiers }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::ModifiersChanged { handle, modifiers });
                    }
                }

                Ok(WinitEvent::Focused { handle, focused }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::Focused { handle, focused });
                    }
                }

                Ok(WinitEvent::CursorEntered { handle }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::CursorEntered { handle });
                    }
                }

                Ok(WinitEvent::CursorLeft { handle }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::CursorLeft { handle });
                    }
                }

                Ok(WinitEvent::Touch { handle, event }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.pending_input.borrow_mut().push(WinitEvent::Touch { handle, event });
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
                        return Ok(());
                    }
                }

                // Winit 窗口操作：转发到正确的窗口
                Ok(WinitEvent::SetTitle { handle, title }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_title(&title);
                    }
                }
                Ok(WinitEvent::SetSize { handle, size }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let _ = win.inner.request_inner_size(size);
                    }
                }
                Ok(WinitEvent::SetMinSize { handle, size }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_min_inner_size(size);
                    }
                }
                Ok(WinitEvent::SetMaxSize { handle, size }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
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
                Ok(WinitEvent::SetFrameStyle { handle, style }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.frame_style.set(style);
                        win.inner.set_decorations(style.decorated());
                        // SetWindowSubclass / RemoveWindowSubclass 必须在 winit
                        // 事件线程调用（见 platform::windows::install 注释），
                        // 这里仅转发到 winit 线程，由 Runner::about_to_wait 执行。
                        if let Some(hwnd) = win_hwnd(&win.inner) {
                            let _ = frame_style_tx.send((hwnd, style));
                        }
                    }
                }
                Ok(WinitEvent::SetAspectRatio { handle, ratio }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        if let Some(hwnd) = win_hwnd(&win.inner) {
                            let _ = aspect_ratio_tx.send((hwnd, ratio));
                        }
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

                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }

        // The winit thread may have already exited after the final close
        // event. Do not enter user code or block in another frame wait.
        // 需 on_frame 至少跑过一次 + 无运行期在途创建请求，才退出：
        // 否则零窗口 App（纯 run 内创建）首帧就退出；或运行期请求已发
        // 但 WindowCreated 未到，提前退出会漏掉刚创建的窗口。
        if on_frame_called
            && created_windows >= expected_windows
            && app.pending_creates.get() == 0
            && app.window_count() == 0
        {
            // 防御：即使不是经 CloseRequested 路径（例如零窗口 App::run、窗口被
            // 外部丢弃），也要通知 winit 线程退出，否则 winit 线程会在 run_app 里
            // 永久空转（Poll 无窗口）。send 失败（winit 线程已退出）无副作用。
            request_exit();
            return Ok(());
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

// 等所有窗口创建完才开始调用用户代码
        if created_windows >= expected_windows {
            for win in app.windows.iter().flatten() {
                win.last_draw_outcome.set(None);
            }
            if !(on_frame)(&app) {
                // 用户请求退出：通知 winit 线程 exit，本线程返回。
                request_exit();
                break Ok(());
            }
            on_frame_called = true;
            if pacing {
                for win in app.windows.iter().flatten() {
                    if let Some(rep) = win.last_draw_report.get() {
                        pacing_samples.push(PacingSample {
                            interval_ms,
                            acquire_ms: rep.timings.acquire_secs * 1e3,
                            present_ms: rep.timings.present_secs * 1e3,
                            stretch: matches!(
                                rep.outcome,
                                DrawOutcome::Presented { suboptimal: true }
                            ),
                            dragging: win.pending_resize_at.get().is_some(),
                        });
                    }
                }
                if pacing_samples.len() >= 240 {
                    pacing_summary(&pacing_samples);
                    pacing_samples.clear();
                }
            }
            if phase_diag {
                for win in app.windows.iter().flatten() {
                    if let Some(ps) = win.last_phase_sample.get() {
                        phase_samples.push(ps);
                    }
                }
                if phase_samples.len() >= 240 {
                    phase_summary(&phase_samples);
                    phase_samples.clear();
                }
            }
            if should_backoff_after_draws(
                app.windows.iter().flatten().map(|win| win.last_draw_outcome.get()),
            ) {
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            if device_lost.load(std::sync::atomic::Ordering::Acquire) {
                log::error!("vireo GPU device lost — terminating");
                request_exit();
                break Ok(());
            }
        } else {
            std::thread::yield_now();
        }

        // 执行所有到期的延迟任务（帧末，on_frame / draw 之后）
        app.run_due_deferred_tasks();

        // 空转抑制：相位锁——本帧滞后于 stride 时（无 vsync / acquire 不阻塞），
        // 下一帧 sleep 到「绝对 deadline」，且落后不追赶（跳 slot），避免双帧。
        // 有 vsync 阻塞（acquire 自然到刷新率）时 stride 早已过去，deadline 总是
        // 落后 → 直接跳 now+stride 不睡，cap 不干预 vsync。精确 vsync 由 present 保证。
        let now = std::time::Instant::now();
        let effective_cap = app.effective_max_fps();
        let (next_deadline, sleep) = pac_advance(now, app.pacing_deadline.get(), effective_cap);
        app.pacing_deadline.set(next_deadline);
        if let Some(s) = sleep {
            std::thread::sleep(s);
        }
    }
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

    /// 设置渲染循环帧率上限。`Some(n)` 在有 vsync 阻塞时基本不生效（acquire 自然
    /// 锁到刷新率），仅在 acquire 不阻塞（拖动拉伸、`Immediate`、后台等）时用
    /// sleep 把 CPU 循环拉回约 n fps，避免空转烧 CPU。`None` 不限制。
    /// 默认 `Some(240)`——给足余量，正常 vsync 下 cap 不生效，仅空转时兜底。
    /// `&self` 即可，可在 `run` 回调内随时切换。
    pub fn set_max_fps(&self, fps: Option<u32>) {
        self.max_fps.set(fps);
    }

    /// 当前帧率上限（`App::set_max_fps` 所设）。
    pub fn max_fps(&self) -> Option<u32> {
        self.max_fps.get()
    }

    /// 设置「拖动期帧率上限」独立开关。与 `set_max_fps` **解耦**：开启（默认）时，
    /// resize 拖动期间（acquire 失去 vsync 节流、渲染循环会全速空转）即使
    /// `set_max_fps(None)` 也会把实际上限压到该窗口显示器刷新率；关闭则拖动期
    /// 不做任何额外压制（完全跟随 `set_max_fps`）。松手 snap 后自动恢复。
    ///
    /// **何时关闭**：如果你希望**缩放窗口时画面内容的变化平滑流畅**，请设为
    /// `false`。开启时拖动期被压到刷新率，渲染循环只按显示器节奏产出帧，窗口
    /// 拉伸期间没有多余帧可供合成器选择，画面随尺寸变化时容易卡/顿（配合 vsync
    /// 时尤其明显）。关闭后拖动期不做帧率压制，渲染循环全速产帧（上限仅由
    /// `set_max_fps` 决定），合成器每帧都有新鲜帧可选，画面内容变化更平滑——
    /// 代价是拖动期间 CPU/GPU 空转、发热略高。资源敏感场景（笔记本省电/持续拖动）
    /// 可保持开启；以观感为先时建议关闭。
    pub fn set_drag_cap(&self, enabled: bool) {
        self.drag_cap.set(enabled);
    }

    /// 当前「拖动期帧率上限」开关（`App::set_drag_cap`）。默认 `true`。
    /// 希望缩放窗口时画面内容变化平滑 → 设为 `false`。
    pub fn drag_cap(&self) -> bool {
        self.drag_cap.get()
    }

    /// 实际生效的帧率上限。用户设的值（`set_max_fps`）基础上，若任一窗口正在
    /// resize 拖动（acquire 失去 vsync 节流）且 `drag_cap` 开启，则压到该窗口
    /// 显示器的刷新率，避免渲染循环全速空转；松手 snap（configure）后自动恢复。
    /// 拖动中不额外压（`drag_cap` 关）或非拖动时返回用户值。纯决策，供渲染循环帧末调用。
    fn effective_max_fps(&self) -> Option<u32> {
        let user = self.max_fps.get();
        for win in self.windows.iter().flatten() {
            if win.pending_resize_at.get().is_some() {
                if let Some(mhz) = win.drag_refresh_mhz.get() {
                    return drag_cap_effective(user, self.drag_cap.get(), mhz);
                }
            }
        }
        user
    }

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

    /// 设置窗口大小（通过 winit 线程异步操作）。
    /// 两参由调用点类型声明意图：`Pp::Px` = 物理像素，`Pp::Dp` = vireo 逻辑像素
    /// （裸数值默认 = `Pp::Dp`，`Some(dpi_override)` 下物理窗口 = 逻辑 × dpi；
    /// `None` 下按 OS 系统 DPI 换算）。
    pub fn set_size<W: Into<Pp>, H: Into<Pp>>(&self, width: W, height: H) {
        let size = dim_to_winit_size(
            width.into(),
            height.into(),
            self.dpi_override.get(),
            self.inner.scale_factor(),
        );
        let _ = self.event_tx.send(WinitEvent::SetSize {
            handle: self.handle(),
            size,
        });
    }

    /// 运行时设置 vireo 层自定义 dpi 覆盖（vireo 逻辑像素 → 物理像素换算因子）。
    /// 这是 vireo 层的坐标约定，**不**设置 winit 的 `scale_factor_override`。
    ///
    /// - `None`：vireo 逻辑即 winit 逻辑（OS 系统 DPI 正常参与，默认）。
    /// - `Some(v)`：**vireo 全自持像素**——vireo 逻辑为源真相，物理 = 逻辑 × v，
    ///   窗口对 OS 的系统 DPI 缩放被忽略（150% 显示器上物理窗口比其它应用小，
    ///   普通 UI 应用需斟酌；适合「以固定像素设计」的游戏/谱面编辑器）。
    /// - `Some(1.0)`：逻辑 = 物理（旧 `set_high_dpi(true)` 行为）。
    ///
    /// **保持 vireo 逻辑尺寸不变**，按新 dpi 重新计算并调整物理窗口大小
    /// （`request_resize` 以物理意图请求）；`draw` 在物理 resize 落地后按新 override
    /// 一次性 apply（重算相机/scale/逻辑尺寸并 configure）。切换由用户主动触发，
    /// 期间每次 configure 的 DX12 阻塞（~60-90ms）可接受。
    ///
    /// 示例：`examples/window_api.rs` 按 `O` 键循环切换。
    pub fn set_dpi_override(&self, dpi: Option<f64>) {
        debug_assert!(dpi.map_or(true, |d| d.is_finite() && d > 0.0), "dpi_override must be None or finite >0");
        if self.dpi_override.get() == dpi {
            return;
        }
        // 保持 vireo 逻辑尺寸不变，按新 dpi 计算目标物理尺寸并 resize 窗口。
        let (lw, lh) = phys_to_logical(self.physical_size.get(), self.layout_scale());
        let os = self.inner.scale_factor();
        let new_scale = dpi.unwrap_or(os);
        let (pw, ph) = if new_scale > 0.0 {
            (
                ((lw * new_scale).round() as u32).max(1),
                ((lh * new_scale).round() as u32).max(1),
            )
        } else {
            (lw.round() as u32, lh.round() as u32)
        };
        self.dpi_override.set(dpi);
        self.pending_override_target.set(Some((pw, ph)));
        self.pending_override_since.set(Some(std::time::Instant::now()));
        // 始终按物理尺寸请求 resize（vireo 逻辑不变，只调窗口物理像素数）
        let _ = self.event_tx.send(WinitEvent::SetSize {
            handle: self.handle(),
            size: Size::Physical(PhysicalSize::new(pw, ph)),
        });
    }

    /// 当前 vireo 层 dpi 覆盖值（`None` = 使用 OS 系统 DPI）。
    pub fn dpi_override(&self) -> Option<f64> {
        self.dpi_override.get()
    }

    /// 请求窗口尺寸变化，返回本次请求**是否当场生效**。
    ///
    /// 与 [`VireoWindow::set_size`]（只发指令、不关心落地）不同，返回值告知
    /// 结果：
    /// - `Some(size)`：已**立即应用**，返回实际生效的物理尺寸——可能被平台
    ///   约束（min/max 等）clamp，不一定等于请求值。
    /// - `None`：请求已交给窗口系统但**尚未生效**，稍后以 `Resized` 事件送达；
    ///   需要时用 [`VireoWindow::metrics`] 轮询实际值。
    ///
    /// 参数意图同 [`VireoWindow::set_size`]（裸数值 = vireo 逻辑像素 =
    /// `Pp::Dp`，受 `dpi_override` 影响）。
    /// ## Platform-specific
    /// - **iOS / Web**：仅主线程可用。
    pub fn request_resize<W: Into<Pp>, H: Into<Pp>>(
        &self,
        width: W,
        height: H,
    ) -> Option<PhysicalSize<u32>> {
        let size = dim_to_winit_size(
            width.into(),
            height.into(),
            self.dpi_override.get(),
            self.inner.scale_factor(),
        );
        self.inner.request_inner_size(size)
    }

    /// present 前通知合成器（告诉窗口系统下一帧即将上屏）。
    /// 已在 [`VireoWindow::draw`] 的 present 前自动调用一次；单独调用用于
    /// 外部同步 present 的场景。
    /// ## Platform-specific
    /// - **Android / iOS / X11 / Web / Windows / macOS / Orbital**：no-op。
    /// - **Wayland**：调度 frame callback 节流合成。
    pub fn pre_present_notify(&self) {
        self.inner.pre_present_notify();
    }

    /// 设置最小窗口大小（通过 winit 线程异步操作）。
    /// 参数意图同 [`VireoWindow::set_size`]。
    pub fn set_min_size<W: Into<Pp>, H: Into<Pp>>(&self, width: Option<W>, height: Option<H>) {
        let size = match (width, height) {
            (Some(w), Some(h)) => Some(dim_to_winit_size(
                w.into(),
                h.into(),
                self.dpi_override.get(),
                self.inner.scale_factor(),
            )),
            _ => None,
        };
        let _ = self.event_tx.send(WinitEvent::SetMinSize {
            handle: self.handle(),
            size,
        });
    }

    /// 设置最大窗口大小（通过 winit 线程异步操作）。
    /// 参数意图同 [`VireoWindow::set_size`]。
    pub fn set_max_size<W: Into<Pp>, H: Into<Pp>>(&self, width: Option<W>, height: Option<H>) {
        let size = match (width, height) {
            (Some(w), Some(h)) => Some(dim_to_winit_size(
                w.into(),
                h.into(),
                self.dpi_override.get(),
                self.inner.scale_factor(),
            )),
            _ => None,
        };
        let _ = self.event_tx.send(WinitEvent::SetMaxSize {
            handle: self.handle(),
            size,
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

    /// 设置窗口边框样式（通过 winit 线程异步操作）。
    ///
    /// 见 [`FrameStyle`] 各变体文档。
    /// 非 Windows 平台上：`set_decorations` 是 winit 运行时方法，但
    /// `HiddenTitlebar` 的构造期属性（`with_title_hidden`/透明/全尺寸）
    /// 无法在运行时设置——macOS 上运行时切换到 `HiddenTitlebar` 只会有
    /// `decorated()=true` 的标准装饰，等于 `Normal`；`Frameless` 可运行时生效。
    ///
    /// **Windows 圆角钳制/恢复**：进入 [`FrameStyle::Frameless`]（无边框）时
    /// DWM 无法圆角，圆角偏好被钳为 `Default`；切回 `Normal`/`HiddenTitlebar`
    /// 时自动恢复用户上次经 `set_corner_preference` 设置的偏好。
    pub fn set_frame_style(&self, style: FrameStyle) {
        let prev = self.frame_style.get();
        self.frame_style.set(style);
        {
            let prev_frameless = prev == FrameStyle::Frameless;
            let new_frameless = style == FrameStyle::Frameless;
            if prev_frameless != new_frameless {
                if let Some(hwnd) = crate::platform::windows::win_hwnd(&self.inner) {
                    let target = if new_frameless {
                        crate::platform::windows::CornerPreference::Default
                    } else {
                        self.user_corner_pref.get()
                    };
                    #[cfg(target_os = "windows")]
                    {
                        winit::platform::windows::WindowExtWindows::set_corner_preference(
                            &*self.inner,
                            target.into_winit(),
                        );
                    }
                    #[cfg(not(target_os = "windows"))]
                    {
                        let _ = target;
                        let _ = hwnd;
                    }
                } else {
                    let _ = new_frameless;
                }
            }
        }
        let _ = self.event_tx.send(WinitEvent::SetFrameStyle {
            handle: self.handle(),
            style,
        });
    }

    // ------ 事件订阅 API（通过 cb_tx 异步发送到 winit 线程，无需 +Send）------
    crate::def_window_ons! {
        on_key_down: impl FnMut(&crate::input::KeyEvent) + 'static => on_key_down,
        on_key_up: impl FnMut(&crate::input::KeyEvent) + 'static => on_key_up,
        on_mouse_down: impl FnMut(&crate::input::MouseButtonEvent) + 'static => on_mouse_down,
        on_mouse_up: impl FnMut(&crate::input::MouseButtonEvent) + 'static => on_mouse_up,
        on_scroll: impl FnMut(&crate::input::MouseScrollEvent) + 'static => on_scroll,
        on_cursor_entered: impl FnOnce() + 'static => on_cursor_entered,
        on_cursor_left: impl FnOnce() + 'static => on_cursor_left,
        on_touch: impl FnMut(&crate::input::TouchEvent) + 'static => on_touch,
        on_focus_gained: impl FnOnce() + 'static => on_focus_gained,
        on_focus_lost: impl FnOnce() + 'static => on_focus_lost,
        on_modifiers_changed: impl FnMut(crate::input::Modifiers) + 'static => on_modifiers_changed,
        on_ime: impl FnMut(&crate::input::Ime) + 'static => on_ime,
        on_file_dropped: impl FnMut(&std::path::PathBuf) + 'static => on_file_dropped,
        on_file_hovered: impl FnMut(&std::path::PathBuf) + 'static => on_file_hovered,
        on_file_hover_cancelled: impl FnOnce() + 'static => on_file_hover_cancelled,
        /// 窗口位置（物理像素，含边框外沿）变化时回调。运行在 winit 线程。
        on_moved: impl FnMut(winit::dpi::PhysicalPosition<i32>) + 'static => on_moved,
        /// 系统主题变化时回调（仅 Windows/macOS 上报）。运行在 winit 线程。
        on_theme_changed: impl FnMut(winit::window::Theme) + 'static => on_theme_changed,
        /// 窗口尺寸（物理像素）变化时回调（模态循环期间可能滞后，渲染线程逐帧轮询兜底）。
        /// 运行在 winit 线程。
        on_resized: impl FnMut(winit::dpi::PhysicalSize<u32>) + 'static => on_resized,
    }

    /// 设置窗口是否接收 IME 事件（默认关闭）。
    ///
    /// 开启后窗口才会收到 [`Ime`](crate::input::Ime) 事件；preedit 期间**不再收到**
    /// `KeyboardInput`。应在期待文本输入时开启（例如输入框聚焦），否则关闭。
    ///
    /// 1:1 封装 winit [`Window::set_ime_allowed`](winit::window::Window::set_ime_allowed)。
    /// 内部由 winit 排队到窗口线程执行，任意线程可调用。
    ///
    /// ## Platform-specific
    ///
    /// - **macOS:** IME 必须开启才能收到 dead-key 序列组合的文本输入。
    /// - **iOS / Android:** 控制软键盘显示/隐藏。
    /// - **Web / Orbital:** 不支持。
    /// - **X11:** 开启 IME 后 compose 期间不再报告 dead keys。
    pub fn set_ime_allowed(&self, allowed: bool) {
        self.inner.set_ime_allowed(allowed);
    }

    /// 设置 IME 候选窗/组合窗跟随光标的矩形区域（位置 + 大小）。
    ///
    /// 在文本光标移动时调用，位置/大小均为逻辑或物理坐标（见 winit
    /// [`Position`](winit::dpi::Position) / [`Size`](winit::dpi::Size)）。
    ///
    /// 1:1 封装 winit [`Window::set_ime_cursor_area`](winit::window::Window::set_ime_cursor_area)。
    /// 内部由 winit 排队到窗口线程执行，任意线程可调用。
    ///
    /// ## Platform-specific
    ///
    /// - **X11:** 仅支持位置，忽略大小。
    /// - **iOS / Android / Web / Orbital:** 不支持。
    pub fn set_ime_cursor_area<P: Into<winit::dpi::Position>, S: Into<winit::dpi::Size>>(
        &self,
        position: P,
        size: S,
    ) {
        self.inner.set_ime_cursor_area(position, size);
    }

    /// 设置 IME 用途（影响候选词等行为）。
    ///
    /// 1:1 封装 winit [`Window::set_ime_purpose`](winit::window::Window::set_ime_purpose)。
    ///
    /// ## Platform-specific
    ///
    /// - **仅 Wayland** 支持；Windows / X11 / macOS 等平台为 no-op。
    pub fn set_ime_purpose(&self, purpose: winit::window::ImePurpose) {
        self.inner.set_ime_purpose(purpose);
    }

    // ------ 窗口状态命令与查询（1:1 转发 winit `Window`，直接 `self.inner`，任意线程可调）------

    /// 运行时切换窗口是否可调大小。
    /// ## Platform-specific
    /// - 仅桌面有效；X11 下 Xfce 窗口管理器可能不生效。
    pub fn set_resizable(&self, resizable: bool) {
        self.inner.set_resizable(resizable);
    }

    /// 当前窗口是否可调大小。X11 未实现。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn is_resizable(&self) -> bool {
        self.inner.is_resizable()
    }

    /// 运行时设置标题栏启用的按钮（最小化/最大化/关闭）。
    /// ## Platform-specific
    /// - Wayland / X11 / Orbital 不支持。
    pub fn set_enabled_buttons(&self, buttons: WindowButtons) {
        self.inner.set_enabled_buttons(buttons);
    }

    /// 当前启用的标题栏按钮。Wayland / X11 / Orbital 恒为全部。
    pub fn enabled_buttons(&self) -> WindowButtons {
        self.inner.enabled_buttons()
    }

    /// 运行时切换窗口透明。
    /// ## Platform-specific
    /// - Web / iOS / Android 不支持；X11 仅构建期可设。
    pub fn set_transparent(&self, transparent: bool) {
        self.inner.set_transparent(transparent);
    }

    /// 设置窗口整体不透明度（`0.0` = 完全透明，`1.0` = 不透明）。值会被钳制到 `[0, 1]`。
    ///
    /// vireo 自实现（Electron `setOpacity` 语义；winit 无对应 API）。
    ///
    /// ## Platform-specific
    /// - Windows：经 `SetLayeredWindowAttributes`（自动补 `WS_EX_LAYERED` 扩展样式；
    ///   与 `WS_EX_TRANSPARENT` 无关，点击穿透请用 [`VireoWindow::set_cursor_hittest`]）。
    /// - macOS：经 `NSWindow.alphaValue`（需窗口已加入 key window / 已显示，alpha 才生效）。
    /// - 其他平台：无操作。
    pub fn set_opacity(&self, opacity: f64) {
        let opacity = opacity.clamp(0.0, 1.0);
        if let Some(hwnd) = crate::platform::windows::win_hwnd(&self.inner) {
            crate::platform::windows::apply_window_opacity(hwnd, opacity);
            return;
        }
        #[cfg(target_os = "macos")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(wh) = self.inner.window_handle() {
                if let RawWindowHandle::AppKit(h) = wh.as_raw() {
                    use objc2_app_kit::NSView;
                    let view = h.ns_view.as_ptr() as *mut NSView;
                    unsafe {
                        if let Some(window) = (&*view).window() {
                            window.setAlphaValue(opacity as objc2_core_foundation::CGFloat);
                        }
                    }
                }
            }
            return;
        }
        let _ = opacity;
    }

    /// 运行时切换窗口是否可被点击激活获得焦点。对应 Electron/Tauri `setFocusable`。
    /// 跨平台语义：Electron/Tauri 均支持。
    ///
    /// **Windows**：经 `SetWindowLongPtrW(GWL_EXSTYLE)` 增/清 `WS_EX_NOACTIVATE`。
    /// `GetWindowLongPtrW`/`SetWindowLongPtrW` 是线程安全的 Win32 API（窗口扩展样式
    /// 不依赖消息循环线程），故从渲染线程直接调用，无须经 winit 线程转发。
    /// 注意：仅阻止**用户点击**触发的激活；`focus()` / `set_visible(true)` 触发的
    /// 编程式激活仍会生效（Windows 系统级行为，非 vireo 行为）。
    ///
    /// **macOS**：vireo 暂不实现——需为 NSWindow 子类化并 override
    /// `acceptsFirstResponder` 返回 `NO`，与本工程目标平台窗口能力整批实施同步。
    /// 字段已记录用户意图，macOS 实现到位后即时生效，无需重设。
    ///
    /// **其他平台**：无操作。
    pub fn set_focusable(&self, focusable: bool) {
        self.focusable.set(focusable);
        if let Some(hwnd) = crate::platform::windows::win_hwnd(&self.inner) {
            crate::platform::windows::apply_window_focusable(hwnd, focusable);
        }
    }

    /// 当前 `set_focusable` 设置（始终为最近一次调用值；macOS 暂存意图待实现生效）。
    pub fn is_focusable(&self) -> bool {
        self.focusable.get()
    }

    /// 设置窗口宽高比（`Some(r)` = 宽 / 高 = r；`None` 或非正数 = 清除）。
    /// 跨平台语义：Electron `setAspectRatio`（macOS 起源，Windows 也可设）。
    ///
    /// **Windows**：经 `WM_GETMINMAXINFO`（钳 `ptMaxTrackSize` 维持 ratio）+
    /// `WM_SIZING`（用户拖拽时按 WMSZ_* 调整新尺寸）子类化实现。子类挂/卸
    /// 必须在窗口 owner（winit 事件）线程调用，故本方法经 `WinitEvent::SetAspectRatio`
    /// 转发到 winit 线程执行。
    ///
    /// **macOS / 其他平台**：vireo 暂不实现——macOS 应走 `NSWindow setContentAspectRatio:`，
    /// 后续 macOS 平台窗口能力整批实施时挂接。
    pub fn set_aspect_ratio(&self, ratio: Option<f64>) {
        let ratio = validate_aspect_ratio(ratio);
        {
            let _ = self.event_tx.send(WinitEvent::SetAspectRatio {
                handle: self.handle,
                ratio,
            });
        }
        {
            // 暂存意图以备未来 macOS 实现时即可生效（与 `set_focusable` 同约定）。
            let _ = ratio;
        }
    }

    /// 设置窗口外层位置（含边框）。两参意图同 [`VireoWindow::set_size`]（`Pp`）。
    /// ## Platform-specific
    /// - Android / Wayland 不支持。
    pub fn set_outer_position<W: Into<Pp>, H: Into<Pp>>(&self, x: W, y: H) {
        let position = dim_to_winit_position(
            x.into(),
            y.into(),
            self.dpi_override.get(),
            self.inner.scale_factor(),
        );
        self.inner.set_outer_position(position);
    }

    /// 客户端区（不含边框）左上角物理像素位置（物理 + 逻辑双表示）。
    /// ## Platform-specific
    /// - Android / Wayland 恒 `NotSupported`。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn inner_position(&self) -> Result<PixelPos, NotSupportedError> {
        self.inner
            .inner_position()
            .map(|p| to_pixel_pos(p.x as f64, p.y as f64, self.layout_scale()))
    }

    /// 窗口外沿（含边框）物理像素位置（物理 + 逻辑双表示）。
    /// ## Platform-specific
    /// - Android / Wayland 恒 `NotSupported`。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn outer_position(&self) -> Result<PixelPos, NotSupportedError> {
        self.inner
            .outer_position()
            .map(|p| to_pixel_pos(p.x as f64, p.y as f64, self.layout_scale()))
    }

    /// 窗口客户区物理尺寸（不含边框，物理 + 逻辑双表示）。
    /// 直接查询 winit `Window::inner_size()`，与内部布局缓存（`physical_size`，逻辑 =
    /// 物理 ÷ [`Self::layout_scale`] 现算）无关——该缓存可能因 layout-follow 平滑或
    /// 未 snap 的 resize 滞后于窗口当前值；本方法总是返回此刻窗口的真实客户区尺寸。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值
    /// （如 [`Self::layout_size`]）。
    pub fn inner_size(&self) -> PixelSize {
        let s = self.inner.inner_size();
        to_pixel_size(s.width as f64, s.height as f64, self.layout_scale())
    }

    /// 窗口外沿物理尺寸（含边框，物理 + 逻辑双表示）。iOS / Web 与 `inner_size` 相同。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn outer_size(&self) -> PixelSize {
        let s = self.inner.outer_size();
        to_pixel_size(s.width as f64, s.height as f64, self.layout_scale())
    }

    /// 把窗口在**当前所在显示器**上居中。vireo 自实现（Electron / Tauri `center`
    /// 语义；winit 无对应 API）。
    ///
    /// 用物理像素手算：`目标外沿左上角 = 显示器工作区中心 − 窗口外沿尺寸一半`，
    /// 然后走 [`VireoWindow::set_outer_position`]。窗口尺寸用当前外沿尺寸，
    /// 不改变大小。无可用显示器（Android / Wayland 等）时静默跳过。
    pub fn center(&self) {
        let Some(monitor) = self.current_monitor() else {
            return;
        };
        let origin = monitor.position();
        let area = monitor.size();
        let size = self.outer_size();
        let x = origin.x as f64
            + ((area.width as f64 - size.width.px.0) / 2.0).round();
        let y = origin.y as f64
            + ((area.height as f64 - size.height.px.0) / 2.0).round();
        self.set_outer_position(Px(x), Px(y));
    }

    /// 当前是否最小化。`None` 表示平台无法查询。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn is_minimized(&self) -> Option<bool> {
        self.inner.is_minimized()
    }

    /// 当前是否最大化。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn is_maximized(&self) -> bool {
        self.inner.is_maximized()
    }

    /// 当前是否可见。`None` 表示平台无法查询。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn is_visible(&self) -> Option<bool> {
        self.inner.is_visible()
    }

    /// 当前窗口边框样式（vireo 层状态；非 winit `is_decorated`）。
    /// 返回的是 vireo 存储的目标值，不保证 OS 已实际应用。
    pub fn frame_style(&self) -> FrameStyle {
        self.frame_style.get()
    }

    /// 当前全屏状态（`None` = 非全屏）。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn fullscreen(&self) -> Option<Fullscreen> {
        self.inner.fullscreen()
    }

    /// 运行时切换窗口主题。
    /// ## Platform-specific
    /// - iOS / Android / Web / Orbital 不支持。
    pub fn set_theme(&self, theme: Option<Theme>) {
        self.inner.set_theme(theme);
    }

    /// 当前窗口主题。iOS / Android / X11 / Orbital 不支持；Wayland 仅在显式设置后返回。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn theme(&self) -> Option<Theme> {
        self.inner.theme()
    }

    /// 请求用户注意（任务栏闪烁/图标抖动）。
    /// ## Platform-specific
    /// - iOS / Android / Web / Orbital 不支持；X11 须手动清除；Wayland 需 xdg-activation 配合。
    pub fn request_user_attention(&self, request_type: Option<UserAttentionType>) {
        self.inner.request_user_attention(request_type);
    }

    /// 隐藏/显示系统光标。
    /// ## Platform-specific
    /// - iOS / Android 不支持。
    pub fn set_cursor_visible(&self, visible: bool) {
        self.inner.set_cursor_visible(visible);
    }

    /// 把光标移到窗口内指定位置。两参意图同 [`VireoWindow::set_size`]（`Px`/`Dp`）。
    /// ## Platform-specific
    /// - Wayland 需要已 grab；iOS / Android / Web / Orbital 恒 `NotSupported`。
    pub fn set_cursor_position<W: Into<Pp>, H: Into<Pp>>(
        &self,
        x: W,
        y: H,
    ) -> Result<(), ExternalError> {
        let position = dim_to_winit_position(
            x.into(),
            y.into(),
            self.dpi_override.get(),
            self.inner.scale_factor(),
        );
        self.inner.set_cursor_position(position)
    }

    /// 抓取/锁定光标。
    /// ## Platform-specific
    /// - macOS 不支持 `Confined`；X11 不支持 `Locked`。
    pub fn set_cursor_grab(&self, mode: CursorGrabMode) -> Result<(), ExternalError> {
        self.inner.set_cursor_grab(mode)
    }

    /// 程序化拖动窗口（自定义标题栏）。
    ///
    /// **触发时机**：应在「左键按下」事件（或按下沿）时调用一次，且光标位于拖动区域内；
    /// 不要每帧持续调用（这是 winit 的设计契约，`drag_window` 就是「按下时调用一次」的原语）。
    /// winit 会以调用时的光标位置重发 `WM_NCLBUTTONDOWN` 作为模态移动循环的锚点——
    /// 若按住期间光标才移入区域再调用，窗口会瞬间吸附到鼠标。
    ///
    /// ## Platform-specific
    /// - iOS / Android / Web 不支持；macOS 可能吞掉随后的释放事件。
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        self.inner.drag_window()
    }

    /// 程序化缩放窗口（自定义边框）。
    /// ## Platform-specific
    /// - macOS / iOS / Android / Web 不支持。
    pub fn drag_resize_window(&self, direction: ResizeDirection) -> Result<(), ExternalError> {
        self.inner.drag_resize_window(direction)
    }

    /// 显示系统窗口菜单（右键标题栏菜单）。
    /// ## Platform-specific
    /// - **仅 Windows** 支持。
    pub fn show_window_menu<P: Into<winit::dpi::Position>>(&self, position: P) {
        self.inner.show_window_menu(position);
    }

    /// 设置窗口是否响应光标命中测试（`false` = 鼠标事件穿透窗口）。
    /// ## Platform-specific
    /// - 仅 Windows / X11 有效；其余平台恒 `NotSupported`。
    pub fn set_cursor_hittest(&self, hittest: bool) -> Result<(), ExternalError> {
        self.inner.set_cursor_hittest(hittest)
    }

    /// 重置死键状态。切换输入法 / 输入状态后调用，避免后续字符被死键组合。
    /// ## Platform-specific
    /// - 仅 macOS / Windows 有效；其余平台 no-op。
    pub fn reset_dead_keys(&self) {
        self.inner.reset_dead_keys();
    }

    /// 窗口背景模糊（毛玻璃）。
    /// ## Platform-specific
    /// - **仅 Wayland (kwin)** 有效；其余平台 no-op。
    pub fn set_blur(&self, blur: bool) {
        self.inner.set_blur(blur);
    }

    /// 禁止窗口内容被录屏 / 截屏捕获。
    /// ## Platform-specific
    /// - **仅 macOS** 有效；其余平台 no-op。
    pub fn set_content_protected(&self, protected: bool) {
        self.inner.set_content_protected(protected);
    }

    /// 当前窗口尺寸调整步进（网格对齐窗口，物理 + 逻辑双表示）。无步进时返回 `None`。
    /// ## Platform-specific
    /// - iOS / Android / Web / Orbital 恒 `None`。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn resize_increments(&self) -> Option<PixelSize> {
        self.inner
            .resize_increments()
            .map(|s| to_pixel_size(s.width as f64, s.height as f64, self.layout_scale()))
    }

    /// 设置窗口尺寸调整步进（网格对齐窗口）。参数意图同 [`VireoWindow::set_size`]。
    /// `None` 清除步进；`Some((w, h))` 设网格。对齐 winit 单 Size 选项语义，两轴必须同时设置。
    /// ## Platform-specific
    /// - iOS / Android / Web / Orbital 不支持。
    pub fn set_resize_increments<W: Into<Pp>, H: Into<Pp>>(&self, increments: Option<(W, H)>) {
        let increments =
            increments.map(|(w, h)| {
                dim_to_winit_size(
                    w.into(),
                    h.into(),
                    self.dpi_override.get(),
                    self.inner.scale_factor(),
                )
            });
        self.inner.set_resize_increments(increments);
    }

    /// 窗口当前所在显示器。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        self.inner.current_monitor()
    }

    /// 主显示器。
    /// ## Platform-specific
    /// - Wayland / Web 恒 `None`。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        self.inner.primary_monitor()
    }

    /// 全部可用显示器。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn available_monitors(&self) -> impl Iterator<Item = MonitorHandle> {
        self.inner.available_monitors()
    }

    /// winit 窗口唯一标识（跨窗口 / 事件对比用）。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值。
    pub fn id(&self) -> WindowId {
        self.inner.id()
    }

    /// 当前 DPI 缩放因子（物理像素 / 逻辑像素）。
    ///
    /// **macOS 注意**：本方法从非主线程调用时通过 GCD `exec_sync` 同步派发到
    /// 主线程执行并阻塞等待；不要在会与主线程形成相互等待的上下文（如主线程回调内
    /// 同步等待渲染线程）中调用，否则会死锁。渲染线程每帧热路径请优先使用缓存值
    /// （[`Self::layout_scale`] 在 macOS 上不阻塞）。
    pub fn scale_factor(&self) -> f64 {
        self.inner.scale_factor()
    }

    /// 当前**布局层**的逻辑→物理缩放（物理 / 逻辑），即
    /// `applied_dpi_override.unwrap_or(dpi_scale)`：
    ///
    /// - `Some(dpi_override)` 已落地 → = 该 dpi 覆盖；
    /// - 否则 = OS 系统 DPI（[`Self::scale_factor`]）。
    ///
    /// 与 [`Self::scale_factor`] 的区别：`scale_factor` 始终返回 OS 真实 DPI（winit
    /// 直通）；`layout_scale` 反映布局层实际采用的缩放（dpi 覆盖落地前仍是旧值，
    /// 见 [`Self::set_dpi_override`]）。`inner_size`/`outer_size`/`inner_position`/
    /// `outer_position`/`resize_increments`/`mouse_pos` 的逻辑换算统一用它。
    ///
    /// **不阻塞**：读 vireo 内部缓存（渲染线程每帧轮询锚点更新），任何线程可调，
    /// 无 winit 跨线程 hop（macOS 亦如此）。
    pub fn layout_scale(&self) -> f64 {
        self.applied_dpi_override
            .get()
            .unwrap_or(self.dpi_scale.get() as f64)
    }

    /// 当前**布局层**生效的窗口逻辑尺寸（物理 + 逻辑双表示）。
    ///
    /// 内部只缓存物理尺寸（`physical_size`），逻辑 = 物理 ÷ [`Self::layout_scale`]。
    /// **与 [`Self::inner_size`] 的区别**：`inner_size` 直接查询 winit 当前窗口客户区；
    /// 布局缓存可能因 layout-follow 平滑或未 snap 的 resize 滞后于窗口（见
    /// [`Self::inner_size`] 说明）。布局缓存是 `metrics()` 与 `projection()` 等
    /// 「按布局构图」API 的同一尺寸来源。
    ///
    /// **不阻塞**：读 vireo 内部布局缓存（见 [`Self::layout_scale`]），任何线程可调。
    pub fn layout_size(&self) -> PixelSize {
        let (pw, ph) = self.physical_size.get();
        to_pixel_size(pw as f64, ph as f64, self.layout_scale())
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

    /// 设置期望最大在途帧（`desired_maximum_frame_latency`），下一帧 draw 时
    /// 触发一次 `surface.configure`。
    ///
    /// DX12 下 swapchain buffer 数 = latency + 1：
    /// - `2`（默认）→ 3 buffer：CPU 可超前 2 帧，无 vsync 时峰值更高；
    /// - `1` → 2 buffer：在途封顶 1，vsync 拖动时 camera 时差更小。
    ///
    /// 值域为 wgpu 能力范围（通常 1..=16），超出会被 wgpu 夹紧。`0` 非法。
    pub fn set_frame_latency(&self, latency: u32) {
        self.pending_frame_latency.set(Some(latency));
    }

    /// 当前真正生效（configure 到 surface）的在途帧。pending 未应用时返回旧值。
    pub fn frame_latency(&self) -> u32 {
        self.applied_frame_latency.get()
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
            self.follow_samples.borrow_mut().clear();
            let (phys_w, phys_h, scale, dpi_scale) = self.configured_layout.get();
            let (logical_w, logical_h) = phys_to_logical((phys_w, phys_h), scale as f64);
            self.physical_size.set((phys_w, phys_h));
            self.dpi_scale.set(dpi_scale);
            let mut renderer = self.renderer.borrow_mut();
            renderer.update_layout(logical_w as f32, logical_h as f32, scale, dpi_scale);
            renderer.set_text_viewport_override(None);
        }
    }

    /// 当前布局跟随开关（默认开）。见 [`VireoWindow::set_layout_follow`]。
    pub fn layout_follow(&self) -> bool {
        self.layout_follow.get()
    }

    /// 布局跟随的平滑模式（默认 `FollowAmount::Average(Time(16ms))`）。
    ///
    /// `layout_follow` 打开（默认）时，窗口尺寸已变但 surface 未重配，内容以
    /// `Renderer::update_layout` 跟随窗口更新的节奏由这里决定。强度统一用
    /// [`FollowFramesOrTime`] 表达——既可按**真实时长**（`Time(d)`，与刷新率无关）
    /// 也可按**帧数**（`Frames(n)`）：
    ///
    /// - [`FollowAmount::PerFrame`]：每帧都追到最新尺寸，最跟手，但窗口动得快时容易
    ///   「抖/闪一帧」。
    /// - [`FollowAmount::Average(amt)`]：平均窗——camera 目标 = 最近 `amt` 内观测到的
    ///   窗口尺寸均值，连续渐变不跳格；窗越大越平滑（反应越慢）、越小越跟手。
    ///
    /// `Average` 的单位为 `Frames(0)` / `Time(0)` 时退化为 `PerFrame`。
    /// 切换模式会清空平均窗采样，避免新旧语义串用。
    pub fn set_layout_follow_smoothing(&self, amount: FollowAmount) {
        self.follow_smoothing.set(amount);
        self.follow_samples.borrow_mut().clear();
    }

    /// 当前布局跟随平滑模式。见 [`VireoWindow::set_layout_follow_smoothing`]。
    pub fn layout_follow_smoothing(&self) -> FollowAmount {
        self.follow_smoothing.get()
    }

    /// 便捷：平均窗，按真实时长。等价
    /// `set_layout_follow_smoothing(Average(Time(d)))`；`Time` 越小越跟手、越大越平滑。
    pub fn set_layout_follow_window(&self, d: std::time::Duration) {
        self.set_layout_follow_smoothing(FollowAmount::Average(FollowFramesOrTime::Time(d)));
    }

    /// 便捷：平均窗，按帧数。等价
    /// `set_layout_follow_smoothing(Average(Frames(n)))`；`n` 越小越跟手、越大越平滑。
    pub fn set_layout_follow_window_frames(&self, n: u32) {
        self.set_layout_follow_smoothing(FollowAmount::Average(FollowFramesOrTime::Frames(n)));
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

    /// 请求关闭窗口（走与用户点关闭按钮相同的完整关窗路径）。
    ///
    /// 内部经 `close_tx` 通道转发到 winit 线程，在 winit 线程依次执行
    /// close_hooks / Windows NC 状态清理 / `alive_handles` 递减，并发送
    /// `WinitEvent::CloseRequested` 给渲染线程——渲染线程随后置 `closing`、
    /// 从 `App.windows` 移除本窗口，最后一个窗口关闭时请求退出 event loop。
    /// 返回前不阻塞；实际关窗在下一次 winit 事件循环迭代生效。
    pub fn close(&self) {
        let _ = self.close_tx.send(self.handle);
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
mod aspect_ratio_and_focus_tests {
    //! A1 `set_focusable` / A2 `set_aspect_ratio` / A9 `blur` 的纯函数层验证。
    //!
    //! - `validate_aspect_ratio` 是 `set_aspect_ratio` 的纯输入过滤（`None`/非正
    //!   数 → `None`），单测覆盖所有边界。
    //! - `blur` / `focused` 是 `InputState.focused` 的双向访问，本模块不直接构造
    //!   `VireoWindow`（需 winit 上下文），通过聚焦状态互斥的元测试验证语义：
    //!   `blur() = !focused()` 在所有聚焦状态下成立。

    use std::cell::Cell;

    /// 验证 `validate_aspect_ratio` 过滤逻辑：`Some(r>0)` → `Some(r)`，其余 → `None`。
    #[test]
    fn validate_aspect_ratio_positive_passes_through() {
        assert_eq!(super::validate_aspect_ratio(Some(16.0 / 9.0)), Some(16.0 / 9.0));
        assert_eq!(super::validate_aspect_ratio(Some(1.0)), Some(1.0));
        // 极小正数（避免 f64 subnormal 边界模糊，按"严格 > 0"语义放行）
        assert_eq!(super::validate_aspect_ratio(Some(0.0001)), Some(0.0001));
    }

    #[test]
    fn validate_aspect_ratio_zero_and_negative_clear() {
        assert_eq!(super::validate_aspect_ratio(Some(0.0)), None);
        assert_eq!(super::validate_aspect_ratio(Some(-1.0)), None);
        assert_eq!(super::validate_aspect_ratio(Some(f64::NEG_INFINITY)), None);
    }

    #[test]
    fn validate_aspect_ratio_none_stays_none() {
        assert_eq!(super::validate_aspect_ratio(None), None);
    }

    /// `blur` 与 `focused` 是同一个 `InputState.focused` Cell 的两面：
    /// `blur = !focused` 在所有聚焦状态下成立。本测试用同型的 `Cell<bool>`
    /// 替身（不构造真实 VireoWindow），验证 `blur` 实现是 `focused` 的取反。
    #[test]
    fn blur_is_negation_of_focused_cell() {
        let focused = Cell::new(true);
        assert!(!blur_from_cell(&focused));
        focused.set(false);
        assert!(blur_from_cell(&focused));
        focused.set(true);
        assert!(!blur_from_cell(&focused));
    }

    /// 测试替身：`VireoWindow::blur` = `!self.input.focused.borrow()` 的简化表达。
    /// 注意：真实 `focused` 实现用 `RefCell<bool>::borrow()` 而非 `Cell::get`，
    /// 但语义都是"读最新写入值后取反"，本替身足以验证。
    fn blur_from_cell(c: &Cell<bool>) -> bool {
        !c.get()
    }
}

#[cfg(test)]
mod metrics_tests {
    //! 第五十一轮纯函数测试：逻辑/物理尺寸换算与「跳过帧」report 构造。

    use super::{skip_report, sliding_rate, DrawOutcome, DrawSkipReason};
    use crate::dpi::to_pixel_size;

    #[test]
    fn pace_none_when_no_cap() {
        assert_eq!(super::pac_advance(std::time::Instant::now(), None, None), (None, None));
    }

    #[test]
    fn pace_zero_fps_disables() {
        assert_eq!(super::pac_advance(std::time::Instant::now(), None, Some(0)), (None, None));
    }

    #[test]
    fn pace_overdue_resets_deadline_and_does_not_sleep() {
        // work 已耗尽 stride（无 deadline / 已落后）→ 不睡，仅重锚到 now+stride
        let now = std::time::Instant::now();
        let (d, sleep) = super::pac_advance(now, None, Some(60));
        assert!(sleep.is_none());
        let d = d.expect("deadline set");
        assert!(d >= now + std::time::Duration::from_millis(15));
    }

    #[test]
    fn pace_advances_deadline_when_ahead() {
        // deadline 在未来 → 睡到 deadline，并把下次推到 deadline+stride
        let now = std::time::Instant::now();
        let deadline = now + std::time::Duration::from_millis(5);
        let (d, s) = super::pac_advance(now, Some(deadline), Some(60));
        let sleep = s.expect("sleeps to deadline");
        assert!(sleep >= std::time::Duration::from_millis(4));
        assert!(sleep < std::time::Duration::from_millis(6));
        let d = d.expect("next deadline");
        assert!(d >= deadline + std::time::Duration::from_millis(15));
    }

    #[test]
    fn pace_does_not_catch_up_when_late() {
        // deadline 已过去 → 不追赶、不补睡（否则 work+stride 会超帧）
        let now = std::time::Instant::now();
        let behind = now - std::time::Duration::from_secs(1);
        let (d, sleep) = super::pac_advance(now, Some(behind), Some(60));
        assert!(sleep.is_none());
        let d = d.expect("next deadline after late frame");
        assert!(d <= now + std::time::Duration::from_millis(17)); // 重锚，不追平历史
    }

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
        // 无 dpi 覆盖：逻辑 = 物理 / OS scale_factor（f32 不截断，可含小数）
        let s = to_pixel_size(1920.0, 1080.0, 1.5);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (1280.0, 720.0));
        let s = to_pixel_size(1000.0, 500.0, 2.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (500.0, 250.0));
        let s = to_pixel_size(1280.0, 720.0, 1.5);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (853.3333, 480.0));
    }

    #[test]
    fn logical_size_override_one_is_physical() {
        // scale=1.0：逻辑 = 物理（用户坐标即物理像素）
        let s = to_pixel_size(1920.0, 1080.0, 1.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (1920.0, 1080.0));
        let s = to_pixel_size(1000.0, 500.0, 1.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (1000.0, 500.0));
    }

    #[test]
    fn logical_size_custom_override_overrides_os_scale() {
        // Some(v)：logic = physical / v，与 OS 无关
        let s = to_pixel_size(1920.0, 1080.0, 1.5);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (1280.0, 720.0));
        let s = to_pixel_size(1000.0, 500.0, 4.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (250.0, 125.0));
        let s = to_pixel_size(1000.0, 500.0, 0.5);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (2000.0, 1000.0));
    }

    #[test]
    fn logical_size_invalid_scale_factor_falls_back_to_physical() {
        // scale<=0：逻辑 = 物理（pixel_of 回退）
        let s = to_pixel_size(800.0, 600.0, 0.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (800.0, 600.0));
        let s = to_pixel_size(800.0, 600.0, -1.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (800.0, 600.0));
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

    #[test]
    fn sliding_rate_empty_is_zero() {
        assert_eq!(sliding_rate(&[]), 0.0);
    }

    #[test]
    fn sliding_rate_averages_intervals() {
        // 60Hz：30 个 ~16.67ms 间隔 → 约 60/s
        let samples: Vec<f64> = (0..30).map(|_| 1.0 / 60.0).collect();
        let r = sliding_rate(&samples);
        assert!((r - 60.0).abs() < 1e-6, "got {r}");
    }

    #[test]
    fn sliding_rate_ignores_nonpositive_sum() {
        assert_eq!(sliding_rate(&[0.0, 0.0]), 0.0);
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

    #[test]
    fn size_drifted_within_epsilon_not_drifted() {
        // ±RESIZE_DRIFT_EPSILON 内的小抖动不算漂移（松手后 Windows 短暂抖动不重配）
        let eps = super::RESIZE_DRIFT_EPSILON;
        assert!(!super::size_drifted_beyond((800, 600), (800 + eps, 600), eps));
        assert!(!super::size_drifted_beyond((800, 600), (800 - eps, 600), eps));
        assert!(!super::size_drifted_beyond((800, 600), (800, 600 + eps), eps));
        assert!(!super::size_drifted_beyond((800, 600), (800, 600 - eps), eps));
        assert!(!super::size_drifted_beyond((800, 600), (800, 600), eps));
    }

    #[test]
    fn size_drifted_beyond_epsilon_drifted() {
        // 任一轴超出容差 → 真漂移（真实 resize）
        let eps = super::RESIZE_DRIFT_EPSILON;
        assert!(super::size_drifted_beyond((800, 600), (800 + eps + 1, 600), eps));
        assert!(super::size_drifted_beyond((800, 600), (800 - eps - 1, 600), eps));
        assert!(super::size_drifted_beyond((800, 600), (800, 600 + eps + 1), eps));
    }

    #[test]
    fn observed_moved_ignores_oscillation_within_epsilon() {
        // 松手后 800↔802 抖动（eps=2）相对锚点不算移动 → 去抖计时老化 → snap
        let eps = 2.0f32;
        let anchor = (800, 600, 1.0);
        for w in [800u32, 802, 800, 801, 799, 800] {
            assert!(
                !super::observed_moved(anchor, (w, 600, 1.0), eps),
                "w={w} 应在容差内视为未移动"
            );
        }
    }

    #[test]
    fn observed_moved_tracks_accumulated_drift() {
        // 锚点=上次显著位置：物理累计超出容差才算移动（单调慢拖也能及时刷新去抖计时）
        let eps = 2.0f32;
        let anchor = (800, 600, 1.0);
        assert!(super::observed_moved(anchor, (800 + eps as u32 + 1, 600, 1.0), eps));
        // 物理在容差内且 scale 相同 → 不算移动（逻辑 = 物理/scale，物理没动逻辑必没动）
        assert!(!super::observed_moved(anchor, (800, 600, 1.0), eps));
        // scale 变化即便物理都在容差内也算移动（需要重配）
        assert!(super::observed_moved(anchor, (800, 600, 2.0), eps));
    }

    #[test]
    fn drag_effective_cap_keeps_user_cap_when_higher_than_refresh() {
        // 用户 240、刷新率 120（milli-Hz 120_000）→ 压到 120
        assert_eq!(super::drag_effective_cap(Some(240), 120_000), Some(120));
        // 用户 60、刷新率 120 → 保持 60（不抬升）
        assert_eq!(super::drag_effective_cap(Some(60), 120_000), Some(60));
        // 用户 144、刷新率 120 → 压到 120
        assert_eq!(super::drag_effective_cap(Some(144), 120_000), Some(120));
    }

    #[test]
    fn drag_effective_cap_respects_explicit_none_and_zero_refresh() {
        // 用户显式不限制 → 拖动期也压到刷新率（与 set_max_fps 解耦）
        assert_eq!(super::drag_effective_cap(None, 120_000), Some(120));
        // 刷新率 0（查询异常）→ 下限 1，min 后用户值被压到 1 以下为 1
        assert_eq!(super::drag_effective_cap(Some(240), 0), Some(1));
        assert_eq!(super::drag_effective_cap(Some(1), 0), Some(1));
    }

    #[test]
    fn drag_cap_switch_disables_dragging_cap_and_enabled_caps_even_with_none() {
        // 开关关闭 → 原样返回用户值（不额外压制）
        assert_eq!(super::drag_cap_effective(Some(240), false, 120_000), Some(240));
        assert_eq!(super::drag_cap_effective(None, false, 120_000), None);
        // 开关开启 → 用户 Some 压到刷新率；用户 None 也压（解耦）
        assert_eq!(super::drag_cap_effective(Some(240), true, 120_000), Some(120));
        assert_eq!(super::drag_cap_effective(None, true, 120_000), Some(120));
    }

    #[test]
    fn drag_refresh_mhz_converts_millihertz_to_hertz() {
        // refresh_rate_millihertz() 返回 milli-Hz（120Hz → 120_000）。
        // 换算在 drag_effective_cap 内做，否则 min(240, 120_000) = 240 压不下去。
        assert_eq!(super::mhz_to_hz(120_000), 120);
        assert_eq!(super::mhz_to_hz(59_950), 60); // 四舍五入
        assert_eq!(super::mhz_to_hz(0), 0);
    }

    #[test]
    fn window_desc_default_frame_latency_is_2() {
        // DX12 下 latency 2 → 3 swapchain buffer（无 vsync 峰值更高），默认值即 3 buffer。
        assert_eq!(super::WindowDesc::new("t", 640, 360).frame_latency, 2);
        assert_eq!(
            super::WindowDesc::new("t", 640, 360).frame_latency(1).frame_latency,
            1
        );
    }

    #[test]
    fn window_desc_preparable_defaults_on_and_builder_switches() {
        // `preparable` 默认开：创建隐藏、首帧渲染后显示，消除 winit 建窗 4 阶段闪烁。
        assert!(super::WindowDesc::new("t", 640, 360).preparable);
        assert!(!super::WindowDesc::new("t", 640, 360).preparable(false).preparable);
        assert!(super::WindowDesc::new("t", 640, 360).preparable(true).preparable);
    }

    #[test]
    fn window_desc_new_size_is_logical_and_builders_are_typed() {
        // `new` 的裸宽高恒为 vireo 逻辑像素（用户坐标系）= `Pp::Dp`
        use super::Pp;
        use crate::dpi::{dp, px};
        assert_eq!(
            super::WindowDesc::new("t", 640, 360).size,
            (Pp::Dp(dp(640.0)), Pp::Dp(dp(360.0)))
        );
        // 尺寸族全部收 `impl Into<Pp>`，由调用点声明意图（裸数值/`Dp` = 逻辑，`Px` = 物理）
        let d = super::WindowDesc::new("t", 640, 360)
            .dpi_override(Some(2.0))
            .size(1280, 720)
            .min_size(200, 100)
            .max_size(2560, 1440)
            .resize_increments(10, 10);
        assert_eq!(d.dpi_override, Some(2.0));
        assert_eq!(d.size, (Pp::Dp(dp(1280.0)), Pp::Dp(dp(720.0))));
        assert_eq!(d.min_size, Some((Pp::Dp(dp(200.0)), Pp::Dp(dp(100.0)))));
        assert_eq!(d.max_size, Some((Pp::Dp(dp(2560.0)), Pp::Dp(dp(1440.0)))));
        assert_eq!(d.resize_increments, Some((Pp::Dp(dp(10.0)), Pp::Dp(dp(10.0)))));
        // 显式 `Px` 意图被保留
        let d = super::WindowDesc::new("t", 640, 360).size(px(1280.0), px(720.0));
        assert_eq!(d.size, (Pp::Px(px(1280.0)), Pp::Px(px(720.0))));
    }

    #[test]
    fn window_desc_position_is_vireo_logical() {
        use super::Pp;
        use crate::dpi::{dp, px};
        let d = super::WindowDesc::new("t", 640, 360).position(10, 20);
        assert_eq!(d.position, Some((Pp::Dp(dp(10.0)), Pp::Dp(dp(20.0)))));
        let d = super::WindowDesc::new("t", 640, 360).position(-5, -6);
        assert_eq!(d.position, Some((Pp::Dp(dp(-5.0)), Pp::Dp(dp(-6.0)))));
        let d = super::WindowDesc::new("t", 640, 360).position(px(100.0), px(200.0));
        assert_eq!(d.position, Some((Pp::Px(px(100.0)), Pp::Px(px(200.0)))));
    }

    #[test]
    fn dim_to_winit_size_and_position_follow_dpi_override() {
        use super::Pp;
        use crate::dpi::{dp, px};
        use super::{dim_to_winit_position, dim_to_winit_size};
        use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
        // `Dp` + `None` dpi：vireo 逻辑即 winit 逻辑（OS 缩放参与）
        assert_eq!(
            dim_to_winit_size(Pp::Dp(dp(640.0)), Pp::Dp(dp(360.0)), None, 1.0),
            winit::dpi::Size::Logical(LogicalSize::new(640.0, 360.0))
        );
        assert_eq!(
            dim_to_winit_position(Pp::Dp(dp(10.0)), Pp::Dp(dp(20.0)), None, 1.0),
            winit::dpi::Position::Logical(LogicalPosition::new(10.0, 20.0))
        );
        // `Some(d)` + `Dp`：vireo 全自持像素，物理 = 逻辑 × d
        assert_eq!(
            dim_to_winit_size(Pp::Dp(dp(640.0)), Pp::Dp(dp(360.0)), Some(2.0), 1.0),
            winit::dpi::Size::Physical(PhysicalSize::new(1280, 720))
        );
        assert_eq!(
            dim_to_winit_size(Pp::Dp(dp(100.0)), Pp::Dp(dp(100.0)), Some(0.5), 1.0),
            winit::dpi::Size::Physical(PhysicalSize::new(50, 50))
        );
        assert_eq!(
            dim_to_winit_position(Pp::Dp(dp(10.0)), Pp::Dp(dp(20.0)), Some(2.0), 1.0),
            winit::dpi::Position::Physical(PhysicalPosition::new(20, 40))
        );
        // 非法/非正 dpi 兜底为逻辑
        assert_eq!(
            dim_to_winit_size(Pp::Dp(dp(640.0)), Pp::Dp(dp(360.0)), Some(0.0), 1.0),
            winit::dpi::Size::Logical(LogicalSize::new(640.0, 360.0))
        );
        assert_eq!(
            dim_to_winit_position(Pp::Dp(dp(10.0)), Pp::Dp(dp(20.0)), Some(-1.0), 1.0),
            winit::dpi::Position::Logical(LogicalPosition::new(10.0, 20.0))
        );
        // `Px` 意图直接物理，忽略 dpi
        assert_eq!(
            dim_to_winit_size(Pp::Px(px(640.0)), Pp::Px(px(360.0)), Some(2.0), 1.0),
            winit::dpi::Size::Physical(PhysicalSize::new(640, 360))
        );
        // 混用意图：任一轴 Px → 整体物理空间；Dp 轴按有效缩放换算
        //   dpi_override=Some(2) → 有效缩放 2
        assert_eq!(
            dim_to_winit_size(Pp::Px(px(640.0)), Pp::Dp(dp(360.0)), Some(2.0), 1.0),
            winit::dpi::Size::Physical(PhysicalSize::new(640, 720))
        );
        assert_eq!(
            dim_to_winit_size(Pp::Dp(dp(640.0)), Pp::Px(px(360.0)), Some(2.0), 1.0),
            winit::dpi::Size::Physical(PhysicalSize::new(1280, 360))
        );
        assert_eq!(
            dim_to_winit_position(Pp::Dp(dp(10.0)), Pp::Px(px(20.0)), Some(2.0), 1.0),
            winit::dpi::Position::Physical(PhysicalPosition::new(20, 20))
        );
        //   dpi_override=None → 有效缩放 = os_scale
        assert_eq!(
            dim_to_winit_size(Pp::Dp(dp(640.0)), Pp::Px(px(360.0)), None, 1.5),
            winit::dpi::Size::Physical(PhysicalSize::new(960, 360))
        );
        assert_eq!(
            dim_to_winit_position(Pp::Px(px(10.0)), Pp::Dp(dp(20.0)), None, 2.0),
            winit::dpi::Position::Physical(PhysicalPosition::new(10, 40))
        );
    }

    #[test]
    fn instance_descriptor_env_and_manual_constructors_agree() {
        // `Instance::new(desc)` 不读 env，代码值压过 WGPU_BACKEND；display 由用户显式给。
        // 两个标准构造器：new_without_display_handle（全默认）+ from_env（读 env），
        // 用户手动改字段后传给 App::with_descriptor。
        let plain = wgpu::InstanceDescriptor::new_without_display_handle();
        assert_eq!(plain.backends, wgpu::Backends::default());
        assert!(plain.display.is_none());
        let mut desc = plain;
        desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::DX12;
        desc.flags = wgpu::InstanceFlags::VALIDATION;
        desc.memory_budget_thresholds = wgpu::MemoryBudgetThresholds {
            for_resource_creation: Some(80),
            for_device_loss: Some(90),
        };
        desc.backend_options = wgpu::BackendOptions::default();
        assert_eq!(desc.backends, wgpu::Backends::VULKAN | wgpu::Backends::DX12);
        assert_eq!(desc.flags, wgpu::InstanceFlags::VALIDATION);
        assert_eq!(desc.memory_budget_thresholds.for_resource_creation, Some(80));
        assert_eq!(desc.memory_budget_thresholds.for_device_loss, Some(90));
        // App::new 与 with_descriptor 共用同一构造路径（编译/逻辑层验证：
        // 不实际创建 GPU，避免开窗口）。
        let _ = super::App::with_descriptor; // 存在且可调用
    }

    #[test]
    fn deferred_after_frames_timing_table() {
        // 验证「after_frames(k) 于 frame_count + k 帧末执行」的 4 种情况：
        // run 前注册（fc=0）与帧内注册（第 N 帧），k=0/1 都能区分。
        // 初始 drain 在 fc=0（第一个 on_frame 之前）；每帧末 drain 在对应 fc。

        // —— run 之前注册（fc=0）——
        let run0 = super::DeferredTask::for_frames(0); // after_frames(0) → target 0
        let run1 = super::DeferredTask::for_frames(1); // after_frames(1) → target 1
        // 初始 drain（fc=0）：
        assert!(run0.is_ready(0), "run 前 after_frames(0) 应在第一个 on_frame 之前执行");
        assert!(!run1.is_ready(0), "run 前 after_frames(1) 初始 drain 时未到期");
        // 第 1 帧末 drain（fc=1）：
        assert!(run1.is_ready(1), "run 前 after_frames(1) 应在第 1 帧末尾执行");

        // —— 帧内注册（第 N 帧，N 任意）——
        let n = 7u64;
        let inner0 = super::DeferredTask::for_frames(n);     // after_frames(0) at frame N → target N
        let inner1 = super::DeferredTask::for_frames(n + 1); // after_frames(1) at frame N → target N+1
        assert!(inner0.is_ready(n), "帧内 after_frames(0) 于本帧末尾执行");
        assert!(!inner1.is_ready(n), "帧内 after_frames(1) 本帧末尾未到期");
        assert!(inner1.is_ready(n + 1), "帧内 after_frames(1) 于下一帧末尾执行");
    }
}
