//! 平台专用窗口扩展。
//!
//! 只放「无法跨平台 1:1、必须按 OS 特化」的能力。模块按 OS 门控：
//! 非目标平台编译时模块**不存在**（非空壳），不进 prelude。

#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(not(target_os = "windows"))]
pub mod windows {
    //! Windows 平台存根（非 Windows 目标编译时的空实现）。
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
    pub(crate) fn win_hwnd_stub(_window: &winit::window::Window) -> Option<isize> {
        None
    }
    // 别名，供 window/mod.rs 的 `win_hwnd` 导入兼容
    pub(crate) fn win_hwnd_isize(_window: &winit::window::Window) -> Option<isize> {
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
    pub(crate) fn cleanup_window_state(_hwnd: isize) {}
    pub fn dwm_timing() -> Option<(u64, u64)> {
        None
    }
    pub fn qpc_now() -> u64 {
        0
    }
    pub fn qpc_ticks_per_sec() -> u64 {
        1
    }
}
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(not(target_os = "macos"))]
pub mod macos {}
