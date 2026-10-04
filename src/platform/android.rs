//! Android 专用窗口扩展：系统 UI 显隐、安全区、挖孔模式、窗口属性。
//!
//! 需要显式导入后调用：
//! ```no_run
//! use vireo::platform::android::{SystemBarBehavior, SystemBars, WindowExtAndroid};
//!
//! win.set_system_bars(SystemBars::NONE)?;
//! win.set_system_bar_behavior(SystemBarBehavior::ShowTransientBarsBySwipe)?;
//! let safe = win.safe_area();
//! # Ok::<(), vireo::error::VireoError>(())
//! ```
//!
//! 非 Android 平台本模块不存在（`#[cfg(target_os = "android")]` 门控）。
//!
//! # 为什么放在 `platform` 而不是核心 API 面
//!
//! winit 0.30 的 [`winit::platform::android::WindowExtAndroid`] 只有 `content_rect()` 和
//! `config()` 两个方法，`FEATURES.md` 里 Android 的 Fullscreen / Decorations / Resizing
//! 全标 N/A，而 Platform 章节列了 iOS 的三条系统 UI 能力却完全没有 Android 章节——
//! winit 明确表达「Android 没有这类能力」。vireo 补上，正是 `platform` 模块存在的意义。
//!
//! # 设计依据（跨引擎调研）
//!
//! 完整调研见 `.opencode/android_platform_design.md`。三条关键取舍：
//!
//! - **安全区给三个值而不是一个**（Flutter 模型）。Unity 的单值 `Screen.safeArea` 语义
//!   会随构建期开关漂移，Godot 4.4→4.5 也因此回归（godot#109768）。三个值分别是
//!   [`safe_area`] / [`padding`] / [`ime_insets`]。
//! - **系统栏是 bitmask + 独立 behavior 轴**（不是单值枚举）。Android 原生
//!   `WindowInsets.Type` 就是 bitflag，且「只隐藏状态栏不隐藏导航栏」是真实需求。
//!   behavior 只提供 Android 31+ 真正支持的两档，不提供僵尸枚举
//!   `BEHAVIOR_SHOW_BARS_BY_TOUCH`（API 30+ 废弃、S+ 无效）。
//! - **不支持时返回 `Result`，不静默 no-op**。见 crate 内 AGENTS「表达规则」。
//!
//! # 平台强制行为（不要试图绕过）
//!
//! - **targetSdk 35+ 强制 edge-to-edge**：内容延伸到系统栏下方，应用无法退出
//!   （API 36 起彻底不可退出）。此时 [`WindowExtAndroid::set_system_bars`] 的
//!   `hide(systemBars)` 可能被平台忽略，用
//!   [`WindowExtAndroid::effective_system_bars`] 查实际效果。
//! - **桌面窗口化模式下 caption bar 永远可见**，即使开了沉浸式（Android 大屏行为）。
//! - Android 官方明确「**不要在个人设备上永久隐藏系统栏**」，沉浸式应给用户滑动唤回。
//!
//! # 建窗期属性：vireo 管不了，必须写进 manifest
//!
//! 上面所有 API 都是**运行期**的。但 Android 上真正决定窗口形态的属性绝大多数是
//! **APK 构建期**的 manifest 条目，**vireo 作为库无法注入**——原因不是偷懒：
//!
//! - 这些条目写在 APK 的 `AndroidManifest.xml` 里，由**系统进程**在拉起我们的
//!   Activity **之前**读取并施加（决定窗口朝向、是否 letterbox、键盘弹出时怎么 resize）。
//! - vireo 只能通过 `WindowAttributes` 传给 winit，而 winit 在 Android 上
//!   **基本全部忽略** `WindowAttributes`。
//! - 即使运行期改 `Window.setAttributes`，也**不会触发 WM 重算窗口边框**——
//!   实测：改 `layoutInDisplayCutoutMode` 后窗口仍被 letterbox
//!   （物理 `2264×1080` vs 屏幕 `2392×1080`，差的 `128px` 正好是挖孔区）。
//!
//! 涉及的条目：
//!
//! | manifest 条目 | 作用 | 备注 |
//! |---|---|---|
//! | `android:screenOrientation` | 朝向 | 纯竖屏 app 必填 |
//! | `android:configChanges` | 自行处理旋转/密度变化 | 不填则旋转时 Activity 重建 |
//! | `android:windowSoftInputMode` | 键盘弹出时是否 resize | 见下，**对文字输入影响最大** |
//! | `<style>` 的 `android:windowLayoutInDisplayCutoutMode` | 内容是否延伸到挖孔区 | 见下 |
//!
//! ## `windowSoftInputMode`
//!
//! - `adjustResize`（游戏常用）：键盘弹出时窗口变小 → 配合本模块的
//!   `WindowExtAndroid::ime_insets()` 做布局避让。
//! - `adjustPan` / `adjustNothing`（编辑类常用）：窗口不动，由系统平移/遮挡。
//!
//! 三种都**不会**让 vireo 拿到输入法的文本——那是 IME 后端的事，见 crate 文档的
//! 文本输入一节。
//!
//! ## 挖孔（`windowLayoutInDisplayCutoutMode`）
//!
//! - `never`：不延伸 → 挖孔区黑边（**vireo 当前的实测状态**）。
//! - `shortEdges` / `always`（API 30+）：内容延伸到挖孔区。
//!
//! 需要写在 `<style>` 里（`Theme.Vireo` 之类），且**要求 `targetSdk ≥ 35`**；
//! `targetSdk < 35` 时该属性被系统忽略。走 `xbuild` 时默认 target SDK 够新，
//! 但注意 `xbuild` **不支持显式设置 `target_sdk_version`**（内置 NDK 报
//! `ndk doesn't support sdk version`），所以只能吃默认值。
//!
//! # 两个刻意缺席的方法
//!
//! | API | Android 状况 | vireo 的处理 |
//! |---|---|---|
//! | `set_ime_cursor_area` | Android **有** `Window.setCursorAnchorInfo`（API 21+），但候选窗位置最终由输入法实现决定，不保证生效 | **不提供**。JNI 通道现成，但收益不确定、无法验证，不值得占公开 API 面 |
//! | `fullscreen(Borderless)` / `frame_style(Frameless)` | Android **无**「窗口模式」概念：全屏由系统栏显隐表达，无边框则自己画 UI | 用户意图用 [`set_system_bars`](WindowExtAndroid::set_system_bars) 覆盖 |
//!
//! 曾经缺席的 `set_input_type` **现已实现**（`EditorInfo.inputType`）。
//! 它先前被列为「无处可写」，理由是「NativeActivity 从不创建 InputConnection」——
//! 切换到 GameActivity 后该前提不成立（真机已确认搜狗输入法 `onStartInput` 连上），
//! 该阻塞已解除。
//!
//! 反过来，桌面 `VireoWindowLevel` 的 `AlwaysOnBottom` 在 Android **没有**对应档
//! （低于 `TYPE_APPLICATION` 的都是系统保留窗口），所以 [`VireoWindowLevel`] 是按
//! Android 的 `LayoutParams.type` 重新表达的，不是硬套 winit 四档。
//!
//! ## 参考 manifest
//!
//! ```xml
//! <activity android:name=".MainActivity"
//!           android:screenOrientation="landscape"
//!           android:configChanges="orientation|screenSize|screenLayout|keyboardHidden|density|uiMode"
//!           android:windowSoftInputMode="adjustResize"
//!           android:exported="true">
//!   <meta-data android:name="android.app.lib_name" android:value="pong" />
//! </activity>
//! ```
//!
//! 对应 `xbuild` 的 `manifest.yaml`：
//!
//! ```yaml
//! manifest:
//!   activities:
//!     - name: .MainActivity
//!       screenOrientation: landscape
//!       configChanges: orientation|screenSize|screenLayout|keyboardHidden|density|uiMode
//!       windowSoftInputMode: adjustResize
//! ```

// 纯逻辑部分（Insets / SystemBars / 枚举 / AOSP 常量 / 安全区换算）在
// `android_pure` —— 那个模块无条件编译，所以它的单测能在桌面上跑。
pub use super::android_pure::{
    DisplayCutoutMode, DisplayOrientation, Insets, Orientation, SystemBarBehavior, SystemBars,
    VireoWindowLevel,
};
pub(crate) use super::android_pure::{SAFETY_MASK, api, to_logical};
use crate::error::VireoError;
use crate::math::Rect;
use crate::window::VireoWindow;
use jni::{jni_sig, jni_str};

// 文本输入的数据模型与换算放在无条件编译的兄弟模块（便于桌面跑单测），
// 这里 re-export，用户仍从 `vireo::platform::android::*` 取。
use crate::platform::android_text_input::set_ime_wanted;
pub use crate::platform::android_text_input::{
    InputType, TextInputState, TextSpan, byte_to_utf16, ime_wanted, utf16_to_byte,
};

/// Android API level 常量（稳定值，见 `android.view.WindowInsets.Type` 等）。

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// JNI 内部错误（不泄漏到公开 API）
// ---------------------------------------------------------------------------

pub(crate) enum JniErr {
    Java(jni::errors::Error),
    Null(&'static str),
    NoActivity,
    Unsupported(&'static str),
}

impl From<jni::errors::Error> for JniErr {
    fn from(e: jni::errors::Error) -> Self {
        Self::Java(e)
    }
}

impl core::fmt::Debug for JniErr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Java(e) => write!(f, "jni: {e:?}"),
            Self::Null(what) => write!(f, "null java object: {what}"),
            Self::NoActivity => write!(f, "android activity unavailable"),
            Self::Unsupported(what) => write!(f, "unsupported on this api level: {what}"),
        }
    }
}

type JRes<T> = Result<T, JniErr>;

/// `safe_area` / `padding` 用的 `WindowInsets.Type` 掩码。
///
/// **必须显式 `or` `displayCutout()`** —— Android 的 `systemBars()` 聚合值
/// （statusBars | navigationBars | captionBar | systemOverlays）**不含**刘海，
/// 这是 Unity / Godot 安全区出错的根源之一。
/// `ime()` 刻意不在内：键盘遮挡走独立的 [`WindowExtAndroid::ime_insets`]。

fn sdk() -> i32 {
    crate::android::AndroidApp::sdk_version()
}

/// 附加到当前线程的 JVM 并拿到 Activity 对象。
fn with_activity<F, R>(app: &crate::android::AndroidApp, f: F) -> JRes<R>
where
    F: FnOnce(&mut jni::Env, &jni::objects::JObject<'_>) -> JRes<R>,
{
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) };
    vm.attach_current_thread(|env| {
        let raw = app.activity_as_ptr() as jni::sys::jobject;
        if raw.is_null() {
            return Err(JniErr::NoActivity);
        }
        // SAFETY: `raw` 来自 android-activity 持有的 jobject（native glue 持有的全局引用），
        // 在 activity 存活期间有效；此处只借用不删除，符合 android-activity 文档的
        // 「unowned reference」约定。
        let activity = unsafe { jni::objects::JObject::from_raw(env, raw) };
        f(env, &activity)
    })
}

/// `Activity.getWindow()`
fn get_window<'local>(
    env: &mut jni::Env<'local>,
    activity: &jni::objects::JObject<'_>,
) -> JRes<jni::objects::JObject<'local>> {
    Ok(env
        .call_method(
            activity,
            jni_str!("getWindow"),
            jni_sig!("()Landroid/view/Window;"),
            &[],
        )?
        .l()?)
}

/// `Window.getDecorView()`
fn get_decor_view<'local>(
    env: &mut jni::Env<'local>,
    window: &jni::objects::JObject<'_>,
) -> JRes<jni::objects::JObject<'local>> {
    Ok(env
        .call_method(
            window,
            jni_str!("getDecorView"),
            jni_sig!("()Landroid/view/View;"),
            &[],
        )?
        .l()?)
}

/// `View.getRootWindowInsets()`（API 23+）
fn get_root_insets<'local>(
    env: &mut jni::Env<'local>,
    decor: &jni::objects::JObject<'_>,
) -> JRes<Option<jni::objects::JObject<'local>>> {
    let obj = env
        .call_method(
            decor,
            jni_str!("getRootWindowInsets"),
            jni_sig!("()Landroid/view/WindowInsets;"),
            &[],
        )?
        .l()?;
    if obj.is_null() {
        Ok(None)
    } else {
        Ok(Some(obj))
    }
}

/// 取物理像素内缩。API 30+ 走 `getInsets`/`getInsetsIgnoringVisibility`，
/// 更低版本分别退回 `getSystemWindowInset*` / `getStableInset*`。
fn read_insets_px(
    env: &mut jni::Env,
    insets_obj: &jni::objects::JObject,
    type_mask: u32,
    ignoring_visibility: bool,
) -> JRes<Insets> {
    if sdk() >= api::SDK_INSETS_CONTROLLER {
        // `jni_str!` 只接受字面量，所以两条分支各写一遍。
        let o = if ignoring_visibility {
            env.call_method(
                insets_obj,
                jni_str!("getInsetsIgnoringVisibility"),
                jni_sig!("(I)Landroid/graphics/Insets;"),
                &[jni::objects::JValue::Int(type_mask as i32)],
            )?
            .l()?
        } else {
            env.call_method(
                insets_obj,
                jni_str!("getInsets"),
                jni_sig!("(I)Landroid/graphics/Insets;"),
                &[jni::objects::JValue::Int(type_mask as i32)],
            )?
            .l()?
        };
        if o.is_null() {
            return Err(JniErr::Null("WindowInsets.getInsets"));
        }
        Ok(Insets {
            left: env.get_field(&o, jni_str!("left"), jni_sig!("I"))?.i()? as f32,
            top: env.get_field(&o, jni_str!("top"), jni_sig!("I"))?.i()? as f32,
            right: env.get_field(&o, jni_str!("right"), jni_sig!("I"))?.i()? as f32,
            bottom: env.get_field(&o, jni_str!("bottom"), jni_sig!("I"))?.i()? as f32,
        })
    } else {
        // API 30 以下没有 `getInsets*`，退回 `getSystemWindowInset*`（当前遮挡）
        // 与 `getStableInset*`（忽略可见性）。`jni_str!` 只收字面量，四条各写一遍。
        Ok(if ignoring_visibility {
            Insets {
                left: env
                    .call_method(
                        insets_obj,
                        jni_str!("getStableInsetLeft"),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()? as f32,
                top: env
                    .call_method(
                        insets_obj,
                        jni_str!("getStableInsetTop"),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()? as f32,
                right: env
                    .call_method(
                        insets_obj,
                        jni_str!("getStableInsetRight"),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()? as f32,
                bottom: env
                    .call_method(
                        insets_obj,
                        jni_str!("getStableInsetBottom"),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()? as f32,
            }
        } else {
            Insets {
                left: env
                    .call_method(
                        insets_obj,
                        jni_str!("getSystemWindowInsetLeft"),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()? as f32,
                top: env
                    .call_method(
                        insets_obj,
                        jni_str!("getSystemWindowInsetTop"),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()? as f32,
                right: env
                    .call_method(
                        insets_obj,
                        jni_str!("getSystemWindowInsetRight"),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()? as f32,
                bottom: env
                    .call_method(
                        insets_obj,
                        jni_str!("getSystemWindowInsetBottom"),
                        jni_sig!("()I"),
                        &[],
                    )?
                    .i()? as f32,
            }
        })
    }
}

/// 物理 px 内缩 → 逻辑 px（Dp）。`scale` 非法时原样返回，不产生 inf/NaN。

/// JNI 错误 → `VireoError`。
fn to_err(what: &str, e: JniErr) -> VireoError {
    VireoError::Other(format!("android: {what} failed: {e:?}"))
}

/// getter 失败：打 warn 并返回 `None`，让调用方走降级分支。
fn warn_getter<T>(what: &str, e: JniErr) -> Option<T> {
    log::warn!("vireo::platform::android: {what} failed: {e:?}");
    None
}

// ---------------------------------------------------------------------------
// VireoWindow 内部辅助（字段在 window/mod.rs，这里只放逻辑）
// ---------------------------------------------------------------------------

/// 上次请求的状态（本地缓存；平台可能改写，所以 getter 语义是「请求值」）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct AndroidRequested {
    pub system_bars: SystemBars,
    pub system_bar_behavior: SystemBarBehavior,
}

impl Default for AndroidRequested {
    fn default() -> Self {
        Self {
            system_bars: SystemBars::STATUS_NAVIGATION,
            system_bar_behavior: SystemBarBehavior::ShowTransientBarsBySwipe,
        }
    }
}

impl VireoWindow {
    /// 拿到底层 `AndroidApp`。
    pub(crate) fn android_app(&self) -> Result<&crate::android::AndroidApp, JniErr> {
        self.android_app.as_ref().ok_or(JniErr::NoActivity)
    }

    pub(crate) fn android_requested(&self) -> AndroidRequested {
        *self.android_requested.lock()
    }

    /// setter 前置检查：拿 `AndroidApp`，失败直接转 `VireoError` 上抛。
    fn require_android(
        &self,
        what: &'static str,
    ) -> Result<&crate::android::AndroidApp, VireoError> {
        self.android_app().map_err(|e| to_err(what, e))
    }

    pub(crate) fn set_android_requested(&self, f: impl FnOnce(&mut AndroidRequested)) {
        let mut g = self.android_requested.lock();
        f(&mut g);
    }

    /// 物理 px 内缩。
    pub(crate) fn read_insets_px(
        &self,
        type_mask: u32,
        ignoring_visibility: bool,
    ) -> Result<Insets, JniErr> {
        let app = self.android_app()?;
        with_activity(app, |env, activity| {
            let window = get_window(env, activity)?;
            let decor = get_decor_view(env, &window)?;
            let Some(insets_obj) = get_root_insets(env, &decor)? else {
                return Err(JniErr::Null("View.getRootWindowInsets"));
            };
            read_insets_px(env, &insets_obj, type_mask, ignoring_visibility)
        })
    }

    /// 逻辑 px 内缩（对外 getter 用）。
    fn read_insets_logical(&self, type_mask: u32, ignoring_visibility: bool) -> JRes<Insets> {
        let px = self.read_insets_px(type_mask, ignoring_visibility)?;
        Ok(to_logical(self.layout_scale() as f32, px))
    }

    /// 把一段 JNI 逻辑投递到 Android UI 线程执行（系统 UI 改动必须在 UI 线程）。
    /// 失败只打 warn——setter 的 `Result` 只表达「能不能发起」，实际生效用
    /// `effective_system_bars()` 之类查询（SDL 的 request/confirm 模型）。
    fn post_to_java_thread<F>(&self, what: &'static str, f: F)
    where
        F: FnOnce(&mut jni::Env, &jni::objects::JObject<'_>) -> JRes<()> + Send + 'static,
    {
        let app = match self.android_app() {
            Ok(a) => a.clone(),
            Err(e) => {
                log::warn!("vireo::platform::android: {what} not posted: {e:?}");
                return;
            }
        };
        // closure 要 move `app`，所以调用方再拿一份（`AndroidApp` 是廉价 Arc clone）。
        let owner = app.clone();
        app.run_on_java_main_thread(Box::new(move || {
            let vm = unsafe { jni::JavaVM::from_raw(owner.vm_as_ptr() as *mut jni::sys::JavaVM) };
            let res = vm.attach_current_thread(|env| {
                let raw = owner.activity_as_ptr() as jni::sys::jobject;
                if raw.is_null() {
                    return Err(JniErr::NoActivity);
                }
                let activity = unsafe { jni::objects::JObject::from_raw(env, raw) };
                f(env, &activity)
            });
            if let Err(e) = res {
                log::warn!("vireo::platform::android: {what} on UI thread failed: {e:?}");
            }
        }));
    }

    /// 实际执行系统栏显隐。API 30+ 走 `WindowInsetsController`，
    /// 更低版本退回 `View.setSystemUiVisibility`（legacy）。
    fn apply_system_bars(&self, bars: SystemBars, behavior: SystemBarBehavior) {
        let all = api::STATUS_BARS | api::NAVIGATION_BARS | api::CAPTION_BAR;
        let mask = bars.bits();

        if sdk() >= api::SDK_INSETS_CONTROLLER {
            self.post_to_java_thread("set_system_bars", move |env, activity| {
                let window = get_window(env, activity)?;
                let ctrl = env
                    .call_method(
                        window.as_ref(),
                        jni_str!("getInsetsController"),
                        jni_sig!("()Landroid/view/WindowInsetsController;"),
                        &[],
                    )?
                    .l()?;
                if ctrl.is_null() {
                    return Err(JniErr::Null("Window.getInsetsController"));
                }
                env.call_method(
                    &ctrl,
                    jni_str!("setSystemBarsBehavior"),
                    jni_sig!("(I)V"),
                    &[jni::objects::JValue::Int(behavior.to_android())],
                )?;
                if mask != 0 {
                    env.call_method(
                        &ctrl,
                        jni_str!("hide"),
                        jni_sig!("(I)V"),
                        &[jni::objects::JValue::Int(mask as i32)],
                    )?;
                }
                let show_mask = all & !mask;
                if show_mask != 0 {
                    env.call_method(
                        &ctrl,
                        jni_str!("show"),
                        jni_sig!("(I)V"),
                        &[jni::objects::JValue::Int(show_mask as i32)],
                    )?;
                }
                Ok(())
            });
        } else {
            // legacy：组合 setSystemUiVisibility 标志（API 30+ 起废弃，但 minSdk 21 需要）
            let mut flags = api::LEGACY_LAYOUT_STABLE;
            let mut hide_mask = 0;
            if mask & api::STATUS_BARS != 0 {
                hide_mask |= api::LEGACY_FULLSCREEN;
                flags |= api::LEGACY_LAYOUT_FULLSCREEN;
            }
            if mask & api::NAVIGATION_BARS != 0 {
                hide_mask |= api::LEGACY_HIDE_NAVIGATION;
                flags |= api::LEGACY_LAYOUT_HIDE_NAVIGATION;
            }
            let sticky = behavior == SystemBarBehavior::ShowTransientBarsBySwipe;
            let effective = if hide_mask == 0 {
                flags
            } else if sticky {
                flags | hide_mask | api::LEGACY_IMMERSIVE_STICKY
            } else {
                flags | hide_mask
            };
            self.post_to_java_thread("set_system_bars(legacy)", move |env, activity| {
                let window = get_window(env, activity)?;
                let decor = get_decor_view(env, &window)?;
                env.call_method(
                    &decor,
                    jni_str!("setSystemUiVisibility"),
                    jni_sig!("(I)V"),
                    &[jni::objects::JValue::Int(effective)],
                )?;
                Ok(())
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Android 窗口扩展。
///
/// 平台特化 API，需显式 `use vireo::platform::android::WindowExtAndroid;`。
/// 非 Android 平台本 trait 不存在（模块编译期缺失）。
///
/// **setter 一律返回 `Result`**：Android 上「不支持」意味着「需要 JNI 而未实现」，
/// 是框架 invariant 缺失，不能静默 no-op（参照 SDL 的
/// 「just a request, it can be denied」模型）。`Ok(())` 只表示请求已投递到 UI 线程，
/// 实际是否生效用 [`effective_system_bars`](Self::effective_system_bars) 查。
///
/// **getter 返回具体值**：这些是每帧热路径（音游 UI 本来就每帧重建），失败时打
/// `log::warn!` 并返回零值 / 空列表。返回 `Option` 的表示「平台查不到」。
pub trait WindowExtAndroid {
    // ---- 安全区（Flutter 三值模型）----

    /// **物理最大遮挡区**：刘海 + 系统栏，**不管它们当前是否可见**。
    ///
    /// = Android `getInsetsIgnoringVisibility(systemBars|displayCutout)`
    /// = Flutter `MediaQuery.viewPadding`
    ///
    /// 与 [`padding`](Self::padding) 的区别：系统栏隐藏后 `padding` 变 0，
    /// 但 `safe_area` 保持物理值。做「HUD 固定避开刘海」应该用这个。
    fn safe_area(&self) -> Insets;

    /// **当前可见遮挡**：系统栏隐藏后即为 0。
    ///
    /// = Android `getInsets(systemBars|displayCutout)`
    ///
    /// 单位是 vireo 逻辑像素。**布局跟随时应该用这个**。
    ///
    /// ⚠️ **它与键盘是独立的两个值，不是差集**。这里读的是
    /// `systemBars|displayCutout` 且**不含** `ime`，所以它**不会**因为键盘弹出
    /// 而自动变大或变小。某些 ROM 在键盘弹出时会顺带隐藏导航栏，那时这个值会
    /// 变小 —— 但那是 ROM 行为，不是契约，别依赖。
    ///
    /// 换 Flutter 的说法：它大致等于 `MediaQuery.padding`，但**不是**
    /// `max(0, safe_area - ime_insets)` 那个公式。照搬该公式会和本 getter
    /// 得到不同的数。要「避开键盘」请把 [`ime_insets`](Self::ime_insets) 单独
    /// 加进来，不要指望本值已经含了键盘。
    fn padding(&self) -> Insets;

    /// 软键盘等「完全遮挡」区域。
    ///
    /// = Android `getInsets(ime)` = Flutter `viewInsets`
    ///
    /// 音游底部 HUD + 中文输入法场景必须把它和 `safe_area` 分开算，否则 HUD 被键盘顶掉。
    fn ime_insets(&self) -> Insets;

    /// 摄像头 / 传感器占用的物理矩形（可能多个；无挖孔返回空）。
    ///
    /// = Godot `get_display_cutouts()` / Unity `Screen.cutouts`
    ///
    /// 这些区域**物理上无法显示内容**（背后是镜头模组），但**周边可以**——
    /// 所以「内容穿挖孔」和「这里是黑块」是两件事，见
    /// [`set_display_cutout_mode`](Self::set_display_cutout_mode)。
    fn cutouts(&self) -> Vec<Rect>;

    // ---- 系统栏 ----

    /// 设置要**隐藏**哪些系统栏（集合外的保持系统默认可见）。
    ///
    /// 游戏的沉浸式配置 = [`SystemBars::ALL`] +
    /// [`set_system_bar_behavior`](Self::set_system_bar_behavior) 配
    /// [`ShowTransientBarsBySwipe`](SystemBarBehavior::ShowTransientBarsBySwipe)
    /// （滑动唤回时应用仍能收到 touch）。
    ///
    /// 只隐藏状态栏、保留导航栏则传 [`SystemBars::STATUS`]。
    /// **注意**：[`SystemBars::NONE`] 是「一个都不隐藏」，不是「全隐藏」。
    ///
    /// **targetSdk 35+ 强制 edge-to-edge**，此时系统可能忽略 `hide(systemBars)`；
    /// 用 [`effective_system_bars`](Self::effective_system_bars) 查实际效果。
    fn set_system_bars(&self, bars: SystemBars) -> Result<(), VireoError>;

    /// 上次**请求**的系统栏集合（本地缓存，不查询平台）。
    fn system_bars(&self) -> SystemBars;

    /// 隐藏后边缘滑动唤回的行为。**游戏应选
    /// [`ShowTransientBarsBySwipe`](SystemBarBehavior::ShowTransientBarsBySwipe)**，
    /// 这样滑动时应用仍能收到 touch。
    fn set_system_bar_behavior(&self, behavior: SystemBarBehavior) -> Result<(), VireoError>;

    /// 上次**请求**的 behavior（本地缓存）。
    fn system_bar_behavior(&self) -> SystemBarBehavior;

    /// 系统栏**实际**可见状态，逐条返回。`None` = 平台查不到（API < 30 无可靠查询）。
    ///
    /// 与 [`set_system_bars`](Self::set_system_bars) 的**请求值**对照，可以确认
    /// 系统是否照办、或被 ROM / 用户手势改掉 —— 对应 SDL 的
    /// 「just a request, it can be denied」模型。
    ///
    /// 返回值只含 `STATUS` / `NAVIGATION` / `CAPTION` 三类里**当前可见**的那些：
    /// 全可见 = `SystemBars::ALL`，全隐藏 = `SystemBars::NONE`。
    ///
    /// ⚠️ 这是**活读** `WindowInsets.isVisible(type)`，逐类查 3 次 JNI。
    /// 它是诊断 / 对齐用的，别放进每帧几十次的热路径。
    fn effective_system_bars(&self) -> Option<SystemBars>;

    // ---- 布局模式 ----

    /// 内容是否延伸进摄像头挖孔区。
    ///
    /// 对应 Android `WindowManager.LayoutParams.layoutInDisplayCutoutMode`（API 28+）。
    ///
    /// # ⚠️ 运行期只是 best-effort，真机上实测**不足以**让窗口扩展进挖孔区
    ///
    /// `layoutInDisplayCutoutMode` 是**建窗期读取的 theme 属性**
    /// （`android:windowLayoutInDisplayCutoutMode`）。本方法走
    /// `Window.setAttributes()` 在运行期改它，**不会触发窗口管理器重算窗口边框**。
    /// 真机（Android 16 / 25098RA98C / targetSdk 34）实测：调用成功、getter 正确
    /// 回报 `Always`，但窗口仍被 letterbox（2264×1080 而非屏幕 2392×1080），
    /// 挖孔那 128px 依然没有 App 表面。`set_edge_to_edge(true)` 同测无效。
    ///
    /// # 真正可靠的途径（**app 侧，不是 vireo API**）
    ///
    /// 1. **targetSdk ≥ 35**：平台强制 edge-to-edge，`Always` 成为事实。
    ///    Android 16（API 36）起**无法退出**。
    ///    （注：`xbuild` 内置 NDK 最高只支持 sdk 34，此路径 vireo 侧未验证。）
    /// 2. **manifest theme 属性**：App 自带 `styles.xml` 里写
    ///    `android:windowLayoutInDisplayCutoutMode="always"`。
    ///    vireo 不打包 `styles.xml`，无法代劳——这也是本方法只在运行期提供的根因。
    ///
    /// 换句话说：**这个开关最终由 app 的 manifest / targetSdk 决定**，
    /// 本方法用于「已按其中一种配置好之后，仍需在运行期查询/微调」的场景。
    fn set_display_cutout_mode(&self, mode: DisplayCutoutMode) -> Result<(), VireoError>;

    /// 当前挖孔模式。`None` = 平台查不到（API < 28）。
    fn display_cutout_mode(&self) -> Option<DisplayCutoutMode>;

    /// 内容是否延伸到系统栏下方（`Window.setDecorFitsSystemWindows(false)`，API 30+）。
    fn set_edge_to_edge(&self, enabled: bool) -> Result<(), VireoError>;

    // ---- 朝向 ----

    /// 运行期请求屏幕朝向（横屏游戏刚需；构建期只能靠 manifest）。
    fn set_orientation(&self, orientation: Orientation) -> Result<(), VireoError>;

    /// 当前**实际**显示朝向。与 [`Orientation`]（请求）刻意分开。
    fn orientation(&self) -> DisplayOrientation;

    // ---- 窗口属性 ----

    /// 保持屏幕常亮（`FLAG_KEEP_SCREEN_ON`）。
    ///
    /// 游戏默认就该开：否则用户玩着玩着息屏。
    fn set_keep_screen_on(&self, enabled: bool) -> Result<(), VireoError>;

    /// 屏幕亮度。`None` = 跟随系统；`Some(v)`，`v ∈ [0.0, 1.0]`。
    fn set_screen_brightness(&self, brightness: Option<f32>) -> Result<(), VireoError>;

    /// 窗口层级（`WindowManager.LayoutParams.type`）。
    ///
    /// ⚠️ [`VireoWindowLevel::Overlay`] 需要 `SYSTEM_ALERT_WINDOW` 权限**且**用户已授权，
    /// 否则系统抛 `BadTokenException`。但那个异常发生在**稍后的 UI 线程**上
    /// （本方法是 fire-and-forget，见 trait 级说明），所以**不会**变成返回的 `Err` ——
    /// `Ok(())` 只表示「请求已投递」。失败表现为 `log::warn!` 一行 + 层级没生效。
    ///
    /// 要确认是否真的生效，读 [`window_level`](Self::window_level)（活读
    /// `LayoutParams.type`）—— 它返回 `None` 就是没设上。
    fn set_window_level(&self, level: VireoWindowLevel) -> Result<(), VireoError>;

    /// 当前**实际**生效的窗口层级；`type` 是系统保留值时返回 `None`。
    ///
    /// 与 [`WindowExtAndroid::display_cutout_mode`] 一致走活读（`LayoutParams.type`），
    /// 不走本地缓存——层级有可能被平台或第三方库改掉。
    fn window_level(&self) -> Option<VireoWindowLevel>;

    // ---- 文本输入（IME）------------------------------------------------

    /// 弹出软键盘（**强制显示**）。
    ///
    /// 内部用 `show_implicit = false`：游戏没有聚焦的 View，隐式请求永不发生，
    /// 传隐式会导致键盘永远不弹。getter见 [`ime_wanted`](ime_wanted)。
    ///
    /// # 可以每帧调，但不能周期性重发
    ///
    /// 调用按「请求值是否变化」去重，只在真正要弹时才 post 一次到 Java 主线程。
    /// 帧回调里每帧无条件 post 会把主线程淹掉 → `Input dispatching timed out` ANR。
    ///
    /// **引擎不会周期性重发**（曾加过 200ms 重发窗口，副作用是输入法每次收到
    /// show 都重置组合态：候选词 / preedit 一闪即消，且用户按不掉收起键）。
    ///
    /// # 用户用系统按钮收起键盘后怎么办
    ///
    /// 键盘被收起时我们**观测不到**（`android-activity` 既不给 IME 可见性也不给
    /// 会话状态），`want` 会变陈旧，之后的 `show_soft_input()` 会被跳变去重跳过。
    /// 判定与恢复：
    ///
    /// - 判断键盘是否还在：看 [`WindowExtAndroid::ime_insets`] 的 `bottom`，
    ///   大于 0 即键盘占着屏幕。
    /// - 想重新拉起：先 [`WindowExtAndroid::hide_soft_input`]（把陈旧意图清成
    ///   false）再 [`WindowExtAndroid::show_soft_input`]，两次都是跳变，都会真下发。
    fn show_soft_input(&self) -> Result<(), VireoError>;

    /// 收起软键盘。
    ///
    /// **可以每帧调**：纯跳变去重，初始意图即「未请求显示」，所以未聚焦时
    /// 每帧调用**不会**产生任何 Java 主线程 post。
    fn hide_soft_input(&self) -> Result<(), VireoError>;

    /// 声明这里期望输入什么（`EditorInfo.inputType`），决定输入法的候选栏形态。
    ///
    /// **必须在 [`WindowExtAndroid::show_soft_input`] 之前调**（IME 会话建立时
    /// 才读这个字段）。中文务必用 [`InputType::Text`]（默认档，带拼音转写候选）。
    fn set_input_type(&self, purpose: InputType) -> Result<(), VireoError>;

    /// 拉取一次 IME 状态快照。**每帧轮询，不是事件驱动** —— Android 的
    /// `MainEvent` 里没有文本输入通知，只能 diff。
    ///
    /// 下标已从 Android 的 **UTF-16** 换算成 **UTF-8 字节**（见
    /// [`TextInputState`] 模块文档），可直接切片。
    fn text_input_state(&self) -> Option<TextInputState>;

    /// 把状态回填给 IME（光标/选区/文本）。
    ///
    /// 不做的话：用户点击输入框移动光标后 IME 的状态会失步，
    /// **下一次输入可能重复插入全部文本**（rib 在 winit PR #4451 上实测过）。
    fn set_text_input_state(&self, state: &TextInputState) -> Result<(), VireoError>;
}

impl WindowExtAndroid for VireoWindow {
    fn safe_area(&self) -> Insets {
        match self.read_insets_logical(SAFETY_MASK, true) {
            Ok(v) => v,
            Err(e) => warn_getter("safe_area", e).unwrap_or(Insets::ZERO),
        }
    }

    fn padding(&self) -> Insets {
        match self.read_insets_logical(SAFETY_MASK, false) {
            Ok(v) => v,
            Err(e) => warn_getter("padding", e).unwrap_or(Insets::ZERO),
        }
    }

    fn ime_insets(&self) -> Insets {
        match self.read_insets_logical(api::IME, false) {
            Ok(v) => v,
            Err(e) => warn_getter("ime_insets", e).unwrap_or(Insets::ZERO),
        }
    }

    fn cutouts(&self) -> Vec<Rect> {
        if sdk() < api::SDK_CUTOUT {
            return Vec::new();
        }
        let app = match self.android_app() {
            Ok(a) => a,
            Err(e) => {
                warn_getter::<()>("cutouts", e);
                return Vec::new();
            }
        };
        let scale = self.layout_scale() as f32;
        let inv = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        match with_activity(app, |env, activity| {
            let window = get_window(env, activity)?;
            let decor = get_decor_view(env, &window)?;
            let Some(insets_obj) = get_root_insets(env, &decor)? else {
                return Ok(Vec::new());
            };
            let cutout = env
                .call_method(
                    &insets_obj,
                    jni_str!("getDisplayCutout"),
                    jni_sig!("()Landroid/view/DisplayCutout;"),
                    &[],
                )?
                .l()?;
            if cutout.is_null() {
                log::debug!("vireo::platform::android: cutouts(): getDisplayCutout() null");
                return Ok(Vec::new());
            }
            let list = env
                .call_method(
                    &cutout,
                    jni_str!("getBoundingRects"),
                    jni_sig!("()Ljava/util/List;"),
                    &[],
                )?
                .l()?;
            if list.is_null() {
                log::debug!("vireo::platform::android: cutouts(): getBoundingRects() null");
                return Ok(Vec::new());
            }
            let size = env
                .call_method(&list, jni_str!("size"), jni_sig!("()I"), &[])?
                .i()?;
            if size <= 0 {
                log::debug!("vireo::platform::android: cutouts(): bounding rects empty");
                return Ok(Vec::new());
            }
            let mut out = Vec::with_capacity(size as usize);
            for i in 0..size {
                let item = env
                    .call_method(
                        &list,
                        jni_str!("get"),
                        jni_sig!("(I)Ljava/lang/Object;"),
                        &[jni::objects::JValue::Int(i)],
                    )?
                    .l()?;
                if item.is_null() {
                    continue;
                }
                let l = env.get_field(&item, jni_str!("left"), jni_sig!("I"))?.i()? as f32 * inv;
                let t = env.get_field(&item, jni_str!("top"), jni_sig!("I"))?.i()? as f32 * inv;
                let r = env
                    .get_field(&item, jni_str!("right"), jni_sig!("I"))?
                    .i()? as f32
                    * inv;
                let b = env
                    .get_field(&item, jni_str!("bottom"), jni_sig!("I"))?
                    .i()? as f32
                    * inv;
                out.push(Rect::new(l, t, (r - l).max(0.0), (b - t).max(0.0)));
            }
            Ok(out)
        }) {
            Ok(v) => v,
            Err(e) => {
                warn_getter::<()>("cutouts", e);
                Vec::new()
            }
        }
    }

    fn set_system_bars(&self, bars: SystemBars) -> Result<(), VireoError> {
        self.require_android("set_system_bars")?;
        let behavior = self.android_requested().system_bar_behavior;
        self.apply_system_bars(bars, behavior);
        self.set_android_requested(|r| r.system_bars = bars);
        Ok(())
    }

    fn system_bars(&self) -> SystemBars {
        self.android_requested().system_bars
    }

    fn set_system_bar_behavior(&self, behavior: SystemBarBehavior) -> Result<(), VireoError> {
        self.require_android("set_system_bar_behavior")?;
        let bars = self.android_requested().system_bars;
        self.apply_system_bars(bars, behavior);
        self.set_android_requested(|r| r.system_bar_behavior = behavior);
        Ok(())
    }

    fn system_bar_behavior(&self) -> SystemBarBehavior {
        self.android_requested().system_bar_behavior
    }

    fn effective_system_bars(&self) -> Option<SystemBars> {
        if sdk() < api::SDK_INSETS_CONTROLLER {
            return None;
        }
        let app = match self.android_app() {
            Ok(a) => a,
            Err(e) => return warn_getter("effective_system_bars", e),
        };
        match with_activity(app, |env, activity| {
            let window = get_window(env, activity)?;
            let decor = get_decor_view(env, &window)?;
            let Some(insets_obj) = get_root_insets(env, &decor)? else {
                return Ok(None);
            };
            // 逐类查，不能只查 status/nav 两类 —— caption bar（API 30+ 才有）
            // 单独可见时是真实状态，旧实现会把它整个丢掉。
            let mut bits = 0i32;
            for kind in [
                api::STATUS_BARS as i32,
                api::NAVIGATION_BARS as i32,
                api::CAPTION_BAR as i32,
            ] {
                let visible = env
                    .call_method(
                        &insets_obj,
                        jni_str!("isVisible"),
                        jni_sig!("(I)Z"),
                        &[jni::objects::JValue::Int(kind)],
                    )?
                    .z()?;
                if visible {
                    bits |= kind;
                }
            }
            Ok(Some(SystemBars::from_android_bits(bits)))
        }) {
            Ok(v) => v,
            Err(e) => warn_getter("effective_system_bars", e),
        }
    }

    fn set_display_cutout_mode(&self, mode: DisplayCutoutMode) -> Result<(), VireoError> {
        if sdk() < api::SDK_CUTOUT {
            return Err(to_err(
                "set_display_cutout_mode",
                JniErr::Unsupported("layoutInDisplayCutoutMode requires API 28"),
            ));
        }
        self.require_android("set_display_cutout_mode")?;
        let raw = mode.to_android();
        self.post_to_java_thread("set_display_cutout_mode", move |env, activity| {
            let window = get_window(env, activity)?;
            let attrs = env
                .call_method(
                    window.as_ref(),
                    jni_str!("getAttributes"),
                    jni_sig!("()Landroid/view/WindowManager$LayoutParams;"),
                    &[],
                )?
                .l()?;
            if attrs.is_null() {
                return Err(JniErr::Null("Window.getAttributes"));
            }
            env.set_field(
                &attrs,
                jni_str!("layoutInDisplayCutoutMode"),
                jni_sig!("I"),
                jni::objects::JValue::Int(raw),
            )?;
            env.call_method(
                window.as_ref(),
                jni_str!("setAttributes"),
                jni_sig!("(Landroid/view/WindowManager$LayoutParams;)V"),
                &[jni::objects::JValue::Object(&attrs)],
            )?;
            Ok(())
        });
        Ok(())
    }

    fn display_cutout_mode(&self) -> Option<DisplayCutoutMode> {
        if sdk() < api::SDK_CUTOUT {
            return None;
        }
        let app = match self.android_app() {
            Ok(a) => a,
            Err(e) => return warn_getter("display_cutout_mode", e),
        };
        match with_activity(app, |env, activity| {
            let window = get_window(env, activity)?;
            let attrs = env
                .call_method(
                    window.as_ref(),
                    jni_str!("getAttributes"),
                    jni_sig!("()Landroid/view/WindowManager$LayoutParams;"),
                    &[],
                )?
                .l()?;
            if attrs.is_null() {
                return Ok(None);
            }
            let v = env
                .get_field(&attrs, jni_str!("layoutInDisplayCutoutMode"), jni_sig!("I"))?
                .i()?;
            Ok(DisplayCutoutMode::from_android(v))
        }) {
            Ok(v) => v,
            Err(e) => warn_getter("display_cutout_mode", e),
        }
    }

    fn set_edge_to_edge(&self, enabled: bool) -> Result<(), VireoError> {
        if sdk() < api::SDK_DECOR_FITS {
            return Err(to_err(
                "set_edge_to_edge",
                JniErr::Unsupported("setDecorFitsSystemWindows requires API 30"),
            ));
        }
        self.require_android("set_edge_to_edge")?;
        self.post_to_java_thread("set_edge_to_edge", move |env, activity| {
            let window = get_window(env, activity)?;
            env.call_method(
                window.as_ref(),
                jni_str!("setDecorFitsSystemWindows"),
                jni_sig!("(Z)V"),
                &[jni::objects::JValue::Bool(enabled)],
            )?;
            Ok(())
        });
        Ok(())
    }

    fn set_orientation(&self, orientation: Orientation) -> Result<(), VireoError> {
        self.require_android("set_orientation")?;
        let raw = orientation.to_android();
        self.post_to_java_thread("set_orientation", move |env, activity| {
            env.call_method(
                activity,
                jni_str!("setRequestedOrientation"),
                jni_sig!("(I)V"),
                &[jni::objects::JValue::Int(raw)],
            )?;
            Ok(())
        });
        Ok(())
    }

    fn orientation(&self) -> DisplayOrientation {
        let app = match self.android_app() {
            Ok(a) => a,
            Err(e) => return warn_getter("orientation", e).unwrap_or_default(),
        };
        match with_activity(app, |env, activity| {
            let resources = env
                .call_method(
                    activity,
                    jni_str!("getResources"),
                    jni_sig!("()Landroid/content/res/Resources;"),
                    &[],
                )?
                .l()?;
            if resources.is_null() {
                return Ok(DisplayOrientation::Unknown);
            }
            let config = env
                .call_method(
                    &resources,
                    jni_str!("getConfiguration"),
                    jni_sig!("()Landroid/content/res/Configuration;"),
                    &[],
                )?
                .l()?;
            if config.is_null() {
                return Ok(DisplayOrientation::Unknown);
            }
            let o = env
                .get_field(&config, jni_str!("orientation"), jni_sig!("I"))?
                .i()?;
            Ok(match o {
                api::CONFIGURATION_PORTRAIT => DisplayOrientation::Portrait,
                api::CONFIGURATION_LANDSCAPE => DisplayOrientation::Landscape,
                _ => DisplayOrientation::Unknown,
            })
        }) {
            Ok(v) => v,
            Err(e) => warn_getter("orientation", e).unwrap_or_default(),
        }
    }

    fn set_keep_screen_on(&self, enabled: bool) -> Result<(), VireoError> {
        self.require_android("set_keep_screen_on")?;
        self.post_to_java_thread("set_keep_screen_on", move |env, activity| {
            let window = get_window(env, activity)?;
            // `jni_str!` 只收字面量，两条分支各写一遍
            let flags = &[jni::objects::JValue::Int(api::FLAG_KEEP_SCREEN_ON)];
            if enabled {
                env.call_method(
                    window.as_ref(),
                    jni_str!("addFlags"),
                    jni_sig!("(I)V"),
                    flags,
                )?;
            } else {
                env.call_method(
                    window.as_ref(),
                    jni_str!("clearFlags"),
                    jni_sig!("(I)V"),
                    flags,
                )?;
            }
            Ok(())
        });
        Ok(())
    }

    fn set_screen_brightness(&self, brightness: Option<f32>) -> Result<(), VireoError> {
        self.require_android("set_screen_brightness")?;
        let value = brightness.map(|v| v.clamp(0.0, 1.0)).unwrap_or(-1.0);
        self.post_to_java_thread("set_screen_brightness", move |env, activity| {
            let window = get_window(env, activity)?;
            let attrs = env
                .call_method(
                    window.as_ref(),
                    jni_str!("getAttributes"),
                    jni_sig!("()Landroid/view/WindowManager$LayoutParams;"),
                    &[],
                )?
                .l()?;
            if attrs.is_null() {
                return Err(JniErr::Null("Window.getAttributes"));
            }
            env.set_field(
                &attrs,
                jni_str!("screenBrightness"),
                jni_sig!("F"),
                jni::objects::JValue::Float(value),
            )?;
            env.call_method(
                window.as_ref(),
                jni_str!("setAttributes"),
                jni_sig!("(Landroid/view/WindowManager$LayoutParams;)V"),
                &[jni::objects::JValue::Object(&attrs)],
            )?;
            Ok(())
        });
        Ok(())
    }

    fn set_window_level(&self, level: VireoWindowLevel) -> Result<(), VireoError> {
        self.require_android("set_window_level")?;
        let raw = level.to_android();
        self.post_to_java_thread("set_window_level", move |env, activity| {
            let window = get_window(env, activity)?;
            let attrs = env
                .call_method(
                    window.as_ref(),
                    jni_str!("getAttributes"),
                    jni_sig!("()Landroid/view/WindowManager$LayoutParams;"),
                    &[],
                )?
                .l()?;
            if attrs.is_null() {
                return Err(JniErr::Null("Window.getAttributes"));
            }
            env.set_field(
                &attrs,
                jni_str!("type"),
                jni_sig!("I"),
                jni::objects::JValue::Int(raw),
            )?;
            // 未授权 SYSTEM_ALERT_WINDOW 时这里抛 BadTokenException，
            // 经 post_to_java_thread → `set_window_level` 上抛给调用方。
            env.call_method(
                window.as_ref(),
                jni_str!("setAttributes"),
                jni_sig!("(Landroid/view/WindowManager$LayoutParams;)V"),
                &[jni::objects::JValue::Object(&attrs)],
            )?;
            Ok(())
        });
        Ok(())
    }

    fn window_level(&self) -> Option<VireoWindowLevel> {
        let app = match self.android_app() {
            Ok(a) => a,
            Err(e) => return warn_getter("window_level", e),
        };
        match with_activity(app, |env, activity| {
            let window = get_window(env, activity)?;
            let attrs = env
                .call_method(
                    window.as_ref(),
                    jni_str!("getAttributes"),
                    jni_sig!("()Landroid/view/WindowManager$LayoutParams;"),
                    &[],
                )?
                .l()?;
            if attrs.is_null() {
                return Ok(None);
            }
            let v = env
                .get_field(&attrs, jni_str!("type"), jni_sig!("I"))?
                .i()?;
            Ok(VireoWindowLevel::from_android(v))
        }) {
            Ok(v) => v,
            Err(e) => warn_getter("window_level", e),
        }
    }

    // ---- 文本输入 ------------------------------------------------------

    fn show_soft_input(&self) -> Result<(), VireoError> {
        let app = self.require_android("show_soft_input")?;
        // 纯跳变去重，且**永不周期性重发** —— 输入法每次收到 show 都会重置组合态，
        // 真机上就是「候选词 / preedit 一闪即消」+「用户按不掉收起键」。
        if set_ime_wanted(true) {
            // `show_implicit = false`（flags = 0）= **强制显示**。
            // 传 `true` 会带上 `SHOW_IMPLICIT`，语义是「仅当输入法被隐式请求时才显示」——
            // 而游戏里没有聚焦的 View，永远不会有隐式请求，键盘就永远不弹（实测）。
            app.show_soft_input(false);
        }
        Ok(())
    }

    fn hide_soft_input(&self) -> Result<(), VireoError> {
        let app = self.require_android("hide_soft_input")?;
        // 纯跳变去重：初始 `want` 就是 false（没请求显示），键盘本来就没显示，
        // 帧回调里每帧 `hide_soft_input()` 也不会 post 一次 —— 这正是
        // 模板在未聚焦时每帧调它导致 ANR 的那一条路径。
        //
        // 这里也是「清掉陈旧请求值」的天然时机：调用方按文档说的
        // 「hide 再 show」把键盘拉回来时，本函数负责把 `want` 打成 false，
        // 于是紧接着的 `show_soft_input()` 是跳变、会真正下发。
        if set_ime_wanted(false) {
            app.hide_soft_input(false);
        }
        Ok(())
    }

    fn set_input_type(&self, purpose: InputType) -> Result<(), VireoError> {
        let app = self.require_android("set_input_type")?;
        let input_type =
            android_activity::input::InputType::from_bits_truncate(purpose.to_android());
        // IME_ACTION_UNSPECIFIED：vireo 自绘输入 UI，不让输入法替我们决定
        // 「完成/发送」这类语义。
        let action = android_activity::input::TextInputAction::Unspecified;
        // **必须**带 `IME_FLAG_NO_FULLSCREEN`，否则输入法在横屏下进入 AOSP 的
        // fullscreen extract mode，顶部弹全宽白条 +「执行」按钮，且 `ime()` inset
        // 恒为 0。详见 `IME_OPTIONS` 的文档（这正是本行曾经传 `empty()` 的后果）。
        let options = android_activity::input::ImeOptions::from_bits_truncate(
            crate::platform::android_text_input::IME_OPTIONS,
        );
        app.set_ime_editor_info(input_type, action, options);
        Ok(())
    }

    fn text_input_state(&self) -> Option<TextInputState> {
        let app = self.android_app().ok()?;
        //顶层 `AndroidApp::text_input_state()` 已经内部固定 `take = false`
        //（见 android-activity `src/lib.rs:1088` 的注释："guaranteed to return
        // Some if take is false so we can unwrap here"），所以：
        // -不会抢走android-activity 给 winit 投递 `InputEvent::TextEvent` 的标记；
        // - 返回值直接是 `TextInputState`（不是 Option）。
        let raw = app.text_input_state();
        Some(TextInputState::from_android_utf16(
            raw.text,
            raw.selection.start,
            raw.selection.end,
            raw.compose_region.map(|c| (c.start, c.end)),
        ))
    }

    fn set_text_input_state(&self, state: &TextInputState) -> Result<(), VireoError> {
        let app = self.require_android("set_text_input_state")?;
        let (text, sel_start, sel_end, compose) = state.to_android_utf16();
        let raw = android_activity::input::TextInputState {
            text,
            selection: android_activity::input::TextSpan {
                start: sel_start,
                end: sel_end,
            },
            compose_region: compose
                .map(|(start, end)| android_activity::input::TextSpan { start, end }),
        };
        app.set_text_input_state(raw);
        Ok(())
    }
}
