//! Android 生命周期桥（`onPause` / `onResume` / `onStop`）。
//!
//! # 为什么需要这个模块
//!
//! winit 的 Android 后端**注册了** Activity 生命周期回调，但绝大多数变体
//! **不发事件**给应用 —— `MainEvent::Stop` / `Destroy` / `SaveState` 走的都是
//! `warn!("TODO: ...")`，`Pause` 也只是设 `self.running = false` 不转发。
//! （唯一例外是 `LowMemory`：winit `platform_impl/android/mod.rs:265-267`
//! 把它转成了跨平台的 `Event::MemoryWarning`，所以内存警告**不需要**本模块的
//! JNI 桥，`Runner::memory_warning` 接一下即可。）
//!
//! 结果：vireo 收不到「我进后台了」，切后台后继续全速渲染，直到被系统杀进程。
//!
//! # `onStop` 为什么要单独桥，而不是只用 `onPause`
//!
//! Android 官方语义里两者不是一回事：
//!
//! | 场景 | `onPause` | `onStop` |
//! |---|---|---|
//! | 回桌面 / 切别的 app | ✅ | ✅ |
//! | 锁屏 | ✅ | ✅ |
//! | 半透明弹窗、权限对话框盖在上面 | ✅ | ❌ |
//!
//! 所以 `onStop` 才是「彻底离开（不可见）」的信号，`onPause` 也包含大量短暂中断。
//!
//! # 但 `onStop` **不能**当「离开时立刻干活」的钩子 —— 「是否彻底离开」是**事后**才知道的
//!
//! 渲染线程在暂停期间**整体阻塞**（见 `src/thread.rs` 的生命周期闸门：不 `on_tick`、
//! 不 `draw`、不 acquire/present，为的是把后台的 CPU+GPU 占用降到零）。加上 Android 的
//! 顺序是 `onPause → onStop → … → onStart → onResume`，而 `onStart` **紧挨**着
//! `onResume`、都在渲染线程被唤醒之前跑完，于是：
//!
//! - 暂停期间**没有任何用户代码在跑** —— 「离开时立刻保存进度」做不到；
//! - 恢复后 `is_paused()` 已经是 `false`，**实时标志也读不出「刚才那次暂停算不算
//!   彻底离开」**。
//!
//! 因此本模块用**锁存** `SAW_STOP` 记住「那次暂停期间发生过 onStop」，让
//! [`App::on_stopped`](crate::App::on_stopped) 在恢复时能区分两种暂停 —— 这才是
//! 本条唯一能兑现的价值。**进程在后台被系统杀掉时不会有任何回调**，
//! 「离开时持久化」必须由用户在自己的 `MainActivity.onSaveInstanceState` 里做
//! （见 `TODO.md` 的 `onSaveInstanceState` 条目）。
//!
//! （`on_paused` / `on_resumed` 是**实时**的，不依赖锁存：渲染线程在**进入闸门之前**
//! 先判一次代号，那一刻 `is_paused()` 还是 `true`，所以 `on_paused` 真的会在暂停
//! 生效时触发。这个顺序是必须的 —— 反过来就永远只走 resumed 分支。）
//!
//! # 为什么走「Java 转发」而不是别的办法
//!
//! | 方案 | 成本 | 问题 |
//! |---|---|---|
//! | 降低 Android 默认 `max_fps` | 几行 | **没解决问题** —— 后台仍全速，只是变慢；还拖累前台 |
//! | Rust 侧注册 `ActivityLifecycleCallbacks` | 大 | 要用 `java.lang.reflect.Proxy` 动态实现接口，且回调在 UI 线程 |
//! | **Java 侧转发（本模块）** | 小 | `MainActivity` 本来就是用户的代码，加几个 override |
//!
//! 选第三条：`MainActivity.java` 是 vireo 构建产物里的宿主类（见 `examples/android/`），
//! 加几个 override 是零成本，换来的是**真正准确**的生命周期信号。
//!
//! # 符号绑定：为什么用 `RegisterNatives` 而不是 JNI 命名约定
//!
//! JNI 命名约定要求符号名形如 `Java_<包名>_<类名>_<方法名>` —— 这把 **Java 包名**
//! 编进了 Rust 侧的导出名。用户改 `applicationId` 就得同步改 Rust 常量，忘了就
//! 在运行时找不到符号（而且不报编译错）。
//!
//! `RegisterNatives` 换一种做法：Java 侧只声明
//! `private static native void nativeOnPause();`（名字与包名无关），Rust 侧在运行时
//! 把「方法名 + 签名 → **函数指针**」这张表交给 JVM。
//!
//! 因为是直接传函数指针，所以这里**不需要** `#[no_mangle]` 导出、也不需要维护
//! C 符号名 —— 只有「Java 方法名」这一个契约要同步，改包名/类名完全不受影响。
//!
//! # 绑定时机
//!
//! 在 `bind_lifecycle_hooks` 里做，即 `android_main` 期间 —— 那时 `MainActivity`
//! 已被 AGSK 加载（`android_main` 正是它调用的），而 `onResume` / `onPause` 必然在
//! `onCreate` 之后，不会漏。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// 进程级「已暂停」标志。由 JNI 回调（UI 线程）写，多线程读。
static PAUSED: AtomicBool = AtomicBool::new(false);

/// 进程级「已销毁」标志（**用户主动关闭**：划掉最近任务 / `finish()`）。
///
/// ⚠️ **进程被杀时不置位** —— 所以它只回答「用户是不是明确要把你关掉」，
/// 不能用来做资源清理或存档（见 [`is_destroyed`] 的说明）。
static DESTROYED: AtomicBool = AtomicBool::new(false);

/// **锁存**：上一次暂停期间是否发生过 `onStop`。
///
/// # 为什么是锁存，而不是一个「当前是否 stopped」的实时标志
///
/// Android 回调顺序是 `onPause → onStop → … → onStart → onResume`，而 `onStart`
/// **紧挨**着 `onResume`，两者都在渲染线程被唤醒**之前**就跑完。所以：
///
/// - 渲染线程醒来时若读「实时 stopped 标志」，读到的已经是 `false` —— 直接读会把
///   「彻底离开」和「短暂中断」混成同一件事，而这恰恰是要解决的核心问题；
/// - 而且实时标志**没有任何地方会把它重置回 `false`**（只有 `onStop` 会置位）。
///   一旦漏了 reset，它就永久停在 `true`，后续每次 `onStop` 都被「没变化」守卫
///   吞掉 —— 真机跑第二轮 `on_stopped` 就不触发了，正是这个坑。
///
/// 于是改为**只置位、由渲染线程一次性消费**（[`take_saw_stop`]），消费即清零，
/// 每个暂停周期恰好上报一次，也不需要 reset。
///
/// 至于「当前是否不可见」这个查询，由 [`is_stopped`] **派生**（`已暂停 && 锁存`），
/// 于是连 reset 路径都不需要存在。
static SAW_STOP: AtomicBool = AtomicBool::new(false);

/// 生命周期变更代号。每次**真实**状态变化 +1。
///
/// 渲染线程用它判断「是否需要触发用户回调」：多 Thread 时每个 loop 各自跟踪
/// 已处理的代号，于是每个 loop 对每次变更恰好触发一次，不会重复也不会漏。
static GEN: AtomicU64 = AtomicU64::new(0);

/// 进程级「surface 已知死亡」标志（winit `Event::Suspended` 置位，`Resumed` 清除）。
///
/// 背景：另一个 Activity 盖到前面（如 SAF 系统选择器）时，GameActivity 会拆掉
/// native surface（回来时 `onSurfaceCreated_native` 重新触发）。此时旧的
/// `wgpu::Surface` 对象已死 —— 拿它调 `configure` 直接走 wgpu fatal handler
/// panic（真机实测：SAF 选择器关闭后的第一帧必现）。
///
/// 为什么是「peek 而不是 take」：suspend 期间渲染线程可能还在画
/// （`auto_pause=false` 时），每帧都必须跳过；只有 `Resumed` 能清除
/// （那时 native window 重新有效）。消费点在 `draw_frame` 帧首 + resume 后首帧
/// 的重建（`was_stale` 闩，见 `VireoWindow`）。
///
/// 桌面永不置位（winit 只在移动端发 `Suspended`），零行为变化。
static SURFACE_STALE: AtomicBool = AtomicBool::new(false);

/// winit `Event::Suspended` 到达（winit 线程经 Runner 调用）。
pub(crate) fn mark_surface_stale() {
    SURFACE_STALE.store(true, Ordering::Release);
}

/// winit `Event::Resumed` 到达。清除即允许下一帧重建 surface。
pub(crate) fn clear_surface_stale() {
    SURFACE_STALE.store(false, Ordering::Release);
}

/// surface 是否已知死亡（渲染线程每帧读）。
pub(crate) fn surface_stale() -> bool {
    SURFACE_STALE.load(Ordering::Acquire)
}

/// 是否已绑定过。防止 `android_main` 被重复调用时二次 `RegisterNatives`。
static BOUND: AtomicBool = AtomicBool::new(false);

/// Java 侧必须声明的 native 方法名 —— 与 `MainActivity.java` 里的完全一致。
///
/// ⚠️ 改这些名字 = 破坏已发布的 `MainActivity.java` 模板。绑定会失败，且失败
/// 只 `log::warn!` —— 用户看到的是「后台仍全速」而不是编译错误。
pub const JAVA_METHOD_PAUSE: &str = "nativeOnPause";
pub const JAVA_METHOD_RESUME: &str = "nativeOnResume";
pub const JAVA_METHOD_STOP: &str = "nativeOnStop";
pub const JAVA_METHOD_DESTROY: &str = "nativeOnDestroy";
/// 返回键。**可选** —— 不声明 / 不调用时 `App::on_back` 永不触发，且返回键退回
/// Android 默认行为（退出 Activity），其余生命周期行为不受影响。
pub const JAVA_METHOD_BACK: &str = "nativeOnBackPressed";
/// 返回「本次返回键是否已被消费」。**读即清零** —— 见 [`take_back_consumed`]。
pub const JAVA_METHOD_TAKE_BACK_CONSUMED: &str = "nativeTakeBackConsumed";

/// const 友好的字节串相等（`PartialEq` 在 const 上下文里还没稳定）。
#[cfg(target_os = "android")]
#[allow(dead_code)] // 只在下面的 const 里做编译期求值，运行时无人调用
const fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// 编译期把四个常量与 `bind_inner` 里 `jni_str!` 用的**字面量**钉在一起。
///
/// `jni_str!` 展开成从字面量构造的 `JNIStr`，不接受 `const` 标识符，所以绑定表里
/// 只能写字面量。两处不一致的后果是**运行时** `RegisterNatives` 失败（只 `warn`，
/// 用户看到「后台仍全速」而没有任何编译错误）—— 极难排查。这里让编译器盯住。
#[cfg(target_os = "android")]
#[allow(dead_code)] // 编译期断言，运行时没有「调用」可言
const JAVA_NAMES_MATCH_LITERALS: () = {
    assert!(bytes_eq(JAVA_METHOD_PAUSE.as_bytes(), b"nativeOnPause"));
    assert!(bytes_eq(JAVA_METHOD_RESUME.as_bytes(), b"nativeOnResume"));
    assert!(bytes_eq(JAVA_METHOD_STOP.as_bytes(), b"nativeOnStop"));
    assert!(bytes_eq(JAVA_METHOD_DESTROY.as_bytes(), b"nativeOnDestroy"));
    assert!(bytes_eq(
        JAVA_METHOD_BACK.as_bytes(),
        b"nativeOnBackPressed"
    ));
    assert!(bytes_eq(
        JAVA_METHOD_TAKE_BACK_CONSUMED.as_bytes(),
        b"nativeTakeBackConsumed"
    ));
};

// ---------------------------------------------------------------------------
// 状态（跨平台可读）
// ---------------------------------------------------------------------------

/// App 当前是否处于「已暂停」（activity 在后台但可能仍部分可见）。
///
/// 桌面 / macOS 恒为 `false` —— 那边的窗口没有「切后台」概念。
/// 桌面对应概念是 `VireoWindow::focused`。
///
/// # 线程安全
///
/// 可从任意线程读。Android 上由 UI 线程的 `onPause` / `onResume` 经 JNI 写入。
pub fn is_paused() -> bool {
    PAUSED.load(Ordering::Acquire)
}

/// activity 当前是否**不可见**（`onStop` 状态）。
///
/// # 派生量，不是独立标志
///
/// `= 已暂停 && 锁存`。Android 保证「不可见 ⊆ 已暂停」（不可见的 activity 一定
/// 也被暂停），所以这个组合就是「不可见」。这样做的直接好处：**不需要任何 reset
/// 路径** —— 恢复时 `is_paused()` 变 `false`，`is_stopped()` 自动跟着变 `false`。
///
/// 之前有过一个独立的 `STOPPED` 实时标志，但它只有 `onStop` 置位、**没有任何地方
/// 重置**，于是第一轮之后恒为 `true`，第二轮 `onStop` 被「值没变化」守卫吞掉 ——
/// 真机跑第二轮 `on_stopped` 不触发才暴露出来。
///
/// ⚠️ 在 `onStart` 与 `onResume` 之间（本函数返回 `true` 但窗口已开始恢复）这个极短
/// 窗口里会略报「不可见」。毫秒级、自愈，且这个阶段应用正在前台，不影响任何决策。
///
/// # 与 [`is_paused`] 的区别
///
/// | | `onPause` | `onStop` |
/// |---|---|---|
/// | 锁屏 | ✅ | ✅ |
/// | 半透明弹窗 / 权限对话框盖在上面 | ✅ | ❌ |
/// | 回桌面 / 切别的 app | ✅ | ✅ |
///
/// 桌面 / macOS 恒为 `false`（`is_paused()` 恒 `false`，本函数是它的派生量）。
///
/// ⚠️ **恢复后这个值已经翻回 `false`** —— 渲染线程在暂停期间整体阻塞，醒来时
/// `onStart` 已经跑完。要判断「刚才那次暂停算不算彻底离开」，用
/// [`App::on_stopped`](crate::App::on_stopped) 回调，不要用这个查询。
pub fn is_stopped() -> bool {
    is_paused() && saw_stop()
}

/// 非消费地读锁存（给 [`is_stopped`] / 测试用；消费请用 [`take_saw_stop`]）。
fn saw_stop() -> bool {
    SAW_STOP.load(Ordering::Acquire)
}

/// # 渲染线程消费锁存：返回「上一次暂停期间是否发生过 `onStop`」并清零。
///
/// 消费即清零，保证每个暂停周期恰好上报一次。
///
/// 调用点在**代号变化时**（与 `on_paused` / `on_resumed` 同一处），即恢复之后。
pub(crate) fn take_saw_stop() -> bool {
    SAW_STOP.swap(false, Ordering::AcqRel)
}

/// 生命周期变更代号。渲染线程用它判断要不要触发用户回调。
pub(crate) fn generation() -> u64 {
    GEN.load(Ordering::Acquire)
}

/// 全局「状态变了」事件原语：`(代号, Condvar)`。
///
/// 谁写代号：`set_paused` / `set_stopped` / `set_destroyed`（平台事件）与
/// `App::notify_waiters`（用户自定义条件）。谁读代号：渲染线程的
/// `fire_lifecycle_callbacks` 判定，以及 `App::wait_until` / `wait_while_paused`。
///
/// # 为什么用「代号」而不是「裸标志 + condvar」
///
/// 裸标志方案有经典的 lost-wakeup：等待方在「检查到标志没变」与「注册进
/// condvar 等待」之间被通知掉通知，于是**永久阻塞**（症状是「切回前台后画面
/// 再也不动」）。老代码靠「写标志时必须持锁」来规避 —— 也就是把正确性押在
/// notifier 排在 waiter 之后这个时序上，注释里得专门解释一遍。
///
/// 代号方案天然免疫：等待方先读到代号 `g`，被唤醒后**复查代号是否变了**
/// （而不是复查标志）。只要通知发生过，`g` 必然不同；即使通知发生在读 `g`
/// 之前，复查也会立刻通过。`wait_while_paused` 里那句
/// 「必须持锁再写标志」的负担直接消失。
pub(crate) static EVENTS: std::sync::LazyLock<(std::sync::Mutex<u64>, std::sync::Condvar)> =
    std::sync::LazyLock::new(|| (std::sync::Mutex::new(0), std::sync::Condvar::new()));

/// 推进代号并唤醒所有等待者。可从任意线程调用。
///
/// 幂等 —— 重复调用只是多推进几次代号，等待方复查条件后自行决定。
pub(crate) fn notify_all() {
    let (m, cv) = &*EVENTS;
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    *g = g.wrapping_add(1);
    cv.notify_all();
}

/// 阻塞直到代号变化或超时。返回「是否因代号变化而返回」。
///
/// `timeout = None` 表示无限等待。**必须**在调用前后复查调用者的条件 ——
/// 本函数只保证「期间发生过事件」，不保证「事件满足你的条件」。
///
/// 供 `App::wait_until` 使用（跨平台，桌面上平台事件永不推进但超时照常生效）。
pub(crate) fn wait_events(timeout: Option<std::time::Duration>) -> bool {
    let (m, cv) = &*EVENTS;
    let guard = m.lock().unwrap_or_else(|e| e.into_inner());
    let start = *guard;
    match timeout {
        None => {
            let mut guard = guard;
            while *guard == start {
                guard = cv.wait(guard).unwrap_or_else(|e| e.into_inner());
            }
        }
        Some(d) => {
            let (guard, wait) = cv.wait_timeout(guard, d).unwrap_or_else(|e| e.into_inner());
            if wait.timed_out() && *guard == start {
                return false;
            }
        }
    }
    true
}

/// 由 JNI 回调写标志。
fn set_paused(v: bool) {
    let prev = PAUSED.swap(v, Ordering::AcqRel);
    if prev != v {
        GEN.fetch_add(1, Ordering::AcqRel);
    }
    // 即使状态没变也通知：`wait_while_paused` 只在 paused 时阻塞，
    // 而重复的 onPause 在部分 ROM 上很常见。代号式 wait 免疫丢唤醒，
    // 所以这里不必再靠「持锁写标志」来保证时序。
    notify_all();
}

/// activity 是否已被**用户主动关闭**（划掉最近任务 / `finish()`）。
///
/// # 与 `is_paused` / `is_stopped` 的分工
///
/// | 问题 | 靠什么 |
/// |---|---|
/// | 我在前台吗 | [`is_paused`] |
/// | 我完全不可见吗 | [`is_stopped`] |
/// | **用户是切走了还是主动关了我** | **本函数** |
/// | 我的进程要死了吗 | 无解（进程被杀时没有任何回调） |
///
/// # 关键性质：**进程被杀时不触发**
///
/// 这是 Android 官方文档「might be called」的含义 —— 所以本函数**不能**用来
/// 做资源清理或存档，它只回答「用户是不是明确要把你关掉」。
///
/// 它也**不是**单调的：activity 被销毁后若用户从最近任务重新打开，进程通常是
/// 新的（标志重置）。同一次进程里 `onDestroy` 之后不会再有 `onResume`。
///
/// 桌面 / macOS 恒为 `false`（无 Activity 概念）。
pub fn is_destroyed() -> bool {
    DESTROYED.load(Ordering::Acquire)
}

/// 由 JNI 回调（UI 线程）写。只在真实变化时推进代号。
fn set_destroyed() {
    let prev = DESTROYED.swap(true, Ordering::AcqRel);
    if !prev {
        GEN.fetch_add(1, Ordering::AcqRel);
        notify_all();
    }
}

/// 渲染线程在 paused 时阻塞，直到恢复。
///
/// `done` 用于退出时打断等待（否则关闭流程会挂住）。
pub(crate) fn wait_while_paused(done: &std::sync::atomic::AtomicBool) {
    while is_paused() && !done.load(Ordering::Acquire) {
        if !wait_events(None) {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// JNI 绑定（仅 Android）
// ---------------------------------------------------------------------------

/// JNI 导出：Java `nativeOnPause()` 的实现。static 方法，故签名是
/// `(EnvUnowned, JClass)` —— 见 `jni::NativeMethod::from_raw_parts` 的文档。
///
/// 函数指针由 `bind_lifecycle_hooks` 直接交给 `RegisterNatives`，不参与符号导出。
///
/// # Safety
///
/// 由 JVM 经 `bind_lifecycle_hooks` 绑定调用，签名固定 `()V`。
#[cfg(target_os = "android")]
pub extern "system" fn vireo_android_on_pause(
    _env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
) {
    set_paused(true);
}

/// JNI 导出：Java `nativeOnResume()` 的实现。
///
/// # Safety
///
/// 由 JVM 经 `bind_lifecycle_hooks` 绑定调用，签名固定 `()V`。
#[cfg(target_os = "android")]
pub extern "system" fn vireo_android_on_resume(
    _env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
) {
    set_paused(false);
}

/// JNI 导出：Java `nativeOnStop()` 的实现。
///
/// 与 pause/resume 不同，这个**不**唤醒等待者（锁存不参与闸门，闸门只看 `PAUSED`），
/// 只置位 `SAW_STOP`。
///
/// 幂等：Android 会在一次暂停里连续多次 `onStop`（分屏、多窗口），重复置位无害 ——
/// 消费端是「读一次就清零」，不需要调用方去重。
/// **曾经**在这里按「`STOPPED` 有没有变化」去重，那是错的：那个标志没人重置，
/// 第二轮的 `onStop` 全被吞掉（真机实测）。
///
/// # Safety
///
/// 由 JVM 经 `bind_lifecycle_hooks` 绑定调用，签名固定 `()V`。
#[cfg(target_os = "android")]
pub extern "system" fn vireo_android_on_stop(
    _env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
) {
    // 不参与闸门、也不需要 notify（锁存只在代号变化时被消费，而那时
    // `onPause`/`onResume` 必然已经推进过代号）。这里只置位。
    //
    // store 只允许 Release/Relaxed/SeqCst —— AcqRel 是编译错误（仅 Android
    // target 才会编到这段，桌面 `cargo test` 抓不到）。
    SAW_STOP.store(true, Ordering::Release);
}

/// JNI 导出：Java `nativeOnDestroy()` 的实现。
///
/// # Safety
///
/// 由 JVM 经 `bind_lifecycle_hooks` 绑定调用，签名固定 `()V`。
#[cfg(target_os = "android")]
pub extern "system" fn vireo_android_on_destroy(
    _env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
) {
    set_destroyed();
}

// ---------------------------------------------------------------------------
// 返回键（Back / 预测性返回手势）
// ---------------------------------------------------------------------------

/// **本次**返回键是否已被登记请求（一次性事件）。
///
/// # 为什么是锁存而不是实时标志
///
/// 和 [`SAW_STOP`] 同理：**Android 没有「返回键被按下了」这种持续状态** —— 它是
/// 一个瞬时事件。标志必须「写一次、消费即清零」，否则第二次返回会读到第一次的
/// 残留值（用户已经消费过了却又触发一次动作）。
///
/// 更关键：这个标志是**跨线程单向**的 —— UI 线程写、渲染线程读并清零。反方向的
/// 「我要消费这次返回」是 [`BACK_CONSUMED`]（渲染线程写、UI 线程读），**必须**用
/// 实时标志而非锁存：UI 线程读的那一瞬间必须拿到渲染线程此刻的意愿，而不能拿到
/// 一次历史记录。两者的时序恰好相反，这正是它们必须用两种原语的原因。
static BACK_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 让渲染线程在下一帧重新检查返回键请求。
///
/// # 为什么必须推进代号
///
/// 渲染线程是**靠代号变化**才调 `fire_lifecycle_callbacks` 的
/// （见 `src/thread.rs`：`let g = generation(); if g != lifecycle_gen { ... }`）。
/// 返回键是**独立**事件 —— 它既不伴随 `onPause` 也不伴随 `onResume`，代号不动。
/// 于是不推进代号的话 `on_back` **一次都不会触发**（App 照样按默认行为退出，
/// 现象与「完全没实现」无法区分）。
///
/// 用代号而不是「渲染线程每帧无条件查标志」是为了保持与既有生命周期一致：
/// 每个 Thread 各自跟踪已处理代号，恰好触发一次、不重不漏。
fn request_back() {
    BACK_REQUESTED.store(true, Ordering::Release);
    GEN.fetch_add(1, Ordering::AcqRel);
    notify_all();
}

/// 渲染线程对**当前**这次返回键的处置决定。
///
/// `true` = 已消费（不要退出 Activity）；`false` = 不消费（走默认行为，即
/// `Activity.finish()`）。
///
/// # 为什么不用锁存
///
/// 见 [`BACK_REQUESTED`] 的说明：这里需要的是**实时**值。UI 线程读它来决定是否
/// `finish()`，读到的必须是渲染线程刚刚写入的意愿。
///
/// # 时限（重要）
///
/// 「刚刚」有上限：Java 侧**不可能**同步等渲染线程跑完一帧（`onBackPressed`
/// 在 UI 线程上同步阻塞会死锁 —— 渲染线程可能正等 UI 线程做某件事）。所以
/// Java 侧的做法是「**先无条件拦住，再延后判定**」：拦下后 postDelayed 一个
/// 时限（模板取 34ms ≈ 2 帧 @60Hz），到点再读这里的值。
///
/// 于是 {@link consume_back} 必须在**该时限内**被调用，否则按「未消费」退出。
/// 这不影响同步调用 —— 渲染线程的 `on_back` 回调本身就在那之前跑完了。真正会被
/// 这个时限坑到的是「把 consume_back 推迟到别的线程做」的错误写法。
static BACK_CONSUMED: AtomicBool = AtomicBool::new(false);

/// 本次返回键是否已被应用层请求处理（由渲染线程消费，见 [`take_back_requested`]）。
///
/// 桌面 / macOS 恒为 `false` —— 那边窗口有关闭按钮，`WindowEvent::CloseRequested`
/// 走的是完全不同的路径（且桌面**有**这个事件，Android 没有）。
pub fn back_requested() -> bool {
    BACK_REQUESTED.load(Ordering::Acquire)
}

/// 渲染线程消费「本次返回键」请求，返回是否消费过。消费即清零。
///
/// 每个暂停周期…呃，每个返回周期恰好上报一次。
pub(crate) fn take_back_requested() -> bool {
    BACK_REQUESTED.swap(false, Ordering::AcqRel)
}

/// 声明「本次返回键我处理了，别退出 Activity」。
///
/// 供 [`App::on_back`](crate::App::on_back) 的回调在渲染线程上调用。
///
/// # 时序
///
/// 必须在**返回键回调之内**同步调用 —— UI 线程在同一轮 `onBackPressed` 里就会读
/// 这个值决定是否 `finish()`。异步（spawn 到别的线程再调）会来不及，表现为
/// 「回调跑了但 App 还是退了」。
pub fn consume_back() {
    BACK_CONSUMED.store(true, Ordering::Release);
}

/// UI 线程读：本次返回键是否已被消费。读即清零（下一个返回键回到默认行为）。
///
/// 只有 Java 侧的 back handler 调用它 —— 每次返回键必须得到一个明确的答复，
/// 残留的 `true` 会让**下一次**返回键也被吞掉。
pub(crate) fn take_back_consumed() -> bool {
    BACK_CONSUMED.swap(false, Ordering::AcqRel)
}

/// JNI 导出：Java `nativeOnBackPressed()` 的实现。
///
/// # 为什么这里不直接 finish，也不直接决定退出
///
/// 因为「该不该退」是**应用层决策**（弹确认框？回暂停菜单？直接退？），而这个 JNI
/// 回调跑在 **UI 线程**、是**瞬时**的 —— 它不能等渲染线程（渲染线程此刻可能正阻塞
/// 在暂停闸门里，或者干脆在忙一帧）。所以职责拆成两半：
///
/// ```text
/// UI 线程                        渲染线程
/// nativeOnBackPressed()
///   ├─ BACK_REQUESTED = true
///   └─ 读 take_back_consumed()  ──►  fire_lifecycle_callbacks()
///       │  false → finish()          └─ on_back 回调 → consume_back()
///       └ true  → 什么都不做
/// ```
///
/// UI 线程读到的是**上一帧**渲染线程留下的意愿（多数情况是「没消费」= 默认退出）。
/// 这有一个可感知的时延：若渲染线程正在闸门里阻塞（App 在后台），返回键会退到
/// 默认行为 —— 但那种情况下 Activity 本来也不在前台，返回键不会有意义。
///
/// # Safety
///
/// 由 JVM 经 `bind_lifecycle_hooks` 绑定调用，签名固定 `()V`。
#[cfg(target_os = "android")]
pub extern "system" fn vireo_android_on_back_pressed(
    _env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
) {
    request_back();
}

/// JNI 导出：Java `nativeTakeBackConsumed()` 的实现 —— 返回「本次返回键是否已被
/// 应用层消费」，**读即清零**。
///
/// # 返回值语义（`jni::sys::jboolean` = `u8`）
///
/// 必须严格是 `JNI_TRUE`(1) / `JNI_FALSE`(0) —— Java 侧接的是 `boolean`，
/// 任何非 0/1 的值（如 `u8::MAX`）的行为未定义。
///
/// # 为什么必须清零
///
/// 读即清零是为了「每次返回键都有独立答案」。若不清，上一次残留的 `true` 会把
/// **下一次**返回键也吞掉 —— 用户第二次想退出却退不出，且没有任何崩溃可查。
///
/// # 与 `vireo_android_on_back_pressed` 的调用顺序
///
/// Java 侧先调本函数**再**调 `nativeOnBackPressed()`。看着反直觉（不是应该先
/// 通知再问意见吗），但时序就是这样：
///
/// ```text
/// UI 线程                        渲染线程
/// takeBackConsumed()  ──► 读到上一帧的 consume_back() 结果
/// onBackPressed()     ──► 置 BACK_REQUESTED，本帧稍后被消费
///   ├─ !consumed → finish()
///   └─ consumed  → 什么都不做
/// ```
///
/// 「消费」只能来自**上一帧**渲染线程里跑的 `on_back` 回调 —— 返回键是瞬时事件，
/// UI 线程不可能等渲染线程跑完一轮。所以本次的处置必然基于上一帧的意愿。
///
/// # Safety
///
/// 由 JVM 经 `bind_lifecycle_hooks` 绑定调用，签名固定 `()Z`。
#[cfg(target_os = "android")]
pub extern "system" fn vireo_android_take_back_consumed(
    _env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
) -> jni::sys::jboolean {
    if take_back_consumed() {
        jni::sys::JNI_TRUE
    } else {
        jni::sys::JNI_FALSE
    }
}

/// 把 Java 侧的 `nativeOnPause` / `nativeOnResume` / `nativeOnStop` 绑到本模块的
/// C 符号上。
///
/// 由 `android_main` 调用（见 [`crate::app::android_main_entry`]）。非 Android
/// 平台是 no-op。
///
/// 绑定失败**不是致命错误**：App 照常跑，只是没有生命周期信号（= 旧行为）。
/// 典型原因是用户的 `MainActivity.java` 没照模板声明那几个方法 —— 所以这里
/// `warn` 且把原文案写全，用户 `logcat` 一搜就知道要改什么。
#[cfg(target_os = "android")]
pub fn bind_lifecycle_hooks(app: &crate::android::AndroidApp) {
    if BOUND.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(e) = bind_inner(app) {
        BOUND.store(false, Ordering::Release);
        log::warn!(
            "vireo: Android lifecycle hooks not bound ({e:?}). MainActivity.java must declare \
             `private static native void {JAVA_METHOD_PAUSE}();` and \
             `private static native void {JAVA_METHOD_RESUME}();` (plus optional \
             `private static native void {JAVA_METHOD_STOP}();`), and call them from \
             onPause()/onResume()/onStop(). Without them the app keeps rendering while \
             backgrounded and cannot tell a brief interruption from a real departure."
        );
    }
}

#[cfg(target_os = "android")]
fn bind_inner(app: &crate::android::AndroidApp) -> Result<(), jni::errors::Error> {
    use jni::jni_str;
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) };
    vm.attach_current_thread(|env| {
        // 关键：取 **Activity 实例的实际类**，而不是写死包名 findClass ——
        // 这样用户改 applicationId / 类名都不影响绑定。
        let raw = app.activity_as_ptr() as jni::sys::jobject;
        if raw.is_null() {
            return Err(jni::errors::Error::NullPtr("activity"));
        }
        let activity = unsafe { jni::objects::JObject::from_raw(env, raw) };
        let class = env.get_object_class(&activity)?;

        // SAFETY: 三个函数指针都是 `extern "system" fn(EnvUnowned, JClass)`，
        // 与「static native 方法 + `()V` 签名」的要求一致（见 jni 文档）。
        // 三者无捕获、无状态，不持任何 JNI 引用。
        //
        // ⚠️ `jni_str!` 要求**字面量**，所以这里不能用上面的常量。字面量与常量
        // 由 `JAVA_NAMES_MATCH_LITERALS` 在编译期钉在一起。
        let methods = [
            unsafe {
                jni::NativeMethod::from_raw_parts(
                    jni_str!("nativeOnPause"),
                    jni_str!("()V"),
                    vireo_android_on_pause as *mut std::ffi::c_void,
                )
            },
            unsafe {
                jni::NativeMethod::from_raw_parts(
                    jni_str!("nativeOnResume"),
                    jni_str!("()V"),
                    vireo_android_on_resume as *mut std::ffi::c_void,
                )
            },
            unsafe {
                jni::NativeMethod::from_raw_parts(
                    jni_str!("nativeOnStop"),
                    jni_str!("()V"),
                    vireo_android_on_stop as *mut std::ffi::c_void,
                )
            },
            unsafe {
                jni::NativeMethod::from_raw_parts(
                    jni_str!("nativeOnDestroy"),
                    jni_str!("()V"),
                    vireo_android_on_destroy as *mut std::ffi::c_void,
                )
            },
            unsafe {
                jni::NativeMethod::from_raw_parts(
                    jni_str!("nativeOnBackPressed"),
                    jni_str!("()V"),
                    vireo_android_on_back_pressed as *mut std::ffi::c_void,
                )
            },
            unsafe {
                jni::NativeMethod::from_raw_parts(
                    jni_str!("nativeTakeBackConsumed"),
                    jni_str!("()Z"),
                    vireo_android_take_back_consumed as *mut std::ffi::c_void,
                )
            },
        ];
        // SAFETY: `class` 是本帧有效的 local ref；`methods` 在调用期间有效。
        unsafe { env.register_native_methods(class, &methods) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_method_names_are_stable() {
        // 这些字符串是 Java↔Rust 契约。改名会让已发布的 MainActivity 模板静默失效
        // （绑定失败只 warn，用户会看到「后台仍全速」而不知道原因）。
        assert_eq!(JAVA_METHOD_PAUSE, "nativeOnPause");
        assert_eq!(JAVA_METHOD_RESUME, "nativeOnResume");
        assert_eq!(JAVA_METHOD_STOP, "nativeOnStop");
        assert_eq!(JAVA_METHOD_BACK, "nativeOnBackPressed");
    }

    /// 返回键锁存：写一次消费一次，绝不重复上报。
    ///
    /// 回归风险点：如果这里改成「实时标志」，第二次返回键会读到第一次的残留
    /// `true`，用户已经消费过了却又被触发一次动作 —— 与 `SAW_STOP` 那次踩的
    /// 「没人重置的实时标志」是同一类错误的镜像。
    #[test]
    fn back_requested_is_consume_once() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_back_requested();

        BACK_REQUESTED.store(true, Ordering::Release);
        assert!(take_back_requested(), "第一次返回必须被看到");
        assert!(!take_back_requested(), "消费即清零：同一次返回不得重复上报");

        // 连续两次按返回：各上报一次
        BACK_REQUESTED.store(true, Ordering::Release);
        assert!(take_back_requested());
        BACK_REQUESTED.store(true, Ordering::Release);
        assert!(take_back_requested(), "第二次按返回是独立事件");
    }

    /// 回归：返回键**必须**推进代号，否则渲染线程压根不会检查它。
    ///
    /// 渲染线程靠 `generation() != lifecycle_gen` 才触发回调，而返回键既不伴随
    /// `onPause` 也不伴随 `onResume`。第一版只置 `BACK_REQUESTED` 不动代号，
    /// 真机表现是「按返回键 App 直接退出，on_back 日志一次都没有」——
    /// 与完全没实现无法区分。
    #[test]
    fn back_request_bumps_generation() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_back_requested();

        let g0 = generation();
        request_back();
        assert!(take_back_requested(), "请求本身要被记录");
        assert_eq!(generation(), g0 + 1, "必须推进代号，渲染线程才会检查");
    }

    /// `consume_back` / `take_back_consumed` 是一次性往返，且**不**跨事件残留。
    ///
    /// 这是「回调里同步 consume 才拦得住」的实现基础：UI 线程读的就是这个值。
    /// 如果它像 `BACK_REQUESTED` 那样是锁存语义，残留的 `true` 会把**下一次**
    /// 返回键也吞掉 —— 用户第二次想退出却退不出。
    #[test]
    fn back_consumed_round_trips_without_leaking_into_next_press() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_back_consumed();

        assert!(!take_back_consumed(), "初始必须是「未消费」= 允许默认退出");
        consume_back();
        assert!(take_back_consumed(), "consume_back 之后必须读到 true");
        assert!(
            !take_back_consumed(),
            "必须读到即清零：残留会让下一次返回键也退不出"
        );
    }

    /// 两个方向必须**独立**：处理了请求不等于消费了返回。
    ///
    /// 这是最容易写混的一对 —— 它们时序相反（请求是锁存、消费是实时），
    /// 用错原语会表现为「on_back 回调跑了，App 还是退了」或反过来
    /// 「没注册回调却拦住了返回键」。
    #[test]
    fn back_request_and_consume_are_independent() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_back_requested();
        let _ = take_back_consumed();

        // 场景 A：只请求，不消费 → 请求被看到，UI 侧读到「未消费」→ 默认退出
        BACK_REQUESTED.store(true, Ordering::Release);
        assert!(take_back_requested());
        assert!(!take_back_consumed(), "没调 consume_back 就必须是 false");

        // 场景 B：请求 + 消费 → 两者都独立为 true
        BACK_REQUESTED.store(true, Ordering::Release);
        consume_back();
        assert!(take_back_requested());
        assert!(take_back_consumed());
    }

    #[test]
    fn desktop_never_reports_paused() {
        // 桌面没有 Activity 生命周期，必须恒 false，否则渲染线程会永久停摆。
        // 读的是派生量 `PAUSED`，并发测试会中途翻它 → 必须与改写方互斥。
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !cfg!(target_os = "android") {
            assert!(!is_paused());
        }
    }

    #[test]
    fn desktop_never_reports_stopped() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !cfg!(target_os = "android") {
            assert!(!is_stopped());
        }
    }

    #[test]
    fn desktop_never_reports_back_requested() {
        // 桌面有 WindowEvent::CloseRequested（Android 没有），所以桌面不该走这条路径。
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !cfg!(target_os = "android") {
            assert!(!back_requested());
        }
    }

    #[test]
    fn surface_stale_flag_round_trips() {
        // Suspended→脏→跳过→Resumed→清→重建：渲染线程只读 peek，
        // 清除只能由 Resumed 做（draw 侧永不清，否则死 surface 又会被 configure）。
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let was = surface_stale();
        mark_surface_stale();
        assert!(surface_stale());
        // 重复置位幂等
        mark_surface_stale();
        assert!(surface_stale());
        clear_surface_stale();
        assert!(!surface_stale());
        // 复原，避免影响其它测试
        if was {
            mark_surface_stale();
        }
    }

    #[test]
    fn set_paused_bumps_generation_only_on_real_change() {
        // 渲染线程用代号判断要不要触发用户回调。部分 ROM 会连续多次 onPause，
        // 若每次都 bump，用户回调会被重复触发。
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let g0 = generation();
        let was = is_paused();
        set_paused(was); // 无变化
        assert_eq!(generation(), g0, "状态未变时不应 bump 代号");
        set_paused(!was);
        assert_eq!(generation(), g0 + 1, "真实变化必须 bump");
        // 复原，避免影响其它测试
        set_paused(was);
        assert_eq!(generation(), g0 + 2);
    }

    /// 测试期间独占全局生命周期标志的锁。
    ///
    /// 这些测试读写 `PAUSED` / `GEN` / `SAW_STOP` 三个进程级全局，且都要
    /// 「保存 → 改 → 断言 → 复原」。cargo 默认并行跑测试，并发时后一个测试的
    /// `g0` 会捕到前一个的中间态，断言互相打架。
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn saw_stop_latches_across_resume_and_is_consume_once() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 前置：清干净
        let _ = take_saw_stop();
        let paused0 = is_paused();

        // 模拟「回桌面」：onPause → onStop → … → onStart → onResume
        set_paused(true);
        SAW_STOP.store(true, Ordering::Release); // == vireo_android_on_stop 的行为
        assert!(is_stopped(), "暂停 + 发生过 onStop ⇒ 不可见");
        set_paused(false);

        // is_stopped() 是「已暂停」的派生量，恢复后自动翻回 —— 不需要任何 reset 路径
        assert!(!is_stopped(), "恢复后 is_stopped() 必须自动为 false");
        assert!(
            take_saw_stop(),
            "恢复后仍必须能读到「那次暂停发生过 onStop」"
        );
        assert!(!take_saw_stop(), "锁存消费即清零：同一暂停周期不得重复上报");

        set_paused(paused0);
    }

    /// 回归：第一版按「实时 stopped 标志有没有变化」去重，而那个标志只有 onStop
    /// 置位、**没有任何地方重置**，于是第一轮之后恒为 `true`，第二轮的 `onStop`
    /// 全被吞掉。真机实测表现为 `paused=2` 但 `stopped=1`。
    #[test]
    fn every_stop_sets_the_latch_regardless_of_previous_state() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_saw_stop();
        let paused0 = is_paused();

        for round in 1..=2 {
            set_paused(true);
            SAW_STOP.store(true, Ordering::Release);
            set_paused(false);
            assert!(take_saw_stop(), "round {round}: 锁存必须每次都置位");
            assert!(!take_saw_stop(), "round {round}: 不得重复上报");
        }
        set_paused(paused0);
    }

    #[test]
    fn brief_interruption_leaves_no_stop_latch() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_saw_stop();
        let paused0 = is_paused();

        // 模拟「半透明弹窗盖上来」：只有 onPause/onResume，没有 onStop
        set_paused(true);
        assert!(
            !is_stopped(),
            "没有 onStop 的暂停，activity 仍可见 —— 这正是 on_stopped 存在的意义"
        );
        set_paused(false);

        assert!(!take_saw_stop(), "没有 onStop 的暂停不得被当成「彻底离开」");
        set_paused(paused0);
    }

    #[test]
    fn repeated_stops_within_one_pause_report_once() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_saw_stop();

        // 部分 ROM 会连续多次 onStop（分屏、多窗口），不得重复上报
        SAW_STOP.store(true, Ordering::Release);
        SAW_STOP.store(true, Ordering::Release);
        assert!(take_saw_stop());
        assert!(!take_saw_stop(), "一个暂停周期只上报一次");
    }
}
