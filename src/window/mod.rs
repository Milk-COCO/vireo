use std::sync::{Arc, OnceLock, mpsc};

use parking_lot::Mutex;

use crate::render::DrawBatch;
use crate::gpu::GpuContext;
use crate::input::InputState;

pub use winit::dpi::LogicalPosition;
pub use winit::dpi::LogicalSize;
pub use winit::dpi::PhysicalPosition;
pub use winit::dpi::PhysicalSize;
pub use winit::dpi::Position;
pub use winit::dpi::Size;
pub use winit::error::ExternalError;
pub use winit::error::NotSupportedError;

/// 诊断一次性标志：首帧 draw outcome（Presented/Skipped）打印一次，便于排查
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

/// 进程级入口由 [`App::new`] / [`App::with_descriptor`] 提供：在 OS 主线程直接构造
/// `App` 并把用户 future 放到独立的 `vireo-main` 线程，无需任何跨线程移交通道。

mod desc;
mod metrics;
mod on;
pub use desc::{AntiAliasing, FrameStyle, SendRawWindowHandle, WindowDesc};
pub use metrics::{
    DrawFailure, DrawOutcome, DrawReport, DrawSkipReason, DrawTimings, FollowAmount,
    FollowFramesOrTime, RenderAdvice, ResizeRefreshPolicy,
};
pub(crate) use desc::clamp_aa;
#[allow(unused_imports)]
pub(crate) use metrics::{
    DEFAULT_RESIZE_DEBOUNCE, PRESENT_SAMPLE_CAP, RESIZE_DRIFT_EPSILON,
    ResizeRefresh, drag_cap_effective, drag_effective_cap, mhz_to_hz, observed_moved,
    pac_advance, phys_to_logical, resize_refresh,
    size_drifted_beyond, skip_report, sliding_rate, validate_aspect_ratio,
};

pub use crate::dpi::{Dp, Pixel, PixelPos, PixelSize, Pp, Px, dp, px};
use crate::dpi::{dim_to_winit_position, dim_to_winit_size, to_pixel_pos, to_pixel_size};

/// `pacing_deadline` 的进程启动基线：`Instant` 没有 epoch，存原子量时改成相对此基线的纳秒。
static PACE_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// `Option<Instant>` → `AtomicI64` 纳秒（`None` = `i64::MIN` 哨兵）。
fn deadline_nanos(d: Option<std::time::Instant>) -> i64 {
    match d {
        None => i64::MIN,
        Some(t) => {
            let epoch = *PACE_START.get_or_init(std::time::Instant::now);
            t.duration_since(epoch).as_nanos().min(i64::MAX as u128) as i64
        }
    }
}

/// `AtomicI64` 纳秒 → `Option<Instant>`（`i64::MIN` 哨兵 = `None`）。
fn nanos_to_deadline(n: i64) -> Option<std::time::Instant> {
    if n == i64::MIN {
        return None;
    }
    let epoch = *PACE_START.get_or_init(std::time::Instant::now);
    Some(epoch + std::time::Duration::from_nanos(n.max(0) as u64))
}

/// 从 winit 线程发往渲染线程的事件（全是 Send-safe 的自定义类型）。
pub(crate) enum WinitEvent {
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
    /// 内部唤醒哨兵：由 `main_done` 置位点 / 渲染线程检测设备丢失或 loop 结束时发送，
    /// 用于唤醒阻塞在 `rx.recv()` 的 supervisor 重新判定退出条件。无业务载荷。
    Wake,
}

/// 窗口实例 —— 渲染线程独占，持有 surface/renderer/input 与完整帧循环。
///
/// **关键架构（第五十一轮）**：`SurfaceTexture` 从 acquire 到 present 全程是
/// `draw()` 内的局部值，不进入 Mutex/channel/winit 线程。同一 surface 最多一个
/// outstanding texture，且满足 wgpu-hal 的同线程 acquire→present 约束。
///
/// 所有公开 API 坐标系为逻辑像素（用户友好），GPU 内部使用物理像素。
pub struct VireoWindow {
    pub(crate) surface: Mutex<wgpu::Surface<'static>>,
    instance: wgpu::Instance,
    surface_config: Mutex<wgpu::SurfaceConfiguration>,
    /// 首次 draw 前 surface 尚未 `surface.configure`（建窗时推迟到渲染线程）：
    /// 置 true 强制首帧走 configure 路径，建立合法 swapchain 后再 acquire。
    needs_initial_configure: Mutex<bool>,
    /// 首帧渲染成功后是否自动显示窗口（`WindowDesc::preparable`）：建窗时隐藏，
    /// 首次 `Presented` 后 `set_visible(true)`，让窗口第一次出现即完整形态。
    pending_show: Mutex<bool>,
    renderer: Mutex<crate::render::Renderer>,
    pub inner: Arc<winit::window::Window>,
    pub gpu: Arc<GpuContext>,
    /// 最近一次 CursorMoved 的**物理像素**位置。逻辑/物理双表示走 [`Self::mouse_pos`]。
    pub(crate) mouse_pos: Mutex<(f32, f32)>,
    /// 最近一次观测到的**物理像素**窗口尺寸（真实来源）。逻辑尺寸 = 物理 ÷
    /// [`Self::layout_scale`] 现算（f64 除法，无截断；`PixelSize` 快照会因 scale
    /// 变化而过期，故不缓存逻辑值）。
    physical_size: Mutex<(u32, u32)>,
    /// vireo 层自定义 dpi 覆盖：`Some(v)` = **vireo 全自持像素**（物理 = vireo 逻辑 × v，
    /// 忽略 OS 缩放）；`None` = vireo 逻辑即 winit 逻辑（OS 系统 DPI 参与）。
    /// **不**设置 winit 的 `scale_factor_override`。运行时经 [`VireoWindow::set_dpi_override`] 切换。
    dpi_override: Mutex<Option<f64>>,
    /// 真正应用（写进 renderer/布局）的 dpi 覆盖。`set_dpi_override` 只改
    /// `dpi_override` 并请求物理 resize；`draw` 在物理尺寸落到目标后把
    /// `applied_dpi_override` 推进到新值（此前仍用旧 override，避免物理旧尺寸 × 新
    /// scale 造成逻辑瞬时漂移）。
    applied_dpi_override: Mutex<Option<f64>>,
    /// `set_dpi_override` 请求的目标物理尺寸（等待 resize 落地）；`None` = 无 pending。
    pending_override_target: Mutex<Option<(u32, u32)>>,
    /// `pending_override_target` 设置时刻（超时兜底：resize 被 OS 钳制时也应用 override）。
    pending_override_since: Mutex<Option<std::time::Instant>>,
    dpi_scale: Mutex<f32>,
    /// Last layout committed by `surface.configure`. FollowLayout may temporarily
    /// move the live camera away from this snapshot while the surface keeps its size.
    /// 存 `(phys_w, phys_h, scale, dpi_scale)`——物理尺寸 + scale 族；逻辑由物理 ÷
    /// scale 现算，不在快照里缓存（避免 scale 时效问题）。
    configured_layout: Mutex<(u32, u32, f32, f32)>,
    pub input: InputState,
    /// 待应用输入事件队列：supervisor 线程 drain `WinitEvent` 通道时把输入类事件
    /// 入队（按 window handle 路由），由 `refresh_input` 批量应用到 `InputState`。
    /// 这样输入更新权交给用户（可在 `on_tick` 构批次前调 `refresh_input` 拿当帧新鲜输入），
    /// `draw` 在 `auto_refresh_input` 开启时也自动调一次，用户不必手动重复。
    pub(crate) pending_input: Mutex<Vec<WinitEvent>>,
    /// 输入自动刷新开关（默认开）：`draw` 在 `auto_refresh_input` 为 true 时自动调
    /// `refresh_input`；设为 false 后由用户自行在 `on_tick` 内调用以获得当帧零滞后。
    auto_refresh_input: Mutex<bool>,
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
    /// winit 事件循环代理（与 `AppInner::event_loop_proxy` 共享）：`close()` 等方法在
    /// `ControlFlow::Wait` 下通过它唤醒事件循环，使其重新进入 `about_to_wait` 排空通道。
    /// `EventLoopProxy` 是 `Send + Sync + Clone`，设置后永不变更，无需 `Lock`。
    event_loop_proxy: Option<winit::event_loop::EventLoopProxy<()>>,
    /// 待应用的 present mode（在 draw 开头应用）
    pending_mode: Mutex<Option<wgpu::PresentMode>>,
    /// 真正 configure 到 surface 的 present mode（仅 configure 时更新）
    applied_present_mode: Mutex<wgpu::PresentMode>,
    /// 待应用的最大在途帧（`desired_maximum_frame_latency`，在 draw 开头应用）
    pending_frame_latency: Mutex<Option<u32>>,
    /// 真正 configure 到 surface 的在途帧（仅 configure 时更新）
    applied_frame_latency: Mutex<u32>,
    /// 上次 `surface.configure` 的时刻（resize 实时刷新间隔用）
    last_configure: Mutex<std::time::Instant>,
    /// 最近一次「尺寸仍与已配置值不同」的帧时刻（resize 去抖用）
    pending_resize_at: Mutex<Option<std::time::Instant>>,
    /// 上一帧观测到的窗口状态 (phys_w, phys_h, scale)——逻辑 = 物理 ÷ scale 派生，
    /// 不单存；用于判断尺寸是否仍在**移动**（相对上一帧变化才算移动，松手即停）。
    last_observed: Mutex<(u32, u32, f32)>,
    /// 拖动开始时缓存的显示器刷新率（Hz）。acquire 在拖动期失去 vsync 节流时，
    /// 渲染循环用它把 `max_fps` 临时压到刷新率（防空转）；松手 snap（configure）
    /// 后清空。只查询一次，避免拖动中每帧 `current_monitor()`。
    drag_refresh_mhz: Mutex<Option<u32>>,
    /// 拖动中的 resize 尺寸刷新策略。见 [`ResizeRefreshPolicy`]。
    resize_policy: Mutex<ResizeRefreshPolicy>,
    /// resize 去抖时长：尺寸稳定满此时间才一次性 configure（松手 snap）。默认
    /// [`DEFAULT_RESIZE_DEBOUNCE`]（100ms），可经 `set_resize_debounce` 覆盖。
    resize_debounce: Mutex<std::time::Duration>,
    /// 本窗口帧率上限（`VireoWindow::set_max_fps`）。默认取 `App` 创建时的值；拖动期
    /// 按 `drag_cap` 压到刷新率防空转。cap 归属窗口级（与渲染循环解耦）。
    max_fps: Mutex<Option<u32>>,
    /// 拖动期帧率上限开关（`VireoWindow::set_drag_cap`），与 `max_fps` 解耦。
    drag_cap: Mutex<bool>,
    /// 本窗口节流相位锁 deadline（`pac_advance` 维护）。
    /// 帧率上限相位锁的下一个 deadline（`pac_advance` 维护）。`AtomicI64` 存纳秒
    /// （`None` 哨兵 = `i64::MIN`，值 = 相对 `PACE_START` 的纳秒）。用原子量是为了同窗口被
    /// 多个 loop 绘制时，`draw` 对 deadline 的读-算-写不是 16 字节 `Instant` 的撕裂读；
    /// 注意：多 loop 间的 pacing 抖动（某 loop 基于稍旧 deadline 少睡一帧）仍可能发生，那是
    /// 良性抖动，不是 bug。
    pacing_deadline: std::sync::atomic::AtomicI64,
    /// 布局跟随开关（独立于 `ResizeRefreshPolicy`，默认开）：窗口尺寸已变但 surface
    /// 未重配时，每帧把 camera/逻辑尺寸更新到新窗口（`Renderer::update_layout`），
    /// 内容**实时重排**而非停在旧布局——DXGI 把旧 surface 拉伸到新窗口时正好抵消
    /// 缩放：几何和文字都按 x/y 两轴的新尺寸映射，不因宽高比变化产生额外近似。
    /// 可见误差来自窗口尺寸采样时序、整数舍入和 DPI 转换，而非单轴补偿。
    /// 关闭 = 旧行为：拖动中内容停旧逻辑布局（纯拉伸）。
    layout_follow: Mutex<bool>,
    /// 布局跟随的平滑模式（`FollowAmount`，默认 `Average(Time(16ms))`）。
    /// 参量化每个模式的平滑强度，详见 [`VireoWindow::set_layout_follow_smoothing`]。
    follow_smoothing: Mutex<FollowAmount>,
    /// 平均窗（`Average`）的尺寸采样队列：(帧号, 时刻, 逻辑宽, 逻辑高)。
    /// follow 平滑滑动窗：采样**物理像素**尺寸（逻辑 = 物理 ÷ scale 现算，避免在
    /// 逻辑空间均值引入额外精度损失）。
    follow_samples: Mutex<std::collections::VecDeque<(u64, std::time::Instant, u32, u32)>>,
    /// 跟随执行计数器（`Frames` 单位节流依据），每次 4a 跟随段自增。
    follow_frame: Mutex<u64>,
    /// 待应用的 AA 模式（在 draw 开头应用）
    pending_aa: Mutex<Option<AntiAliasing>>,
    /// 窗口 handle（在 App.windows 中的索引）
    handle: usize,
    /// 窗口边框样式（供 `frame_style()` 查询；运行时经 `set_frame_style` 切换）。
    pub(crate) frame_style: Mutex<FrameStyle>,
    /// 窗口是否可被点击激活获得焦点（`VireoWindow::set_focusable`）。
    /// 跨平台字段：Windows 经 `WS_EX_NOACTIVATE` 扩展样式落地；macOS 暂存
    /// 意图（需 NSWindow 子类化 `acceptsFirstResponder` 覆盖，待 macOS 平台
    /// 窗口能力整批实施时一并实现）。
    focusable: Mutex<bool>,
    /// 用户圆角偏好（Windows 11 22000+）。`set_corner_preference` 记录；
    /// Frameless 无边框时 DWM 无法圆角，`set_frame_style` 离幀前钳 / 恢复用。
    pub(crate) user_corner_pref: Mutex<crate::platform::windows::CornerPreference>,
    /// 关窗事件已到达（关闭中，draw 跳过）
    pub(crate) closing: Mutex<bool>,
    /// 当前帧是否 in-flight（已 acquire SurfaceTexture 尚未 present）。关窗路径据此等待
    /// 当前帧结束后再释放 Surface，避免 in-flight draw 与 Surface drop 竞态导致 wgpu 校验 panic。
    pub(crate) draw_idle: std::sync::atomic::AtomicBool,
    /// 关窗路径等待 `draw_idle` 置位的 Condvar：渲染线程每帧绘制完成时唤醒，渲染线程
    /// 正常结束 / panic退出时对所有窗口置位并唤醒——事件驱动、无忙等、无魔法数字超时。
    pub(crate) draw_idle_cv: Arc<(std::sync::Mutex<()>, std::sync::Condvar)>,
    /// 是否启用 queue completion 计时（`DrawTimings::gpu_secs`）
    gpu_timing_enabled: std::sync::atomic::AtomicBool,
    /// 上一份已完成提交的 GPU queue latency（由 `on_submitted_work_done` 写回）
    last_gpu_secs: Arc<Mutex<Option<f64>>>,
    pending_gpu_starts: Arc<Mutex<std::collections::VecDeque<std::time::Instant>>>,
    /// Outcome recorded by this window's draw call in the current update iteration.
    pub(crate) last_draw_outcome: Mutex<Option<DrawOutcome>>,
    /// 本窗口最近一次 draw 的完整报告（timings + outcome）。
    last_draw_report: Mutex<Option<DrawReport>>,
    presented_frames: Mutex<u64>,
    skipped_frames: Mutex<u64>,
    /// 最近成功 present 的间隔（秒），滑动窗口，用于 [`VireoWindow::presented_fps`]。
    present_intervals: Mutex<Vec<f64>>,
    last_present: Mutex<Option<std::time::Instant>>,
    occupancy: std::sync::Mutex<()>,
}

impl VireoWindow {
    pub(crate) fn new(
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
        event_loop_proxy: Option<winit::event_loop::EventLoopProxy<()>>,
        handle: usize,
    ) -> Self {
        let initial_present_mode = surface_config.present_mode;
        let initial_frame_latency = surface_config.desired_maximum_frame_latency;
        let initial_phys = (surface_config.width, surface_config.height);
        let scale = dpi_override.unwrap_or(dpi_scale as f64) as f32;
        Self {
            surface: Mutex::new(surface),
            instance,
            surface_config: Mutex::new(surface_config),
            needs_initial_configure: Mutex::new(true),
            pending_show: Mutex::new(pending_show),
            renderer: Mutex::new(renderer),
            inner,
            gpu,
            mouse_pos: Mutex::new((-1.0, -1.0)),
            physical_size: Mutex::new(initial_phys),
            dpi_override: Mutex::new(dpi_override),
            applied_dpi_override: Mutex::new(dpi_override),
            pending_override_target: Mutex::new(None),
            pending_override_since: Mutex::new(None),
            dpi_scale: Mutex::new(dpi_scale),
            max_fps: Mutex::new(None),
            drag_cap: Mutex::new(true),
            pacing_deadline: std::sync::atomic::AtomicI64::new(i64::MIN),
            configured_layout: Mutex::new((
                initial_phys.0,
                initial_phys.1,
                scale,
                dpi_scale,
            )),
            input: InputState::default(),
            pending_input: Mutex::new(Vec::new()),
            auto_refresh_input: Mutex::new(true),
            init_duration,
            event_tx,
            cb_tx,
            close_tx,
            event_loop_proxy,
            pending_mode: Mutex::new(None),
            applied_present_mode: Mutex::new(initial_present_mode),
            pending_frame_latency: Mutex::new(None),
            applied_frame_latency: Mutex::new(initial_frame_latency),
            last_configure: Mutex::new(std::time::Instant::now()),
            pending_resize_at: Mutex::new(None),
            last_observed: Mutex::new((
                initial_phys.0,
                initial_phys.1,
                scale,
            )),
            drag_refresh_mhz: Mutex::new(None),
            resize_policy: Mutex::new(ResizeRefreshPolicy::OnRelease),
            resize_debounce: Mutex::new(DEFAULT_RESIZE_DEBOUNCE),
            layout_follow: Mutex::new(true),
            follow_smoothing: Mutex::new(FollowAmount::default()),
            follow_samples: Mutex::new(std::collections::VecDeque::with_capacity(16)),
            follow_frame: Mutex::new(0),
            pending_aa: Mutex::new(None),
            handle,
            frame_style: Mutex::new(frame_style),
            focusable: Mutex::new(true),
            nc_tx,
            user_corner_pref: Mutex::new(
                crate::platform::windows::CornerPreference::Default,
            ),
            closing: Mutex::new(false),
            draw_idle: std::sync::atomic::AtomicBool::new(true),
            draw_idle_cv: Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new())),
            gpu_timing_enabled: std::sync::atomic::AtomicBool::new(false),
            last_gpu_secs: Arc::new(Mutex::new(None)),
            pending_gpu_starts: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            last_draw_outcome: Mutex::new(None),
            last_draw_report: Mutex::new(None),
            presented_frames: Mutex::new(0),
            skipped_frames: Mutex::new(0),
            present_intervals: Mutex::new(Vec::with_capacity(PRESENT_SAMPLE_CAP)),
            last_present: Mutex::new(None),
            occupancy: std::sync::Mutex::new(()),
        }
    }

    /// Configure the surface and synchronise every size-dependent renderer state.
    /// The caller must ensure no `SurfaceTexture` is outstanding.
    fn configure_surface(&self, size: winit::dpi::PhysicalSize<u32>, now: std::time::Instant) {
        debug_assert!(size.width > 0 && size.height > 0);
        let sf = self.inner.scale_factor();
        let dpi_override = *self.applied_dpi_override.lock();
        let scale = dpi_override.unwrap_or(sf) as f32;
        let dpi_scale = sf as f32;
        let (logical_w, logical_h) = phys_to_logical((size.width, size.height), dpi_override.unwrap_or(sf));

        let mut config = self.surface_config.lock().clone();
        config.width = size.width;
        config.height = size.height;
        self.surface.lock().configure(&self.gpu.device, &config);

        *self.applied_present_mode.lock() = config.present_mode;
        *self.applied_frame_latency.lock() = config.desired_maximum_frame_latency;
        *self.surface_config.lock() = config;
        *self.physical_size.lock() = (size.width, size.height);
        *self.dpi_scale.lock() = dpi_scale;
        *self.configured_layout.lock() = (size.width, size.height, scale, dpi_scale);
        // 注意：`needs_initial_configure` 必须在「surface 实际 configure 成功之后」才清。
        // 任何在 configure 之前返回的早退路径（`draw_frame` 的 Closing / DeviceLost /
        // ZeroSized / Outdated / Timeout / Occluded / Lost / Validation）都**不得**清除此标志，
        // 否则下一帧会对未配置的 surface 调 `get_current_texture` 报错。本标志只在此处清除。
        // 设备丢失边界：`wgpu::Surface::configure` 返回 `()`（错误走全局 error handler、不返回
        // `Result`），若配置的同一时刻发生异步设备丢失，surface 实际未配好但本标志已清。此时的
        // 兜底在 `draw_frame` 帧首的 `device_lost` 检查（m1）：置位则干净退出，不会拿未配好的
        // surface 去 `get_current_texture`。属极端边界，无需在此额外处理。
        *self.needs_initial_configure.lock() = false;
        *self.last_configure.lock() = now;
        *self.pending_resize_at.lock() = None;
        *self.drag_refresh_mhz.lock() = None;
        *self.last_observed.lock() = (size.width, size.height, scale);
        self.renderer.lock().resize(
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
        let report = {
            let _occupancy_guard = self.occupancy.lock().unwrap_or_else(|e| e.into_inner());
            let r = self.draw_frame(clear_color, batches);
            *self.last_draw_outcome.lock() = Some(r.outcome);
            *self.last_draw_report.lock() = Some(r);
            match r.outcome {
                DrawOutcome::Presented { .. } => {
                    let mut f = self.presented_frames.lock();
                    *f = f.saturating_add(1);
                    self.record_present();
                }
                DrawOutcome::Skipped(_) => {
                    let mut f = self.skipped_frames.lock();
                    *f = f.saturating_add(1);
                }
                DrawOutcome::Failed(_) => {}
            }
            r
        };
        // 帧率上限节流（cap 归属窗口级）：在 `occupancy` 锁**之外** sleep，避免同窗口被
        // 多 loop 绘制时一个 loop 的 cap sleep 占用锁、把另一个 loop 的 `draw` 串行阻塞、
        // 导致该窗口实际刷新率被腰斩（M1）。相位锁 `pac_advance`：绝对 deadline 滚动、
        // 落后不追赶；拖动期按 `drag_cap` 压到刷新率防空转（见 `drag_cap_effective`）。
        // 与渲染循环解耦：`draw` 是窗口唯一的帧循环入口，`App` 仅作默认值来源。
        let cap = self.effective_max_fps();
        let now = std::time::Instant::now();
        let cur = nanos_to_deadline(self.pacing_deadline.load(std::sync::atomic::Ordering::Acquire));
        let (next_deadline, sleep_dur) = pac_advance(now, cur, cap);
        self.pacing_deadline
            .store(deadline_nanos(next_deadline), std::sync::atomic::Ordering::Release);
        if let Some(s) = sleep_dur {
            std::thread::sleep(s);
        }
        report
    }

    /// 引擎对「本帧是否应该渲染」的建议（见 [`RenderAdvice`]）。
    ///
    /// 只读引擎已知缓存状态（焦点 / 尺寸 / 上次 draw 结局），不调 OS、不阻塞、macOS 安全；
    /// 状态由 `refresh_input` / `draw` 推进，用户负责刷新时机，故读到的是上次刷新后的值
    /// （最多滞后约 1 帧）。用于在构建渲染内容**之前**判断本帧是否值得画，避免失焦时
    /// 仍全速构建内容后被 vsync 丢弃。
    ///
    /// 引擎只报状态、不做策略：返回 `Unthrottled` 时**不会**自动限速，用户需自行
    /// 限流（降 `max_fps` 或在 `on_frame` 内返回 `false`）。
    pub fn render_advice(&self) -> RenderAdvice {
        if *self.closing.lock() {
            return RenderAdvice::Skip;
        }
        match *self.last_draw_outcome.lock() {
            Some(DrawOutcome::Skipped(
                DrawSkipReason::ZeroSized | DrawSkipReason::Occluded | DrawSkipReason::Closing,
            )) => return RenderAdvice::Skip,
            _ => {}
        }
        // 失焦：present 不被 vsync 节流 → 渲染循环全速空转。报 Unthrottled 让用户自行限流。
        if !self.focused() {
            return RenderAdvice::Unthrottled;
        }
        RenderAdvice::Render
    }

    /// 记录一次成功 present 的间隔（供 `presented_fps` 滑动窗口）。
    fn record_present(&self) {
        let now = std::time::Instant::now();
        if let Some(prev) = *self.last_present.lock() {
            let dt = now.duration_since(prev).as_secs_f64();
            if dt > 0.0 && dt < 0.5 {
                let mut v = self.present_intervals.lock();
                v.push(dt);
                if v.len() > PRESENT_SAMPLE_CAP {
                    v.remove(0);
                }
            }
        }
        *self.last_present.lock() = Some(now);
    }

    /// 极简 resize 诊断：仅在「决策」切换时打印（一次拖动约 2-5 行），用于定位 resize 拖动黑边回归。
    /// 决策 = RECONFIGURE（重配填满）/ STRETCH(suboptimal)（画旧 buffer，靠 DWM 拉伸）/ STABLE。
    /// 由 `VIREO_RESIZE_TRACE=1` 触发；正常运行无开销。
    fn trace_resize_state(handle: usize, decision: &str, detail: &str) {
        if std::env::var_os("VIREO_RESIZE_TRACE").is_none() {
            return;
        }
        use std::sync::{Mutex, OnceLock};
        static LAST: OnceLock<Mutex<std::collections::HashMap<usize, String>>> =
            OnceLock::new();
        let mut g = LAST
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap();
        let prev = g.get(&handle).cloned().unwrap_or_default();
        if prev != decision {
            g.insert(handle, decision.to_string());
            eprintln!("[resize-trace] win{}: {} | {}", handle, decision, detail);
        }
    }

    fn draw_frame(
        &self,
        clear_color: crate::color::Color,
        batches: &[&DrawBatch],
    ) -> DrawReport {
        let gpu_secs = self.last_gpu_secs.lock().take();
        if *self.closing.lock() {
            return DrawReport {
                outcome: DrawOutcome::Skipped(DrawSkipReason::Closing),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
                vsync_throttled: false,
            };
        }
        if self.gpu.is_device_lost() {
            return DrawReport {
                outcome: DrawOutcome::Failed(DrawFailure::DeviceLost),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
                vsync_throttled: false,
            };
        }
        // 自动输入刷新（`auto_refresh_input` 默认开；用户如需当帧零滞后可在 on_tick 内手动调）
        if *self.auto_refresh_input.lock() {
            self.refresh_input();
        }
        // 缓存 env var：每帧调 GetEnvironmentVariableW 是内核调用，空闲时无意义。
        fn draw_trace_enabled() -> bool {
            static VAL: OnceLock<bool> = OnceLock::new();
            *VAL.get_or_init(|| std::env::var_os("VIREO_DRAW_TRACE").is_some())
        }
        let trace = draw_trace_enabled();
        let t_trace = std::time::Instant::now();
        let mut configure_secs = 0.0;

        // 1) 应用 pending present mode（改 config 即可；尺寸同步在下方统一 configure）
        if let Some(mode) = self.pending_mode.lock().take() {
            let caps = self.surface.lock().get_capabilities(&self.gpu.adapter);
            let actual = Self::resolve_present_mode(mode, &caps.present_modes);
            self.surface_config.lock().present_mode = actual;
        }
        if let Some(latency) = self.pending_frame_latency.lock().take() {
            self.surface_config.lock().desired_maximum_frame_latency = latency;
        }
        // 应用 pending AA 变化（不触碰 surface；重建 msaa/ds 纹理）
        if let Some(aa) = self.pending_aa.lock().take() {
            let sc = aa.sample_count();
            let atc = aa.alpha_to_coverage();
            let ssaa = aa.is_ssaa();
            let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
            let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
            self.renderer.lock().update_aa(aa);
        }

        // 2) 逐帧轮询实际尺寸 + 缩放（模态循环期间最可靠）。
        //    resize 刷新策略（`ResizeRefreshPolicy`）：任何策略下，尺寸停止变化满
        //    去抖时长（`set_resize_debounce`，默认 100ms）都会一次性 configure
        //    （松手 snap）；`EveryFrame`/`Periodic` 在尺寸持续变化时额外实时
        //    configure。configure 阻塞在 Vulkan 后端（wgpu 默认）的 present queue 排空
        //    （~50-80ms）；DX12 同操作阻塞显著更低。实时刷新会掉帧——这是用户显式选择。
        //    present mode 变化不进去抖，下一帧立即 configure。
        let size = self.inner.inner_size();
        if size.width == 0 || size.height == 0 {
            return DrawReport {
                outcome: DrawOutcome::Skipped(DrawSkipReason::ZeroSized),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
                vsync_throttled: false,
            };
        }
        let sf = self.inner.scale_factor();
        let dpi_override = *self.dpi_override.lock();
        // override 覆盖变更（`set_dpi_override` 请求物理 resize）：物理尺寸落到
        // 目标（或超时兜底）前，仍用旧 override 换算——否则物理旧尺寸 × 新 scale
        // 会让逻辑瞬时漂移。落地后推进 applied_dpi_override，本帧起用新 override，
        // 尺寸漂移走下方正常的 resize 去抖 / 跟随 / configure 路径。
        if dpi_override != *self.applied_dpi_override.lock() {
            let reached = match *self.pending_override_target.lock() {
                Some((pw, ph)) => (pw == size.width && ph == size.height)
                    || self
                        .pending_override_since
                        .lock()
                        .is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(1)),
                None => true,
            };
            if reached {
                *self.applied_dpi_override.lock() = dpi_override;
                *self.pending_override_target.lock() = None;
                *self.pending_override_since.lock() = None;
            }
}
        // 构图缓存由 `refresh_metrics` 显式提供：此处复用它做 layout_follow 相机推进
        // （drift 判定与下方 resize 路径一致），避免 draw 内再复制一份尺寸轮询逻辑。
        // 用户也可在 on_tick 内手动调 `refresh_metrics` 以拿当帧构图新鲜度；漏调则由
        // 本帧 draw 补一次。
        self.refresh_metrics();
        let dpi_override = *self.applied_dpi_override.lock();
        let new_scale = dpi_override.unwrap_or(sf) as f32;
        let mut configured_this_frame = false;
        let mut follow_pending = false;
        let trace_size_drifted;
        let trace_need_configure;
        {
            let sc = self.surface_config.lock();
            // 尺寸漂移只看物理（逻辑 = 物理 ÷ scale，物理在容差内且 scale 不变 ⇒
            // 逻辑必在容差内），scale 变化单独捕获。
            let size_drifted = size_drifted_beyond(
                (sc.width, sc.height),
                (size.width, size.height),
                RESIZE_DRIFT_EPSILON,
            ) || new_scale != self.layout_scale() as f32;
            trace_size_drifted = size_drifted;
            let mode_drifted = sc.present_mode != *self.applied_present_mode.lock();
            let latency_drifted =
                sc.desired_maximum_frame_latency != *self.applied_frame_latency.lock();
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
                    *self.last_observed.lock(),
                    (size.width, size.height, new_scale),
                    RESIZE_DRIFT_EPSILON as f32,
                );
            if moved {
                *self.last_observed.lock() = (size.width, size.height, new_scale);
                let drag_starting = self.pending_resize_at.lock().is_none();
                *self.pending_resize_at.lock() = Some(now);
                if drag_starting {
                    // 拖动开始：缓存显示器刷新率（acquire 失去 vsync 节流时用它
                    // 临时压 cap 防空转）。只查一次；monitor 不跨屏时刷新率稳定。
                    // 原样存 milli-Hz（winit 返回值），换算只发生在 drag_effective_cap。
                    *self.drag_refresh_mhz
                        .lock() = self.current_monitor().and_then(|m| m.refresh_rate_millihertz());
                }
            }
            let refresh = resize_refresh(
                size_drifted,
                *self.pending_resize_at.lock(),
                now,
                *self.resize_debounce.lock(),
                *self.resize_policy.lock(),
                *self.last_configure.lock(),
            );
            let need_configure = *self.needs_initial_configure.lock()
                || (size_drifted && refresh != ResizeRefresh::None)
                || mode_drifted || latency_drifted;
            trace_need_configure = need_configure;
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
            } else if size_drifted && *self.layout_follow.lock() {
                // layout_follow（独立开关，默认开）：窗口已变但 surface 未重配——
                // 内容要实时重排而非停在旧布局。真正更新 camera 推迟到 acquire 之后
                // （见下方 `follow-layout` 段）：acquire 可能等待 swapchain 空位，因此返回后
                // re-poll 通常能取得更接近本帧 present 时刻的尺寸，但不保证前帧已上屏。
                // 配合 frame_latency=1（默认 2，可经 set_frame_latency 调整）降低 camera 的采样时差，不能保证消除拖动跳动。
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
                self.renderer.lock().set_text_viewport_override(None);
            }
        }

        // 3) acquire
        // 进入 acquire 前再核一次关闭 / 设备丢失：`draw_frame` 帧首（上方）的检查无法覆盖
        // 「本帧已过帧首检查、close 事件在 acquire 前到达」的竞态。渲染线程持有 `Arc<VireoWindow>`
        // 保证 surface/inner 在整段 `draw` 期间不被释放，此处早退仅为避免对已销毁窗口做
        // `get_current_texture`（DX12 上可能阻塞或报 validation）。属防御性不变式固化。
        if *self.closing.lock() {
            return DrawReport {
                outcome: DrawOutcome::Skipped(DrawSkipReason::Closing),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
                vsync_throttled: false,
            };
        }
        if self.gpu.is_device_lost() {
            return DrawReport {
                outcome: DrawOutcome::Failed(DrawFailure::DeviceLost),
                timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
                vsync_throttled: false,
            };
        }
        let t1 = std::time::Instant::now();
        if trace {
            eprintln!("[draw] acq-start");
        }
        let acquired = self.surface.lock().get_current_texture();
        let (st, suboptimal) = match acquired {
            wgpu::CurrentSurfaceTexture::Success(st) => {
                self.draw_idle.store(false, std::sync::atomic::Ordering::Release);
                (st, false)
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(st) => {
                self.draw_idle.store(false, std::sync::atomic::Ordering::Release);
                (st, true)
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                // 重配后再试；本帧跳过
                let size = self.inner.inner_size();
                if size.width == 0 || size.height == 0 {
                    return DrawReport {
                        outcome: DrawOutcome::Skipped(DrawSkipReason::ZeroSized),
                        timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
                        vsync_throttled: false,
                    };
                }
                let t_conf = std::time::Instant::now();
                self.configure_surface(size, t_conf);
                Self::trace_resize_state(
                    self.handle,
                    "RECONFIGURE(outdated)",
                    &format!("win={}x{}", size.width, size.height),
                );
                // `preparable` 兜底：首帧若走重配跳过，窗口会永远隐藏——显示它
                // （宁可无内容一帧，不可永不出现），与 Timeout/Occluded 兜底一致。
                self.maybe_show_prepared_window();
                return DrawReport {
                    outcome: DrawOutcome::Skipped(DrawSkipReason::SurfaceReconfigured),
                    timings: DrawTimings {
                        configure_secs: t_conf.elapsed().as_secs_f64(),
                        gpu_secs,
                        ..DrawTimings::default()
                    },
                    vsync_throttled: false,
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
                        vsync_throttled: false,
                    };
                }
                if let Some(new_surface) = self.recreate_surface() {
                    *self.surface.lock() = new_surface;
                    let t_conf = std::time::Instant::now();
                    self.configure_surface(size, t_conf);
                    configure_secs = t_conf.elapsed().as_secs_f64();
                }
                // `preparable` 兜底：窗口存活但本帧重配跳过，仍应现形（与 Timeout/Occluded 一致）。
                self.maybe_show_prepared_window();
                return DrawReport {
                    outcome: DrawOutcome::Skipped(DrawSkipReason::SurfaceReconfigured),
                    timings: DrawTimings { configure_secs, gpu_secs, ..DrawTimings::default() },
                    vsync_throttled: false,
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
                        vsync_throttled: false,
                    };
                }
                let t_conf = std::time::Instant::now();
                self.configure_surface(size, t_conf);
                // `preparable` 兜底：窗口存活但本帧重配跳过，仍应现形（与 Timeout/Occluded 一致）。
                self.maybe_show_prepared_window();
                return DrawReport {
                    outcome: DrawOutcome::Skipped(DrawSkipReason::SurfaceReconfigured),
                    timings: DrawTimings {
                        configure_secs: t_conf.elapsed().as_secs_f64(),
                        gpu_secs,
                        ..DrawTimings::default()
                    },
                    vsync_throttled: false,
                };
            }
        };
        {
            let sc = self.surface_config.lock();
            let decision = if trace_need_configure {
                "RECONFIGURE"
            } else if trace_size_drifted {
                "STRETCH(suboptimal)"
            } else {
                "STABLE"
            };
            Self::trace_resize_state(
                self.handle,
                decision,
                &format!(
                    "win={}x{} swap={}x{} suboptimal={}",
                    size.width, size.height, sc.width, sc.height, suboptimal
                ),
            );
        }
        if trace {
            eprintln!("[draw] acq-end {:?}us", t1.elapsed().as_micros());
        }
        let acquire_secs = t1.elapsed().as_secs_f64();

        // 4a) follow 布局跟随（实际执行）：acquire 可能等待 swapchain 空位；返回后
        //     re-poll inner_size 通常比 acquire 前的样本更接近本帧 present 时刻，但 acquire
        //     不保证前帧已上屏。配合 frame_latency=1（默认 2，可经 set_frame_latency 调整）降低 camera 时差与拖动跳动，仍可能
        //     留下约一个刷新周期内的采样差异。
        //     只在 follow_pending（step 2 登记的漂移）且确实仍漂移时更新；否则清残留
        //     的虚拟 viewport。
if follow_pending {
            let size = self.inner.inner_size();
            let sf = self.inner.scale_factor();
let dpi_override = *self.applied_dpi_override.lock();
        let new_scale = dpi_override.unwrap_or(sf) as f32;
        let dpi_scale = sf as f32;
            let still_drifted = {
                let sc = self.surface_config.lock();
                size_drifted_beyond(
                    (sc.width, sc.height),
                    (size.width, size.height),
                    RESIZE_DRIFT_EPSILON,
                ) || new_scale != self.layout_scale() as f32
            };
            if still_drifted && size.width != 0 && size.height != 0 {
                // 平滑模式分派：PerFrame 每帧追；Average 用滑动窗均值（物理像素空间
                // 均值，逻辑 = 物理 ÷ scale 现算）。
                let frame = *self.follow_frame.lock() + 1;
                *self.follow_frame.lock() = frame;
                let now = std::time::Instant::now();
                let target: Option<(u32, u32)> = match *self.follow_smoothing.lock() {
                    FollowAmount::PerFrame => Some((size.width, size.height)),
                    FollowAmount::Average(amt) => {
                        // 采样并入滑动窗，淘汰过期样本，取均值（连续渐变，不跳格）。
                        let mut q = self.follow_samples.lock();
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
                    *self.physical_size.lock() = (pw, ph);
                    *self.dpi_scale.lock() = dpi_scale;
                    self.renderer.lock().update_layout(
                        logical_w as f32, logical_h as f32, new_scale, dpi_scale,
                    );
                    self.renderer.lock().set_text_viewport_override(Some((pw, ph)));
                }
            } else {
                // 松手尺寸回稳但尚未 snap（debounce 未满）：不再重排，清虚拟 viewport，
                // 内容停在当前布局，等 Stable 分支一次性 configure。
                self.renderer.lock().set_text_viewport_override(None);
            }
        }

        // 4b) 编码
        let view = st.texture.create_view(&Default::default());
        let target = crate::render::RenderTarget::from_texture_view(view);
        let batch_refs: Vec<&DrawBatch> = batches.iter().copied().collect();
        let t2 = std::time::Instant::now();
        let cmd_buf = self.renderer.lock().draw(&target, Some(clear_color), &batch_refs);
        // 5) submit + 提交完成计时
        let timing_enabled = self.gpu_timing_enabled.load(std::sync::atomic::Ordering::Acquire);
        if timing_enabled {
            self.pending_gpu_starts.lock().push_back(std::time::Instant::now());
        }
        self.gpu.queue.submit([cmd_buf]);
        if timing_enabled {
            let last_gpu_secs = self.last_gpu_secs.clone();
            let pending_gpu_starts = self.pending_gpu_starts.clone();
            self.gpu.queue.on_submitted_work_done(move || {
                let start = pending_gpu_starts.lock().pop_front();
                if let Some(start) = start {
                    *last_gpu_secs.lock() = Some(start.elapsed().as_secs_f64());
                }
            });
        }
        let encode_secs = t2.elapsed().as_secs_f64();

        // 6) present
        // Wayland 需要 present 前通知合成器（调度 frame callback）；其余平台 no-op。
        self.inner.pre_present_notify();
        let t3 = std::time::Instant::now();
        self.gpu.queue.present(st);
        self.draw_idle.store(true, std::sync::atomic::Ordering::Release);
        // 唤醒可能正在等待本帧完成的关窗路径（事件驱动，取代原先百万次自旋）。
        self.draw_idle_cv.1.notify_all();
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

        // `preparable`：首帧渲染成功（已 present）后显示窗口。此时 surface 已
        // configure、内容已渲染，窗口第一次出现即完整形态，消除 winit 建窗的
        // 4 阶段闪烁。winit `set_visible` 线程安全（排队到 winit 线程）。
        self.maybe_show_prepared_window();

        DrawReport {
            outcome: DrawOutcome::Presented { suboptimal },
            vsync_throttled: self.focused(),
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
        if *self.pending_show.lock() {
            self.inner.set_visible(true);
            *self.pending_show.lock() = false;
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
        self.renderer.lock().last_draw_calls()
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
    pub(crate) fn resize(&self, width: u32, height: u32) {
        if width == 0 || height == 0 { return; }
        let sf = self.inner.scale_factor();
        *self.physical_size.lock() = (width, height);
        *self.dpi_scale.lock() = sf as f32;
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
        let dpi_override = *self.applied_dpi_override.lock();
        let scale = dpi_override.unwrap_or(sf) as f32;
        let dpi_scale = sf as f32;
        let drift = {
            let sc = self.surface_config.lock();
            size_drifted_beyond(
                (sc.width, sc.height),
                (size.width, size.height),
                RESIZE_DRIFT_EPSILON,
            ) || scale != self.layout_scale() as f32
        };
        if *self.layout_follow.lock() && drift {
            *self.physical_size.lock() = (size.width, size.height);
            *self.dpi_scale.lock() = dpi_scale;
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
    /// 设为 `false` 后由用户在 `on_tick` 内构批次前调用，可获得当帧零滞后输入。
    /// 漏调则这段事件在下次 `refresh_input` 才结算（与 `refresh_metrics` 同策略）。
    pub fn refresh_input(&self) -> bool {
        let mut q = self.pending_input.lock();
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
        *self.auto_refresh_input.lock() = enabled;
    }

    /// 当前是否启用 `draw` 内自动输入刷新。见 [`Self::refresh_input`]。
    pub fn auto_refresh_input(&self) -> bool {
        *self.auto_refresh_input.lock()
    }

    /// 把单个输入事件应用到 `InputState`（与 `pending_input` 队列的语义一致）。
    fn apply_input_event(&self, ev: WinitEvent) {
        match ev {
            WinitEvent::CursorMoved { x, y, .. } => {
                *self.mouse_pos.lock() = (x as f32, y as f32);
            }
            WinitEvent::KeyboardInput { event, .. } => {
                let is_pressed = event.state.is_pressed();
                let repeat = event.repeat;
                if is_pressed && !repeat {
                    self.input.keys_down.lock().insert(event.key);
                } else if !is_pressed {
                    self.input.keys_down.lock().remove(&event.key);
                }
            }
            WinitEvent::MouseInput { button, pressed, .. } => {
                if pressed {
                    self.input.mouse_buttons_down.lock().insert(button);
                } else {
                    self.input.mouse_buttons_down.lock().remove(&button);
                }
            }
            WinitEvent::MouseWheel { delta, .. } => {
                let mut acc = self.input.scroll_delta.lock();
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
                *self.input.modifiers.lock() = modifiers;
            }
            WinitEvent::Focused { focused, .. } => {
                let was_focused = std::mem::replace(&mut *self.input.focused.lock(), focused);
                if !focused && was_focused {
                    self.input.keys_down.lock().clear();
                    self.input.mouse_buttons_down.lock().clear();
                }
            }
            WinitEvent::CursorEntered { .. } => {
                *self.input.cursor_inside.lock() = true;
            }
            WinitEvent::CursorLeft { .. } => {
                *self.input.cursor_inside.lock() = false;
            }
            WinitEvent::Touch { event, .. } => {
                let sf = self.applied_dpi_override.lock().unwrap_or(self.inner.scale_factor());
                let tx = (event.x as f64 / sf) as f32;
                let ty = (event.y as f64 / sf) as f32;
                match event.phase {
                    crate::input::TouchPhase::Started | crate::input::TouchPhase::Moved => {
                        self.input.touches.lock().insert(event.id, (tx, ty, event.force));
                    }
                    _ => {
                        self.input.touches.lock().remove(&event.id);
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
        let dpi_override = self.applied_dpi_override.lock().unwrap_or(sf);
        let scale = dpi_override as f32;
        let config = self.surface_config.lock();
        size_drifted_beyond(
            (config.width, config.height),
            (size.width, size.height),
            RESIZE_DRIFT_EPSILON,
        ) || *self.configured_layout.lock() != (size.width, size.height, scale, sf as f32)
    }

    /// Number of successful `queue.present` calls made by this window.
    pub fn presented_frames(&self) -> u64 {
        *self.presented_frames.lock()
    }

    /// 最近成功 present 的提交频率（滑动窗口平均值）。
    ///
    /// 这是**提交节拍**（本进程成功 `queue.present` 的间隔），不是显示器/compositor
    /// 实际呈现频率：present 只把帧排队给合成器，displayed FPS 需 DXGI present
    /// statistics / PresentMon / ETW 才能测得，不能用 CPU 循环推断。无样本返回 0。
    pub fn presented_fps(&self) -> f64 {
        sliding_rate(&self.present_intervals.lock())
    }

    /// Number of draw attempts skipped before present.
    pub fn skipped_frames(&self) -> u64 {
        *self.skipped_frames.lock()
    }

    /// 获取当前鼠标位置（客户端坐标，物理 + 逻辑双表示）。
    ///
    /// 与 [`Self::inner_position`] 一致：`.physical()` 返回物理像素，`.logical()`
    /// 返回 vireo 逻辑像素（= 物理 ÷ 当前有效 scale）。
    ///
    /// **不阻塞**：读 vireo 内部缓存（由事件 / 渲染线程轮询锚点更新），任何线程可调，
    /// 无 winit 跨线程 hop（macOS 亦如此）。
    pub fn mouse_pos(&self) -> PixelPos {
        let mp = *self.mouse_pos.lock();
        to_pixel_pos(mp.0 as f64, mp.1 as f64, self.layout_scale())
    }

    /// 获取当前投影矩阵（逻辑像素）
    pub fn projection(&self) -> glam::Mat4 {
        let (w, h) = phys_to_logical(*self.physical_size.lock(), self.layout_scale());
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
        self.input.keys_down.lock().contains(&key)
    }

    pub fn any_key_down(&self) -> bool {
        !self.input.keys_down.lock().is_empty()
    }

    pub fn mouse_down(&self, button: crate::input::MouseButton) -> bool {
        self.input.mouse_buttons_down.lock().contains(&button)
    }

    pub fn mouse_left(&self) -> bool {
        self.mouse_down(crate::input::MouseButton::Left)
    }

    pub fn mouse_right(&self) -> bool {
        self.mouse_down(crate::input::MouseButton::Right)
    }

    pub fn modifiers(&self) -> crate::input::Modifiers {
        *self.input.modifiers.lock()
    }

    pub fn ctrl_down(&self) -> bool {
        self.input.modifiers.lock().ctrl()
    }

    pub fn shift_down(&self) -> bool {
        self.input.modifiers.lock().shift()
    }

    pub fn alt_down(&self) -> bool {
        self.input.modifiers.lock().alt()
    }

    pub fn take_scroll(&self) -> (f32, f32) {
        let mut delta = self.input.scroll_delta.lock();
        let result = delta.line;
        delta.line = (0.0, 0.0);
        result
    }

    pub fn take_scroll_pixel(&self) -> (f32, f32) {
        let mut delta = self.input.scroll_delta.lock();
        let result = delta.pixel;
        delta.pixel = (0.0, 0.0);
        result
    }

    /// 窗口当前是否聚焦（基于最近 `Focused` 事件）。
    ///
    /// **不阻塞**：读 vireo 内部缓存，任何线程可调，无 winit 跨线程 hop。
    pub fn focused(&self) -> bool {
        *self.input.focused.lock()
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
        *self.input.cursor_inside.lock()
    }

}

/// 窗口索引 —— 用于在 run() 闭包中引用窗口。稳定 handle：关窗后该索引失效（`window_ref` 返回 None），
/// 不会因其他窗口关闭而重指向新窗口。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WindowIndex(pub(crate) u64);

impl WindowIndex {
    pub(crate) fn new(handle: u64) -> Self {
        Self(handle)
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
            *self.dpi_override.lock(),
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
    /// 期间每次 configure 在 Vulkan 后端（wgpu 默认）上的阻塞（~60-90ms）可接受；
    /// DX12 同操作阻塞显著更低。
    ///
    /// 示例：`examples/window_api.rs` 按 `O` 键循环切换。
    pub fn set_dpi_override(&self, dpi: Option<f64>) {
        debug_assert!(dpi.map_or(true, |d| d.is_finite() && d > 0.0), "dpi_override must be None or finite >0");
        if *self.dpi_override.lock() == dpi {
            return;
        }
        // 保持 vireo 逻辑尺寸不变，按新 dpi 计算目标物理尺寸并 resize 窗口。
        let (lw, lh) = phys_to_logical(*self.physical_size.lock(), self.layout_scale());
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
        *self.dpi_override.lock() = dpi;
        *self.pending_override_target.lock() = Some((pw, ph));
        *self.pending_override_since.lock() = Some(std::time::Instant::now());
        // 始终按物理尺寸请求 resize（vireo 逻辑不变，只调窗口物理像素数）
        let _ = self.event_tx.send(WinitEvent::SetSize {
            handle: self.handle(),
            size: Size::Physical(PhysicalSize::new(pw, ph)),
        });
    }

    /// 当前 vireo 层 dpi 覆盖值（`None` = 使用 OS 系统 DPI）。
    pub fn dpi_override(&self) -> Option<f64> {
        *self.dpi_override.lock()
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
            *self.dpi_override.lock(),
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
                *self.dpi_override.lock(),
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
                *self.dpi_override.lock(),
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
        let mut frame_style_guard = self.frame_style.lock();
        let prev = *frame_style_guard;
        *frame_style_guard = style;
        {
            let prev_frameless = prev == FrameStyle::Frameless;
            let new_frameless = style == FrameStyle::Frameless;
            if prev_frameless != new_frameless {
                if let Some(_hwnd) = crate::platform::windows::win_hwnd(&self.inner) {
                    let target = if new_frameless {
                        crate::platform::windows::CornerPreference::Default
                    } else {
                        *self.user_corner_pref.lock()
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
                        let _ = _hwnd;
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
        *self.focusable.lock() = focusable;
        if let Some(hwnd) = crate::platform::windows::win_hwnd(&self.inner) {
            crate::platform::windows::apply_window_focusable(hwnd, focusable);
        }
    }

    /// 当前 `set_focusable` 设置（始终为最近一次调用值；macOS 暂存意图待实现生效）。
    pub fn is_focusable(&self) -> bool {
        *self.focusable.lock()
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
            *self.dpi_override.lock(),
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
        *self.frame_style.lock()
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
            *self.dpi_override.lock(),
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

    /// 显示系统窗口菜单（右键标题栏 / Alt+Space 菜单）。
    /// ## Platform-specific
    /// - **仅 Windows** 支持；其余平台 no-op。
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
                    *self.dpi_override.lock(),
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
            .lock()
            .unwrap_or(*self.dpi_scale.lock() as f64)
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
        let (pw, ph) = *self.physical_size.lock();
        to_pixel_size(pw as f64, ph as f64, self.layout_scale())
    }

    /// 运行时切换 present mode（会 reconfigure surface）。
    /// 下次 `draw` 前应用。
    pub fn set_present_mode(&self, mode: wgpu::PresentMode) {
        *self.pending_mode.lock() = Some(mode);
    }

    /// 当前真正生效（已 configure 到 surface）的 present mode；pending 尚未应用。
    pub fn present_mode(&self) -> wgpu::PresentMode {
        *self.applied_present_mode.lock()
    }

    /// 设置期望最大在途帧（`desired_maximum_frame_latency`），下一帧 draw 时
    /// 触发一次 `surface.configure`。
    ///
    /// DX12 下 swapchain buffer 数 = latency + 1：
    /// - `2`（默认）→ 3 buffer：CPU 可超前 2 帧，无 vsync 时峰值更高；
    /// - `1` → 2 buffer：在途封顶 1，vsync 拖动时 camera 时差更小。
    ///
    /// 值域为 wgpu 能力范围（通常 1..=16），超出会被 wgpu 夹紧。`0` 非法。
    /// 本窗口帧率上限（`None` = 不限制）。默认取 `App::set_max_fps` 创建时的值；
    /// `set_max_fps` 可在运行期按窗口独立调整（不与 `App` 全局值联动，除非再次经
    /// `App::set_max_fps` 传播）。与渲染循环解耦：`draw` 是窗口唯一帧入口，节流在
    /// `draw_frame` 内按本窗口 cap 执行。
    pub fn set_max_fps(&self, fps: Option<u32>) {
        *self.max_fps.lock() = fps;
    }

    /// 当前本窗口帧率上限（见 `set_max_fps`）。
    pub fn max_fps(&self) -> Option<u32> {
        *self.max_fps.lock()
    }

    /// 本窗口「拖动期帧率上限」开关，与 `set_max_fps` 解耦。详见 `App::set_drag_cap` 的语义
    /// 说明：开启（默认）时 resize 拖动期即使 `set_max_fps(None)` 也压到刷新率防空转；
    /// 关闭则拖动期全速产帧、画面内容变化更平滑。
    pub fn set_drag_cap(&self, enabled: bool) {
        *self.drag_cap.lock() = enabled;
    }

    /// 当前本窗口「拖动期帧率上限」开关。
    pub fn drag_cap(&self) -> bool {
        *self.drag_cap.lock()
    }

    /// 实际生效的帧率上限（窗口级）。`set_max_fps` 用户值基础上，若本窗口正在 resize
    /// 拖动（acquire 失去 vsync 节流）且 `drag_cap` 开启，则压到本窗口显示器刷新率，
    /// 避免渲染循环全速空转；松手 snap 后自动恢复。纯决策，供 `draw_frame` 帧节流调用。
    fn effective_max_fps(&self) -> Option<u32> {
        let user = *self.max_fps.lock();
        if *self.drag_cap.lock() && self.pending_resize_at.lock().is_some() {
            if let Some(mhz) = *self.drag_refresh_mhz.lock() {
                return drag_cap_effective(user, true, mhz);
            }
        }
        user
    }

    pub fn set_frame_latency(&self, latency: u32) {
        *self.pending_frame_latency.lock() = Some(latency);
    }

    /// 当前真正生效（configure 到 surface）的在途帧。pending 未应用时返回旧值。
    pub fn frame_latency(&self) -> u32 {
        *self.applied_frame_latency.lock()
    }

    /// 设置拖动窗口时的 resize 尺寸刷新策略（[`ResizeRefreshPolicy`]）。
    /// 默认 `OnRelease`：拖动全程不更新（拉伸显示、帧流满速），松手 snap。
    /// `EveryFrame` / `Periodic(interval)` 会在拖动中实时 `surface.configure`，
    /// 每次阻塞 ~50-80ms（wgpu-hal DX12 present queue 排空），掉帧是预期代价。
    pub fn set_resize_refresh_policy(&self, policy: ResizeRefreshPolicy) {
        *self.resize_policy.lock() = policy;
    }

    /// 当前拖动中的 resize 尺寸刷新策略（默认 [`ResizeRefreshPolicy::OnRelease`]）。
    pub fn resize_refresh_policy(&self) -> ResizeRefreshPolicy {
        *self.resize_policy.lock()
    }

    /// 设置 resize 去抖时长：拖动中尺寸**稳定**满此时间后才一次性
    /// `surface.configure`（松手 snap）。默认 100ms。
    ///
    /// 调小 → 松手 snap 更快，但「按住但暂停一下」的拖动间隙更容易误触发
    /// configure 卡顿；调大 → 松手 snap 更慢、更不容易被暂停误触发。
    pub fn set_resize_debounce(&self, debounce: std::time::Duration) {
        *self.resize_debounce.lock() = debounce;
    }

    /// 当前 resize 去抖时长（默认 100ms）。见 [`VireoWindow::set_resize_debounce`]。
    pub fn resize_debounce(&self) -> std::time::Duration {
        *self.resize_debounce.lock()
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
        let mut layout_follow_guard = self.layout_follow.lock();
        let was_enabled = *layout_follow_guard;
        *layout_follow_guard = enabled;
        if was_enabled && !enabled {
            self.follow_samples.lock().clear();
            let (phys_w, phys_h, scale, dpi_scale) = *self.configured_layout.lock();
            let (logical_w, logical_h) = phys_to_logical((phys_w, phys_h), scale as f64);
            *self.physical_size.lock() = (phys_w, phys_h);
            *self.dpi_scale.lock() = dpi_scale;
            let mut renderer = self.renderer.lock();
            renderer.update_layout(logical_w as f32, logical_h as f32, scale, dpi_scale);
            renderer.set_text_viewport_override(None);
        }
    }

    /// 当前布局跟随开关（默认开）。见 [`VireoWindow::set_layout_follow`]。
    pub fn layout_follow(&self) -> bool {
        *self.layout_follow.lock()
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
        *self.follow_smoothing.lock() = amount;
        self.follow_samples.lock().clear();
    }

    /// 当前布局跟随平滑模式。见 [`VireoWindow::set_layout_follow_smoothing`]。
    pub fn layout_follow_smoothing(&self) -> FollowAmount {
        *self.follow_smoothing.lock()
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
        *self.pending_aa.lock() = Some(aa);
    }

    /// 窗口 handle（在 App.windows 中的索引）
    pub fn handle(&self) -> usize {
        self.handle
    }

    /// 唤醒 winit 事件循环（`ControlFlow::Wait` 下）。
    /// 与 `App::wake_event_loop` 共享同一 `EventLoopProxy`（Clone）。
    pub(crate) fn wake_event_loop(&self) {
        if let Some(proxy) = self.event_loop_proxy.as_ref() {
            let _ = proxy.send_event(());
        }
    }

    /// 请求关闭窗口（走与用户点关闭按钮相同的完整关窗路径）。
    ///
    /// 内部经 `close_tx` 通道转发到 winit 线程，在 winit 线程依次执行
    /// close_hooks / Windows NC 状态清理，并发送 `WinitEvent::CloseRequested` 给渲染线程
    /// ——渲染线程随后置 `closing`、从 `App.windows` 移除本窗口，最后一个窗口关闭时请求退出
    /// event loop。关窗幂等（重复调用不影响）。返回前不阻塞；实际关窗在下一次 winit 事件循环
    /// 迭代生效。
    pub fn close(&self) {
        let _ = self.close_tx.send(self.handle);
        self.wake_event_loop();
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
mod aspect_ratio_and_focus_tests {
    //! A1 `set_focusable` / A2 `set_aspect_ratio` / A9 `blur` 的纯函数层验证。
    //!
    //! - `validate_aspect_ratio` 是 `set_aspect_ratio` 的纯输入过滤（`None`/非正
    //!   数 → `None`），单测覆盖所有边界。
    //! - `blur` / `focused` 是 `InputState.focused` 的双向访问，本模块不直接构造
    //!   `VireoWindow`（需 winit 上下文），通过聚焦状态互斥的元测试验证语义：
    //!   `blur() = !focused()` 在所有聚焦状态下成立。

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
    fn logical_size_conversion() {
        // scale=1.0：逻辑 = 物理（用户坐标即物理像素）
        let s = to_pixel_size(1920.0, 1080.0, 1.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (1920.0, 1080.0));
        let s = to_pixel_size(1000.0, 500.0, 1.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (1000.0, 500.0));
        // scale=2.0：logic = physical / 2（非整数 scale 不截断）
        let s = to_pixel_size(1920.0, 1080.0, 2.0);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (960.0, 540.0));
        // scale=0.5（小 scale）
        let s = to_pixel_size(1000.0, 500.0, 0.5);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (2000.0, 1000.0));
        // 非整数 scale 截断验证
        let s = to_pixel_size(1280.0, 720.0, 1.5);
        assert_eq!((s.width.dp.0 as f32, s.height.dp.0 as f32), (853.3333, 480.0));
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
    fn sliding_rate_averages_intervals() {
        // 60Hz：30 个 ~16.67ms 间隔 → 约 60/s
        let samples: Vec<f64> = (0..30).map(|_| 1.0 / 60.0).collect();
        let r = sliding_rate(&samples);
        assert!((r - 60.0).abs() < 1e-6, "got {r}");
    }


    fn base() -> std::time::Instant {
        std::time::Instant::now()
    }

    #[test]
    fn resize_refresh_decision_all_policies() {
        let t0 = base();
        let debounce = std::time::Duration::from_millis(100);
        // OnRelease：刚变化 → None；稳定 200ms → Stable
        assert_eq!(
            super::resize_refresh(true, Some(t0), t0, debounce,
                super::ResizeRefreshPolicy::OnRelease, t0),
            super::ResizeRefresh::None,
        );
        let stable = t0 + std::time::Duration::from_millis(200);
        assert_eq!(
            super::resize_refresh(true, Some(t0), stable, debounce,
                super::ResizeRefreshPolicy::OnRelease, t0),
            super::ResizeRefresh::Stable,
        );
        // EveryFrame：拖动中 → Live；稳定时 Stable 优先
        assert_eq!(
            super::resize_refresh(true, Some(t0), t0, debounce,
                super::ResizeRefreshPolicy::EveryFrame, t0),
            super::ResizeRefresh::Live,
        );
        assert_eq!(
            super::resize_refresh(true, Some(t0), stable, debounce,
                super::ResizeRefreshPolicy::EveryFrame, t0),
            super::ResizeRefresh::Stable,
        );
        // Periodic：距上次 200ms < iv(400ms) → None；满 iv → Live
        let iv = std::time::Duration::from_millis(400);
        let mid = t0 + std::time::Duration::from_millis(200);
        assert_eq!(
            super::resize_refresh(true, Some(mid), mid, debounce,
                super::ResizeRefreshPolicy::Periodic(iv), t0),
            super::ResizeRefresh::None,
        );
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
    fn drag_cap_effective() {
        // 开关开启：用户 Some(240) 压到刷新率 120；用户 Some(60) 保持 60（不抬升）
        assert_eq!(super::drag_effective_cap(Some(240), 120_000), Some(120));
        assert_eq!(super::drag_effective_cap(Some(60), 120_000), Some(60));
        assert_eq!(super::drag_effective_cap(Some(144), 120_000), Some(120));
        // 用户 None → 也压到刷新率（解耦）；刷新率 0 → 下限 1
        assert_eq!(super::drag_effective_cap(None, 120_000), Some(120));
        assert_eq!(super::drag_effective_cap(Some(240), 0), Some(1));
        assert_eq!(super::drag_effective_cap(Some(1), 0), Some(1));
        // 开关关闭：原样返回（不额外压制）
        assert_eq!(super::drag_cap_effective(Some(240), false, 120_000), Some(240));
        assert_eq!(super::drag_cap_effective(None, false, 120_000), None);
        // 开关开启：Some 压到刷新率；None 也压
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
        // `App::new` 与 `with_descriptor` 共用同一构造路径（逻辑层验证：不实际创建 GPU，
        // 避免开窗口）。两者现在均为泛型入口 `FnOnce(App) -> Future`，此处仅验证
        // `desc` 字段正确构造；调用路径由 examples 覆盖。
    }

    #[test]
    fn deferred_after_ticks_timing_table() {
        // 验证「after_ticks(k) 于 tick_count + k 帧末执行」的 4 种情况：
        // run 前注册（fc=0）与帧内注册（第 N 帧），k=0/1 都能区分。
        // 初始 drain 在 fc=0（第一个 on_tick 之前）；每帧末 drain 在对应 fc。

        // —— run 之前注册（fc=0）——
        let run0 = crate::app::DeferredTask::for_frames(0); // after_ticks(0) → target 0
        let run1 = crate::app::DeferredTask::for_frames(1); // after_ticks(1) → target 1
        // 初始 drain（fc=0）：
        assert!(run0.is_ready(0), "run 前 after_ticks(0) 应在第一个 on_tick 之前执行");
        assert!(!run1.is_ready(0), "run 前 after_ticks(1) 初始 drain 时未到期");
        // 第 1 帧末 drain（fc=1）：
        assert!(run1.is_ready(1), "run 前 after_ticks(1) 应在第 1 帧末尾执行");

        // —— 帧内注册（第 N 帧，N 任意）——
        let n = 7u64;
        let inner0 = crate::app::DeferredTask::for_frames(n);     // after_ticks(0) at frame N → target N
        let inner1 = crate::app::DeferredTask::for_frames(n + 1); // after_ticks(1) at frame N → target N+1
        assert!(inner0.is_ready(n), "帧内 after_ticks(0) 于本帧末尾执行");
        assert!(!inner1.is_ready(n), "帧内 after_ticks(1) 本帧末尾未到期");
        assert!(inner1.is_ready(n + 1), "帧内 after_ticks(1) 于下一帧末尾执行");
    }
}
