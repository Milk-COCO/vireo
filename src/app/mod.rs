use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use winit::window::Icon;
use winit::window::WindowId;

use crate::error::VireoError;
use crate::gpu::GpuContext;
use crate::offscreen::OffscreenCanvas;
use crate::texture::Texture;
use crate::window::{
    AntiAliasing, FrameStyle, OffscreenIndex, VireoWindow, WindowDesc, WindowIndex, WinitEvent,
};

/// 渲染线程 → winit 线程的运行期窗口创建请求（`App::window` 在 on_tick 里调用时）。
pub(crate) struct CreateWindowRequest {
    pub handle: u64,
    pub desc: WindowDesc,
    pub init_duration: f64,
    pub on_close: Option<Box<dyn FnOnce() + Send>>,
}

/// 入口期一次性创建的通道集合。
pub(crate) struct RunChannels {
    pub event_tx: mpsc::Sender<WinitEvent>,
    pub event_rx: mpsc::Receiver<WinitEvent>,
    pub cb_rx: mpsc::Receiver<(usize, crate::input::InputCallbacks)>,
    pub exit_rx: mpsc::Receiver<()>,
    pub exit_tx: mpsc::Sender<()>,
    pub exit_ack_rx: mpsc::Receiver<()>,
    pub exit_ack_tx: mpsc::Sender<()>,
    pub frame_style_rx: mpsc::Receiver<(isize, FrameStyle)>,
    pub aspect_ratio_rx: mpsc::Receiver<(isize, Option<f64>)>,
    pub nc_rx: mpsc::Receiver<(isize, crate::platform::windows::NcUpdate)>,
    pub create_rx: mpsc::Receiver<CreateWindowRequest>,
    pub close_rx: mpsc::Receiver<usize>,
    pub supervisor_event_tx: mpsc::Sender<WinitEvent>,
    pub frame_style_tx: mpsc::Sender<(isize, FrameStyle)>,
    pub aspect_ratio_tx: mpsc::Sender<(isize, Option<f64>)>,
    pub nc_tx: mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    pub close_tx: mpsc::Sender<usize>,
}

/// 退出链路诊断（`VIREO_EXIT_TRACE=1` 开；`=0`/未设关）：supervisor/winit 退出握手、
/// wake 结果、drain 命中、关窗等待全链路打点。默认零开销（OnceLock 缓存开关）。
pub(crate) fn exit_trace() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("VIREO_EXIT_TRACE").is_some_and(|v| v != "0"))
}

pub(crate) fn exit_log(msg: &str) {
    if exit_trace() {
        eprintln!("[vireo-exit] {}", msg);
    }
}

pub struct AppInner {
    pub windows: Mutex<Vec<Option<Arc<VireoWindow>>>>,
    alive_window_count: AtomicUsize,
    created_window_count: AtomicUsize,
    pub gpu: Arc<GpuContext>,
    instance: Mutex<Option<wgpu::Instance>>,
    device_lost: Arc<AtomicBool>,
    handle_to_id: Mutex<FxHashMap<u64, WindowId>>,
    next_handle: Mutex<u64>,
    default_icon: Mutex<Option<Icon>>,
    textures: Mutex<Vec<Arc<Texture>>>,
    offscreens: Mutex<Vec<Arc<OffscreenCanvas>>>,
    pub init_duration: f64,
    max_fps: Mutex<Option<u32>>,
    drag_cap: Mutex<bool>,
    create_tx: Mutex<Option<mpsc::Sender<CreateWindowRequest>>>,
    cb_tx: Mutex<Option<mpsc::Sender<(usize, crate::input::InputCallbacks)>>>,
    pub(crate) loop_states: Mutex<Vec<Arc<crate::thread::LoopHandleState>>>,
    pub(crate) windows_ready: AtomicBool,
    pub(crate) pending_window_creates: AtomicUsize,
    pub(crate) loops_ever_requested: AtomicBool,
    pub(crate) main_done: AtomicBool,
    /// vireo-main 是否以 panic 结束（`main.rs` 的 `catch_unwind` 的 `Err` 分支置位）。
    /// 收尾完成后决定进程退出码：true → `process::exit(1)`，false → 0。
    /// 只反映 main 自身的结局；loop panic 被 main 妥善处理后正常返回仍是 0。
    pub(crate) main_panicked: AtomicBool,
    pub(crate) event_tx: Mutex<Option<mpsc::Sender<WinitEvent>>>,
    pub(crate) loop_wake: Arc<(std::sync::Mutex<()>, std::sync::Condvar)>,
    pub(crate) event_loop_proxy: OnceLock<winit::event_loop::EventLoopProxy<()>>,
    /// 生命周期回调：`on_paused` 与 `on_resumed`。
    ///
    /// 只由**渲染线程**在观察到状态变化后调用（`thread::run_thread_loop`），
    /// 所以回调里可以安全地用 `App` / `DrawBatch`，不必自己做跨线程同步。
    pub(crate) lifecycle_cbs: Mutex<LifecycleCbs>,
    /// `on_memory_warning` 回调表。只由**渲染线程**在观察到代号变化后调用。
    pub(crate) memory_warning_cbs: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    /// 内存警告代号。winit/owner 线程收到 `Event::MemoryWarning` 时
    /// `fetch_add`（见 `note_memory_warning`），渲染线程比对后补发回调 ——
    /// 与 `android_lifecycle::GEN` 同一套「跨线程代号 + 渲染线程补发」模式。
    memory_warn_gen: std::sync::atomic::AtomicU64,
}

impl AppInner {
    /// 由 supervisor 在收到 winit `Event::MemoryWarning` 时调用（winit/owner 线程）。
    ///
    /// 真正的回调在渲染线程 fire（见 [`fire_memory_warnings`](Self::fire_memory_warnings)），
    /// 这里只推进代号。
    pub(crate) fn note_memory_warning(&self) {
        self.memory_warn_gen
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// 当前内存警告代号；渲染线程用它判断「有没有新警告没处理」。
    pub(crate) fn memory_warn_gen(&self) -> u64 {
        self.memory_warn_gen
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// 由渲染线程在观察到代号变化后调用。单回调 panic 不打断渲染循环。
    pub(crate) fn fire_memory_warnings(&self) {
        let cbs = {
            let cbs = self.memory_warning_cbs.lock();
            if cbs.is_empty() {
                return;
            }
            // clone Arc 而不是持锁调用：回调完全可能注册新回调。
            cbs.clone()
        };
        for cb in &cbs {
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb())) {
                log::error!(
                    "vireo: memory warning callback panicked: {}",
                    crate::app::supervisor::panic_payload_to_string(payload)
                );
            }
        }
    }
}

pub struct App {
    pub(crate) inner: Arc<AppInner>,
}

impl Clone for App {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl std::ops::Deref for App {
    type Target = AppInner;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub struct DeferredTask {
    kind: DeferredTaskKind,
    pub(crate) f: Box<dyn FnOnce() + Send>,
}

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

static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);

/// `on_paused` / `on_resumed` 回调表：`(paused, resumed)`。
///
/// 元素是 `Arc` 而非 `Box` —— 派发时要先把锁释放再逐个调用，否则「回调里再注册
/// 一个回调」会自锁。
type LifecycleCbs = (
    Vec<Arc<dyn Fn() + Send + Sync>>,
    Vec<Arc<dyn Fn() + Send + Sync>>,
);

impl AppInner {
    /// 由渲染线程在观察到生命周期状态变化后调用。
    ///
    /// 单个回调 panic 不会打断渲染循环 —— 与 `on_tick` 的处理一致。
    pub(crate) fn fire_lifecycle_callbacks(&self) {
        let (paused, resumed) = {
            let cbs = self.lifecycle_cbs.lock();
            if cbs.0.is_empty() && cbs.1.is_empty() {
                return;
            }
            // clone Arc 而不是持锁调用：回调完全可能在里面注册新回调。
            (cbs.0.clone(), cbs.1.clone())
        };
        let fire = |cbs: &[Arc<dyn Fn() + Send + Sync>]| {
            for cb in cbs {
                if let Err(payload) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb()))
                {
                    log::error!(
                        "vireo: lifecycle callback panicked: {}",
                        crate::app::supervisor::panic_payload_to_string(payload)
                    );
                }
            }
        };
        if crate::platform::android_lifecycle::is_paused() {
            fire(&paused);
        } else {
            fire(&resumed);
        }
    }
}

/// 安卓进程级 `AndroidApp` 句柄（仅安卓）。
///
/// `AndroidApp` 只能从 activity 的 `onCreate` 回调拿到（android-activity
/// 不提供全局访问器），但它本身是**进程级单例** —— 一个进程一个 Activity，
/// 生命周期等于进程。所以存进 `OnceLock` 即可：
///
/// - `#[vireo::main]` 展开的 `android_main` 只需「存句柄 → 调 [`App::new`]」，
///   公开 API 上不必区分安卓 / 桌面两个构造器。
/// - `run_blocking` 建 winit 事件循环、`WindowExtAndroid` 的 JNI 都从这里 clone 取用。
///
/// 代价是这个隐藏的进程级全局；换来的是用户侧只需要记住 `App::new` 一个入口。
#[cfg(target_os = "android")]
static ANDROID_APP: OnceLock<crate::android::AndroidApp> = OnceLock::new();

/// 取进程级 `AndroidApp` 句柄（仅安卓；未经 `android_main` 入口时为 `None`）。
#[cfg(target_os = "android")]
pub(crate) fn android_app() -> Option<&'static crate::android::AndroidApp> {
    ANDROID_APP.get()
}

/// 安卓进程入口（仅安卓）：`#[vireo::main]` 展开的 `android_main` 调这个。
///
/// 存下 [`crate::android::AndroidApp`] 后转 [`App::new`]，其余流程与桌面完全一致。
#[doc(hidden)]
#[cfg(target_os = "android")]
pub fn android_main_entry<F, Fut>(android_app: crate::android::AndroidApp, main: F)
where
    F: FnOnce(App) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    // 绑定放在 set 之前：直接用这个所有权值，不需要回头查单例
    // （参数名 `android_app` 遮蔽了同名取值函数，用路径反而绕）。
    crate::platform::android_lifecycle::bind_lifecycle_hooks(&android_app);
    let _ = ANDROID_APP.set(android_app);
    App::new(main)
}

/// [`android_main_entry`] 的「指定 wgpu 后端」版，供
/// `#[vireo::main(descriptor = ...)]` 在 Android 上使用。
///
/// 与桌面版的差别只有一点：Android 上 wgpu 只靠 `InstanceDescriptor::backends`
/// 决定候选后端（没有别的默认来源），所以 `backends` 里必须显式包含 `VULKAN`
/// ——否则 wgpu 会按 `Backends::default()` 协商，可能落到未验证的 GLES 路径。
#[doc(hidden)]
#[cfg(target_os = "android")]
pub fn android_main_entry_with_descriptor<F, Fut>(
    android_app: crate::android::AndroidApp,
    desc: wgpu::InstanceDescriptor,
    main: F,
) where
    F: FnOnce(App) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    // 绑定放在 set 之前：直接用这个所有权值，不需要回头查单例
    // （参数名 `android_app` 遮蔽了同名取值函数，用路径反而绕）。
    crate::platform::android_lifecycle::bind_lifecycle_hooks(&android_app);
    let _ = ANDROID_APP.set(android_app);
    App::with_descriptor(desc, main)
}

impl App {
    /// 进程入口（宏风格）：OS 主线程构造 `App`，`vireo-main` 线程跑用户 future，
    /// OS 主线程跑 winit 事件循环；阻塞到进程退出。
    ///
    /// 生命周期归属：**vireo-main 结束（正常返回或 panic）即关闭所有窗口并退出进程**
    /// （supervisor 走逐窗 close 路径收尾，与 X 关闭一致）。loop panic 本身不结束进程，
    /// 而是经 `ThreadHandle` 以 `Err` 交付；若其导致 main 随之结束（如 `.await.unwrap()`），
    /// 收尾机制同样接管，不会留下冻住的窗口与僵尸进程。
    /// 手动 `App::run`/`spawn` 自驱 main 的程序不受此规则影响。
    /// 进程退出码只反映 main 自身的结局：main panic → 1，其余 0
    ///（loop panic 被 main 妥善处理后正常返回仍是 0）。
    #[allow(clippy::new_ret_no_self)]
    pub fn new<F, Fut>(main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        App::from_instance_descriptor(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        )
        .run_entry(main)
    }

    pub fn with_descriptor<F, Fut>(desc: wgpu::InstanceDescriptor, main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        App::from_instance_descriptor(desc).run_entry(main)
    }

    fn run_entry<F, Fut>(self, main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        // 安卓上 stderr 进不了 logcat，必须走 `android_logger`（logcat 可见）。
        // 桌面**不自动初始化** logger：仍由用户在 `main` 首行显式调
        // [`crate::init_logger`]，保持 `RUST_LOG` 的既有语义。
        #[cfg(target_os = "android")]
        {
            let _ = android_logger::init_once(
                android_logger::Config::default()
                    .with_max_level(log::LevelFilter::Info)
                    .with_tag("vireo"),
            );
            // panic 信息改走 log，否则闪退无日志可查。
            std::panic::set_hook(Box::new(|info| {
                log::error!("panic: {info}");
            }));
        }

        let channels = self.init_run_channels();
        let app = self.clone();
        main::spawn(app.clone(), main);
        if let Err(e) = host::run_blocking(self, channels) {
            eprintln!("[vireo] app exited with error: {:?}", e);
        }
        // 退出码只反映 main 自身的结局：main panic → 1，其余 0。
        // 此时 teardown 已全部完成（event loop 已退、supervisor 已 join），
        // `process::exit` 跳过析构是安全的。loop panic 被 main 妥善处理后不影响。
        // 安卓例外：`android_main` 内禁 `process::exit`（activity 生命周期由系统接管，
        // 直接返回即可；见 android-activity 文档）。
        #[cfg(not(target_os = "android"))]
        if app.inner.main_panicked.load(Ordering::Acquire) {
            std::process::exit(1);
        }
    }

    fn init_run_channels(&self) -> RunChannels {
        let (event_tx, event_rx) = mpsc::channel();
        let supervisor_event_tx = event_tx.clone();
        let (cb_tx, cb_rx) = mpsc::channel();
        let (exit_tx, exit_rx) = mpsc::channel();
        let (exit_ack_tx, exit_ack_rx) = mpsc::channel();
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
            exit_ack_rx,
            exit_ack_tx,
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
        // 默认图标编译进库（根目录 `logo.png` 经 `include_bytes!` baked，
        // 与进程启动目录无关；此前运行时读 CWD 的 `logo.png`，在哪启动决定
        // 有没有图标，已删除）。
        let default_icon = {
            const LOGO: &[u8] = include_bytes!("../../logo.png");
            image::load_from_memory(LOGO).ok().and_then(|img| {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Icon::from_rgba(rgba.into_raw(), w, h).ok()
            })
        };
        let init_duration = init_start.elapsed().as_secs_f64();

        Self {
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
                main_panicked: AtomicBool::new(false),
                event_tx: Mutex::new(None),
                loop_wake: Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new())),
                event_loop_proxy: OnceLock::new(),
                lifecycle_cbs: Mutex::new((Vec::new(), Vec::new())),
                memory_warning_cbs: Mutex::new(Vec::new()),
                memory_warn_gen: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    /// 创建离屏画布（预热 AA 管线）。
    pub fn offscreen(&self, width: u32, height: u32, aa: AntiAliasing) -> OffscreenIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(aa, self.gpu.supported_sample_counts());
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self
            .gpu
            .ensure_pipeline(sc, atc, ssaa, false, wgpu::BlendState::ALPHA_BLENDING);
        let _ = self
            .gpu
            .ensure_pipeline(sc, atc, ssaa, true, wgpu::BlendState::ALPHA_BLENDING);
        let init_duration = start.elapsed().as_secs_f64();
        let mut guard = self.offscreens.lock();
        let idx = guard.len();
        let mut offscreen = OffscreenCanvas::with_aa(&self.gpu, width, height, aa, init_duration);
        offscreen.index = OffscreenIndex(idx);
        guard.push(Arc::new(offscreen));
        OffscreenIndex(idx)
    }

    pub fn offscreen_ref(&self, idx: &OffscreenIndex) -> Result<Arc<OffscreenCanvas>, VireoError> {
        match self.offscreens.lock().get(idx.0).cloned() {
            Some(c) => Ok(c),
            None => Err(VireoError::OffscreenNotFound(idx.0)),
        }
    }

    pub fn load_texture(&self, path: impl AsRef<std::path::Path>) -> usize {
        let tex = Texture::from_file(path, &self.gpu);
        let mut guard = self.textures.lock();
        let idx = guard.len();
        guard.push(Arc::new(tex));
        idx
    }

    /// 获取贴图（`Err` = 索引越界/未加载完成）。
    pub fn texture(&self, index: usize) -> Result<Arc<Texture>, VireoError> {
        match self.textures.lock().get(index).cloned() {
            Some(t) => Ok(t),
            None => Err(VireoError::TextureNotFound(index)),
        }
    }

    fn wake_event_loop(&self) {
        if let Some(proxy) = self.event_loop_proxy.get() {
            match proxy.send_event(()) {
                Ok(()) => exit_log("wake_event_loop: send_event ok"),
                Err(_) => exit_log("wake_event_loop: send_event FAILED (loop closed?)"),
            }
        } else {
            exit_log("wake_event_loop: no proxy yet");
        }
    }

    /// 创建窗口（`run` 前同步入队、`run` 中异步发送到 winit 线程）。
    /// 返回 `WindowIndex`，后续可经 `window_ref` 拿到 `VireoWindow`。
    pub fn window(
        &self,
        mut desc: WindowDesc,
        on_close: Option<impl FnOnce() + Send + 'static>,
    ) -> WindowIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(desc.anti_aliasing, self.gpu.supported_sample_counts());
        desc.anti_aliasing = aa;
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self
            .gpu
            .ensure_pipeline(sc, atc, ssaa, false, wgpu::BlendState::ALPHA_BLENDING);
        let _ = self
            .gpu
            .ensure_pipeline(sc, atc, ssaa, true, wgpu::BlendState::ALPHA_BLENDING);
        let init_duration = start.elapsed().as_secs_f64();
        let handle = *self.next_handle.lock();
        *self.next_handle.lock() = handle + 1;
        let on_close = on_close.map(|f| Box::new(f) as Box<dyn FnOnce() + Send>);
        match self.create_tx.lock().as_ref().cloned() {
            Some(tx) => {
                let _ = tx.send(CreateWindowRequest {
                    handle,
                    desc,
                    init_duration,
                    on_close,
                });
                self.pending_window_creates.fetch_add(1, Ordering::AcqRel);
                self.wake_event_loop();
            }
            None => unreachable!(
                "vireo: create_tx is always Some after run_entry/init_run_channels; \
                 App::window is only reachable with create_tx set"
            ),
        }
        WindowIndex::new(handle)
    }

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
        on_resized: impl FnMut(winit::dpi::PhysicalSize<u32>) + 'static => on_resized,
        #[cfg(target_os = "windows")]
        on_thumb_button: impl FnMut(u32) + 'static => on_thumb_button,
    }

    /// 运行主循环（单线程，传入 `on_tick` 闭包，返回 `ThreadHandle` 可 `.await`）。
    pub fn run<F: FnMut(&mut crate::thread::LoopContext) -> bool + Send + 'static>(
        &self,
        on_tick: F,
    ) -> crate::thread::ThreadHandle {
        self.spawn(crate::thread::Thread::new().with_loop(crate::thread::Loop::new(on_tick)))
    }

    /// 在独立 OS 线程启动一组循环（真并行），返回 `ThreadHandle` 可 `.await` / `.push()` / `.extend()`。
    pub fn spawn(&self, thread: crate::thread::Thread) -> crate::thread::ThreadHandle {
        let loops = thread.loops;
        let shared = Arc::new(Mutex::new(Vec::<crate::thread::Loop>::new()));
        let fps_stats = Arc::new(Mutex::new(crate::thread::FpsStats::new()));
        let state = Arc::new(crate::thread::LoopHandleState::new());
        self.loops_ever_requested.store(true, Ordering::Release);
        self.loop_states.lock().push(state.clone());
        let app = self.clone();
        let device_lost = self.device_lost.clone();
        let state_for_thread = state.clone();
        let shared_for_thread = shared.clone();
        let fps_for_thread = fps_stats.clone();
        let device_lost_for_thread = device_lost.clone();
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
        crate::thread::ThreadHandle {
            state,
            thread: Some(join),
            shared,
            fps_stats,
        }
    }

    pub fn loops(
        &self,
        loops: impl IntoIterator<Item = crate::thread::Loop>,
    ) -> crate::thread::ThreadHandle {
        self.spawn(crate::thread::Thread::new().with_loops(loops))
    }

    /// 创建仅包含片段着色器的材质（内置 SDF/geo 实例化 vertex shader）。
    pub fn material(&self, source: &str) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material(source)
    }

    /// 创建带自定义 vertex shader 的材质（需自行匹配 `VertexInput @0-7` 签名）。
    pub fn material_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu
            .create_material_with_vertex_shader(source, vertex_source)
    }

    /// 创建材质并手动指定资源（uniform buffer / 贴图 / sampler）。
    pub fn material_with_resources(
        &self,
        source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_with_resources(source, resources)
    }

    pub fn material_with_resources_and_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu
            .create_material_with_resources_and_vertex_shader(source, vertex_source, resources)
    }

    /// 创建材质 + 手动指定 bind group layout（完全自管管线布局）。
    pub fn material_manual(
        &self,
        source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_manual(source, bgl)
    }

    pub fn material_manual_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu
            .create_material_manual_with_vertex_shader(source, vertex_source, bgl)
    }

    /// 获取窗口（`Err` = 从未创建或已关闭）。
    pub fn window_ref(&self, idx: &WindowIndex) -> Result<Arc<VireoWindow>, VireoError> {
        match self
            .windows
            .lock()
            .get(idx.0 as usize)
            .and_then(|w| w.clone())
        {
            Some(w) => Ok(w),
            None => Err(VireoError::WindowNotFound(idx.0)),
        }
    }

    pub fn window_count(&self) -> usize {
        self.alive_window_count.load(Ordering::Acquire)
    }

    pub(crate) fn created_window_count(&self) -> usize {
        self.created_window_count.load(Ordering::Acquire)
    }

    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    // ---- 生命周期（Android 有效，其余平台恒「未暂停」）--------------------

    /// App 当前是否处于**暂停**（Android 上 = activity 在后台）。
    ///
    /// # 它替你做了什么
    ///
    /// `true` 期间整条渲染 Thread **完全停摆** —— 不跑 `on_tick`、不 `draw`、
    /// 不 acquire/present。所以你不需要（也不该）在 `on_tick` 里自己判断：
    /// **恢复时会自动补上停摆期间漏掉的帧逻辑**，`on_tick` 里的代码照常写。
    ///
    /// # 平台
    ///
    /// - **Android**：`MainActivity.onPause()` → JNI → 本标志。
    ///   需要 `MainActivity.java` 照 `examples/android/` 那样声明并调用两个
    ///   native 方法；没声明时本值恒 `false`（= 旧的「后台仍全速」行为），
    ///   并在 logcat 打一条 warn 说明怎么改。
    /// - **其它平台**：恒 `false`。桌面对应概念是
    ///   [`VireoWindow::focused`](crate::window::VireoWindow::focused)。
    ///
    /// ⚠️ Android 的 `onPause` 对**短暂**中断也会触发（锁屏、部分弹窗、
    /// 分屏切换）。这是 Unity `OnApplicationPause` 同样的语义；如果你需要
    /// 「彻底离开才算」，得自己在 `on_paused` 里配合 `on_stopped` 之类的额外判定。
    pub fn is_paused(&self) -> bool {
        crate::platform::android_lifecycle::is_paused()
    }

    /// 注册「进入暂停」回调。
    ///
    /// 在**渲染线程**上、且在恢复之后调用（回调需要能碰 `App`，所以不能直接在
    /// UI 线程的 JNI 回调里跑）。
    ///
    /// 典型用途：保存游戏进度、释放音频、停止网络轮询。
    /// 注意它是**恢复时**才拿到通知，所以「暂停时要做的紧急收尾」请直接写在
    /// `on_tick` 里判 `is_paused()`。
    pub fn on_paused(&self, cb: impl Fn() + Send + Sync + 'static) {
        self.lifecycle_cbs.lock().0.push(Arc::new(cb));
    }

    /// 注册「恢复」回调。语义/线程见 [`on_paused`](Self::on_paused)。
    pub fn on_resumed(&self, cb: impl Fn() + Send + Sync + 'static) {
        self.lifecycle_cbs.lock().1.push(Arc::new(cb));
    }

    /// 注册「系统内存吃紧」回调。
    ///
    /// 来源是跨平台的 winit [`Event::MemoryWarning`](winit::event::Event::MemoryWarning)：
    /// Android 来自 `onTrimMemory`，iOS 来自
    /// `UIApplicationDidReceiveMemoryWarningNotification`。**零 JNI** —— winit 是
    /// 唯一把 Android 生命周期事件正常转发出来的那个，所以这条不需要写
    /// `MainActivity.java`。
    ///
    /// 在**渲染线程**上触发（与 [`on_paused`](Self::on_paused) 同理：回调要能碰
    /// `App` / `DrawBatch`）。单个回调 panic 不会打断渲染循环。
    ///
    /// # 这不是「vireo 帮你防 OOM」
    ///
    /// 回调触发时能做的清理，vireo 侧只有 **glyph atlas**
    /// （`TextContext.text_atlas`，其 `trim()` 已存在但从不自动调用）。
    /// **显存大头是用户贴图，而 vireo 结构上释放不了** —— `App.textures` 是
    /// append-only 的 `Vec<Arc<Texture>>`，只有 `load_texture` 没有移除 API。
    /// 对贴图密集型应用，请在自己的回调里决定丢弃什么。
    ///
    /// 拿不到 Android 的具体 level（`TRIM_MEMORY_*`）：AGSK 的 C glue 只
    /// `LOGV` 打印后写无 payload 的 `APP_CMD_LOW_MEMORY`。但响应动作在所有
    /// level 上是同一个，缺失不构成阻塞。
    pub fn on_memory_warning(&self, cb: impl Fn() + Send + Sync + 'static) {
        self.inner.memory_warning_cbs.lock().push(Arc::new(cb));
    }

    pub fn set_max_fps(&self, fps: Option<u32>) {
        *self.max_fps.lock() = fps;
    }

    pub fn max_fps(&self) -> Option<u32> {
        *self.max_fps.lock()
    }

    pub fn set_drag_cap(&self, enabled: bool) {
        *self.drag_cap.lock() = enabled;
    }

    pub fn drag_cap(&self) -> bool {
        *self.drag_cap.lock()
    }

    pub fn windows(&self) -> Vec<Arc<VireoWindow>> {
        self.windows
            .lock()
            .iter()
            .filter_map(|w| w.clone())
            .collect()
    }

    pub fn window_indices(&self) -> Vec<WindowIndex> {
        self.windows
            .lock()
            .iter()
            .enumerate()
            .filter(|(_, w)| w.is_some())
            .map(|(i, _)| WindowIndex::new(i as u64))
            .collect()
    }
}

mod host;
mod main;
mod supervisor;

pub(crate) use crate::app::supervisor::{panic_payload_to_string, supervisor_loop};

#[cfg(test)]
mod tests {

    /// 内存警告代号必须单调递增，且渲染线程能靠它判断「有没有新警告」。
    ///
    /// 这套「winit/owner 线程推进代号 + 渲染线程比对后补发」的模式与
    /// `android_lifecycle::GEN` 相同，是跨线程把回调搬到渲染线程执行的手段
    /// —— 回调要能碰 `App` / `DrawBatch`，不能在 winit 线程跑。
    ///
    /// 用裸 `AtomicU64` 测而不是 `AppInner`：`AppInner` 构造需要真实 GPU
    /// （要 adapter/device/queue），单测里造不出来，但代号逻辑本身不依赖它。
    #[test]
    fn memory_warning_generation_is_monotonic() {
        let cell = std::sync::atomic::AtomicU64::new(0);
        assert_eq!(
            cell.load(std::sync::atomic::Ordering::Acquire),
            0,
            "初始为 0"
        );

        let mut last = 0u64;
        for i in 1..=8u64 {
            cell.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let g = cell.load(std::sync::atomic::Ordering::Acquire);
            assert!(g > last, "代号必须严格递增（第 {i} 次）");
            last = g;
        }
        assert_eq!(last, 8);
        // 渲染线程靠 `g != tracked` 判断；未变化时不该误触发。
        assert_eq!(cell.load(std::sync::atomic::Ordering::Acquire), last);
    }
}
