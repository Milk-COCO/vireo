//! Windows 窗口装饰分档：`titlebar` / `border` 独立开关。
//!
//! 只在 **`HiddenTitlebar`**（`titlebar(false) + border(true)` = 只留边框不留标题）
//! 时才用 `SetWindowSubclass` 挂一个 `WM_NCCALCSIZE` 子类接管非客户区计算：
//!
//! - 客户区 = 窗口矩形减去系统边框宽度（`SM_CXSIZEFRAME + SM_CXPADDEDBORDER`
//!   ≈ 8px），但**顶部不加 inset**（Electron `titleBarStyle:'hidden'` 语义）：
//!   客户区顶到窗口最上沿，DWM 无顶部非客户区可画 → 消除 Windows 10/11 把
//!   顶部边框画成不透明白条的残留。窗口矩形比客户区大，左/右/下窗外带由系统
//!   默认 `WM_NCHITTEST` 自动接管为 resize 热区（无需手写 hit-test）；
//!   顶部边缘因此失去系统 resize 热区（需 `drag_window`/`drag_resize_window`
//!   自绘或接受）。
//! - 最大化时把客户区钳到工作区 `rcWork`（镜像 winit 内部逻辑）——必须在本
//!   子类做：接管消息后 winit 的 wndproc 收不到 `WM_NCCALCSIZE`。
//!
//! **`Normal` 与 `Frameless` 完全不装子类，放行给 winit 原生**：
//! `Frameless` 用 winit `set_decorations(false)` + `WM_NCCALCSIZE` 返回 0
//! （客户区 = 整个窗口），行为与 vireo 旧实现逐位一致，且 winit 的
//! `undecorated_shadow`（`event_loop.rs` 里 `rgrc[0].top += 1`）不再被绕过。
//! 注意 winit 阴影**默认关闭**（`decoration_shadow: false`），需显式
//! `set_undecorated_shadow(true)` 才启用。
//!
//! 子类化用独立链表挂新 WNDPROC（`SetWindowSubclass`），`DefSubclassProc`
//! 自动调用原 wndproc，**不**触碰 winit 的 `GWL_WNDPROC` / `GWL_USERDATA`。
//!
//! 注意：这里 `windows-sys` 与 winit 各自独立的 windows-sys 版本类型不互通，
//! 但作为普通函数调用（传 `HWND = isize`）无碍。

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONULL,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetSystemMetrics, IsZoomed, NCCALCSIZE_PARAMS, SM_CXPADDEDBORDER,
    SM_CXSIZEFRAME, SM_CYSIZEFRAME,
};

const WM_NCCALCSIZE: u32 = 131;
// WVR_HREDRAW(0x0100) | WVR_VREDRAW(0x0200)，windows-sys 0.52.0 未定义。
const WVR_REDRAW: u32 = 0x0300;

/// 子类标识符（`SetWindowSubclass` 的 uIdSubclass）。只要与同一窗口上其他
/// 子类不冲突即可；这里用「VIR」的 ASCII。
const SUBCLASS_ID: usize = 0x0056_4952;

/// dwRefData 位布局：
/// bit0 = titlebar（系统标题栏开关）
/// bit1 = border（系统 resize 边框开关）
const REF_TITLEBAR: usize = 1 << 0;
const REF_BORDER: usize = 1 << 1;

fn encode_refdata(titlebar: bool, border: bool) -> usize {
    (if titlebar { REF_TITLEBAR } else { 0 })
        | (if border { REF_BORDER } else { 0 })
}

/// 安装装饰子类。幂等：同一 (proc, SUBCLASS_ID) 重复调用会更新 dwRefData。
/// 必须在窗口所属线程（winit 事件线程）调用。
pub fn install(hwnd: HWND, titlebar: bool, border: bool) {
    unsafe {
        SetWindowSubclass(
            hwnd,
            Some(frame_subclass_proc),
            SUBCLASS_ID,
            encode_refdata(titlebar, border),
        );
    }
}

/// 更新装饰分档（运行期切换 `titlebar`/`border`）。重调 `SetWindowSubclass`
/// 刷新 refdata，然后强制重算非客户区。
pub fn set_frame(hwnd: HWND, titlebar: bool, border: bool) {
    unsafe {
        SetWindowSubclass(
            hwnd,
            Some(frame_subclass_proc),
            SUBCLASS_ID,
            encode_refdata(titlebar, border),
        );
    }
    force_nccalc_recalc(hwnd);
}

/// 卸载装饰子类。
pub fn remove(hwnd: HWND) {
    unsafe {
        RemoveWindowSubclass(hwnd, Some(frame_subclass_proc), SUBCLASS_ID);
    }
    force_nccalc_recalc(hwnd);
}

/// 强制系统重发 `WM_NCCALCSIZE`（`SetWindowPos(SWP_FRAMECHANGED)`）。
fn force_nccalc_recalc(hwnd: HWND) {
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
            hwnd,
            0,
            0,
            0,
            0,
            0,
            windows_sys::Win32::UI::WindowsAndMessaging::SWP_FRAMECHANGED
                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE
                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
        );
    }
}

/// `WM_NCCALCSIZE` 子类回调。
///
/// 只在 `HiddenTitlebar`（`!titlebar && border`）时接管；`Normal`（titlebar）
/// 与 `Frameless`（无 border）都放行给 winit（`DefSubclassProc`）——这两档
/// 根本不该安装本子类，这里放行只是防御。
///
/// - `wparam == 0`：无 insets 调整请求，`DefWindowProc`。
/// - 最大化：客户区钳到所在显示器 `rcWork`。
/// - `border=true`：客户区 = 窗口矩形 - 系统边框宽度（顶部不加 inset）。
unsafe extern "system" fn frame_subclass_proc(
    hwnd: HWND,
    umsg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uidsubclass: usize,
    dwrefdata: usize,
) -> LRESULT {
    if umsg != WM_NCCALCSIZE {
        return unsafe { DefSubclassProc(hwnd, umsg, wparam, lparam) };
    }

    let titlebar = dwrefdata & REF_TITLEBAR != 0;
    let border = dwrefdata & REF_BORDER != 0;
    if titlebar || !border {
        // Normal（有标题栏）或 Frameless（无边框）交给 winit 原生处理，
        // 保留其 undecorated_shadow / 最大化 clamp 逻辑。
        return unsafe { DefSubclassProc(hwnd, umsg, wparam, lparam) };
    }

    if wparam == 0 {
        // wParam=0：只是通知客户区可能变化，不要求调整 insets。
        return unsafe { DefWindowProcW(hwnd, umsg, wparam, lparam) };
    }

    let params = lparam as *mut NCCALCSIZE_PARAMS;
    if params.is_null() {
        return unsafe { DefWindowProcW(hwnd, umsg, wparam, lparam) };
    }

    // 最大化时钳到工作区（镜像 winit event_loop.rs 处理）。
    if unsafe { IsZoomed(hwnd) } != 0 {
        if let Some(work) = monitor_work_rect(unsafe { (*params).rgrc[0] }) {
            unsafe { (*params).rgrc[0] = work };
        }
        return WVR_REDRAW as LRESULT;
    }

    let sx = unsafe {
        GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER)
    };
    let sy = unsafe {
        GetSystemMetrics(SM_CYSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER)
    };
    let r = unsafe { &mut (*params).rgrc[0] };
    // top 不加 inset（Electron titleBarStyle:'hidden' 语义）：
    // 客户区顶到窗口最上沿，DWM 无顶部非客户区可画 → 消除 Windows 10/11
    // 把顶部边框画成不透明白条的残留。代价是顶部边缘失去系统 resize 热区
    // （左/右/下三边仍保留，由系统默认 WM_NCHITTEST 接管）。
    r.left += sx;
    r.right -= sx;
    r.bottom -= sy;
    WVR_REDRAW as LRESULT
}

/// 返回包含 `rect` 的显示器的 `rcWork`（不存在则 None）。
fn monitor_work_rect(rect: RECT) -> Option<RECT> {
    unsafe {
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONULL);
        if monitor == 0 {
            return None;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        Some(info.rcWork)
    }
}

/// Windows 专属窗口扩展（镜像 winit `WindowExtWindows` 的可复用子集）。
///
/// 需要显式导入后调用：
/// ```no_run
/// use vireo::platform::windows::WindowExtWindows;
/// win.set_undecorated_shadow(true);
/// ```
///
/// 非 Windows 平台本模块不存在（`#[cfg(target_os = "windows")]` 门控）。
pub trait WindowExtWindows {
    /// 无装饰窗口的背景阴影（winit `undecorated_shadow`）。
    /// 对 `FrameStyle::Frameless` 生效；开启后 winit 在 `WM_NCCALCSIZE` 里把
    /// 客户区顶部下移 1px 留阴影位（窗口顶部出现 1px 细线为已知副作用）。
    /// `HiddenTitlebar`（vireo 子类接管 `WM_NCCALCSIZE`）下不生效。
    fn set_undecorated_shadow(&self, shadow: bool);
}

impl WindowExtWindows for crate::window::VireoWindow {
    fn set_undecorated_shadow(&self, shadow: bool) {
        winit::platform::windows::WindowExtWindows::set_undecorated_shadow(
            &*self.inner,
            shadow,
        );
    }
}
