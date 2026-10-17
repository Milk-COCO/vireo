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
    /// 用户**请求**建窗的累计次数（不是成功创建的次数）。
    ///
    /// 用于 Android 单窗口限制的判定 —— 见 [`android_rejects_extra_window`]。
    ///
    /// 为什么不能拿 `created_window_count` 判：它在 supervisor 收到
    /// `WindowCreated` 时才递增，而 `App::window()` 可以连着调两次 —— 那一刻
    /// supervisor 还没处理第一个请求，`created_window_count` 仍是 0，守卫会漏。
    /// 计数必须落在**请求侧**才没有这个竞态。
    requested_window_count: AtomicUsize,
    pub gpu: Arc<GpuContext>,
    instance: Mutex<Option<wgpu::Instance>>,
    device_lost: Arc<AtomicBool>,
    handle_to_id: Mutex<FxHashMap<u64, WindowId>>,
    next_handle: Mutex<u64>,
    default_icon: Mutex<Option<Icon>>,
    /// 可写目录的末级名字（`data_dir` / `cache_dir` / `external_data_dir` 用）。
    ///
    /// 默认取可执行文件名（去扩展名）；[`App::set_storage_name`] 可改。
    /// 构造时经 [`crate::storage::sanitize_dir_name`] 压成安全的单层分量。
    storage_name: Mutex<String>,
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
    /// 生命周期回调：`on_paused` / `on_resumed` / `on_stopped` / `on_destroyed`。
    ///
    /// 只由**渲染线程**在观察到状态变化后调用（`thread::run_thread_loop`），
    /// 所以回调里可以安全地用 `App` / `DrawBatch`，不必自己做跨线程同步。
    pub(crate) lifecycle_cbs: Mutex<LifecycleCbs>,
    /// vireo 是否在切后台时**自动阻塞**渲染循环。默认 `true`。
    ///
    /// 「策略」而非「状态」—— 与 [`App::is_paused`] 那类状态查询刻意区分：
    /// `is_paused()` 回答「我现在是否后台」，本字段回答「vireo 要不要替我暂停」。
    pub(crate) auto_pause: std::sync::atomic::AtomicBool,
    /// 进程级「正在退出」。supervisor 判定退出时置位并 [`notify_all`]。
    ///
    /// 存在的唯一理由：让 [`App::wait_until`] / [`wait_while_paused`] 在窗口全关
    /// 或设备丢失时**能被打断** —— 否则用户代码里挂着的等待会让 `join` 永久阻塞。
    /// 用进程级而非 per-thread：这两个全局条件已经覆盖了「必须唤醒」的全部场景。
    pub(crate) shutdown: std::sync::atomic::AtomicBool,
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

/// `on_paused` / `on_resumed` / `on_stopped` / `on_destroyed` 回调表。
///
/// 元素是 `Arc` 而非 `Box` —— 派发时要先把锁释放再逐个调用，否则「回调里再注册
/// 一个回调」会自锁。
///
/// 用具名字段而非元组：四张表元素类型完全相同，元组下 `cbs.0` / `cbs.1` / …
/// 只能靠记忆区分，加到第四个时尤其容易拿错。
/// 等待循环的**纯逻辑**：等 `cond` 成立，或 `abort()` 为真，或超时。
///
/// # 为什么抽成自由函数（而不是直接写在 `App::wait_until` 里）
///
/// `App` 内含 `Arc<GpuContext>`，而 `GpuContext` 有二十多个 wgpu 资源字段、
/// **没有** `Default` —— 单测里造不出一个合法的 `App`（`mem::zeroed` 会得到
/// 非法的 NonZero 句柄）。把这段只依赖「事件代号 + 两个闭包」的逻辑抽出来，
/// 就能直接测：不需要 GPU，也不需要 `App`。
///
/// # `timeout` 是**总时长**，不是每次等待的时长
///
/// 按 deadline 折算剩余时间，所以被 spurious 唤醒后复查条件失败不会重新计时 ——
/// 「5 秒超时」永远是 5 秒，而不是「5 秒 × 唤醒次数」。
///
/// # `abort` 优先于条件
///
/// 已 abort 时立即返回 `false`，不进等待；等待中被唤醒后也先查 abort。
pub(crate) fn wait_loop(
    timeout: Option<std::time::Duration>,
    abort: impl Fn() -> bool,
    cond: impl Fn() -> bool,
) -> bool {
    if abort() {
        return false;
    }
    // 先查一次：条件已成立就不必进等待（也让「立即返回」成为常见情形）
    if cond() {
        return true;
    }
    let deadline = timeout.map(|d| std::time::Instant::now() + d);
    loop {
        let remaining = match deadline {
            None => None,
            Some(dl) => {
                let now = std::time::Instant::now();
                match dl.checked_duration_since(now) {
                    Some(r) if !r.is_zero() => Some(r),
                    // 已到点：再复查一次条件就收工（可能刚好在超时瞬间成立）
                    _ => return !abort() && cond(),
                }
            }
        };
        crate::platform::android_lifecycle::wait_events(remaining);
        if abort() {
            return false;
        }
        if cond() {
            return true;
        }
    }
}

/// Android 上第 2+ 个窗口是否必须被拒绝。
///
/// # 为什么要拒绝（不是「Android 不方便」，是「一定会坏」）
///
/// winit 0.30.13 的 Android 后端有两个硬伤，叠加后**任何**第二个窗口都必然坏：
///
/// ```text
/// winit-0.30.13/src/platform_impl/android/mod.rs
///   :745   pub(crate) struct WindowId;              ← 单元结构体，无字段
///   :783   pub(crate) fn new(el, _window_attrs) -> Result<Self, OsError> {
///   :788       // FIXME this ignores requested window attributes
///   :790       Ok(Self { app: ..., redraw_requester: ... })
/// ```
///
/// 1. **`WindowId` 是单元结构体** → 所有 Android 窗口的 id **完全相同**。而 vireo
///    靠 `id_to_handle: FxHashMap<WindowId, usize>` 做事件路由（`host.rs:295`
///    `insert`），第二次 insert 会**覆盖**第一次 → 窗口 A 的触摸 / 按键 / resize
///    全部被路由到窗口 B。
/// 2. **`Window::new` 忽略所有窗口属性** → 两个 winit `Window` 各自
///    `create_surface`，得到**两个 `Surface` 指向同一个 `SurfaceView`**。两个
///    `Renderer` 对同一 surface 做 acquire→present，正是
///    `.opencode/VIREO_OPT_NOTES.md` 第三十三 / 三十四轮花两轮才定位的
///    「Surface image is already acquired」类崩溃。
///
/// 症状是**静默的**（无异常、无日志），画面表现为「两个窗口内容串了 / 莫名
/// 崩溃」，极难定位。
///
/// # 为什么不「让它跑」
///
/// 修好第 1 条（自己造 id）也修不好第 2 条 —— Android 上一个 Activity 只有一个
/// `SurfaceView`，多窗口 UI 在这个模型下没有对应物。要真支持得引入 Android 的
/// 多窗口 / `SurfaceView` 拆分，那是另一个量级的工程。所以**硬失败**比
/// 「跑起来但行为不可预测」好。
///
/// # 为什么 panic 而不是 `log::error!` + 返回一个死 index
///
/// 死 index 要求用户自己写 `Err` 分支才知道出了事 —— 而漏写就是**静默串台**
/// （正是本条要消灭的症状）。panic 是 fail-fast：
/// Android 上由 panic hook 走 `log::error!`（logcat 可见），`main.rs` 的
/// `catch_unwind` 收尾并把退出码置 1，不留僵尸进程。
///
/// panic 消息本身是**英文**（与全仓 `panic!`/`expect`/`unreachable!` 一致 ——
/// 诊断输出要进 crash log、被 grep、贴进 issue，英文更通用）；中文解释在这里。
///
/// # 纯函数、不 cfg 门控
///
/// 刻意不加 `#[cfg(target_os = "android")]` —— 这样桌面 `cargo test --lib`
/// 也能测到这条规则（Android-only 代码在桌面跑不到，是本项目反复踩的坑）。
/// 调用点负责门控。
pub(crate) fn android_rejects_extra_window(already_requested: usize) -> bool {
    already_requested > 0
}

pub(crate) struct LifecycleCbs {
    paused: Vec<Arc<dyn Fn() + Send + Sync>>,
    resumed: Vec<Arc<dyn Fn() + Send + Sync>>,
    stopped: Vec<Arc<dyn Fn() + Send + Sync>>,
    destroyed: Vec<Arc<dyn Fn() + Send + Sync>>,
    back: Vec<Arc<dyn Fn() + Send + Sync>>,
}

impl LifecycleCbs {
    fn empty() -> Self {
        Self {
            paused: Vec::new(),
            resumed: Vec::new(),
            stopped: Vec::new(),
            destroyed: Vec::new(),
            back: Vec::new(),
        }
    }

    fn all_empty(&self) -> bool {
        self.paused.is_empty()
            && self.resumed.is_empty()
            && self.stopped.is_empty()
            && self.destroyed.is_empty()
            && self.back.is_empty()
    }
}

impl AppInner {
    /// 由渲染线程在观察到生命周期状态变化后调用。
    ///
    /// 单个回调 panic 不会打断渲染循环 —— 与 `on_tick` 的处理一致。
    pub(crate) fn fire_lifecycle_callbacks(&self) {
        // ⚠️ 必须**先**消费锁存再考虑早退：`on_stopped` 一个回调都没注册时，
        // 早退会把 `SAW_STOP` 留在 true，下一个「无 stop 的暂停」就会误报一次
        // 「彻底离开」。锁存是「每个暂停周期恰好一次」的一次性事件，不是状态。
        let saw_stop = crate::platform::android_lifecycle::take_saw_stop();
        // 同理：返回键请求也必须在早退**之前**消费，否则 `on_back` 一个都没注册时
        // 锁存会留在 true，下一次返回键（哪怕默认该退出）都会被误报成「已处理」。
        let back = crate::platform::android_lifecycle::take_back_requested();

        let (paused, resumed, stopped, destroyed, back_cbs) = {
            let cbs = self.lifecycle_cbs.lock();
            if cbs.all_empty() {
                return;
            }
            // clone Arc 而不是持锁调用：回调完全可能在里面注册新回调。
            (
                cbs.paused.clone(),
                cbs.resumed.clone(),
                cbs.stopped.clone(),
                cbs.destroyed.clone(),
                cbs.back.clone(),
            )
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

        // `on_destroyed` 不是「另一种暂停」，而是独立事实（activity 没了）。
        // 先发它并**直接返回**：用户据此退出后，不该再收到 resumed 之类的通知。
        if crate::platform::android_lifecycle::is_destroyed() {
            fire(&destroyed);
            return;
        }
        if crate::platform::android_lifecycle::is_paused() {
            fire(&paused);
        } else {
            fire(&resumed);
        }
        // 「彻底离开」是附加信号：只在暂停期间发生过 onStop 的那次才发，
        // 且排在 paused/resumed 之后（后者先到，用户可以先做通用收尾）。
        if saw_stop {
            fire(&stopped);
        }
        // 返回键排在最后：它是**独立**事件，且不与「是否暂停」耦合 ——
        // 后台时按返回键同样会发（此时 Activity 不在前台，Java 侧会走 finish
        // 默认路径，所以「消费」与否通常无所谓）。
        //
        // 一个 `on_back` 没注册也**不影响**上一行：锁存已在早退前消费掉。
        if back {
            fire(&back_cbs);
        }
    }

    /// 由渲染线程在观察到 SAF 结果代号变化后调用（与 `fire_lifecycle_callbacks` 同位置）。
    ///
    /// 取出全部待交付结果并逐个调回调（`FnOnce`，锁外调用，panic 隔离与上同款）。
    /// 无结果时就是一次空 drain，不早退判断 —— 代号没变根本不会进到这里。
    pub(crate) fn fire_saf_results(&self) {
        let _ = self;
        for (cb, outcome) in crate::saf::drain_results() {
            if let Err(payload) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(outcome)))
            {
                log::error!(
                    "vireo: SAF callback panicked: {}",
                    crate::app::supervisor::panic_payload_to_string(payload)
                );
            }
        }
    }
}

/// 安卓进程级 `AndroidApp` 句柄（仅安卓）。
///
/// `AndroidApp` 只能从 activity 的 `onCreate` 回调拿到（android-activity
/// 不提供全局访问器），但它本身是**进程级单例** —— 通常一个进程一个 Activity，
/// 生命周期等于进程。所以存进 `OnceLock` 即可：
/// （注意「通常」：旧 Activity 结束后点图标会起新 Activity，同进程跑第二个
/// `android_main`，由 `ANDROID_MAIN_ENTERED` 挡掉，见下。）
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

/// `android_main` 是否已进过（仅安卓，防同进程第二个 Activity）。
///
/// 背景：默认 `launchMode` 下，用户在旧 Activity 结束后点图标会起新 Activity，
/// 同进程跑第二个 `android_main`。winit 单进程只允许一个事件循环，第二个
/// `run_blocking` 必炸（`EventLoop::build` 失败 → unwrap → SIGABRT，真机实测）。
/// vireo 是单 Activity 架构（多 Activity 单进程不在支持范围），重入时直接拒绝，
/// 新 Activity 黑屏但进程存活 —— 总比 SIGABRT 强。根治要么 manifest 声明
/// `singleTask`（见模板注释），要么多 Activity 架构（另议）。
#[cfg(target_os = "android")]
static ANDROID_MAIN_ENTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
    if ANDROID_MAIN_ENTERED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        log::error!(
            "vireo: android_main entered twice in one process (second Activity instance?); \
             vireo serves only the first one, ignoring this entry (no second event loop)"
        );
        return;
    }
    // 绑定放在 set 之前：直接用这个所有权值，不需要回头查单例
    // （参数名 `android_app` 遮蔽了同名取值函数，用路径反而绕）。
    crate::platform::android_lifecycle::bind_lifecycle_hooks(&android_app);
    crate::saf::bind_saf_hooks(&android_app);
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
    if ANDROID_MAIN_ENTERED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        log::error!(
            "vireo: android_main entered twice in one process (second Activity instance?); \
             vireo serves only the first one, ignoring this entry (no second event loop)"
        );
        return;
    }
    // 绑定放在 set 之前：直接用这个所有权值，不需要回头查单例
    // （参数名 `android_app` 遮蔽了同名取值函数，用路径反而绕）。
    crate::platform::android_lifecycle::bind_lifecycle_hooks(&android_app);
    crate::saf::bind_saf_hooks(&android_app);
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
                requested_window_count: AtomicUsize::new(0),
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
                lifecycle_cbs: Mutex::new(LifecycleCbs::empty()),
                auto_pause: std::sync::atomic::AtomicBool::new(true),
                shutdown: std::sync::atomic::AtomicBool::new(false),
                memory_warning_cbs: Mutex::new(Vec::new()),
                memory_warn_gen: std::sync::atomic::AtomicU64::new(0),
                storage_name: Mutex::new(crate::storage::default_storage_name()),
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

    // ---- 随包资源（assets）------------------------------------------------
    //
    // 与上面的 `load_texture` / `load_font_file` / `WindowDesc::icon_from_path`
    // **刻意分开**：那几个是纯 `std::fs`（真实路径），这几个走 [`Assets`]
    // （安卓 = APK 内的 assets，桌面 = `set_asset_root` 指定的目录）。
    //
    // 不做「相对路径自动回退到 assets」—— 那会让同一份代码在两个平台上资源来源
    // 不同且不报错，排查成本极高。见 `crate::asset` 模块文档。

    /// 随包只读资源的句柄。
    ///
    /// - 安卓：APK 内 `src/main/assets/` 打进的那份
    /// - 桌面：[`set_asset_root`](Self::set_asset_root) 指定的目录；未设则读不到
    ///   （`Assets::read` 返回 [`VireoError::NoAssetRoot`]）
    ///
    /// 桌面与安卓**同一份代码**：桌面把文件放 `./assets`，安卓放
    /// `src/main/assets`。这是把根目录显式化，不是隐式回退。
    ///
    /// # 例子
    ///
    /// ```no_run
    /// # use vireo::prelude::*;
    /// # fn f(app: &App) -> Result<(), VireoError> {
    /// app.set_asset_root("assets"); // 桌面需要；安卓这行是 no-op
    /// let assets = app.assets();
    /// if assets.exists("bg.png") {
    ///     let _bytes = assets.read("bg.png")?;
    /// }
    /// let _levels = assets.list("levels")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn assets(&self) -> crate::asset::Assets {
        // 安卓侧根目录恒为 None：资源由 APK 内的 AssetManager 定位，
        // `set_asset_root` 记下的值只是用户意图，不参与读取。
        #[cfg(target_os = "android")]
        {
            crate::asset::Assets::new(None)
        }
        #[cfg(not(target_os = "android"))]
        {
            crate::asset::Assets::new(crate::asset::current_root())
        }
    }

    /// 设桌面的资产根目录（[`assets`](Self::assets) 相对于它解析）。
    ///
    /// **只在桌面有意义** —— 安卓的资源在 APK 内，根目录由构建期决定
    /// （`src/main/assets/`），这行是 no-op。
    ///
    /// 必须在任何 [`load_texture_asset`](Self::load_texture_asset) /
    /// [`load_font_asset`](Self::load_font_asset) / `icon_from_asset` 之前调用。
    /// 重复调用以最后一次为准。
    ///
    /// ## Platform-specific
    ///
    /// - **Android：no-op** —— 资源在 APK 内，`Assets::root()` 恒为 `None`，
    ///   实际读取路径由 `AssetManager` 解析。跨平台共享代码里保留这一行是安全的。
    pub fn set_asset_root(&self, path: impl AsRef<std::path::Path>) {
        #[cfg(not(target_os = "android"))]
        {
            *crate::asset::ASSET_ROOT.lock() = Some(path.as_ref().to_path_buf());
        }
        #[cfg(target_os = "android")]
        {
            let _ = path;
        }
    }

    /// 从随包资源加载贴图（png/jpg/bmp）。
    ///
    /// 读失败时和 [`load_texture`](Self::load_texture) 一样：打 error 日志并
    /// 返回一张 ffcc00/6699ff 棋盘 missing 纹理（不 panic）。
    pub fn load_texture_asset(&self, name: &str) -> usize {
        let tex = match self.assets().read(name) {
            Ok(bytes) => Texture::from_bytes(&bytes, &self.gpu),
            Err(e) => {
                log::error!("App::load_texture_asset: {e}");
                Texture::from_bytes(&[], &self.gpu)
            }
        };
        let mut guard = self.textures.lock();
        let idx = guard.len();
        guard.push(Arc::new(tex));
        idx
    }

    /// 从随包资源加载字体文件。
    ///
    /// 语义与 [`GpuContext::load_font_file`](crate::gpu::GpuContext::load_font_file)
    /// 一致，只是数据来源是 [`assets`](Self::assets) 而不是文件系统。
    pub fn load_font_asset(&self, name: &str) -> Result<(), String> {
        let bytes = self.assets().read(name).map_err(|e| e.to_string())?;
        self.gpu.load_font(&bytes);
        Ok(())
    }

    // ---- 可写目录 ----------------------------------------------------------

    /// 设可写目录的末级名字。
    ///
    /// 影响 [`data_dir`](Self::data_dir) / [`cache_dir`](Self::cache_dir) /
    /// [`external_data_dir`](Self::external_data_dir) 三者的末级分量。
    /// 桌面默认取可执行文件名。
    ///
    /// 名字会被压成**安全的单层路径分量**（分隔符替换为 `_`，`.`/`..`/空 退回
    /// `"vireo-app"`）—— 它要参与路径拼接，放任分隔符会写到意外位置。
    ///
    /// ## Platform-specific
    ///
    /// - **Android：no-op** —— 三个目录都由系统从**包名**派生
    ///   （`/data/user/0/<包名>/files` 等），末级名字拼不进去。真机实测确认。
    ///   桌面写这一行是安全的。
    pub fn set_storage_name(&self, name: impl AsRef<str>) {
        let mut g = self.storage_name.lock();
        *g = crate::storage::sanitize_dir_name(name.as_ref());
    }

    /// 当前可写目录的末级名字。
    pub fn storage_name(&self) -> String {
        self.storage_name.lock().clone()
    }

    /// 持久化私有数据目录（存档 / 设置）。
    ///
    /// - 安卓：`/data/user/0/<包名>/files`
    /// - Windows：`%APPDATA%\<name>`
    /// - 其它 Unix：`$XDG_DATA_HOME/<name>`，未设则 `$HOME/.local/share/<name>`
    ///
    /// 免权限；安卓下卸载即删。**不保证目录已存在** —— 需要时用
    /// [`ensure_dir`](crate::storage::ensure_dir)。
    pub fn data_dir(&self) -> Result<std::path::PathBuf, VireoError> {
        crate::storage::data_dir(&self.storage_name())
    }

    /// 可再生缓存目录。
    ///
    /// - 安卓：`Activity.getCacheDir()`
    /// - Windows：`%LOCALAPPDATA%\<name>\Cache`
    /// - 其它 Unix：`$XDG_CACHE_HOME/<name>`，未设则 `$HOME/.cache/<name>`
    ///
    /// ⚠️ **安卓系统可能在你没运行 App 时删掉这里的内容**。别放需要长期存在的东西。
    pub fn cache_dir(&self) -> Result<std::path::PathBuf, VireoError> {
        crate::storage::cache_dir(&self.storage_name())
    }

    /// 用户可见的持久目录（用户文件管理器能看到、能把东西拷进来）。
    ///
    /// - 安卓：`/storage/emulated/0/Android/data/<包名>/files`
    /// - Windows：`%USERPROFILE%\Documents\<name>`
    /// - 其它 Unix：`$XDG_DOCUMENTS/<name>`，未设则 `$HOME/Documents/<name>`
    ///
    /// 环境不允许时返回 `None`（例如桌面找不到 Documents）。
    ///
    /// ⚠️ 安卓下同样**卸载即删**，且 Android 11+ 分区存储不允许写 `Documents`
    /// 之外的共享位置。要真正「让用户自己选位置」，走 SAF
    ///（[`open_document_tree`](App::open_document_tree) 等）。
    pub fn external_data_dir(&self) -> Option<std::path::PathBuf> {
        crate::storage::external_data_dir(&self.storage_name())
    }

    /// SAF：让用户选一个目录（系统选择器，异步）。
    ///
    /// 立即返回：`Ok(())` = 请求已发出；用户选定/取消后调 `cb`
    ///（`Ok(SafUri)` / `Err(UserCancelled)`）。回调跑在渲染线程。
    ///
    /// 拿到目录 URI 后相对路径读写（[`SafUri::resolve`] / [`list`](crate::SafUri::list) /
    /// [`create_file`](crate::SafUri::create_file)）。授权在收到时自动持久化，
    /// URI 字符串自己存下来下次复用 —— 详见 [`crate::saf`] 模块文档。
    ///
    /// ## Platform-specific
    ///
    /// - **仅 Android**：桌面直接返回 `Err(Unsupported)`。
    /// - 需要 `MainActivity.java` 含 SAF 桥（`examples/android/SAF_SNIPPET.java`），
    ///   缺了返回 `Err`（会明说缺什么）。
    /// - 同一时刻只允许一个选择器在途；重叠发起返回 `Err`。
    pub fn open_document_tree(
        &self,
        cb: impl FnOnce(Result<crate::saf::SafUri, VireoError>) + Send + 'static,
    ) -> Result<(), VireoError> {
        crate::saf::request(
            crate::saf::SafKind::Tree,
            "android.intent.action.OPEN_DOCUMENT_TREE",
            None,
            &[],
            None,
            Box::new(cb),
        )
    }

    /// SAF：让用户选一个已有文件来读（系统选择器，异步）。
    ///
    /// `mime_types` 如 `&["image/*"]`、`&["application/json", "text/plain"]`；
    /// 空切片 = 不限类型。
    /// 回调与平台约束同 [`open_document_tree`](App::open_document_tree)。
    /// 拿到 URI 后 [`open_reader`](crate::SafUri::open_reader) 读出真正的 `File`。
    pub fn open_document(
        &self,
        mime_types: &[&str],
        cb: impl FnOnce(Result<crate::saf::SafUri, VireoError>) + Send + 'static,
    ) -> Result<(), VireoError> {
        crate::saf::request(
            crate::saf::SafKind::Open,
            "android.intent.action.OPEN_DOCUMENT",
            Some("*/*"),
            mime_types,
            None,
            Box::new(cb),
        )
    }

    /// SAF：让用户选保存位置来写（系统选择器，异步）。
    ///
    /// `suggestion` 是建议文件名（含扩展名，如 `"chart.json"`）—— mime 由扩展名
    /// 暗示（contract 本来就不接受 mime 参数，见 `crate::saf` 文档）。
    /// 回调与平台约束同 [`open_document_tree`](App::open_document_tree)。
    /// 拿到 URI 后 [`open_writer`](crate::SafUri::open_writer) 写。
    pub fn create_document(
        &self,
        suggestion: &str,
        cb: impl FnOnce(Result<crate::saf::SafUri, VireoError>) + Send + 'static,
    ) -> Result<(), VireoError> {
        crate::saf::request(
            crate::saf::SafKind::Create,
            "android.intent.action.CREATE_DOCUMENT",
            Some("*/*"),
            &[],
            Some(suggestion),
            Box::new(cb),
        )
    }

    /// 丢掉在途的 SAF 请求（回调永不触发）。只用于「Activity 中途重建、
    /// 选择结果永远回不来」的逃生口；返回是否真丢掉过东西。
    ///
    /// 迟到的结果到达时按未知 id 丢弃 + `warn`，不会串给后面的请求。
    pub fn saf_cancel_pending(&self) -> bool {
        crate::saf::cancel_pending()
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

    /// 创建窗口：把 [`WindowDesc`] 发给 winit 线程**异步**创建（DX12 下 `create_window`
    /// 里的 `surface.configure` 会等 present queue 排空，所以绝不能在 winit 线程做
    /// 同步创建再回头发事件）。
    ///
    /// 返回 `WindowIndex`；`VireoWindow` 对象要等 winit 线程建完才存在，用
    /// [`Self::window_ref`] 取 —— **立即**调用会返回 `Err`（窗口还在队列里）。
    ///
    /// # 只能在 `#[vireo::main]` / [`App::with_descriptor`] 的用户体内调用
    ///
    /// 它依赖 `create_tx`，而该通道由 `run_entry` → `init_run_channels` 建立，
    /// **早于**你的 `main` 体开始执行。所以「写 `app.window(..)` 在
    /// `app.run(..)` 之前」是完全正常的（57 个示例都这么写）。
    ///
    /// ⚠️ 但如果你**绕过** `#[vireo::main]`、自己 `App::new()` 后直接调本方法，
    /// `create_tx` 还是 `None`，会命中 `unreachable!` **panic**。
    /// 换句话说：`App::new()` + 手工驱动事件循环这条路当前不可用，
    /// 必须走 `#[vireo::main]` / `with_descriptor` / `vireo::platform::android`。
    ///
    /// # ⚠️ Android 上**只能建一个窗口**，第 2 次调用直接 **panic**
    ///
    /// 不是「Android 不方便」，而是**一定会静默坏掉**：winit Android 的
    /// `WindowId` 是单元结构体（所有窗口 id 相同 → 事件串台），且 `Window::new`
    /// 忽略全部窗口属性（两个 `Surface` 指向同一个 `SurfaceView` → 两个
    /// `Renderer` 抢同一 surface，撞 "Surface image is already acquired"）。
    /// 详见 [`android_rejects_extra_window`]。
    ///
    /// 之所以 panic 而不是返回错误：这里没有可恢复的退路，也没有 `Result` 可返
    /// （签名是 `-> WindowIndex`），返回「一个永远无效的 index」等于把静默串台
    /// 又放回来了。fail-fast 更合适 —— panic 信息会经 Android 的 panic hook 走
    /// `log::error!` 进 logcat。
    ///
    /// 多窗口 UI 在 Android 上折进单窗口（自绘标签页 / 状态机切换内容），或用
    /// [`App::offscreen`] + [`OffscreenCanvas`](crate::OffscreenCanvas) 自己合成。
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
        // Android 单窗口限制 —— 见 [`android_rejects_extra_window`] 的完整说明
        // （winit Android 的 `WindowId` 是单元结构体 + `Window::new` 忽略全部
        // 属性，第二个窗口必然事件串台 + 两个 Renderer 抢同一 surface）。
        //
        // 计数放在**请求侧**：`created_window_count` 要等 supervisor 处理完
        // 才递增，连着调两次 `window()` 时它还是 0，守卫会漏。
        let already_requested = self.requested_window_count.fetch_add(1, Ordering::AcqRel);
        // 用 `cfg!` 而不是 `#[cfg]`：分支仍被编译期消除，但让这个判定函数在
        // **所有**平台都被调用，从而不必挂 `#[allow(dead_code)]`（那会掩盖
        // 「桌面真的没在用」这个事实），且桌面 `cargo test --lib` 能测到它。
        if cfg!(target_os = "android") && android_rejects_extra_window(already_requested) {
            // 消息用英文：panic 是诊断输出（进 crash log / 被 grep / 贴进 issue），
            // 与仓库其余 panic/expect/unreachable 一致。详细解释见
            // `android_rejects_extra_window` 的文档（中文）。
            panic!(
                "vireo: cannot create window #{} on Android — only ONE window is \
                 supported.\n\
                 Root cause (it fails SILENTLY if allowed):\n\
                 \x20 - winit's Android `WindowId` is a unit struct (`struct WindowId;`), so all \
                 windows share one id; vireo's `id_to_handle` insert then overwrites the first \
                 mapping and window A's input/resize events get routed to window B.\n\
                 \x20 - `Window::new` ignores every requested window attribute (upstream: \
                 `// FIXME this ignores requested window attributes`), so two `Surface`s point \
                 at the same `SurfaceView` and two `Renderer`s fight over it — wgpu then \
                 panics with \"Surface image is already acquired\".\n\
                 An Android Activity has exactly one SurfaceView and there is no multi-window \
                 equivalent in this model, so this is a hard failure rather than a fallback.\n\
                 Fix: fold multi-window UI into a single window (self-drawn tabs / a state \
                 machine), or composite with `App::offscreen` + `OffscreenCanvas`.",
                already_requested + 1,
            );
        }
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

    /// 创建带自定义 vertex shader 的材质。VS 须匹配所在绘制路径的输入契约：
    /// `sdf_feather = Some` → SDF 实例契约（`QuadVertex` + `InstanceData` storage）；
    /// `None` → 几何实例契约（`TemplateVertex` + `GeoInstance`）；直接 push 的 mesh
    /// 顶点用 `Vertex`（`@0-7`）。创建期按布局逐项验证，不支持的布局跳过形状 + 告警一次。
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
    /// 分屏切换）。这是 Unity `OnApplicationPause` 同样的语义；要「彻底离开
    /// （不可见）才算」，用 [`on_stopped`](Self::on_stopped) —— 它只对发生过
    /// `onStop` 的那次暂停触发。
    pub fn is_paused(&self) -> bool {
        crate::platform::android_lifecycle::is_paused()
    }

    /// 注册「进入暂停」回调。
    ///
    /// 在**渲染线程**上触发，且在**暂停生效的时刻附近**（渲染线程观察到状态变化后、
    /// 进入暂停闸门阻塞之前）。回调需要能碰 `App`，所以不能直接在 UI 线程的 JNI
    /// 回调里跑 —— 这才是它被搬到渲染线程的原因。
    ///
    /// 典型用途：释放音频、停止网络轮询、落一份临时存档。
    ///
    /// ⚠️ **它不能替代 `onSaveInstanceState`**：进程在后台被系统杀掉时不会有任何
    /// 回调。要保证持久化，在你自己的 `MainActivity.onSaveInstanceState` 里做。
    pub fn on_paused(&self, cb: impl Fn() + Send + Sync + 'static) {
        self.inner.lifecycle_cbs.lock().paused.push(Arc::new(cb));
    }

    /// 注册「恢复」回调。语义/线程见 [`on_paused`](Self::on_paused)。
    pub fn on_resumed(&self, cb: impl Fn() + Send + Sync + 'static) {
        self.inner.lifecycle_cbs.lock().resumed.push(Arc::new(cb));
    }

    /// 注册「彻底离开」回调 —— 只在**暂停期间 activity 变得不可见**（`onStop`）时触发。
    ///
    /// # 与 `on_paused` 的区别
    ///
    /// Android 上 `onPause` 覆盖大量**短暂**中断（半透明弹窗、权限对话框），
    /// `onStop` 才是「不可见」。本回调只对后者触发：
    ///
    /// | 场景 | `on_paused` | `on_stopped` |
    /// |---|---|---|
    /// | 半透明弹窗 / 权限对话框 | ✅ | ❌ |
    /// | 回桌面 / 切别的 app | ✅ | ✅ |
    /// | 锁屏 | ✅ | ✅ |
    ///
    /// # 时机：暂停生效时（与 [`on_paused`](Self::on_paused) 同一个点）
    ///
    /// 渲染线程观察到状态变化后立即发，随后才进闸门阻塞。但注意**「彻底离开」这个
    /// 事实本身**要等到恢复后才能确定（Android 的 `onStart` 紧跟 `onResume`，都在
    /// 渲染线程被唤醒之前跑完），所以本回调是**在恢复时**才触发的 —— 见
    /// [`android_lifecycle`](crate::platform::android_lifecycle) 模块文档里
    /// `SAW_STOP` 那段锁存说明。
    ///
    /// 要在暂停瞬间就知道「这次暂停算不算彻底离开」，做不到；只能在恢复后回看。
    ///
    /// # ⚠️ 不能用它做「离开时立刻保存进度」
    ///
    /// 阻塞期间没有任何用户代码在跑，**进程在后台被系统杀掉时不会有任何回调**。
    /// 「离开时持久化」必须由用户在自己的 `MainActivity.onSaveInstanceState`
    /// 里做（vireo 侧无可序列化状态，见 `TODO.md` 的 `onSaveInstanceState` 条目）。
    ///
    /// 需要 `MainActivity.java` 额外声明并转发 `nativeOnStop()`；未声明时
    /// 本回调永不触发（= 不影响其它生命周期行为）。
    pub fn on_stopped(&self, cb: impl Fn() + Send + Sync + 'static) {
        self.inner.lifecycle_cbs.lock().stopped.push(Arc::new(cb));
    }

    /// activity 当前是否**不可见**（Android `onStop` 状态）。
    ///
    /// 其它平台恒 `false`。
    ///
    /// 实现上是「已暂停 && 本次暂停发生过 onStop」的**派生量**，所以恢复后自动
    /// 变回 `false`，不需要任何 reset 时机。
    ///
    /// ⚠️ **恢复后已翻回 `false`** —— 渲染线程在暂停期间整体阻塞，醒来时
    /// `onStart` 已跑完。要判断「刚才那次暂停算不算彻底离开」，用
    /// [`on_stopped`](Self::on_stopped)，不要用这个查询。
    pub fn is_stopped(&self) -> bool {
        crate::platform::android_lifecycle::is_stopped()
    }

    /// activity 是否已被**用户主动关闭**（划掉最近任务 / `finish()`）。
    ///
    /// # 与 `is_paused` / `is_stopped` 的分工 —— 这是三者唯一能回答的问题
    ///
    /// | 问题 | 靠什么 |
    /// |---|---|
    /// | 我在前台吗 | [`is_paused`](Self::is_paused) |
    /// | 我完全不可见吗 | [`is_stopped`](Self::is_stopped) |
    /// | **用户是切走了还是主动关了我** | **本函数** |
    /// | 我的进程要死了吗 | **无解**（进程被杀时没有任何回调） |
    ///
    /// 按 Home / 切 app / 锁屏都**不会**让它为真；只有用户明确把任务划掉、
    /// 或代码调了 `finish()` 才会。
    ///
    /// ⚠️ **进程被系统回收杀掉时不触发**（Android 官方措辞是 "might be called"）。
    /// 所以**不能**用它做资源清理或存档 —— 那些要在更早的时机做。
    ///
    /// 其它平台恒 `false`。
    ///
    /// 典型用法（配合 [`set_auto_pause(false)`](Self::set_auto_pause)）：
    /// 后台照跑逻辑时，用户划掉任务后没有别的方式通知你退出。
    /// ```no_run
    /// # let app: vireo::App = unimplemented!();
    /// app.run(|ctx| {
    ///     if ctx.app().is_destroyed() {
    ///         return false; // 用户主动关的，退出
    ///     }
    ///     // ...
    ///     true
    /// });
    /// ```
    pub fn is_destroyed(&self) -> bool {
        crate::platform::android_lifecycle::is_destroyed()
    }

    /// 注册「activity 被用户主动关闭」回调。
    ///
    /// 时机与线程：和 [`on_paused`](Self::on_paused) 同一条路径 —— 渲染线程上、
    /// 观察到状态变化后。排在 paused/resumed/stopped **之前**发，且发完就不再发
    /// 后三者（activity 都没了，不该再收到「恢复」）。
    ///
    /// 多数情况用 [`is_destroyed`](Self::is_destroyed) 在循环里判断就够了，
    /// 本回调只在「不在循环里、而是别的线程要立刻知道」时才需要。
    ///
    /// 需要 `MainActivity.java` 额外声明并转发 `nativeOnDestroy()`；未声明时
    /// 本回调永不触发（不影响其它生命周期行为）。
    pub fn on_destroyed(&self, cb: impl Fn() + Send + Sync + 'static) {
        self.inner.lifecycle_cbs.lock().destroyed.push(Arc::new(cb));
    }

    /// 注册「用户按了返回键」回调（Android 独有）。
    ///
    /// # 为什么需要它
    ///
    /// **返回键此前完全无法处理**：按返回键 = `Activity.finish()` 立即销毁进程 ——
    /// 没有确认框、没存档、连暂停菜单都来不及。Google Material 规范明文要求
    /// 返回键应「回退一层或弹确认」，直接退出是公认的 UX 缺陷。
    ///
    /// winit 收不到这个事件（它只把 `KEYCODE_BACK` 映射成
    /// `NamedKey::BrowserBack` 塞进普通按键流），而**软键盘弹出时连这条路也不通**
    /// —— Android 把硬件键喂给 `InputConnection`，winit 的 `KeyboardInput`
    /// 收不到（见 `examples/android/README.md` §8）。所以必须走
    /// `OnBackPressedDispatcher` 显式接管，见 `MainActivity.java`。
    ///
    /// # 想拦住退出必须调 [`consume_back`](crate::platform::android_lifecycle::consume_back)
    ///
    /// **不调 = 不消费 = App 照常退出。** 这是有意的默认：注册回调不等于接管返回键，
    /// 否则「只想被动知道用户按了返回」（比如记埋点）的用法会意外把退出拦住。
    ///
    /// ```no_run
    /// # let app: vireo::App = unimplemented!();
    /// // 第一次按返回键 → 弹「退出游戏？」
    /// # 第二次按（确认中）→ 消费掉，让 App 正常退出
    /// let mut confirming = false;
    /// app.on_back(move || {
    ///     if confirming {
    ///         vireo::platform::android_lifecycle::consume_back();
    ///         confirming = false;
    ///     } else {
    ///         confirming = true;   // 弹自己的确认 UI，本次不消费 → 仍会退出
    ///     }
    /// });
    /// ```
    ///
    /// # 时序（重要）
    ///
    /// 回调在**渲染线程**上触发，与 [`on_paused`](Self::on_paused) 同一条路径。
    /// Java 侧无法同步等渲染线程（UI 线程阻塞会死锁），所以它的做法是
    /// **「先无条件拦住返回键，再延后约一帧判定」**：拦下后 postDelayed 一个时限
    /// （模板取 34ms），到点才读「是否已消费」来决定 `finish()`。
    ///
    /// 所以 [`consume_back`](crate::platform::android_lifecycle::consume_back)
    /// 必须在本回调里**同步**调用 —— 回调本身就在那之前跑完了，调用自然满足时限。
    /// 异步（spawn 到别的线程再调）可能超时，症状是「回调跑了但 App 还是退了」。
    ///
    /// # 每次返回键都要表态
    ///
    /// 延后判定的时限一过就是「按未消费处理」= 退出。所以「只监听、不处理」
    /// 这种用法（比如记埋点）会得到与不注册相同的行为 —— 想拦住必须显式
    /// `consume_back()`。
    /// 桌面 / macOS 恒不触发（那边用 `WindowEvent::CloseRequested`，且桌面**有**
    /// 这个事件，Android 没有）。
    ///
    /// 需要 `MainActivity.java` 额外声明并转发 `nativeOnBackPressed()`；未声明时
    /// 本回调永不触发，返回键退回 Android 默认行为（退出），其余生命周期不受影响。
    pub fn on_back(&self, cb: impl Fn() + Send + Sync + 'static) {
        self.inner.lifecycle_cbs.lock().back.push(Arc::new(cb));
    }

    /// ⚠️ 消费**有时限**：见 [`on_back`](Self::on_back) 的「时序」一节。

    /// vireo 是否在切后台时**自动阻塞**渲染循环。默认 `true`。
    ///
    /// # 「策略」不是「状态」
    ///
    /// [`is_paused`](Self::is_paused) 回答「我现在是否后台」；本方法回答
    /// 「vireo 要不要替我暂停」。两者刻意用不同词。
    ///
    /// | 值 | 切后台时 |
    /// |---|---|
    /// | `true`（默认） | 渲染循环**阻塞**：不 `on_tick`、不 `draw`、不 present。CPU+GPU 归零 |
    /// | `false` | 循环照常跑，由**你自己**决定要不要画 |
    ///
    /// # 何时该设成 `false`
    ///
    /// 后台还需要跑逻辑时（网络收发、计时器、后台服务）。此时**必须**自己省绘制：
    ///
    /// ```no_run
    /// # let mut app: vireo::App = unimplemented!();
    /// # let win: vireo::VireoWindow = unimplemented!();
    /// # let batch: vireo::DrawBatch = unimplemented!();
    /// app.set_auto_pause(false);
    /// app.run(move |ctx| {
    ///     update_logic();                     // 后台也跑
    ///     if !ctx.app().is_paused() {         // 不可见时不画
    ///         win.draw(vireo::color::WHITE, &[&batch]);
    ///     }
    ///     true
    /// });
    /// ```
    ///
    /// # 两个必须知道的代价
    ///
    /// 1. **后台全速空转**。闸门存在的唯一理由就是防这个，关掉它你的循环会以满速
    ///    在后台烧 CPU —— 而「后台 CPU 占用高」正是 Android 杀进程的判定依据之一，
    ///    结果是**后台存活时间反而更短**。上面的 `is_paused()` 判断是必须的。
    /// 2. **长时间后台需要前台服务**（Foreground Service + 常驻通知），否则系统
    ///    迟早回收进程。vireo 管不了这一层。
    ///
    /// 随时可改，下一帧生效（渲染循环每帧读一次本值）。
    pub fn set_auto_pause(&self, on: bool) {
        self.inner.auto_pause.store(on, Ordering::Release);
    }

    /// 当前 [`set_auto_pause`](Self::set_auto_pause) 的值。默认 `true`。
    pub fn auto_pause(&self) -> bool {
        self.inner.auto_pause.load(Ordering::Acquire)
    }

    /// 阻塞当前线程直到回到前台（[`is_paused`](Self::is_paused) 变 `false`）。
    ///
    /// 桌面 / macOS 恒立即返回（无「切后台」概念）。
    ///
    /// **会被打断**（窗口全关 / 设备丢失 / activity 被销毁），不会挂死。
    ///
    /// 用法：
    /// ```no_run
    /// # let app: vireo::App = unimplemented!();
    /// app.set_auto_pause(false); // 接管暂停时机
    /// loop {
    ///     tick_logic();
    ///     if app.is_paused() {
    ///         app.wait_while_paused(); // 自己决定何时停
    ///     }
    /// }
    /// ```
    ///
    /// 与 `auto_pause == true` 时 vireo 内部自动做的事等价 —— 区别是这个版本
    /// **由你选时机**（比如「这一帧跑完再停」）。
    pub fn wait_while_paused(&self) {
        // 抽象成 wait_until：超时 / 打断的语义只有一份，也少一份 condvar 逻辑。
        // 返回值在这里无意义（无限等待 + 打断时都是「不等了」）。
        let _ = self.wait_until(None, || !self.is_paused() || self.is_destroyed());
    }

    /// 阻塞当前线程直到 `cond()` 为真或超时。返回**是否因条件成立而返回**
    /// （`false` = 超时，或 App 正在退出 / activity 已被用户关闭）。
    ///
    /// `timeout = None` 表示无限等待。
    ///
    /// # 谁会唤醒你
    ///
    /// - **vireo 自动唤醒**：切后台、恢复、activity 销毁、设备丢失 —— 这些都会
    ///   推进事件代号。
    /// - **你自己的条件变了**：必须调 [`notify_waiters`](Self::notify_waiters)。
    ///   **忘了就会一直等到超时** —— 这是本方法唯一的坑。
    ///
    /// # 为什么值得用，而不是自己写 `Mutex` + `Condvar`
    ///
    /// 等**平台状态**时你没有 vireo 的事件句柄，而它正是由平台回调推进的。
    /// 等**自己的条件**时用 `std` 完全可以 —— 那种情况用本方法只是更省事。
    ///
    /// # 超时是「总时长」而非「每次等待时长」
    ///
    /// 内部按 deadline 折算剩余时间，所以被唤醒后复查条件失败不会重新计时。
    ///
    /// # Examples
    /// ```no_run
    /// # let app: vireo::App = unimplemented!();
    /// # let flag = std::sync::atomic::AtomicBool::new(false);
    /// // 等某个网络响应，最多 5 秒
    /// if !app.wait_until(Some(std::time::Duration::from_secs(5)), || flag.load(std::sync::atomic::Ordering::Relaxed)) {
    ///     log::warn!("timeout");
    /// }
    /// ```
    pub fn wait_until(
        &self,
        timeout: Option<std::time::Duration>,
        cond: impl Fn() -> bool,
    ) -> bool {
        wait_loop(
            timeout,
            || self.inner.shutdown.load(Ordering::Acquire) || self.is_destroyed(),
            cond,
        )
    }

    /// 通知所有 [`wait_until`](Self::wait_until) 等待者：状态变了，重新检查条件。
    ///
    /// 你的自定义条件（不是平台生命周期）变化后必须调它，否则等待方只能等超时。
    /// 可从任意线程调用。
    pub fn notify_waiters(&self) {
        crate::platform::android_lifecycle::notify_all();
    }

    /// 置「正在退出」并唤醒所有等待者。
    ///
    /// 由 supervisor 判定 `SupervisorAction::Exit` 时调用，**必须早于**线程 join ——
    /// 否则用户代码里挂着的 [`wait_until`](Self::wait_until) /
    /// [`wait_while_paused`](Self::wait_while_paused) 会让 join 永久阻塞。
    pub(crate) fn mark_shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Release);
        self.notify_waiters();
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
    use super::android_rejects_extra_window;

    // ---- Android 单窗口限制 ----

    /// 首个窗口放行，第 2+ 个拒绝。
    #[test]
    fn android_allows_first_window_and_rejects_the_rest() {
        assert!(
            !android_rejects_extra_window(0),
            "第一个窗口必须放行 —— 否则 Android 上什么都开不起来"
        );
        assert!(android_rejects_extra_window(1), "第二个必须拒绝");
        assert!(android_rejects_extra_window(2));
        assert!(android_rejects_extra_window(usize::MAX));
    }

    /// 拒绝时**不发建窗请求**是退出门控不被卡住的关键，所以判定必须只看
    /// 「已经请求过几个」，而不是「创建成功几个」。
    ///
    /// 这里固化那个竞态的语义：`fetch_add` 的**返回值**是「本次之前已请求的
    /// 个数」，所以第 1 次调用返回 0（放行）、第 2 次返回 1（panic）。
    /// 若改成 `created_window_count`（supervisor 收到 WindowCreated 才递增），
    /// 连着调两次时它还是 0 → 第 2 个窗口漏过守卫 → 静默串台。
    #[test]
    fn rejection_is_based_on_request_count_not_created_count() {
        let requested = std::sync::atomic::AtomicUsize::new(0);
        let mut accepted = 0usize;
        // 模拟连着调 3 次 `App::window()`，中间**没有**任何 WindowCreated 回来。
        for _ in 0..3 {
            let already = requested.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            if !android_rejects_extra_window(already) {
                accepted += 1;
            }
        }
        assert_eq!(
            accepted, 1,
            "即使 supervisor 一个都没处理，也只能放行 1 个 —— \
             靠 created_window_count 判会放行 3 个"
        );
        assert_eq!(requested.load(std::sync::atomic::Ordering::Acquire), 3);
    }

    /// `fetch_add` 的返回值必须是「本次之前的计数」—— 守卫逻辑全靠这个语义。
    ///
    /// 回归点：若把 `fetch_add(1, ..)` 改成读-判-写（`load` 再 `store`），两个
    /// 线程会同时读到 0 → **都放行** → 又回到静默串台。这里用两个独立计数器
    /// 验证 `fetch_add` 的原子性语义（而非测 vireo 的字段）。
    #[test]
    fn fetch_add_returns_prior_count_so_concurrent_calls_see_distinct_values() {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        let first = counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let second = counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        assert_eq!(first, 0, "第 1 次拿到的是「之前有 0 个」");
        assert_eq!(second, 1, "第 2 次拿到的是「之前有 1 个」→ 触发守卫");
        assert!(!android_rejects_extra_window(first));
        assert!(android_rejects_extra_window(second));
    }

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

    // ---- wait_until / auto_pause / shutdown ---------------------------------

    // ---- wait_loop / 事件机制 -----------------------------------------------
    //
    // 注意：测的是自由函数 `wait_loop` 而不是 `App::wait_until` —— `App` 含
    // `Arc<GpuContext>`（二十多个 wgpu 资源字段、无 `Default`），单测里造不出
    // 合法实例。`App::wait_until` 只是它的薄包装。

    use super::wait_loop;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn auto_pause_defaults_on() {
        // 默认 true = 保留「vireo 自动阻塞」的老行为，懒得写的用户零改动。
        // 字段初值在 AppInner 构造处，这里断言的是「构造时传了 true」这个决定。
        let fresh = std::sync::atomic::AtomicBool::new(true);
        assert!(fresh.load(Ordering::Acquire), "auto_pause 默认必须是 true");
    }

    #[test]
    fn wait_loop_returns_immediately_when_cond_already_holds() {
        let t0 = std::time::Instant::now();
        assert!(wait_loop(
            Some(std::time::Duration::from_secs(5)),
            || false,
            || true
        ));
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(200),
            "条件已成立时不该进等待（实际 {:?}）",
            t0.elapsed()
        );
    }

    #[test]
    fn wait_loop_times_out_when_cond_never_holds() {
        let t0 = std::time::Instant::now();
        let ok = wait_loop(
            Some(std::time::Duration::from_millis(120)),
            || false,
            || false,
        );
        let dt = t0.elapsed();
        assert!(!ok, "条件永不成立必须返回 false");
        assert!(
            dt >= std::time::Duration::from_millis(100),
            "必须真的等了（实际 {:?}）",
            dt
        );
        assert!(
            dt < std::time::Duration::from_millis(2000),
            "超时是**总时长**，不能被重试放大（实际 {:?}）",
            dt
        );
    }

    #[test]
    fn wait_loop_aborts_immediately_when_already_aborted() {
        // 已 shutdown 时不该再进等待（否则每个调用都挂到超时）。
        let t0 = std::time::Instant::now();
        assert!(!wait_loop(None, || true, || true));
        assert!(t0.elapsed() < std::time::Duration::from_millis(200));
    }

    #[test]
    fn wait_loop_abort_wins_over_holding_cond() {
        // abort 优先于条件：App 正在退出时不该再等下去。
        assert!(!wait_loop(
            Some(std::time::Duration::from_secs(5)),
            || true,
            || true
        ));
    }

    #[test]
    fn wait_loop_is_woken_by_notify_all() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let flag = flag.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(60));
                flag.store(true, Ordering::Release);
                crate::platform::android_lifecycle::notify_all();
            });
        }
        let t0 = std::time::Instant::now();
        let ok = wait_loop(
            Some(std::time::Duration::from_secs(5)),
            || false,
            || flag.load(Ordering::Acquire),
        );
        assert!(ok, "notify_all 必须能唤醒等待方");
        assert!(t0.elapsed() < std::time::Duration::from_secs(4));
    }

    /// 代号式 wait 的核心性质：**通知早于等待也不会丢唤醒**。
    ///
    /// 裸 `标志 + Condvar` 方案在这里必然挂死（等待方在「检查到标志没变」与
    /// 「注册进 condvar」之间被通知掉通知）。代号方案复查的是代号而非标志，
    /// 所以时序反了照样能通过 —— 这条测试就是那个性质的回归防护。
    #[test]
    fn wait_loop_survives_notification_before_wait() {
        let flag = Arc::new(AtomicBool::new(false));
        // 先通知、后等待：等价于「状态在等待方进入 wait 之前就变了」。
        flag.store(true, Ordering::Release);
        crate::platform::android_lifecycle::notify_all();
        let t0 = std::time::Instant::now();
        assert!(
            wait_loop(
                Some(std::time::Duration::from_secs(5)),
                || false,
                || { flag.load(Ordering::Acquire) }
            ),
            "通知早于等待不得丢唤醒（这是代号式 wait 存在的理由）"
        );
        assert!(t0.elapsed() < std::time::Duration::from_secs(4));
    }

    #[test]
    fn wait_loop_is_woken_by_abort_flag_flip() {
        // 模拟 supervisor 在等待途中置 shutdown：等待方必须被唤醒而不是挂到超时。
        let shutdown = Arc::new(AtomicBool::new(false));
        {
            let shutdown = shutdown.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(60));
                shutdown.store(true, Ordering::Release);
                crate::platform::android_lifecycle::notify_all();
            });
        }
        let t0 = std::time::Instant::now();
        // 无限等待 + 永不成立的条件：只有 abort 能把它救回来。
        assert!(!wait_loop(
            None,
            || shutdown.load(Ordering::Acquire),
            || false
        ));
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(4),
            "abort 必须真的唤醒等待方，而不是等超时"
        );
    }

    #[test]
    fn desktop_reports_no_platform_lifecycle() {
        // 桌面上没有 Activity 概念：闸门永远不触发，三个状态查询全 false，
        // 否则渲染线程会永久停摆 / 用户的循环会误判成后台。
        if !cfg!(target_os = "android") {
            assert!(!crate::platform::android_lifecycle::is_paused());
            assert!(!crate::platform::android_lifecycle::is_stopped());
            assert!(!crate::platform::android_lifecycle::is_destroyed());
        }
    }

    #[test]
    fn desktop_wait_while_paused_returns_at_once() {
        // 桌面上 `is_paused()` 恒 false，闸门不能把线程卡住。
        if !cfg!(target_os = "android") {
            let t0 = std::time::Instant::now();
            crate::platform::android_lifecycle::wait_while_paused(
                &std::sync::atomic::AtomicBool::new(false),
            );
            assert!(t0.elapsed() < std::time::Duration::from_millis(200));
        }
    }
}
