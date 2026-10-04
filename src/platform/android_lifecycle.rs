//! Android 生命周期桥（`onPause` / `onResume`）。
//!
//! # 为什么需要这个模块
//!
//! winit 的 Android 后端**注册了** Activity 生命周期回调，但除 `Resume` / `Start` 外
//! **其余全部不发事件** —— `MainEvent::Pause` / `Stop` / `Destroy` / `LowMemory` /
//! `SaveState` 这些变体在 Android 侧永远不会被构造。
//!
//! 结果：vireo 收不到「我进后台了」，切后台后继续全速渲染，直到被系统杀进程。
//!
//! # 为什么走「Java 转发」而不是别的办法
//!
//! | 方案 | 成本 | 问题 |
//! |---|---|---|
//! | 降低 Android 默认 `max_fps` | 几行 | **没解决问题** —— 后台仍全速，只是变慢；还拖累前台 |
//! | Rust 侧注册 `ActivityLifecycleCallbacks` | 大 | 要用 `java.lang.reflect.Proxy` 动态实现接口，且回调在 UI 线程 |
//! | **Java 侧转发（本模块）** | 小 | `MainActivity` 本来就是用户的代码，加两个 override |
//!
//! 选第三条：`MainActivity.java` 是 vireo 构建产物里的宿主类（见 `examples/android/`），
//! 加两个 override 是零成本，换来的是**真正准确**的生命周期信号。
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
/// ⚠️ 改这两个名字 = 破坏已发布的 `MainActivity.java` 模板。绑定会失败，且失败
/// 只 `log::warn!` —— 用户看到的是「后台仍全速」而不是编译错误。
pub const JAVA_METHOD_PAUSE: &str = "nativeOnPause";
pub const JAVA_METHOD_RESUME: &str = "nativeOnResume";

// ---------------------------------------------------------------------------
// 状态（跨平台可读）
// ---------------------------------------------------------------------------

/// App 当前是否处于「已暂停」（activity 在后台）。
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

/// 把 Java 侧的 `nativeOnPause` / `nativeOnResume` 绑到本模块的 C 符号上。
///
/// 由 `android_main` 调用（见 [`crate::app::android_main_entry`]）。非 Android
/// 平台是 no-op。
///
/// 绑定失败**不是致命错误**：App 照常跑，只是没有生命周期信号（= 旧行为）。
/// 典型原因是用户的 `MainActivity.java` 没照模板声明那两个方法 —— 所以这里
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
             `private static native void {JAVA_METHOD_RESUME}();`, and call them from \
             onPause()/onResume(). Without them the app keeps rendering while backgrounded."
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

        // SAFETY: 两个函数指针都是 `extern "system" fn(EnvUnowned, JClass)`，
        // 与「static native 方法 + `()V` 签名」的要求一致（见 jni 文档）。
        // 二者无捕获、无状态，不持任何 JNI 引用。
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
        // 这两个字符串是 Java↔Rust 契约。改名会让已发布的 MainActivity 模板静默失效
        // （绑定失败只 warn，用户会看到「后台仍全速」而不知道原因）。
        assert_eq!(JAVA_METHOD_PAUSE, "nativeOnPause");
        assert_eq!(JAVA_METHOD_RESUME, "nativeOnResume");
    }

    #[test]
    fn desktop_never_reports_paused() {
        // 桌面没有 Activity 生命周期，必须恒 false，否则渲染线程会永久停摆。
        if !cfg!(target_os = "android") {
            assert!(!is_paused());
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
}
