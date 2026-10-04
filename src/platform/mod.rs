//! 平台专用窗口扩展。
//!
//! 只放「无法跨平台 1:1、必须按 OS 特化」的能力。模块按 OS 门控：
//! 非目标平台编译时模块**不存在**（非空壳），不进 prelude。
//!
//! 用法：`use vireo::platform::windows::WindowExtWindows;` 后在 `VireoWindow` 上调用扩展方法。

#[cfg(target_os = "windows")]
/// Windows 专用扩展（`WindowExtWindows` trait + NC API）。
pub mod windows;
#[cfg(not(target_os = "windows"))]
/// Windows 存根（非 Windows 目标编译时的空实现，保持跨平台 API 兼容）。
// 这里的每个符号都只为「让 `window/mod.rs` 不必到处 `#[cfg(windows)]`」而存在，
// 在非 Windows 目标上必然没有任何调用方 —— 这不是死代码，是编译期占位。
// 同理适用于下面 `nc_tx`（只有 Windows 的 NC API 会读它）。
#[allow(dead_code)]
pub mod windows {
    //! 提供与 `windows.rs` 相同的公开符号，使上层 `window/mod.rs`
    //! 无需 `#[cfg(windows)]` 即可调用，cfg 只在 `mod` 门控。

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum CornerPreference {
        #[default]
        Default,
        DoNotRound,
        Round,
        RoundSmall,
    }
    impl CornerPreference {
        pub(crate) fn into_winit(self) -> u32 {
            0
        }
    }

    pub(crate) struct HitTestCallback(
        pub(crate) Box<dyn FnMut(crate::nc::HitTestInput) -> crate::nc::NonClientHit>,
    );
    unsafe impl Send for HitTestCallback {}

    pub(crate) enum NcUpdate {
        SetRegions(Vec<crate::nc::NonClientRegion>),
        SetHitTestCb(HitTestCallback),
        ClearHitTestCb,
        ClearAll,
    }

    pub fn win_hwnd(_window: &winit::window::Window) -> Option<isize> {
        None
    }
    pub fn nc_get_regions(_hwnd: isize) -> Option<Vec<crate::nc::NonClientRegion>> {
        None
    }
    // 平台 helpers — 非 Windows 空实现
    pub(crate) fn nc_apply(_hwnd: isize, _update: NcUpdate) {}
    pub fn nc_remove(_hwnd: isize) {}
    pub fn install(_hwnd: isize, _titlebar: bool, _border: bool) {}
    pub fn set_frame(_hwnd: isize, _titlebar: bool, _border: bool) {}
    pub fn remove(_hwnd: isize) {}
    pub fn set_aspect_ratio(_hwnd: isize, _ratio: Option<f64>) {}
    pub(crate) fn drop_thumbar_icons(_hwnd: isize) {}
    pub(crate) fn drop_overlay_icons(_hwnd: isize) {}
    pub(crate) fn clear_thumbar_callback(_hwnd: isize) {}
    pub(crate) fn remove_window_icons_entry(_hwnd: isize) {}
    pub(crate) fn set_thumbar_callback(_hwnd: isize, _cb: Box<dyn FnMut(u32)>) {}
    pub(crate) fn apply_window_opacity(_hwnd: isize, _opacity: f64) {}
    pub(crate) fn apply_window_focusable(_hwnd: isize, _focusable: bool) {}
}
#[cfg(target_os = "macos")]
/// macOS 专用扩展（窗口样式、事件等）。
pub mod macos;
#[cfg(not(target_os = "macos"))]
/// macOS 存根（非 macOS 目标编译时的空实现）。
pub mod macos {}
#[cfg(target_os = "android")]
/// Android 专用扩展（系统 UI 显隐、安全区、挖孔模式、窗口属性）。
pub mod android;

/// Android 文本输入（IME）数据模型与 UTF-16↔UTF-8 换算。
///
/// **无条件编译**（换算逻辑是纯函数，需要在桌面 `cargo test --lib` 里真跑起来；
/// 挂进 `android` 的话单测永不执行）。桌面非 test 构建下部分项无人调用，
/// 故模块内有 `allow(dead_code)`。由 `android` re-export，非安卓那边
/// `android` 模块不存在。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub mod android_text_input;

/// Android 生命周期桥（`onPause` / `onResume` → 暂停渲染）。
///
/// **无条件编译**：渲染线程要在所有平台上读 `is_paused`，而桌面恒为 `false`。
/// 只有 JNI 绑定那部分是 `cfg(target_os = "android")`。
///
/// 背景与方案取舍见模块文档。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub mod android_lifecycle;

/// Android 平台 API 的**纯逻辑**部分（`Insets` / `SystemBars` bitmask / AOSP
/// 常量 / 各枚举 / 安全区换算）。
///
/// **无条件编译**，为的是让其中的单测能在 `cargo test --lib`（桌面）里跑到 ——
/// `android.rs` 整个被 `cfg(target_os = "android")` 门控，它内部的
/// `#[cfg(test)] mod tests` 在开发机上**从未执行过**。
///
/// 分界标准与逐项归属见该模块文档。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub mod android_pure;
