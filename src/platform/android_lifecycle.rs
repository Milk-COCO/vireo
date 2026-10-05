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
//! # 但 `onStop` **不能**当「离开时立刻干活」的钩子 —— 这是硬约束
//!
//! 渲染线程在暂停期间**整体阻塞**（见 `src/thread.rs` 的生命周期闸门：不 `on_tick`、
//! 不 `draw`、不 acquire/present，为的是把后台的 CPU+GPU 占用降到零）。所以：
//!
//! - 暂停期间**没有任何用户代码在跑** —— 「离开时立刻保存进度」做不到；
//! - 用户回调只能**恢复后补发**（`on_paused` 就是这个语义）；
//! - 恢复后再读 `is_paused()` 已经是 `false`，**信息已丢**。
//!
//! 因此本模块用**锁存** `SAW_STOP` 记住「那次暂停期间发生过 onStop」，让恢复后
//! 补发的 [`App::on_stopped`](crate::App::on_stopped) 能区分两种暂停 —— 这才是
//! 本条唯一能兑现的价值。**进程在后台被系统杀掉时不会有任何回调**，
//! 「离开时持久化」必须由用户在自己的 `MainActivity.onSaveInstanceState` 里做
//! （见 `TODO.md` 的 `onSaveInstanceState` 条目）。
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

/// 进程级「已停止」标志（activity **不可见**）。与 `PAUSED` 的区别见模块文档。
static STOPPED: AtomicBool = AtomicBool::new(false);

/// **锁存**：上一次暂停期间是否发生过 `onStop`。
///
/// # 为什么必须锁存，不能直接读 [`STOPPED`]
///
/// 渲染线程在暂停期间**整体阻塞**（`src/thread.rs` 的生命周期闸门：不 `on_tick`、
/// 不 `draw`），只在恢复后才醒来补发回调。而 Android 的回调顺序是
/// `onPause → onStop → … → onStart → onResume` —— `onStart` 紧挨着 `onResume`，
/// 两者都在渲染线程被唤醒**之前**就跑完了。
///
/// 所以渲染线程醒来时读 `STOPPED` 读到的**已经是 false**，直接读会把
/// 「彻底离开」和「短暂中断」混成同一件事 —— 而这恰恰是本条要解决的核心问题。
///
/// 锁存由 UI 线程写、渲染线程**一次性消费**（[`take_saw_stop`]），消费即清零，
/// 于是「每个暂停周期恰好上报一次」。
static SAW_STOP: AtomicBool = AtomicBool::new(false);

/// 生命周期变更代号。每次**真实**状态变化 +1。
///
/// 渲染线程用它判断「是否需要触发用户回调」：多 Thread 时每个 loop 各自跟踪
/// 已处理的代号，于是每个 loop 对每次变更恰好触发一次，不会重复也不会漏。
static GEN: AtomicU64 = AtomicU64::new(0);

/// 是否已绑定过。防止 `android_main` 被重复调用时二次 `RegisterNatives`。
static BOUND: AtomicBool = AtomicBool::new(false);

/// 等待/唤醒用。key 与 `PAUSED` 配对 —— 见 [`wait_while_paused`] 的 lost-wakeup 说明。
pub(crate) static WAKE: std::sync::LazyLock<(std::sync::Mutex<()>, std::sync::Condvar)> =
    std::sync::LazyLock::new(|| (std::sync::Mutex::new(()), std::sync::Condvar::new()));

/// Java 侧必须声明的 native 方法名 —— 与 `MainActivity.java` 里的完全一致。
///
/// ⚠️ 改这三个名字 = 破坏已发布的 `MainActivity.java` 模板。绑定会失败，且失败
/// 只 `log::warn!` —— 用户看到的是「后台仍全速」而不是编译错误。
pub const JAVA_METHOD_PAUSE: &str = "nativeOnPause";
pub const JAVA_METHOD_RESUME: &str = "nativeOnResume";
pub const JAVA_METHOD_STOP: &str = "nativeOnStop";

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

/// 编译期把三个常量与 `bind_inner` 里 `jni_str!` 用的**字面量**钉在一起。
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

/// activity 当前是否**不可见**（`onStop`）。
///
/// 与 [`is_paused`] 的区别（Android 官方语义）：
///
/// | | `onPause` | `onStop` |
/// |---|---|---|
/// | 锁屏 | ✅ | ✅ |
/// | 半透明弹窗 / 权限对话框盖在上面 | ✅ | ❌ |
/// | 回桌面 / 切别的 app | ✅ | ✅ |
///
/// 桌面 / macOS 恒为 `false`。
///
/// ⚠️ **恢复后这个值已经翻回 `false`** —— 渲染线程在暂停期间整体阻塞，醒来时
/// `onStart` 已经跑完。要判断「刚才那次暂停算不算彻底离开」，用
/// [`App::on_stopped`](crate::App::on_stopped) 回调，不要用这个查询。
pub fn is_stopped() -> bool {
    STOPPED.load(Ordering::Acquire)
}

/// 渲染线程消费锁存：返回「上一次暂停期间是否发生过 `onStop`」并清零。
///
/// 消费即清零，保证每个暂停周期恰好上报一次。
pub(crate) fn take_saw_stop() -> bool {
    SAW_STOP.swap(false, Ordering::AcqRel)
}

/// 生命周期变更代号。渲染线程用它判断要不要触发用户回调。
pub(crate) fn generation() -> u64 {
    GEN.load(Ordering::Acquire)
}

/// 由 JNI 回调写标志。
///
/// **lost-wakeup 说明**：必须**持锁**再写标志。若不持锁，渲染线程可能刚好在
/// 「检查到 paused=true」与「注册进 condvar 等待」之间被通知掉通知，于是永久
/// 阻塞 —— 表现为「切回前台后画面再也不动」。持锁后 notifier 必然排在
/// `wait()` 注册之后。
fn set_paused(v: bool) {
    let (m, cv) = &*WAKE;
    let _guard = m.lock().unwrap_or_else(|e| e.into_inner());
    let prev = PAUSED.swap(v, Ordering::AcqRel);
    if prev != v {
        GEN.fetch_add(1, Ordering::AcqRel);
    }
    // 即使状态没变也唤醒：`wait_while_paused` 只在 paused 时阻塞，
    // 而重复的 onPause 在部分 ROM 上很常见。
    cv.notify_all();
}

/// 渲染线程在 paused 时阻塞，直到恢复。
///
/// `done` 用于退出时打断等待（否则关闭流程会挂住）。
pub(crate) fn wait_while_paused(done: &std::sync::atomic::AtomicBool) {
    if !is_paused() {
        return;
    }
    let (m, cv) = &*WAKE;
    let mut guard = m.lock().unwrap_or_else(|e| e.into_inner());
    while is_paused() && !done.load(Ordering::Acquire) {
        guard = cv.wait(guard).unwrap_or_else(|e| e.into_inner());
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
/// 与 pause/resume 不同，这个**不**唤醒等待者（`STOPPED` 不参与闸门，闸门只看
/// `PAUSED`），只写实时标志 + 锁存。锁存是渲染线程醒来后唯一能拿到的信息 —— 详见
/// `SAW_STOP` 的说明。
///
/// # Safety
///
/// 由 JVM 经 `bind_lifecycle_hooks` 绑定调用，签名固定 `()V`。
#[cfg(target_os = "android")]
pub extern "system" fn vireo_android_on_stop(
    _env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
) {
    let (m, _cv) = &*WAKE;
    // 与 set_paused 同纪律：持锁再写。虽然这里不需要 notify，但锁的互斥语义
    // 让「写 STOPPED」与「写 SAW_STOP」对渲染线程是一次原子可见的操作 ——
    // take_saw_stop() 只读锁存，不看 STOPPED，但保持同一把锁可避免将来
    // 有人加读路径时踩到撕裂。
    let _guard = m.lock().unwrap_or_else(|e| e.into_inner());
    let prev = STOPPED.swap(true, Ordering::AcqRel);
    if prev != true {
        // store 只允许 Release/Relaxed/SeqCst —— AcqRel 是编译错误（仅 Android
        // target 才会编到这段，桌面 `cargo test` 抓不到）。
        SAW_STOP.store(true, Ordering::Release);
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
    }

    #[test]
    fn desktop_never_reports_paused() {
        // 桌面没有 Activity 生命周期，必须恒 false，否则渲染线程会永久停摆。
        if !cfg!(target_os = "android") {
            assert!(!is_paused());
        }
    }

    #[test]
    fn desktop_never_reports_stopped() {
        if !cfg!(target_os = "android") {
            assert!(!is_stopped());
        }
    }

    #[test]
    fn set_paused_bumps_generation_only_on_real_change() {
        // 渲染线程用代号判断要不要触发用户回调。部分 ROM 会连续多次 onPause，
        // 若每次都 bump，用户回调会被重复触发。
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
        let (paused0, stopped0) = (is_paused(), is_stopped());

        // 模拟「回桌面」：onPause → onStop → … → onStart → onResume
        set_paused(true);
        STOPPED.store(true, Ordering::Release);
        SAW_STOP.store(true, Ordering::Release);
        // onStart + onResume：实时标志已经翻回去了……
        STOPPED.store(false, Ordering::Release);
        set_paused(false);

        // ……但锁存还在，这正是它存在的理由。
        assert!(!is_stopped(), "恢复后实时标志必须已翻回");
        assert!(
            take_saw_stop(),
            "恢复后仍必须能读到「那次暂停发生过 onStop」"
        );
        assert!(!take_saw_stop(), "锁存消费即清零：同一暂停周期不得重复上报");

        set_paused(paused0);
        STOPPED.store(stopped0, Ordering::Release);
    }

    #[test]
    fn brief_interruption_leaves_no_stop_latch() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = take_saw_stop();
        let paused0 = is_paused();

        // 模拟「半透明弹窗盖上来」：只有 onPause/onResume，没有 onStop
        set_paused(true);
        set_paused(false);

        assert!(
            !take_saw_stop(),
            "没有 onStop 的暂停不得被当成「彻底离开」—— 这正是 on_stopped 存在的意义"
        );
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
