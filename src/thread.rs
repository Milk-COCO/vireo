//! 多循环（Loop）架构。
//!
//! 设计原则（见 `.opencode/多循环.md`）：`App::run(on_tick)` / `App::spawn(loops)` / `App::loops`
//! 各自在一条独立的 OS 线程上驱动一组 `Loop`。`App` 内部的跨线程字段经 `Lock` / `Atomic*` 包裹
//! （见 `AppInner` / `VireoWindow`），可安全从多条线程并发访问；winit 事件由 owner 线程转发、
//! 经 supervisor 线程集中应用到 `App` 状态（见 `crate::window::supervisor_loop`）。
//!
//! 每条 loop 线程跑 [`run_thread_loop`]，在其中以非阻塞轮询方式驱动多 loop，并在 `App::windows`
//! 就绪（`windows_ready`）后开始；`panic` 经 `LoopHandleState` 转成 `Err` 对外暴露。

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::lock::Lock;
use crate::window::App;
use crate::window::{
    panic_payload_to_string, WinitEvent, DeferredTask, DeferredTaskKind,
};

/// FPS 采样滑动窗口容量（per-thread）。
pub(crate) const FPS_SAMPLE_CAP: usize = 30;

/// per-thread 的 FPS / 帧时间统计。每帧由 [`run_thread_loop`] 调用 [`FpsStats::tick`] 更新；
/// 经 [`LoopContext`] / [`ThreadHandle`] 暴露给调用方。
///
/// 该结构按线程独立存在，不再共享于 `App`（多 `spawn` 真并行时共享 `AtomicU64` 会被重复计数）。
pub(crate) struct FpsStats {
    fps: f64,
    frame_time: f64,
    samples: Vec<f64>,
    last: std::time::Instant,
}

impl FpsStats {
    pub(crate) fn new() -> Self {
        Self {
            fps: 0.0,
            frame_time: 0.0,
            samples: Vec::with_capacity(FPS_SAMPLE_CAP),
            last: std::time::Instant::now(),
        }
    }

    /// 推进一帧：记录与上一帧的间隔，更新滑动窗口与 FPS 估计。
    pub(crate) fn tick(&mut self, now: std::time::Instant) {
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        // 过滤异常间隔（首帧 / 切后台 / 调试断点）。
        if dt > 0.0 && dt < 0.5 {
            self.frame_time = dt;
            self.samples.push(dt);
            if self.samples.len() > FPS_SAMPLE_CAP {
                self.samples.remove(0);
            }
            let sum: f64 = self.samples.iter().sum();
            if sum > 0.0 {
                self.fps = self.samples.len() as f64 / sum;
            }
        }
    }

    pub(crate) fn fps(&self) -> f64 {
        self.fps
    }

    pub(crate) fn frame_time(&self) -> f64 {
        self.frame_time
    }
}

/// 一个绘制循环：每帧接收可变的 `LoopContext`，返回 `true` 继续、`false` 退出。
pub struct Loop {
    pub(crate) f: Box<dyn FnMut(&mut LoopContext) -> bool + Send + 'static>,
}

impl Loop {
    /// 新建循环。闭包签名 `|ctx: &mut LoopContext| -> bool`。
    ///
    /// 通过 [`LoopContext::app`] 取回共享的 `App`；`ctx` 同时承载本循环的帧计数 / 延迟任务 /
    /// FPS 统计（本 Thread 的 tick 速率读数，非上限；tick 上限见 [`Thread::max_tps`]）。
    pub fn new<F>(f: F) -> Self
    where
        F: FnMut(&mut LoopContext) -> bool + Send + 'static,
    {
        Loop { f: Box::new(f) }
    }
}

/// 循环上下文：每帧构造、随循环持久的状态（帧计数 / 延迟任务 / 共享 App / FPS 统计）。
///
/// 与 `App` 上的全局同类 API 对应，但作用域限定在单个循环内（per-loop 计数、per-thread 统计）。
pub struct LoopContext {
    tick_count: u64,
    deferred: Vec<DeferredTask>,
    app: App,
    fps_stats: Arc<Lock<FpsStats>>,
}

impl LoopContext {
    /// 当前帧序号（从 1 开始，跨循环独立计数）。
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// 取回共享的 `App`（等价于此前的闭包首个参数 `&App`）。
    pub fn app(&self) -> &App {
        &self.app
    }

    /// 本循环所属线程的瞬时 FPS（滑动窗口估计）。
    pub fn fps(&self) -> f64 {
        self.fps_stats.borrow().fps()
    }

    /// 本循环所属线程的瞬时帧时间（秒）。
    pub fn frame_time(&self) -> f64 {
        self.fps_stats.borrow().frame_time()
    }

    /// 延迟若干 tick 后执行（计数基于本循环）。
    pub fn after_ticks<F>(&mut self, ticks: u64, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let target = self.tick_count + ticks;
        self.deferred.push(DeferredTask::new(
            DeferredTaskKind::AfterTicks(target),
            Box::new(f),
        ));
    }

    /// 延迟若干秒后执行（墙钟时间）。
    pub fn after_secs<F>(&mut self, secs: f64, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let wakeup = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
        self.deferred.push(DeferredTask::new(
            DeferredTaskKind::AfterSecs(wakeup),
            Box::new(f),
        ));
    }
}

pub(crate) struct LoopRuntime {
    f: Box<dyn FnMut(&mut LoopContext) -> bool + Send + 'static>,
    tick_count: u64,
    deferred: Vec<DeferredTask>,
    finished: bool,
}

impl LoopRuntime {
    pub(crate) fn new(l: Loop) -> Self {
        LoopRuntime {
            f: l.f,
            tick_count: 0,
            deferred: Vec::new(),
            finished: false,
        }
    }
}

/// 驱动所有循环跑一帧：返回 `true` 表示仍有活动循环，`false` 表示全部结束（应退出）。
pub(crate) fn drive_loops(
    runtimes: &mut Vec<LoopRuntime>,
    app: &App,
    fps_stats: &Arc<Lock<FpsStats>>,
) -> Result<bool, Box<dyn std::any::Any + Send>> {
    let mut any = false;
    for i in 0..runtimes.len() {
        let rt = &mut runtimes[i];
        if rt.finished {
            continue;
        }
        let fc = rt.tick_count;
        let mut ctx = LoopContext {
            tick_count: fc + 1,
            deferred: Vec::new(),
            app: app.clone(),
            fps_stats: fps_stats.clone(),
        };
        // 单个 Loop 的 `on_tick` panic：标记本 loop 结束并把 panic 上抛给 `run_thread_loop`，
        // 由其经 `ThreadHandle` 交付错误并停止整条线程（同线程其他 Loop 一并停止）。
        let keep = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (rt.f)(&mut ctx))) {
            Ok(keep) => keep,
            Err(payload) => {
                let msg = if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = payload.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "<non-string panic payload>".to_string()
                };
                eprintln!(
                    "[vireo] loop on_tick panicked (loop {}): {}",
                    i, msg
                );
                rt.finished = true;
                return Err(payload);
            }
        };
        rt.deferred.extend(ctx.deferred.drain(..));
        rt.tick_count = ctx.tick_count;
        // 执行到期延迟任务（帧末，on_tick 之后）。
        let mut j = 0;
        while j < rt.deferred.len() {
            if rt.deferred[j].is_ready(rt.tick_count) {
                let t = rt.deferred.swap_remove(j);
                (t.f)();
            } else {
                j += 1;
            }
        }
        if !keep {
            rt.finished = true;
        } else {
            any = true;
        }
    }
    runtimes.retain(|rt| !rt.finished);
    Ok(any)
}

/// `LoopHandleState` 由 `spawn` 持有，跨线程共享；渲染线程结束时置位唤醒 `await`。
pub(crate) struct LoopHandleState {
    pub(crate) done: std::sync::atomic::AtomicBool,
    pub(crate) result: std::sync::Mutex<Option<Result<(), Box<dyn std::error::Error + Send + Sync>>>>,
    pub(crate) waker: std::sync::Mutex<Option<std::task::Waker>>,
}

impl LoopHandleState {
    pub(crate) fn new() -> Self {
        Self {
            done: std::sync::atomic::AtomicBool::new(false),
            result: std::sync::Mutex::new(None),
            waker: std::sync::Mutex::new(None),
        }
    }
}

/// 一组循环的容器，对应一条 OS 渲染线程。`Thread` 显式持有 `Vec<Loop>`，`spawn` 时整体移入线程。
///
/// 可通过 [`Thread::with_name`] 给本条线程起昵称；未设置时 `spawn` 用
/// `vireo-thread-{n}`（n 为进程内 spawn 创建顺序，从 0 开始单调递增）。昵称只影响线程名
/// （调试器 / 线程 dump 可读性），不改变行为。
pub struct Thread {
    pub(crate) loops: Vec<Loop>,
    pub(crate) name: Option<String>,
    /// 本 Thread 的 tick 速率上限（ticks per second）。
    ///
    /// 一整个 Thread 循环一次称为一个 **tick**（每个 `Loop` 各执行一次，称为该 Loop 的一个 tick，
    /// 而非 frame）。`None` = 不限速；`Some(n)` = 用相位锁 [`pac_advance`] 把 tick 间隔钳到
    /// `1/n` 秒。该上限在 [`App::spawn`] 时由 `App::max_fps` **作为默认值**种子
    /// （未显式设置时取 `app.max_fps()`，运行期改 `App::max_fps` 不影响已 spawn 的 Thread），
    /// 之后由本字段独立决定，App 不再直接控制。
    pub max_tps: Option<u32>,
}

impl Thread {
    pub fn new() -> Self {
        Self {
            loops: Vec::new(),
            name: None,
            max_tps: None,
        }
    }
    pub fn with_loop(mut self, l: Loop) -> Self {
        self.loops.push(l);
        self
    }
    pub fn with_loops(mut self, loops: impl IntoIterator<Item = Loop>) -> Self {
        self.loops.extend(loops);
        self
    }
    /// 设置本 Thread 的 tick 速率上限（覆盖 `App::max_fps` 默认值）。
    pub fn with_max_tps(mut self, tps: impl Into<Option<u32>>) -> Self {
        self.max_tps = tps.into();
        self
    }
    pub fn push(&mut self, l: Loop) {
        self.loops.push(l);
    }
    pub fn extend(&mut self, loops: impl IntoIterator<Item = Loop>) {
        self.loops.extend(loops);
    }
    /// 给本条线程起昵称（对应一条 OS 线程，而非单个 `Loop`）。
    ///
    /// 未调用时 `spawn` 使用默认名 `vireo-thread-{n}`（n 取决于 spawn 创建顺序）。
    pub fn with_name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// `Thread` 对应的句柄，可 `.await` 等待本组所有循环结束。支持运行期通过 `push`/`extend` 动态追加循环。
pub struct ThreadHandle {
    pub(crate) state: Arc<LoopHandleState>,
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
    pub(crate) shared: Arc<std::sync::Mutex<Vec<Loop>>>,
    pub(crate) fps_stats: Arc<Lock<FpsStats>>,
}

impl ThreadHandle {
    /// 向本线程追加单个循环（运行期，跨线程安全）。
    pub fn push(&self, l: Loop) {
        self.shared.lock().unwrap().push(l);
    }
    /// 向本线程追加多个循环（运行期）。
    pub fn extend(&self, loops: impl IntoIterator<Item = Loop>) {
        self.shared.lock().unwrap().extend(loops);
    }

    /// 本组循环所属线程的瞬时 FPS（滑动窗口估计）。
    pub fn fps(&self) -> f64 {
        self.fps_stats.borrow().fps()
    }

    /// 本组循环所属线程的瞬时帧时间（秒）。
    pub fn frame_time(&self) -> f64 {
        self.fps_stats.borrow().frame_time()
    }
}

impl std::future::Future for ThreadHandle {
    type Output = Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        // `done` 检查与 waker 登记必须在同一把锁内，避免与渲染线程 `done.store(true)+wake()`
        // 交错导致丢失唤醒：`wake()` 在锁内取走 waker，若此处先读 `done` 再在锁内存 waker，
        // 渲染线程可能在两次操作之间完成 store+wake（此时 waker 尚为 None）→ 本端存完 waker 后
        // 永久 Pending。
        let mut g = this.state.waker.lock().unwrap();
        if this.state.done.load(std::sync::atomic::Ordering::SeqCst) {
            drop(g);
            if let Some(t) = this.thread.take() {
                let _ = t.join();
            }
            let r = this.state.result.lock().unwrap().take().unwrap_or_else(|| Ok(()));
            std::task::Poll::Ready(r)
        } else {
            *g = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}

impl Drop for ThreadHandle {
    /// 句柄被丢弃时 join 渲染线程：使非阻塞入口（如 `app.run(...)`）返回的临时句柄在语句末析构时
    /// 阻塞主线程直到窗口关闭，从而维持进程存活（无需示例显式 `.await` / `.wait()`）。
    /// `Future::poll` 完成时已 `take` 掉 `thread`，此处 `take` 为 `None`，不会重复 join。
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 唤醒等待本 loop 线程结束的 `ThreadHandle::await`（若有）。
fn wake(state: &Arc<LoopHandleState>) {
    if let Some(w) = state.waker.lock().unwrap().take() {
        w.wake();
    }
}

/// 单条 loop 线程的主驱动：以非阻塞轮询方式跑所给 `Loop` 组，直到全部结束或设备丢失。
///
/// - 由 [`crate::window::App::spawn`] 在独立 OS 线程上调用，对应一条 `ThreadHandle`。
/// - `App` 内部的跨线程字段经 `Lock` / `Atomic*` 包裹，可安全从多线程访问；窗口事件已由
///   supervisor 线程集中应用到 `App`（见 `crate::window::supervisor_loop`），故此处只读 `app.windows`。


/// - `panic` 被捕获并经由 `LoopHandleState.result` 转成 `Err`，使 `ThreadHandle::await` 返回错误，
///   而非向上炸毁进程。
pub(crate) fn run_thread_loop(
    app: App,
    loops: Vec<Loop>,
    state: Arc<LoopHandleState>,
    fps_stats: Arc<Lock<FpsStats>>,
    shared: Arc<Mutex<Vec<Loop>>>,
    device_lost: Arc<AtomicBool>,
    max_tps: Option<u32>,
) {
    let result: std::thread::Result<()> = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut runtimes: Vec<LoopRuntime> = loops.into_iter().map(LoopRuntime::new).collect();
        let mut tick_deadline: Option<std::time::Instant> = None;
        while !state.done.load(Ordering::Acquire) {
            // 动态追加的运行期 loop（跨线程安全）。
            {
                let mut extra = shared.lock().unwrap();
                if !extra.is_empty() {
                    for l in extra.drain(..) {
                        runtimes.push(LoopRuntime::new(l));
                    }
                }
            }
            // 窗口尚未就绪（winit owner 仍在建窗）或仍有运行期建窗在途 → 事件化等待：
            // supervisor 在 `windows_ready` 置位 / `pending_window_creates` 递减时 `notify_all`，
            // 取代原先 `yield_now` 自旋 / 固定间隔 `sleep`（无魔法数字、无忙等）。
            {
                let wake = &app.inner.loop_wake;
                let mut guard = wake.0.lock().unwrap();
                while !app.windows_ready.load(Ordering::Acquire)
                    || app.pending_window_creates.load(Ordering::Acquire) > 0
                {
                    if device_lost.load(Ordering::Acquire) {
                        // 唤醒 supervisor 重新判定退出（设备丢失分支）。
                        if let Some(tx) = app.inner.event_tx.borrow().clone() {
                            let _ = tx.send(WinitEvent::Wake);
                        }
                        return;
                    }
                    guard = wake.1.wait(guard).unwrap();
                }
            }
            if device_lost.load(Ordering::Acquire) {
                if let Some(tx) = app.inner.event_tx.borrow().clone() {
                    let _ = tx.send(WinitEvent::Wake);
                }
                break;
            }
            let any = match drive_loops(&mut runtimes, &app, &fps_stats) {
                Ok(any) => any,
                Err(payload) => {
                    // 单个 Loop panic：经 ThreadHandle 交付错误并停止整条线程（同线程其他 Loop 一并停止）。
                    state
                        .result
                        .lock()
                        .unwrap()
                        .replace(Err(panic_payload_to_string(payload).into()));
                    state.done.store(true, Ordering::Release);
                    wake(&state);
                    return;
                }
            };
            fps_stats.borrow_mut().tick(std::time::Instant::now());
            // 所有窗口已创建且现已全部关闭（关窗后 `on_tick` 仍可能返回 `true`）→ 独立于
            // `on_tick` 返回值退出，与 supervisor 的 `all_windows_closed` 判定对齐，避免进程空转不退出。
            if app.created_window_count() > 0
                && app.window_count() == 0
                && app.pending_window_creates.load(Ordering::Acquire) == 0
            {
                break;
            }
            if !any {
                // `on_tick` 返回 false（或已无窗口）。若仍有「已注册但未建出」的窗口
                // （运行期建窗 / ez 竞态），回到循环顶事件化等待 `WindowCreated` 到达后再正常走；
                // 否则退出（窗口均已就绪，pending 已为 0）。
                if app.pending_window_creates.load(Ordering::Acquire) > 0 {
                    continue;
                }
                break;
            }
            // 整轮 Thread tick 的速率上限（tps）。一整个 Thread 循环一次 = 一个 tick；
            // `max_tps` 由 `App::spawn` 时以 `App::max_fps` 作为默认值种子，运行期独立生效。
            // 相位锁 `pac_advance` 无条件应用：正常 tick（有窗口绘制）与空闲 tick（均 Skipped）
            // 都受同一上限约束，无魔法数字、不硬编码速率；各窗口自身的 `max_fps` 作为更细的
            // 逐窗口覆盖仍生效。用户亦可在 `on_frame` 内据 `render_advice()` 自行暂停（返回 `false`）。
            {
                let now = std::time::Instant::now();
                let (next, sleep_dur) = crate::window::pac_advance(now, tick_deadline, max_tps);
                tick_deadline = next;
                if let Some(s) = sleep_dur {
                    std::thread::sleep(s);
                }
            }
            if device_lost.load(Ordering::Acquire) {
                // 唤醒 supervisor 重新判定退出（设备丢失分支）。
                if let Some(tx) = app.inner.event_tx.borrow().clone() {
                    let _ = tx.send(WinitEvent::Wake);
                }
                break;
            }
            // 不 yield：下轮要么 condvar wait（无事件时阻塞），要么 draw（acquire 阻塞等 vsync），
            // yield_now 只是白白做一次内核上下文切换。
        }
    }));

    match result {
        Ok(()) => {
            state.done.store(true, Ordering::Release);
            wake(&state);
            // loop 正常完成 → 唤醒 supervisor 重新判定退出（事件驱动，取代固定间隔 sleep）。
            if let Some(tx) = app.inner.event_tx.borrow().clone() {
                let _ = tx.send(WinitEvent::Wake);
            }
        }
        Err(payload) => {
            state
                .result
                .lock()
                .unwrap()
                .replace(Err(panic_payload_to_string(payload).into()));
            state.done.store(true, Ordering::Release);
            wake(&state);
            // loop panic → 同样唤醒 supervisor 重新判定退出。
            if let Some(tx) = app.inner.event_tx.borrow().clone() {
                let _ = tx.send(WinitEvent::Wake);
            }
        }
    }

    // 渲染线程退出（正常或 panic）→ 不再有任何 draw，所有窗口的 `draw_idle` 永久为真；
    // 唤醒正在等待关闭的 supervisor 路径，否则其 Condvar 等待会因本线程已死而永不唤醒
    // （最坏情况下渲染线程崩溃、当前帧未置 idle，关窗路径也据此安全放行）。
    for win in app
        .windows
        .borrow()
        .iter()
        .filter_map(|o| o.clone())
    {
        win.draw_idle.store(true, Ordering::Release);
        win.draw_idle_cv.1.notify_all();
    }
}

