use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Icon, WindowAttributes, WindowId},
};

use crate::dpi::{dim_to_winit_position, dim_to_winit_size, to_pixel_size};
use crate::error::VireoError;
use crate::gpu::GpuContext;
use crate::offscreen::OffscreenCanvas;
use crate::platform::windows::win_hwnd;
use crate::render::Renderer;
use crate::texture::Texture;
use crate::window::{
    AntiAliasing, FrameStyle, OffscreenIndex, VireoWindow, WinitEvent, WindowDesc, WindowIndex,
};

/// 渲染线程 → winit 线程的运行期窗口创建请求（`App::window` 在 on_tick 里调用时）。
/// winit 线程在 `about_to_wait` drain 后执行窗口创建。
struct CreateWindowRequest {
    handle: u64,
    desc: WindowDesc,
    init_duration: f64,
    on_close: Option<Box<dyn FnOnce() + Send>>,
}

/// 入口期一次性创建的通道集合：`App::new`/`with_descriptor` 在 spawn 用户闭包**前**建好
/// 并把两个 sender（`create_tx`/`cb_tx`）写到 `AppInner`，随后把本结构交给 `run_blocking`
/// 去构造 `Runner`（持有 receiver）。这样 `App::window` 与 `App::on_*` 在闭包运行前即可经
/// 通道投递，无需注册握手。
struct RunChannels {
    event_tx: mpsc::Sender<WinitEvent>,
    event_rx: mpsc::Receiver<WinitEvent>,
    cb_rx: mpsc::Receiver<(usize, crate::input::InputCallbacks)>,
    exit_rx: mpsc::Receiver<()>,
    exit_tx: mpsc::Sender<()>,
    frame_style_rx: mpsc::Receiver<(isize, FrameStyle)>,
    aspect_ratio_rx: mpsc::Receiver<(isize, Option<f64>)>,
    nc_rx: mpsc::Receiver<(isize, crate::platform::windows::NcUpdate)>,
    create_rx: mpsc::Receiver<CreateWindowRequest>,
    close_rx: mpsc::Receiver<usize>,
    supervisor_event_tx: mpsc::Sender<WinitEvent>,
    frame_style_tx: mpsc::Sender<(isize, FrameStyle)>,
    aspect_ratio_tx: mpsc::Sender<(isize, Option<f64>)>,
    nc_tx: mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    close_tx: mpsc::Sender<usize>,
}

pub struct AppInner {
    /// `Vec<Option<Arc<VireoWindow>>>`，以 handle 为索引。关闭的窗口为 `None`。
    pub windows: Mutex<Vec<Option<Arc<VireoWindow>>>>,
    /// 存活窗口数（O(1) 读，避免 `window_count` 每帧遍历整个 `windows` 历史表——
    /// handle 单调递增不回收，长会话高频开关窗口时 `windows` 只增不缩，`window_count`
    /// 若用 filter 计数会是 O(历史总数)。建窗 +1、关窗 -1）。
    alive_window_count: AtomicUsize,
    /// 累计已创建窗口数（只增不缩）。用于渲染线程判定「曾创建且现已全部关闭」——
    /// 关窗后 `alive_window_count` 归零，但 `on_tick` 可能仍返回 `true`，渲染线程需据此
    /// 独立于 `on_tick` 返回值退出（与 supervisor 的 `all_windows_closed` 判定对齐）。
    created_window_count: AtomicUsize,
    pub gpu: Arc<GpuContext>,
    instance: Mutex<Option<wgpu::Instance>>,
    /// 设备丢失标志：由 `GpuContext` 的 `Device::set_device_lost_callback` 置位
    ///（`GpuContext::device_lost()` 同 Arc）。渲染循环每帧读它，置位则干净终止。
    device_lost: Arc<AtomicBool>,
    /// 稳定 handle → winit WindowId。handle 由 `App::window()` 分配，
    /// 在 run() 中被取出给 winit 线程用。
    handle_to_id: Mutex<FxHashMap<u64, WindowId>>,
    /// 下一个待分配的 handle（单调递增；`App::window()` 自增）。
    next_handle: Mutex<u64>,
    default_icon: Mutex<Option<Icon>>,
    textures: Mutex<Vec<Arc<Texture>>>,
    offscreens: Mutex<Vec<Arc<OffscreenCanvas>>>,
    /// App::new 内部耗时（秒）：GPU 设备、shader 模块、bind group layout 构造。
    pub init_duration: f64,
    /// 可选帧率上限（`App::set_max_fps`）。它**只作为默认值种子**：
    /// - [`Thread::max_tps`] 在 `App::spawn` 时若未显式设置则取此值，之后由 Thread 独立生效；
    /// - 新窗口在创建时以 `App::max_fps()` 种子自身 `max_fps`（`VireoWindow::set_max_fps`），
    ///   运行期改 `App::max_fps` 不影响已 spawn 的 Thread / 已建出的窗口。
    /// 真正限速发生在 acquire 不阻塞（拖动/无 vsync）时由相位锁 `pac_advance` 以 sleep 把 CPU
    /// 循环拉回目标频率，避免空转。默认 `Some(240)`：给足余量，正常 vsync 下 acquire 更早卡住、
    /// cap 不生效；仅在空转时兜底。
    max_fps: Mutex<Option<u32>>,
    /// 拖动期帧率上限开关（`App::set_drag_cap`）。与 `set_max_fps` 解耦：开启时
    /// resize 拖动中即使 `max_fps(None)` 也压到显示器刷新率（省资源但画面内容
    /// 变化实测易卡）；关闭则拖动期不额外压制、渲染循环全速产帧，画面内容随
    /// 窗口尺寸变化更平滑。默认开启。
    drag_cap: Mutex<bool>,
    /// 运行期窗口创建通道：`App::new`/`with_descriptor` 在 spawn 用户闭包前设置
    /// （取 create_rx 给 winit 线程），之后 `App::window` 通过它把创建请求发给 winit 线程。
    /// 现在 `App::window` 始终走通道（预注册与运行期统一），不再有 `window_descs` 登记路径。
    create_tx: Mutex<Option<mpsc::Sender<CreateWindowRequest>>>,
    /// `App` 级输入回调通道：与 `VireoWindow` 级回调（[`def_window_ons`]）共用同一通道，
    /// `App::on_*` 在 [`App::new`] 设置本 sender 后即可经它发给 winit 线程，无需 handshake。
    cb_tx: Mutex<Option<mpsc::Sender<(usize, crate::input::InputCallbacks)>>>,
    /// 已 spawn 的循环线程共享状态（跨线程）：supervisor 据此判定全部结束。
    pub(crate) loop_states: Mutex<Vec<Arc<crate::thread::LoopHandleState>>>,
    /// 所有预期窗口创建完成前为 `false`；loop 线程据此等待（避免无窗口时驱动）。
    /// 置位条件：已创建窗口数 `>=` 预期窗口数（含预期 0 的纯运行期/零窗口场景，置位即放行，
    /// loop 照常运行；真正防止「窗口已注册但未建出来就误退」靠 `pending_window_creates`）。
    pub(crate) windows_ready: AtomicBool,
    /// 已通过 `App::window` 注册、但 `WindowCreated` 尚未到达的窗口计数（运行期建窗路径）。
    /// loop 线程在 `on_tick` 因窗口暂未建出而返回 false 时，若此计数 `>0` 则不退出、继续等待
    /// `WindowCreated`；supervisor 退出判定也据此认定「窗口尚未就绪」。这样 ez 竞态（预注册窗口
    /// 经运行期通道晚到）与「在 `on_tick` 内建窗」「零窗口 app」均为正常行为，无需时间兜底。
    pub(crate) pending_window_creates: AtomicUsize,
    /// `App::run` / `App::spawn` 被调用后置位：标记「应用已请求至少一个渲染循环」。
    /// supervisor 退出判定据此区分两类场景——
    /// - 已请求循环：必须等 `loop_states` 非空且全部 `done` 才退出（防止 `spawn` 尚未把
    ///   状态推入 `loop_states` 前的竞态早退）；
    /// - 从未请求循环（纯建窗 app）：循环计数视为「已完成」，窗口全部关闭即可退出，
    ///   否则 `loop_states` 为空会让 `all().is_empty()` 守卫误判为「尚未注册」而永不退出。
    pub(crate) loops_ever_requested: AtomicBool,
    /// vireo-main 的 `main` future 已完成（整个进程生命周期的顶层权威信号）。
    /// supervisor 退出判定前置条件之一：`main_done && all_loops_done && windows_settled` 时退出
    /// ——即「用户代码已结束 且 所有 loop 已结束 且 所有窗口已关闭/无窗口」。
    /// 取代旧的 `app_started`（后者只在首次 `app.window`/`app.run` 时置位，无法覆盖零窗口零 loop
    /// 的 `main` 立即返回场景，导致 supervisor 永不退出而挂死）。
    /// `main_done` 仅在 `block_on(main)` 真正返回后由 vireo-main 线程置位；此时 `main` 体内的同步
    /// `app.window`/`app.run` 早已执行（`pending_window_creates` 已自增），故不存在「supervisor
    /// 首轮迭代早于 `app.window` 调用而误退」的启动竞态。
    pub(crate) main_done: AtomicBool,
    /// 内部事件 sender 镜像：`main_done` / 设备丢失 / loop 完成等置位时借此发 `WinitEvent::Wake`
    /// 唤醒 supervisor（阻塞在 `rx.recv()`）。`None` 仅在构造早期、通道尚未建立时短暂存在。
    pub(crate) event_tx: Mutex<Option<mpsc::Sender<WinitEvent>>>,
    /// 渲染线程等待「窗口就绪 / 运行期建窗完成」的 Condvar：由 supervisor 在对应状态变更时
    /// `notify_all`，取代原先的 `yield_now` 自旋 / 固定间隔 `sleep`（事件驱动、无魔法数字）。
    pub(crate) loop_wake: Arc<(std::sync::Mutex<()>, std::sync::Condvar)>,
    /// winit 事件循环代理：用于在 `ControlFlow::Wait` 下唤醒事件循环。
    /// 渲染线程通过 `create_tx` / `close_tx` / `cb_tx` 等通道发消息给 winit 线程后，
    /// 调用 `proxy.send_event(())` 唤醒事件循环，使其重新进入 `about_to_wait` 排空所有通道。
    /// `None` 仅在构造早期（`run_blocking` 创建 EventLoop 前）短暂存在。
    /// `EventLoopProxy` 是 `Send + Sync`，设置后永不变更，用 `OnceLock` 无需 `Mutex`。
    pub(crate) event_loop_proxy: OnceLock<winit::event_loop::EventLoopProxy<()>>,
}

pub struct App {
    pub(crate) inner: Arc<AppInner>,
}

impl Clone for App {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl std::ops::Deref for App {
    type Target = AppInner;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// 延迟执行的任务，由 [`LoopContext::after_ticks`] / [`LoopContext::after_secs`] 注册。
pub struct DeferredTask {
    kind: DeferredTaskKind,
    pub(crate) f: Box<dyn FnOnce() + Send>,
}

/// 在 `LoopContext::after_ticks` / `LoopContext::after_secs` 内部使用。
/// 用户不直接构造。
#[doc(hidden)]
pub struct DeferredTaskGuard;

impl DeferredTask {
    pub(crate) fn new(kind: DeferredTaskKind, f: Box<dyn FnOnce() + Send>) -> Self {
        DeferredTask { kind, f }
    }

    pub(crate) fn is_ready(&self, tick_count: u64) -> bool {
        match &self.kind {
            DeferredTaskKind::AfterTicks(target) => tick_count >= *target,
            DeferredTaskKind::AfterSecs(wakeup) => std::time::Instant::now() >= *wakeup,
        }
    }
}

#[cfg(test)]
impl DeferredTask {
    /// 仅测试用：构造一个 `after_ticks(target)` 等价任务（空闭包）。
    pub(crate) fn for_frames(target: u64) -> Self {
        DeferredTask {
            kind: DeferredTaskKind::AfterTicks(target),
            f: Box::new(|| {}),
        }
    }
}

pub(crate) enum DeferredTaskKind {
    AfterTicks(u64),
    AfterSecs(std::time::Instant),
}

/// spawn 创建 loop 线程的顺序计数器：默认线程名 `vireo-thread-{n}` 的 n 取此值
/// （进程内单调递增，取决于 spawn 创建顺序，从 0 开始）。
static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);

impl App {
    /// 进程入口：在 **OS 主线程**构造 `App`（同时初始化 GPU），把用户 future `main`
    /// 放到独立的 `vireo-main` 线程由 `pollster::block_on` 驱动，随后在本线程（OS 主线程）
    /// 跑 winit `EventLoop`（满足 winit 线程要求，含 macOS）。`main` 接收 `App` 自身，
    /// 典型写法是 `App::new(|app| async move { app.run(...); })`；`#[vireo::main]`
    /// 宏即此写法的糖。
    ///
    /// `App` 在 OS 主线程直接构造并交由本调用方，无需任何跨线程移交通道。
    pub fn new<F, Fut>(main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        App::from_instance_descriptor(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
            .run_entry(main)
    }

    /// 用 wgpu `InstanceDescriptor` 构造 App 并作为进程入口（同 [`App::new`]，但显式指定
    /// 后端/flags/内存预算/backend options/display，压过 `WGPU_BACKEND` 等环境变量）。
    ///
    /// `display` 原样透传：vireo 用窗口自身 handle 创建 surface（create_surface），
    /// 若 display 与窗口 handle 所属显示服务器不一致会触发 wgpu 校验错误
    /// `MismatchingDisplayHandle`；非 GLES（Wayland）后端通常传 `None` 即可。
    /// `#[vireo::main(descriptor = EXPR)]` 即调用本方法。
    pub fn with_descriptor<F, Fut>(desc: wgpu::InstanceDescriptor, main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        App::from_instance_descriptor(desc).run_entry(main)
    }

    /// 进程入口宿主：OS 主线程建通道、把 sender 写到 `AppInner`、spawn 用户闭包到 `vireo-main`，
    /// 随后在本线程跑 winit `EventLoop`（`run_blocking`）。把原来 `new`/`with_descriptor` 重复的
    /// 闭包封装 + spawn 逻辑收敛到此一处。
    fn run_entry<F, Fut>(self, main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let channels = self.init_run_channels();
        let app_for_thread = self.clone();
        let app_inner_for_flag = app_for_thread.inner.clone();
        std::thread::Builder::new()
            .name("vireo-main".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pollster::block_on(main(app_for_thread));
                }));
                if let Err(payload) = result {
                    let msg = if let Some(s) = payload.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "<non-string panic payload>".to_string()
                    };
                    eprintln!("[vireo] user main closure panicked: {}", msg);
                }
                // `main` future 已完成 → 置位顶层生命周期权威信号，允许 supervisor 在
                // 所有 loop/窗口 settle 后退出（覆盖零窗口零 loop 场景，且不引入启动竞态）。
                app_inner_for_flag
                    .main_done
                    .store(true, Ordering::Release);
                // 唤醒阻塞在 `rx.recv()` 的 supervisor 重新判定退出（事件驱动，取代固定间隔 sleep）。
                if let Some(tx) = app_inner_for_flag.event_tx.lock().clone() {
                    let _ = tx.send(WinitEvent::Wake);
                }
            })
            .expect("failed to spawn vireo-main thread");
        if let Err(e) = self.run_blocking(channels) {
            eprintln!("[vireo] app exited with error: {:?}", e);
        }
    }

    /// 入口期创建所有通道，并在 spawn 用户闭包**前**把 `create_tx`/`cb_tx` 写到 `AppInner`。
    /// 这样 `App::window` 与 `App::on_*` 在闭包运行前即可经通道投递（取代旧的注册握手）。
    fn init_run_channels(&self) -> RunChannels {
        let (event_tx, event_rx) = mpsc::channel();
        let supervisor_event_tx = event_tx.clone();
        let (cb_tx, cb_rx) = mpsc::channel();
        let (exit_tx, exit_rx) = mpsc::channel();
        let (frame_style_tx, frame_style_rx) = mpsc::channel();
        let (aspect_ratio_tx, aspect_ratio_rx) = mpsc::channel();
        let (nc_tx, nc_rx) = mpsc::channel();
        let (create_tx, create_rx) = mpsc::channel();
        let (close_tx, close_rx) = mpsc::channel();
        *self.create_tx.lock() = Some(create_tx);
        *self.cb_tx.lock() = Some(cb_tx);
        *self.event_tx.lock() = Some(event_tx.clone());
        RunChannels {
            event_tx,
            event_rx,
            cb_rx,
            exit_rx,
            exit_tx,
            frame_style_rx,
            aspect_ratio_rx,
            nc_rx,
            create_rx,
            close_rx,
            supervisor_event_tx,
            frame_style_tx,
            aspect_ratio_tx,
            nc_tx,
            close_tx,
        }
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
        let app = Self {
            inner: Arc::new(AppInner {
                windows: Mutex::new(Vec::new()),
                alive_window_count: AtomicUsize::new(0),
                created_window_count: AtomicUsize::new(0),
                gpu,
                instance: Mutex::new(Some(instance)),
                device_lost,
                handle_to_id: Mutex::new(FxHashMap::default()),
                next_handle: Mutex::new(0),
                default_icon: Mutex::new(default_icon),
                textures: Mutex::new(Vec::new()),
                offscreens: Mutex::new(Vec::new()),
                init_duration,
                max_fps: Mutex::new(Some(240)),
                drag_cap: Mutex::new(true),
                create_tx: Mutex::new(None),
                cb_tx: Mutex::new(None),
                loop_states: Mutex::new(Vec::new()),
                windows_ready: AtomicBool::new(false),
                pending_window_creates: AtomicUsize::new(0),
                loops_ever_requested: AtomicBool::new(false),
                main_done: AtomicBool::new(false),
                event_tx: Mutex::new(None),
                loop_wake: Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new())),
                event_loop_proxy: OnceLock::new(),
            }),
        };

        app
    }

    /// 创建离屏画布。与 window() 对称，可在 run() 之前调用。
    /// 同步预热 AA 对应的 SDF + geo 管线，构造耗时由 `OffscreenCanvas::init_duration()` 暴露。
    pub fn offscreen(&self, width: u32, height: u32, aa: AntiAliasing) -> OffscreenIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(aa, self.gpu.supported_sample_counts());
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        let mut guard = self.offscreens.lock();
        let idx = guard.len();
        let mut offscreen = OffscreenCanvas::with_aa(&self.gpu, width, height, aa, init_duration);
        offscreen.index = OffscreenIndex(idx);
        guard.push(Arc::new(offscreen));
        OffscreenIndex(idx)
    }

    /// 根据索引获取离屏画布引用。
    ///
    /// 返回 `Err` 表示索引无效或离屏画布已释放（例如窗口关闭时关联的离屏资源被清理）。
    /// 调用方应处理 `Err`（例如在 `on_tick` 中 `return false`），而不是 `.unwrap()`。
    pub fn offscreen_ref(&self, idx: &OffscreenIndex) -> Result<Arc<OffscreenCanvas>, VireoError> {
        match self.offscreens.lock().get(idx.0).cloned() {
            Some(c) => Ok(c),
            None => Err(VireoError::OffscreenNotFound(idx.0)),
        }
    }

    /// 从文件加载纹理（存储在 App 中管理生命周期），返回纹理索引。
    /// 读取或解码失败时会打印错误并返回一个"missing"棋盘纹理（不返回 Err）。
    pub fn load_texture(&self, path: impl AsRef<std::path::Path>) -> usize {
        let tex = Texture::from_file(path, &self.gpu);
        let mut guard = self.textures.lock();
        let idx = guard.len();
        guard.push(Arc::new(tex));
        idx
    }

    /// 根据索引获取已加载的纹理。
    ///
    /// 返回 `Err` 表示索引越界或贴图尚未加载完成。调用方应处理 `Err`，而不是 `.unwrap()`。
    pub fn texture(&self, index: usize) -> Result<Arc<Texture>, VireoError> {
        match self.textures.lock().get(index).cloned() {
            Some(t) => Ok(t),
            None => Err(VireoError::TextureNotFound(index)),
        }
    }

    /// 唤醒 winit 事件循环（`ControlFlow::Wait` 下）。
    ///
    /// 渲染线程通过 mpsc 通道（`create_tx`/`close_tx`/`cb_tx` 等）发消息给 winit 线程后调用，
    /// 使事件循环从 `Wait` 中醒来、重新进入 `about_to_wait` 排空所有通道。
    /// `EventLoopProxy` 在 `run_blocking` 创建 `EventLoop` 后设置，此后恒为 `Some`。
    /// 在 `run_blocking` 之前调用（预注册窗口）时 proxy 尚为 `None`，但此时事件循环未启动，
    /// 消息已在通道中，`resumed` + 首次 `about_to_wait` 会立即 drain。
    fn wake_event_loop(&self) {
        if let Some(proxy) = self.event_loop_proxy.get() {
            let _ = proxy.send_event(());
        }
    }

    /// 配置一个待创建的窗口。可选 on_close 钩子在窗口被关闭时调用。
    /// 同步预热窗口 AA 对应的 SDF + geo 管线，并把 AA clamp 到硬件上限（避免 wgpu panic）。
    /// 构造耗时在 `App::run` 创建窗口后由 `VireoWindow::init_duration()` 暴露。
    ///
    /// 始终经 `create_tx` 内部通道发给 winit 线程异步创建；`run` 之前（`#[vireo::main]`
    /// 注入的 `main` 闭包里）或 `run` 之后（on_tick 里）调用都走同一通道（通道在
    /// `run_entry` 的 `init_run_channels` 中建立，`create_tx` 恒为 `Some`）。
    /// 创建完成前 `App::window_ref` 返回 `None`。
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
        let handle = *self.next_handle.lock();
        *self.next_handle.lock() = handle + 1;
        let on_close = on_close.map(|f| Box::new(f) as Box<dyn FnOnce() + Send>);
        // 始终经通道发给 winit 线程异步创建（预注册与运行期统一）。
        // 计数 +1：窗口已注册但 `WindowCreated` 尚未到达，loop 不应因暂未建出而误退
        // （`WindowCreated` 处理器统一 `fetch_sub`）。
        // `create_tx` 在 `run_entry` 的 `init_run_channels` 中已建立，此处恒为 `Some`
        // （旧「预注册」写法的 `None` 分支已不可达，见 `unreachable!`）。
        match self.create_tx.lock().as_ref().cloned() {
            Some(tx) => {
                let _ = tx.send(CreateWindowRequest {
                    handle,
                    desc,
                    init_duration,
                    on_close,
                });
                self.pending_window_creates
                    .fetch_add(1, Ordering::AcqRel);
                // 唤醒 winit 事件循环：`ControlFlow::Wait` 下它可能阻塞在 `about_to_wait`，
                // 不会主动 drain `create_rx`；proxy.send_event 触发 `user_event`→`about_to_wait`
                // 排空通道，在本线程完成窗口创建。
                self.wake_event_loop();
            }
            None => unreachable!(
                "vireo: create_tx is always Some after run_entry/init_run_channels; \
                 App::window is only reachable with create_tx set"
            ),
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

    /// 启动事件循环 + 渲染线程（经典单循环入口，向后兼容）。
    ///
    /// 非阻塞入口（薄包装）：内部 `spawn` 一条含单个 [`Loop`] 的 [`Thread`] 并立即返回
    /// [`ThreadHandle`]。返回的句柄在析构时 join 渲染线程（见 [`ThreadHandle`]），因此
    /// `app.run(on_tick)` 作为末语句仍会阻塞到窗口关闭。闭包签名 `FnMut(&mut LoopContext) -> bool`，
    /// 返回 `true` 继续循环、`false` 退出；帧计数 / FPS 等 loop 环境状态在 [`LoopContext`] 上。
    ///
    /// 每条 `run`/`spawn`/`loops` 在独立 OS 线程上驱动（见 `spawn`）。
    pub fn run<F: FnMut(&mut crate::thread::LoopContext) -> bool + Send + 'static>(
        &self,
        on_tick: F,
    ) -> crate::thread::ThreadHandle {
        self.spawn(crate::thread::Thread::new().with_loop(crate::thread::Loop::new(on_tick)))
    }

    /// 主入口：推一个显式 [`Thread`]（`&self` 可多次调用，真并行）。
    ///
    /// 在独立线程上启动 winit owner + 渲染线程，立即返回 [`ThreadHandle`]；句柄实现 `Future`，
    /// 调用方 `.await` 即可等待本组所有循环结束并取回结果（错误同样经 `.await` 返回），析构时
    /// 亦会 join 渲染线程（见 [`ThreadHandle`]）。与 [`App::run`] / [`App::loops`] 共用同一套渲染机件；
    /// 每个 [`Loop`] 拥有独立的 [`LoopContext`]（帧计数 / 延迟任务 / FPS 统计，即本 Thread 的
    /// tick 速率读数）；tick 速率上限由 [`Thread::max_tps`] 决定（以 [`App::max_fps`] 作默认值种子）。
    pub fn spawn(&self, thread: crate::thread::Thread) -> crate::thread::ThreadHandle {
        let loops = thread.loops;
        let shared = Arc::new(parking_lot::Mutex::new(Vec::<crate::thread::Loop>::new()));
        let fps_stats = Arc::new(parking_lot::Mutex::new(crate::thread::FpsStats::new()));
        let state = Arc::new(crate::thread::LoopHandleState::new());
        self.loops_ever_requested
            .store(true, Ordering::Release);
        self.loop_states.lock().push(state.clone());
        let app = self.clone();
        let device_lost = self.device_lost.clone();
        let state_for_thread = state.clone();
        let shared_for_thread = shared.clone();
        let fps_for_thread = fps_stats.clone();
        let device_lost_for_thread = device_lost.clone();
        // 本 Thread 的 tick 上限：用户显式 `with_max_tps` 优先，否则以 `App::max_fps` 作默认值种子
        // （运行期改 `App::max_fps` 不影响已 spawn 的 Thread，与逐窗口 `max_fps` 同构）。
        let thread_tps = thread.max_tps.or(app.max_fps());
        let thread_name = thread.name.clone().unwrap_or_else(|| {
            let n = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
            format!("vireo-thread-{n}")
        });
        let join = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                crate::thread::run_thread_loop(
                    app,
                    loops,
                    state_for_thread,
                    fps_for_thread,
                    shared_for_thread,
                    device_lost_for_thread,
                    thread_tps,
                );
            })
            .expect("failed to spawn vireo thread");
        crate::thread::ThreadHandle { state, thread: Some(join), shared, fps_stats }
    }

    /// 糖：`spawn(Thread::new().with_loops(loops))`
    pub fn loops(&self, loops: impl IntoIterator<Item = crate::thread::Loop>) -> crate::thread::ThreadHandle {
        self.spawn(crate::thread::Thread::new().with_loops(loops))
    }

    /// 启动渲染循环（内部实现）。由 OS 主线程经 [`App::new`] / [`App::with_descriptor`]
    /// 调用：winit `EventLoop` 在 **OS 主线程**构造并运行（满足 winit 线程要求，含 macOS）；
    /// 另起 supervisor 线程集中应用事件并判定退出；每条 `spawn` 注册的循环在**独立 OS 线程**
    /// 上驱动。事件应用与渲染解耦，多循环真并行。
    ///
    /// winit `EventLoop` 在 **OS 主线程**（即 `#[vireo::main]` 生成的 `fn main` 所在线程）
    /// 构造，满足 winit 的线程要求（含 macOS），无需 `any_thread` 逃逸口。
    fn run_blocking(
        self,
        channels: RunChannels,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // winit 事件循环由 OS 主线程运行的入口构造，满足线程要求（含 macOS）；无需 `any_thread`。
        let event_loop = EventLoop::new().unwrap();
        // 创建 EventLoopProxy：渲染线程通过它在 `ControlFlow::Wait` 下唤醒事件循环，
        // 使其重新进入 `about_to_wait` 排空所有通道（create_rx/close_rx/cb_rx 等）。
        // 需要在 `run_app` 之前设置，保证渲染线程 condvar 唤醒后能立即使用。
        let _ = self.event_loop_proxy.set(event_loop.create_proxy());

        let default_icon = self.default_icon.lock().take();
        // 保留 self.instance（渲染线程重建 surface 需要）；Runner 拿 clone。
        let instance = self.instance.lock().clone().expect("instance already taken");
        self.handle_to_id.lock().clear();
        self.windows.lock().clear();

        // 窗口创建现在统一经 `create_rx` 通道（预注册与运行期不再分两条路径），
        // 故没有「预期窗口数」：置位即放行 loop 线程（真正防误退靠 `pending_window_creates`）。
        let expected_windows = 0usize;
        self.windows_ready.store(true, Ordering::Release);
        // Clone GpuContext Arc for Runner（winit 线程只在创建窗口时用 device/queue 初始化 surface）
        let gpu_for_runner = self.gpu.clone();
        let device_lost = self.device_lost.clone();
        let device_lost_super = device_lost.clone();
        // supervisor 线程：集中应用事件 + 判定退出；持有 `self`（App）以访问 windows / loop_states。
        let supervisor = std::thread::Builder::new()
            .name("vireo-supervisor".into())
            .spawn(move || {
                supervisor_loop(
                    self,
                    channels.event_rx,
                    channels.supervisor_event_tx,
                    channels.frame_style_tx,
                    channels.aspect_ratio_tx,
                    channels.nc_tx,
                    channels.close_tx,
                    channels.exit_tx,
                    expected_windows,
                    device_lost_super,
                )
            })
            .expect("failed to spawn supervisor thread");

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
            /// 接收渲染线程发来的运行期窗口创建请求（on_tick 里 `App::window`）
            create_rx: mpsc::Receiver<CreateWindowRequest>,
            /// 接收渲染线程发来的程序化关窗请求（`VireoWindow::close`；载荷 = handle）。
            close_rx: mpsc::Receiver<usize>,
            /// 已创建窗口的 hwnd（按 handle 索引），窗口关闭时用于清理 NC 状态。
            hwnds: Vec<isize>,
            id_to_handle: FxHashMap<WindowId, usize>,
            close_hooks: FxHashMap<u64, Option<Box<dyn FnOnce() + Send>>>,
            window_callbacks: Vec<crate::input::InputCallbacks>,
            default_icon: Option<Icon>,
            instance: wgpu::Instance,
            /// 用于 winit 线程创建/初始化 surface（后续帧循环全在渲染线程）
            gpu: Arc<GpuContext>,
            created: bool,
            /// macOS 等需 `Resumed` 后才能 `create_window`；在 `resumed` 后置位，
            /// `about_to_wait` 仅在此为 true 后 drain `create_rx`（安全）。Windows/Linux 上为
            /// 纯预防性门控（`resumed` 必然先于 `about_to_wait`）。
            resumed_fired: bool,
        }

        // 辅助：从 Runner 获取 handle（panic-safe）
        impl Runner {
            fn handle_for(&self, window_id: WindowId) -> Option<usize> {
                self.id_to_handle.get(&window_id).copied()
            }

            fn send(&self, event: WinitEvent) {
                let _ = self.event_tx.send(event);
            }

            /// 完整关窗路径：close_hooks / NC 状态清理 / 发 `WinitEvent::CloseRequested`。
            /// 用户点关闭按钮与 `VireoWindow::close`（经 close_rx drain）共用。关窗幂等由
            /// `apply_winit_event_one` 的 `CloseRequested` 处理（置 `closing` + 置 None）保证，
            /// 重复调用（点 X + 程序化 `close()`）不会 panic。
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
                // 不再在此执行**——Vulkan 后端（wgpu 默认）的 configure 会等 present queue
                // 排空（阻塞可达几十 ms；DX12 同操作阻塞显著更低），若在 winit 线程同步执行，
                // 会卡住整个事件循环（所有窗口的输入/事件
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
                self.resumed_fired = true;
                // 预注册窗口现在也经 `create_rx` 通道投递（`App::window` 在用户闭包内
                // 已于 `resumed` 前把请求发到通道）；这里先 drain 一次让它们立即建出，
                // 之后 `about_to_wait` 仍会在 `resumed_fired` 后继续 drain 运行期请求。
                while let Ok(req) = self.create_rx.try_recv() {
                    self.create_window(
                        event_loop,
                        req.handle as usize,
                        &req.desc,
                        req.init_duration,
                        req.on_close,
                    );
                }
            }

            fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
                // 渲染线程请求终止（on_tick 返回 false / 设备丢失）
                let mut exit_requested = false;
                while self.exit_rx.try_recv().is_ok() {
                    exit_requested = true;
                }
                // 运行期窗口创建：drain 渲染线程（on_tick 里 `App::window`）发来的请求，
                // 在本线程（winit 事件线程）创建窗口。必须在 `cb_rx` **之前** drain——
                // 这样同一迭代内 App 级 `on_*` 回调（经 `cb_tx` 通道）落到已扩容的
                // `window_callbacks[handle]`，不会被丢弃。macOS 仅 `resumed` 之后允许
                // `create_window`，故以 `resumed_fired` 门控（Windows/Linux 上恒 true）。
                if self.resumed_fired {
                    while let Ok(req) = self.create_rx.try_recv() {
                        self.create_window(
                            event_loop,
                            req.handle as usize,
                            &req.desc,
                            req.init_duration,
                            req.on_close,
                        );
                    }
                }
                // Drain callback registrations sent from render thread.
                // 注意顺序：窗口创建（上方）先于回调合并，确保 window_callbacks[handle] 已 resize。
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
                        // 窗口已存在（hwnd 已登记）时，合并后安装任务栏缩略图按钮回调
                        // （Windows 专用；预注册窗口的 App 级回调经上方 create_rx 创建窗口时尚
                        // 未合并，故此处补齐安装，避免丢失）。
                        if (handle as usize) < self.hwnds.len() && self.hwnds[handle as usize] != 0 {
                            for cb in std::mem::take(&mut reg.on_thumb_button) {
                                crate::platform::windows::set_thumbar_callback(
                                    self.hwnds[handle as usize],
                                    cb,
                                );
                            }
                        }
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
                // 程序化关窗：`VireoWindow::close` 发来的 handle，走与用户点关闭
                // 按钮相同的完整关窗路径（close_hooks / NC 清理 / 退出判定）。
                while let Ok(handle) = self.close_rx.try_recv() {
                    self.request_close(handle);
                }
                if exit_requested {
                    // 必须在 `set_control_flow(Wait)` 之前短路：否则 Wait 会覆盖 `Exit`，
                    // 导致 supervisor 已发出退出信号、事件循环却永不退出（进程残留）。
                    event_loop.exit();
                } else {
                    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
                }
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
            event_tx: channels.event_tx,
            cb_rx: channels.cb_rx,
            exit_rx: channels.exit_rx,
            frame_style_rx: channels.frame_style_rx,
            aspect_ratio_rx: channels.aspect_ratio_rx,
            nc_rx: channels.nc_rx,
            create_rx: channels.create_rx,
            close_rx: channels.close_rx,
            hwnds: Vec::new(),
            id_to_handle: FxHashMap::default(),
            close_hooks: FxHashMap::default(),
            window_callbacks: Vec::new(),
            default_icon,
            instance,
            gpu: gpu_for_runner,
            created: false,
            resumed_fired: false,
        }).unwrap();

        // Winit loop 结束后等待 supervisor 线程退出。
        match supervisor.join() {
            Ok(()) => Ok(()),
            Err(payload) => Err(panic_payload_to_string(payload).into()),
        }
    }
}

/// 将 `catch_unwind` 捕获的 panic payload 转成可读字符串。
pub(crate) fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// 应用单个 winit 事件到 App 状态（free function，供 supervisor 线程调用）。
/// 这是 supervisor 线程事件处理分支，语义与原渲染帧循环逐位一致，
/// 仅把 `app` 与各 channel 改为参数传入。
fn apply_winit_event_one(
    app: &App,
    event: WinitEvent,
    event_tx: &mpsc::Sender<WinitEvent>,
    frame_style_tx: &mpsc::Sender<(isize, FrameStyle)>,
    aspect_ratio_tx: &mpsc::Sender<(isize, Option<f64>)>,
    nc_tx: &mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    close_tx: &mpsc::Sender<usize>,
) {
    match event {
        WinitEvent::WindowCreated {
            handle,
            window,
            surface,
            surface_config,
            renderer,
            dpi_scale,
            dpi_override,
            init_duration,
            frame_style,
            pending_show,
        } => {
            let vw = VireoWindow::new(
                window,
                app.gpu.clone(),
                surface,
                app.instance.lock().clone().expect("instance available"),
                surface_config,
                renderer,
                dpi_scale,
                dpi_override,
                init_duration,
                frame_style,
                pending_show,
                event_tx.clone(),
                app.inner
                    .cb_tx
                    .lock()
                    .clone()
                    .unwrap_or_else(|| {
                        unreachable!(
                            "vireo: cb_tx is always Some after run_entry/init_run_channels; \
                             App::window is only reachable with cb_tx set"
                        )
                    }),
                nc_tx.clone(),
                close_tx.clone(),
                app.event_loop_proxy.get().cloned(),
                handle,
            );
            vw.set_max_fps(app.max_fps());
            vw.set_drag_cap(app.drag_cap());
            while app.windows.lock().len() <= handle {
                app.windows.lock().push(None);
            }
            app.windows.lock()[handle] = Some(Arc::new(vw));
            app.alive_window_count
                .fetch_add(1, Ordering::AcqRel);
            app.created_window_count
                .fetch_add(1, Ordering::AcqRel);
        }

        WinitEvent::Resized { handle, width, height } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.resize(width, height);
            }
        }

        WinitEvent::ScaleFactorChanged { handle, scale: _scale } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                let size = win.inner.inner_size();
                win.resize(size.width, size.height);
            }
        }

        WinitEvent::CursorMoved { handle, x, y } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::CursorMoved { handle, x, y });
            }
        }

        WinitEvent::KeyboardInput { handle, event } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::KeyboardInput { handle, event });
            }
        }

        WinitEvent::MouseInput { handle, button, pressed } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::MouseInput { handle, button, pressed });
            }
        }

        WinitEvent::MouseWheel { handle, delta } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::MouseWheel { handle, delta });
            }
        }

        WinitEvent::ModifiersChanged { handle, modifiers } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::ModifiersChanged { handle, modifiers });
            }
        }

        WinitEvent::Focused { handle, focused } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::Focused { handle, focused });
            }
        }

        WinitEvent::CursorEntered { handle } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::CursorEntered { handle });
            }
        }

        WinitEvent::CursorLeft { handle } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::CursorLeft { handle });
            }
        }

        WinitEvent::Touch { handle, event } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::Touch { handle, event });
            }
        }

        WinitEvent::CloseRequested { handle, .. } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                *win.closing.lock() = true;
                // 等渲染线程当前帧结束（已 present、不再持有 SurfaceTexture）再释放 Surface，
                // 避免 in-flight draw 与 Surface drop 竞态导致 wgpu 校验 panic。事件驱动：渲染线程
                // 每帧绘制完成时 `notify_all`；其正常结束或 panic退出时也会对所有窗口置位并唤醒，
                // 故无任何忙等、无需超时魔法数字（最坏情况下渲染线程已死，draw_idle 必为真，不会挂起）。
                {
                    let cv = &win.draw_idle_cv;
                    let mut guard = cv.0.lock().unwrap();
                    while !win.draw_idle.load(Ordering::Acquire) {
                        guard = cv.1.wait(guard).unwrap();
                    }
                }
                // 置 closing 后再 drop：此时无 outstanding SurfaceTexture，drop surface 安全。
            }
            if let Some(w) = app.windows.lock().get_mut(handle) {
                *w = None;
                app.alive_window_count
                    .fetch_sub(1, Ordering::AcqRel);
            }
        }

        // Winit 窗口操作：转发到正确的窗口
        WinitEvent::SetTitle { handle, title } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_title(&title);
            }
        }
        WinitEvent::SetSize { handle, size } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                let _ = win.inner.request_inner_size(size);
            }
        }
        WinitEvent::SetMinSize { handle, size } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_min_inner_size(size);
            }
        }
        WinitEvent::SetMaxSize { handle, size } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_max_inner_size(size);
            }
        }
        WinitEvent::SetFullscreen { handle, fullscreen } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_fullscreen(fullscreen);
            }
        }
        WinitEvent::SetMaximized { handle, maximized } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_maximized(maximized);
            }
        }
        WinitEvent::SetMinimized { handle, minimized } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_minimized(minimized);
            }
        }
        WinitEvent::SetVisible { handle, visible } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_visible(visible);
            }
        }
        WinitEvent::FocusWindow { handle } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.focus_window();
            }
        }
        WinitEvent::SetWindowLevel { handle, level } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_window_level(level);
            }
        }
        WinitEvent::SetFrameStyle { handle, style } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                *win.frame_style.lock() = style;
                win.inner.set_decorations(style.decorated());
                // SetWindowSubclass / RemoveWindowSubclass 必须在 winit
                // 事件线程调用（见 platform::windows::install 注释），
                // 这里仅转发到 winit 线程，由 Runner::about_to_wait 执行。
                if let Some(hwnd) = win_hwnd(&win.inner) {
                    let _ = frame_style_tx.send((hwnd, style));
                    // 唤醒 winit 事件循环：frame_style 由 Runner::about_to_wait drain，
                    // Wait 模式下需主动唤醒。
                    app.wake_event_loop();
                }
            }
        }
        WinitEvent::SetAspectRatio { handle, ratio } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                if let Some(hwnd) = win_hwnd(&win.inner) {
                    let _ = aspect_ratio_tx.send((hwnd, ratio));
                    app.wake_event_loop();
                }
            }
        }
        WinitEvent::SetIcon { handle, icon } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_window_icon(Some(icon));
            }
        }
        WinitEvent::SetCursor { handle, cursor } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_cursor(cursor);
            }
        }
        // 内部唤醒哨兵：supervisor 阻塞在 `rx.recv()` 时收到后仅重新判定退出条件（无业务动作）。
        WinitEvent::Wake => {}
    }
}

/// supervisor 线程主循环：集中应用事件 + 判定退出。
/// 与 winit 线程解耦——winit 只发 `WinitEvent`，本线程 drain 并应用到 `App` 状态；
/// 当所有 `loop_states` 结束（或设备丢失）时通知 winit 线程退出事件循环。
fn supervisor_loop(
    app: App,
    rx: mpsc::Receiver<WinitEvent>,
    event_tx: mpsc::Sender<WinitEvent>,
    frame_style_tx: mpsc::Sender<(isize, FrameStyle)>,
    aspect_ratio_tx: mpsc::Sender<(isize, Option<f64>)>,
    nc_tx: mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    close_tx: mpsc::Sender<usize>,
    exit_tx: mpsc::Sender<()>,
    expected_windows: usize,
    device_lost: Arc<AtomicBool>,
) {
    let mut created_windows = 0usize;
    loop {
        let mut processed = false;
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    // 先应用事件（把窗口写入 `app.windows`），再递减待创建计数 / 递增已创建计数——
                    // 保证 window 真实存在后才解除「待创建」状态。
                    let is_created = matches!(ev, WinitEvent::WindowCreated { .. });
                    apply_winit_event_one(
                        &app, ev, &event_tx, &frame_style_tx, &aspect_ratio_tx, &nc_tx, &close_tx,
                    );
                    if is_created {
                        created_windows += 1;
                        app.pending_window_creates
                            .fetch_sub(1, Ordering::AcqRel);
                        // 唤醒等待运行期建窗完成的渲染线程（事件驱动，取代固定间隔 sleep）。
                        app.inner.loop_wake.1.notify_all();
                    }
                    processed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        // 放开 loop 线程：已创建窗口数 `>=` 预期数即放行（含预期 0 的纯运行期 / 零窗口场景）。
        // 「窗口已注册但未建出」的误退防护由 `pending_window_creates` 承担，无需在此卡。
        if created_windows >= expected_windows {
            app.windows_ready
                .store(true, Ordering::Release);
            // 唤醒等待窗口就绪的渲染线程（事件驱动，取代 yield_now 自旋）。
            app.inner.loop_wake.1.notify_all();
        }
        // 退出判定（顶层权威 = vireo-main 的 `main` future 完成，见 `main_done`）：
        // (a) `main_done && all_loops_done && windows_settled` —— 用户代码已结束、无 loop 在跑、
        //     无窗口在途/存活即退出（覆盖零窗口零 loop 的 `main` 立即返回场景；旧 `app_started`
        //     守卫在此会挂死）；
        // (b) `all_windows_closed` —— 至少一窗口曾创建、现已全关（不依赖 loop 返回 false）；
        // (c) `device_lost`。
        // `loops_ever_requested` 仅用于 (a) 的 `all_loops_done`：
        // - 已请求：必须等 `loop_states` 非空且全部 `done`（避免 `spawn` 尚未把状态推入前的竞态早退）；
        // - 从未请求：循环计数视为「已完成」。
        let all_loops_done = {
            let loops_requested =
                app.loops_ever_requested.load(Ordering::Acquire);
            if loops_requested {
                let states = app.loop_states.lock();
                !states.is_empty()
                    && states
                        .iter()
                        .all(|s| s.done.load(Ordering::Acquire))
            } else {
                true
            }
        };
        let windows_settled = created_windows >= expected_windows
            && app.window_count() == 0
            && app.pending_window_creates.load(Ordering::Acquire) == 0;
        // 所有窗口已关闭（至少一个曾被创建、现无存活窗口、无在途建窗）即退出，不再强依赖
        // loop 线程返回 false：关窗那一帧 loop 可能在正在销毁的窗口上 `get_current_texture`
        // 阻塞（draw_frame 的 closing 早退无法覆盖已进入的 in-flight acquire），进程退出时
        // 该线程被强杀，无需等待其 `done`。零窗口 / 纯运行期建窗场景 `created_windows==0`
        // 不触发本分支，避免启动即退出。
        let all_windows_closed =
            created_windows > 0 && app.window_count() == 0 && app.pending_window_creates.load(Ordering::Acquire) == 0;
        if (app.inner
            .main_done
            .load(Ordering::Acquire)
            && all_loops_done
            && windows_settled)
            || all_windows_closed
            || device_lost.load(Ordering::Acquire)
        {
            let _ = exit_tx.send(());
            app.wake_event_loop();
            return;
        }
        // 仅当本轮无任何事件时阻塞在 `rx.recv()`，等待下一个真实事件或 `WinitEvent::Wake`
        // （`main_done` / 设备丢失 / loop 完成由相应置位点发 `Wake` 唤醒）。事件驱动、无忙等、无魔法数字。
        if !processed {
            match rx.recv() {
                Ok(ev) => {
                    let is_created = matches!(ev, WinitEvent::WindowCreated { .. });
                    apply_winit_event_one(
                        &app, ev, &event_tx, &frame_style_tx, &aspect_ratio_tx, &nc_tx, &close_tx,
                    );
                    if is_created {
                        created_windows += 1;
                        app.pending_window_creates
                            .fetch_sub(1, Ordering::AcqRel);
                        app.inner.loop_wake.1.notify_all();
                    }
                }
                Err(_) => return,
            }
        }
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

    /// 根据索引获取窗口引用。
    ///
    /// 返回 `Err` 表示窗口已关闭或索引无效。调用方应处理 `Err`（例如在 `on_tick` 中
    /// `return false`），而不是 `.unwrap()`——否则窗口关闭会让整个渲染线程 panic。
    pub fn window_ref(&self, idx: &WindowIndex) -> Result<Arc<VireoWindow>, VireoError> {
        match self.windows.lock().get(idx.0 as usize).and_then(|w| w.clone()) {
            Some(w) => Ok(w),
            None => Err(VireoError::WindowNotFound(idx.0)),
        }
    }

    /// 存活窗口数量（O(1)，读 `alive_window_count` 原子量；`windows` 历史表只增不缩）。
    pub fn window_count(&self) -> usize {
        self.alive_window_count
            .load(Ordering::Acquire)
    }

    /// 累计已创建窗口数（O(1)，读 `created_window_count` 原子量；只增不缩）。
    /// 渲染线程据此判定「曾创建且现已全部关闭」，独立于 `on_tick` 返回值退出。
    pub(crate) fn created_window_count(&self) -> usize {
        self.created_window_count
            .load(Ordering::Acquire)
    }

/// App::new 内部耗时（秒）：GPU 设备、shader 模块、bind group layout 构造。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    /// 设置帧率上限的**默认值**。`Some(n)` 在有 vsync 阻塞时基本不生效（acquire 自然
    /// 锁到刷新率），仅在 acquire 不阻塞（拖动拉伸、`Immediate`、后台等）时用
    /// sleep 把 CPU 循环拉回约 n fps，避免空转烧 CPU。`None` 不限制。
    /// 默认 `Some(240)`——给足余量，正常 vsync 下 cap 不生效，仅空转时兜底。
    /// 此值仅作为默认值：通过 `App::window` 或在 `run` 回调内动态创建的窗口，在创建时
    /// 读取它；**已存在的窗口不受影响**。要改某个窗口的上限，用该 `VireoWindow::set_max_fps`。
    /// `&self` 即可，可在 `run` 回调内随时切换。
    pub fn set_max_fps(&self, fps: Option<u32>) {
        *self.max_fps.lock() = fps;
    }

    /// 当前帧率上限（`App::set_max_fps` 所设）。
    pub fn max_fps(&self) -> Option<u32> {
        *self.max_fps.lock()
    }

    /// 设置「拖动期帧率上限」独立开关的**默认值**。与 `set_max_fps` **解耦**：开启（默认）时，
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
    /// 此值仅作为默认值，仅作用于之后新建的窗口；已存在的窗口不受影响。要改某个
    /// 窗口，用该 `VireoWindow::set_drag_cap`。
    pub fn set_drag_cap(&self, enabled: bool) {
        *self.drag_cap.lock() = enabled;
    }

    /// 当前「拖动期帧率上限」开关（`App::set_drag_cap`）。默认 `true`。
    /// 希望缩放窗口时画面内容变化平滑 → 设为 `false`。
    pub fn drag_cap(&self) -> bool {
        *self.drag_cap.lock()
    }

pub fn windows(&self) -> Vec<Arc<VireoWindow>> {
        self.windows.lock().iter().filter_map(|w| w.clone()).collect()
    }

    /// 所有存活窗口索引（与 `window_ref` 配合使用）。
    /// handle 是稳定 id；同一 handle 跨关窗事件不变（关窗后 `window_ref` 返回 None）。
    pub fn window_indices(&self) -> Vec<WindowIndex> {
        self.windows.lock().iter().enumerate()
            .filter(|(_, w)| w.is_some())
            .map(|(i, _)| WindowIndex::new(i as u64))
            .collect()
    }
}
