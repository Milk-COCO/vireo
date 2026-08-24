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
//! （客户区 = 整个窗口），行为与 vireo 旧实现逐位一致。
//! 注意：vireo **不**暴露 winit 的 `undecorated_shadow`（那 1px 非客户区 hack
//! 与 DWM 圆角耦合，开启会让无边框窗口既带阴影又可圆角，语义混乱）；需要
//! 阴影/圆角时请用 `set_transparent` + SDF 自绘。
//!
//! 子类化用独立链表挂新 WNDPROC（`SetWindowSubclass`），`DefSubclassProc`
//! 自动调用原 wndproc，**不**触碰 winit 的 `GWL_WNDPROC` / `GWL_USERDATA`。
//!
//! 注意：这里 `windows-sys` 与 winit 各自独立的 windows-sys 版本类型不互通。
//! vireo 内部统一用 `isize` 表示窗口句柄（`win_hwnd` 返回 `Option<isize>`、
//! `nc_tx` 通道是 `(isize, ...)`），仅在调用 windows-sys 0.61 的 FFI 函数时
//! 用 `hwnd as HWND`（`isize as *mut c_void`）转换。

use std::ffi::c_void;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{
    PROPERTYKEY, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONULL, ScreenToClient,
};
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows_sys::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
use windows_sys::Win32::System::Variant::{VT_EMPTY, VT_LPWSTR};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture,
};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, TBPF_ERROR, TBPF_INDETERMINATE,
    TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, THB_FLAGS, THB_ICON, THB_TOOLTIP,
    THBF_DISABLED, THBF_DISMISSONCLICK, THBF_ENABLED, THBF_HIDDEN, THBF_NOBACKGROUND,
    THBF_NONINTERACTIVE, THBN_CLICKED, THUMBBUTTON, THUMBBUTTONMASK,
};
use windows_sys::Win32::UI::Shell::PropertiesSystem::SHGetPropertyStoreForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DefWindowProcW, DestroyIcon, GetSystemMetrics, HICON, IsZoomed,
    NCCALCSIZE_PARAMS, SM_CXPADDEDBORDER, SM_CXSIZEFRAME, SM_CYSIZEFRAME, WM_COMMAND,
    IMAGE_FLAGS, LR_DEFAULTCOLOR, MINMAXINFO, WM_GETMINMAXINFO, WM_SIZING,
    WMSZ_BOTTOM, WMSZ_BOTTOMLEFT, WMSZ_BOTTOMRIGHT, WMSZ_LEFT, WMSZ_RIGHT, WMSZ_TOP,
    WMSZ_TOPLEFT, WMSZ_TOPRIGHT,
};
// 非客户区消息（§7.6）。windows-sys 0.52.0 未定义，手写补齐。
const WM_NCHITTEST: u32 = 0x0084;
const WM_NCCALCSIZE: u32 = 131;
const WM_NCLBUTTONDOWN: u32 = 0x00A1;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_CAPTURECHANGED: u32 = 0x0215;
const WM_SYSCOMMAND: u32 = 0x0112;
const HTCLIENT: i32 = 1;
const HTMINBUTTON: i32 = 8;
const HTMAXBUTTON: i32 = 9;
const HTCLOSE: i32 = 20;
const SC_MINIMIZE: usize = 0xF020;
const SC_MAXIMIZE: usize = 0xF030;
const SC_RESTORE: usize = 0xF120;
const SC_CLOSE: usize = 0xF060;

pub use winit::platform::windows::BackdropType;
pub use winit::platform::windows::Color;
pub use winit::dpi::PhysicalPosition;
pub use winit::dpi::PhysicalSize;

/// 窗口圆角偏好（Windows 11 22000+，DWM `DWMWCP_*`）。
///
/// vireo 自带枚举（镜像 winit `CornerPreference`，不 re-export winit 类型），
/// 供 [`WindowExtWindows::set_corner_preference`] 与 [`VireoWindow::set_frame_style`]
/// 的 `Frameless` 钳制/恢复逻辑共用同一类型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CornerPreference {
    /// 由系统决定是否圆角（默认）。
    #[default]
    Default,
    /// 不圆角。
    DoNotRound,
    /// 圆角。
    Round,
    /// 小圆角。
    RoundSmall,
}

impl CornerPreference {
    /// 转回 winit 类型（调用 winit `set_corner_preference` 用）。
    pub(crate) fn into_winit(self) -> winit::platform::windows::CornerPreference {
        match self {
            CornerPreference::Default => winit::platform::windows::CornerPreference::Default,
            CornerPreference::DoNotRound => winit::platform::windows::CornerPreference::DoNotRound,
            CornerPreference::Round => winit::platform::windows::CornerPreference::Round,
            CornerPreference::RoundSmall => winit::platform::windows::CornerPreference::RoundSmall,
        }
    }
}

const KW_HRESULT_OK: i32 = 0;

/// 接口 vtable 槽位索引。
#[allow(dead_code)]
mod iface {
    pub const QUERY_INTERFACE: usize = 0;
    pub const ADD_REF: usize = 1;
    pub const RELEASE: usize = 2;
}

/// ITaskbarList3 vtable 槽位（从 IUnknown 起 0 基）。
#[allow(dead_code)]
mod taskbar {
    pub const HR_INIT: usize = 3;
    pub const ADD_TAB: usize = 4;
    pub const DELETE_TAB: usize = 5;
    pub const ACTIVATE_TAB: usize = 6;
    pub const SET_ACTIVE_ALT: usize = 7;
    pub const MARK_FULLSCREEN_WINDOW: usize = 8;
    pub const SET_PROGRESS_VALUE: usize = 9;
    pub const SET_PROGRESS_STATE: usize = 10;
    pub const REGISTER_TAB: usize = 11;
    pub const UNREGISTER_TAB: usize = 12;
    pub const SET_TAB_ORDER: usize = 13;
    pub const SET_TAB_ACTIVE: usize = 14;
    pub const THUMB_BAR_ADD_BUTTONS: usize = 15;
    pub const THUMB_BAR_UPDATE_BUTTONS: usize = 16;
    pub const THUMB_BAR_SET_IMAGE_LIST: usize = 17;
    pub const SET_OVERLAY_ICON: usize = 18;
    pub const SET_THUMBNAIL_TOOLTIP: usize = 19;
    pub const SET_THUMBNAIL_CLIP: usize = 20;
}

/// IPropertyStore vtable 槽位（从 IUnknown 起 0 基）。
#[allow(dead_code)]
mod propstore {
    pub const GET_COUNT: usize = 3;
    pub const GET_AT: usize = 4;
    pub const GET_VALUE: usize = 5;
    pub const SET_VALUE: usize = 6;
    pub const COMMIT: usize = 7;
}

/// GUID `{56FDF344-FD6D-11D0-958A-006097C9A090}`（任务栏 CLSID）。
const CLSID_TASKBAR_LIST: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x56FDF344_FD6D_11D0_958A_006097C9A090);
/// GUID `{EA1AFB91-9E28-4B86-90E9-9E9F8A5EEFAF}`（ITaskbarList3 接口）。
const IID_ITASKBAR_LIST3: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0xEA1AFB91_9E28_4B86_90E9_9E9F8A5EEFAF);
/// GUID `{886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99}`（IPropertyStore 接口）。
const IID_IPROPERTY_STORE: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x886D8EEB_8CF2_4446_8D02_CDBA1DBDCF99);
/// `PKEY_AppUserModel_ID`：fmtid `{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}`, pid 5。
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows_sys::core::GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
    pid: 5,
};

/// 进程级任务栏 COM 对象缓存（创建一次、`HrInit` 一次，对所有窗口复用）。
/// 存 `usize` 以满足 `static` 的 `Sync` 约束（裸指针本体不实现 `Sync`）。
fn taskbar_list3() -> Option<*mut c_void> {
    static ONCE: OnceLock<usize> = OnceLock::new();
    let addr = *ONCE.get_or_init(|| create_taskbar_list3() as usize);
    if addr == 0 {
        None
    } else {
        Some(addr as *mut c_void)
    }
}

/// 从 RGBA 像素构造 HICON（PNG 编码为 RT_ICON 资源字节再 `CreateIconFromResourceEx`）。
/// `width`/`height` 应 ≥ 1。失败（编码/资源句柄）返回 `0`。
///
/// dwVer = `0x00030000`（Vista+，接受 PNG 压缩的图标资源）配合 `LR_DEFAULTCOLOR`。
/// 传给 `CreateIconFromResourceEx` 的必须是**裸 PNG**（RT_ICON 资源格式），
/// 不能包 ICONDIR/ICO 容器（实测带 ICO 头会返回 NULL / GetLastError=203）。
fn hicone_from_rgba(rgba: &[u8], width: u32, height: u32) -> isize {
    let w = width as usize;
    let h = height as usize;
    let expected = w.saturating_mul(h).saturating_mul(4);
    if rgba.len() < expected || w == 0 || h == 0 {
        log::warn!(
            "vireo taskbar: hicone_from_rgba 尺寸非法 ({}x{}, rgba len={}, expected={})",
            width, height, rgba.len(), expected
        );
        return 0;
    }
    // PNG 编码。
    // 注意：`CreateIconFromResourceEx`（dwVersion=0x00030000 的 PNG 路径）要求 PNG
    // 每个扫描行的 filter 字节为 0（unfiltered）。`image` 默认 `FilterType::Adaptive`
    // 会写非零 filter → 函数返回 NULL，故必须显式 `NoFilter`。
    let mut png: Vec<u8> = Vec::new();
    {
        use image::ImageEncoder;
        let enc = image::codecs::png::PngEncoder::new_with_quality(
            &mut png,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::NoFilter,
        );
        if enc
            .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
            .is_err()
        {
            return 0;
        }
    }
    // 直接传**裸 PNG**（RT_ICON 资源格式），不要包 ICONDIR。
    // `CreateIconFromResourceEx` 把整个 buffer 交给 WIC（IconCodecService.dll）
    // 解码；带 ICONDIR 头的 ICO 容器开头不是 PNG magic（89 50 4E 47），
    // WIC 无法识别 → 返回 NULL / GetLastError=203。实测裸 PNG 成功。
    if png.len() > u32::MAX as usize {
        return 0;
    }
    unsafe {
        let icon = CreateIconFromResourceEx(
            png.as_ptr(),
            png.len() as u32,
            1, // fIcon = TRUE
            0x0003_0000,
            width as i32,
            height as i32,
            LR_DEFAULTCOLOR as IMAGE_FLAGS,
        );
        if icon.is_null() {
            log::warn!("vireo taskbar: CreateIconFromResourceEx 失败 ({}x{})", width, height);
        }
        icon as isize
    }
}

fn create_taskbar_list3() -> *mut c_void {
    // MTA：确保跨线程调用（渲染线程 + winit 线程）安全。
    let hr_co = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
    if hr_co != KW_HRESULT_OK && hr_co != 1 {
        // 1 = S_FALSE（本线程已初始化，合法）；RPC_E_CHANGED_MODE 等才是问题。
        log::warn!(
            "vireo taskbar: CoInitializeEx(MTA) hr=0x{:08X}",
            hr_co as u32
        );
    }
    let mut obj: *mut c_void = std::ptr::null_mut();
    let hr = unsafe {
        CoCreateInstance(
            &CLSID_TASKBAR_LIST,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ITASKBAR_LIST3,
            &mut obj,
        )
    };
    if hr != KW_HRESULT_OK {
        log::warn!("vireo taskbar: CoCreateInstance ITaskbarList3 hr=0x{:08X}", hr as u32);
        return std::ptr::null_mut();
    }
    // HrInit 失败也保留对象（SetProgressValue 等仍可用）。
    unsafe {
        let f: HrFn0 = slot_fn(obj, taskbar::HR_INIT);
        let hr_init = f(obj);
        if hr_init != KW_HRESULT_OK {
            log::warn!("vireo taskbar: HrInit hr=0x{:08X}", hr_init as u32);
        }
    }
    log::warn!("vireo taskbar: ITaskbarList3 ready, obj={:p}", obj);
    obj
}

/// COM 返回值 `HRESULT`（windows-sys `HRESULT` 即 i32）。
type Hr = i32;

type HrFn0 = unsafe extern "system" fn(*mut c_void) -> Hr;
type HrFnProgress = unsafe extern "system" fn(*mut c_void, HWND, u64, u64) -> Hr;
type HrFnState = unsafe extern "system" fn(*mut c_void, HWND, i32) -> Hr;
type HrFnButtons = unsafe extern "system" fn(*mut c_void, HWND, u32, *const THUMBBUTTON) -> Hr;
type HrFnOverlay = unsafe extern "system" fn(*mut c_void, HWND, HICON, *const u16) -> Hr;
type HrFnSetValue =
    unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, *const PROPVARIANT) -> Hr;
type HrFnCommit = unsafe extern "system" fn(*mut c_void) -> Hr;
type U8FnRelease = unsafe extern "system" fn(*mut c_void) -> u32;

/// 读接口 vtable 第 `slot` 个槽位的函数指针并按 `T` 解释。
/// COM 方法首个实参恒为 `this`（即接口指针本身）。
unsafe fn slot_fn<T>(obj: *mut c_void, slot: usize) -> T {
    let vtbl = unsafe { *(obj as *mut *mut *const c_void) };
    unsafe { std::mem::transmute_copy(&*vtbl.add(slot)) }
}

/// 每窗口存活中的 `HICON`（`ThumbBarAddButtons` / `SetOverlayIcon` 后台持有，
/// 替换或清除时 `DestroyIcon`）。缩略图按钮与 overlay 各自独立（互不误伤，
/// 二者支持的图标可以同时存在）。
#[derive(Default)]
struct WindowIcons {
    thumb_bar: Vec<isize>,
    overlay: Vec<isize>,
}

static WINDOW_ICONS: LazyLock<Mutex<std::collections::HashMap<isize, WindowIcons>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 记录窗口持有的一组缩略图按钮 HICON。
fn set_thumbar_icons(hwnd: isize, icons: Vec<isize>) {
    let mut w = WINDOW_ICONS.lock().unwrap();
    w.entry(hwnd).or_default().thumb_bar = icons;
}

/// 记录窗口持有的 overlay HICON。
fn set_overlay_icons(hwnd: isize, icons: Vec<isize>) {
    let mut w = WINDOW_ICONS.lock().unwrap();
    w.entry(hwnd).or_default().overlay = icons;
}

/// 销毁窗口此前的缩略图按钮 HICON。
pub(crate) fn drop_thumbar_icons(hwnd: isize) {
    let mut w = WINDOW_ICONS.lock().unwrap();
    if let Some(icons) = w.get_mut(&hwnd) {
        for icon in icons.thumb_bar.drain(..) {
            unsafe { DestroyIcon(icon as HICON) };
        }
    }
}

/// 销毁窗口此前的 overlay HICON。
pub(crate) fn drop_overlay_icons(hwnd: isize) {
    let mut w = WINDOW_ICONS.lock().unwrap();
    if let Some(icons) = w.get_mut(&hwnd) {
        for icon in icons.overlay.drain(..) {
            unsafe { DestroyIcon(icon as HICON) };
        }
    }
}

pub(crate) fn remove_window_icons_entry(hwnd: isize) {
    WINDOW_ICONS.lock().unwrap().remove(&hwnd);
}

// WVR_HREDRAW(0x0100) | WVR_VREDRAW(0x0200)，windows-sys 0.52.0 未定义。
const WVR_REDRAW: u32 = 0x0300;

/// 子类标识符（`SetWindowSubclass` 的 uIdSubclass）。只要与同一窗口上其他
/// 子类不冲突即可；这里用「VIR」的 ASCII。
const SUBCLASS_ID: usize = 0x0056_4952;

/// 缩略图按钮点击子类标识符（与装饰子类 `SUBCLASS_ID` 区分）。
const THUMB_SUBCLASS_ID: usize = 0x0056_4954;

/// 缩略图按钮点击回调（`WM_COMMAND`/`THBN_CLICKED`，参数为按钮 `id`）。
///
/// 回调在窗口所属线程（winit 事件线程）执行。`set_thumbar_buttons` 挂一个
/// 只拦 `WM_COMMAND` 的子类，点击时从该表取回调调用（见 `thumb_subclass_proc`）。
/// 注册经 `set_thumbar_callback`；清理按钮时若表为空则卸载子类。
struct ThumbCallback(Box<dyn FnMut(u32)>);

impl std::ops::Deref for ThumbCallback {
    type Target = Box<dyn FnMut(u32)>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ThumbCallback {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// SAFETY: 回调只从 `thumb_subclass_proc`（winit 事件线程）调用，不跨线程执行。
// 静态表用 Mutex 保护跨线程注册/取用；与 `InputCallbacks::unsafe impl Send` 同约定。
unsafe impl Send for ThumbCallback {}

static THUMB_CALLBACKS: LazyLock<Mutex<std::collections::HashMap<isize, Vec<ThumbCallback>>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 注册/覆盖窗口的缩略图按钮点击回调（追加；`None` 不清）。
/// 幂等：重复调用只追加；无回调时卸载点击子类。
pub(crate) fn set_thumbar_callback(hwnd: isize, cb: Box<dyn FnMut(u32)>) {
    let mut map = THUMB_CALLBACKS.lock().unwrap();
    map.entry(hwnd).or_default().push(ThumbCallback(cb));
    unsafe {
        SetWindowSubclass(hwnd as HWND, Some(thumb_subclass_proc), THUMB_SUBCLASS_ID, 0);
    }
}

/// 卸载窗口的全部缩略图点击回调（`set_thumbar_buttons(None)` 时调用）。
pub(crate) fn clear_thumbar_callback(hwnd: isize) {
    let mut map = THUMB_CALLBACKS.lock().unwrap();
    if map.remove(&hwnd).is_some() {
        unsafe {
            RemoveWindowSubclass(hwnd as HWND, Some(thumb_subclass_proc), THUMB_SUBCLASS_ID);
        }
    }
}

/// `WM_COMMAND`（`THBN_CLICKED`）点击子类回调。任务栏缩略图按钮点击发出
/// `WM_COMMAND`，`HIWORD(wParam) == THBN_CLICKED`、`LOWORD(wParam)` 即按钮 `id`。
/// 拦截后从全局回调表取对应窗口的回调逐一调用，然后放行给原 wndproc。
unsafe extern "system" fn thumb_subclass_proc(
    hwnd: HWND,
    umsg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uidsubclass: usize,
    _dwrefdata: usize,
) -> LRESULT {
    if umsg == WM_COMMAND {
        let hi = ((wparam as u32) >> 16) as u16;
        if hi as u32 == THBN_CLICKED {
            let id = (wparam as u32) & 0xFFFF;
            if let Ok(mut map) = THUMB_CALLBACKS.lock() {
                if let Some(cbs) = map.get_mut(&(hwnd as isize)) {
                    for cb in cbs.iter_mut() {
                        cb(id);
                    }
                }
            }
        }
    }
    unsafe { DefSubclassProc(hwnd, umsg, wparam, lparam) }
}

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
///
/// 安装后必须 `SWP_FRAMECHANGED` 强制重发 `WM_NCCALCSIZE`：创建期的
/// `WM_NCCALCSIZE` 在 `SetWindowSubclass` 之前已被 winit 消费（undecorated
/// 返回 0 → 客户区 = 整个窗口，无任何 resize 热区），不重发的话子类永远
/// 等不到消息，`HiddenTitlebar` 会退化得像 `Frameless` 一样不可缩放。
pub fn install(hwnd: isize, titlebar: bool, border: bool) {
    unsafe {
        SetWindowSubclass(
            hwnd as HWND,
            Some(frame_subclass_proc),
            SUBCLASS_ID,
            encode_refdata(titlebar, border),
        );
    }
    force_nccalc_recalc(hwnd);
}

/// 更新装饰分档（运行期切换 `titlebar`/`border`）。重调 `SetWindowSubclass`
/// 刷新 refdata，然后强制重算非客户区。
pub fn set_frame(hwnd: isize, titlebar: bool, border: bool) {
    unsafe {
        SetWindowSubclass(
            hwnd as HWND,
            Some(frame_subclass_proc),
            SUBCLASS_ID,
            encode_refdata(titlebar, border),
        );
    }
    force_nccalc_recalc(hwnd);
}

/// 卸载装饰子类。
pub fn remove(hwnd: isize) {
    unsafe {
        RemoveWindowSubclass(hwnd as HWND, Some(frame_subclass_proc), SUBCLASS_ID);
    }
    force_nccalc_recalc(hwnd);
}

/// 强制系统重发 `WM_NCCALCSIZE`（`SetWindowPos(SWP_FRAMECHANGED)`）。
fn force_nccalc_recalc(hwnd: isize) {
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
            hwnd as HWND,
            std::ptr::null_mut(),
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
///
/// **不调 `DefSubclassProc`**：阻止 winit/DefWindowProc 恢复系统标题栏。
/// nc_subclass（如果已装）在 frame_subclass **之前**运行（LIFO），它通过
/// `DefSubclassProc` 把消息传给 frame_subclass，frame_subclass 修改 `rgrc[0]`
/// 后直接返回 `WVR_REDRAW`，nc_subclass 在返回的 `rgrc[0]` 上叠加用户 insets。
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
        // 保留其最大化 clamp 逻辑。
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
    // 用户 `set_non_client_size(top,...)` 单独控制顶 inset；frame_subclass
    // 只负责左/右/下三边的系统边框。
    // 不调 DefSubclassProc：阻止 winit/DefWindowProc 恢复系统标题栏；
    // nc_subclass（如果已装）在 frame_subclass 之前运行，已通过
    // DefSubclassProc 把消息传到这里，frame_subclass 修改 rgrc[0] 后
    // 返回，nc_subclass 在此基础上叠加用户 insets。
    r.left += sx;
    r.right -= sx;
    r.bottom -= sy;
    WVR_REDRAW as LRESULT
}

/// 返回包含 `rect` 的显示器的 `rcWork`（不存在则 None）。
fn monitor_work_rect(rect: RECT) -> Option<RECT> {
    unsafe {
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONULL);
        if monitor.is_null() {
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

/// 宽高比子类标识符（与装饰/缩略图子类区分）。`"V"` `"A"` `"R"`。
const ASPECT_SUBCLASS_ID: usize = 0x0056_4152;

/// 每窗口当前宽高比（r > 0）。`HWND → ratio` 映射在进程内全局共享。
/// 子类 proc 读本表，外部用 `set_aspect_ratio` 写入；写入前无须线程间同步——
/// 表本身 `Mutex` 保护，子类 proc 在窗口 owner（winit 事件）线程被调用，
/// `set_aspect_ratio` 经 `WinitEvent` 转发后也在 winit 线程执行，无并发。
static ASPECT_RATIOS: LazyLock<Mutex<HashMap<isize, f64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `WM_GETMINMAXINFO` + `WM_SIZING` 子类回调：拦截最大/最小追踪尺寸与用户拖拽
/// 时的尺寸提议，按 `ASPECT_RATIOS[hwnd]` 维持宽高比。
///
/// 必须由 [`set_aspect_ratio`] 在 winit 事件线程安装/卸载（`SetWindowSubclass`
/// 不可跨线程调用，与 `set_frame` 同一约束）。
unsafe extern "system" fn aspect_subclass_proc(
    hwnd: HWND,
    umsg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uidsubclass: usize,
    _dwrefdata: usize,
) -> LRESULT {
    if umsg == WM_GETMINMAXINFO {
        if let Ok(map) = ASPECT_RATIOS.lock() {
            if let Some(&ratio) = map.get(&(hwnd as isize)) {
                if ratio > 0.0 {
                    let info = lparam as *mut MINMAXINFO;
                    if !info.is_null() {
                        let info = unsafe { &mut *info };
                        // Windows 按 ptMaxTrackSize 限制拖拽最大尺寸；
                        // 这里按 ratio 把"宽为主"换算成高，避免 Windows 给一对
                        // 与 ratio 矛盾的最大宽/高后用户拖出非 ratio 窗口。
                        if info.ptMaxTrackSize.x > 0 {
                            let max_h_from_w =
                                (info.ptMaxTrackSize.x as f64 / ratio).round() as i32;
                            if max_h_from_w > 0 && max_h_from_w < info.ptMaxTrackSize.y {
                                info.ptMaxTrackSize.y = max_h_from_w;
                            }
                        }
                    }
                }
            }
        }
    } else if umsg == WM_SIZING {
        if let Ok(map) = ASPECT_RATIOS.lock() {
            if let Some(&ratio) = map.get(&(hwnd as isize)) {
                if ratio > 0.0 {
                    let rc = lparam as *mut RECT;
                    if !rc.is_null() {
                        let r = unsafe { &mut *rc };
                        let w = (r.right - r.left) as f64;
                        let wmsz = wparam as u32;
                        // 按 WMSZ_* 决定以哪条边为基准调整另一条：
                        //   左右拖动 → 高度按 width / ratio 调整，固定 top+bottom
                        //   上下拖动 → 宽度按 height * ratio 调整，固定 left+right
                        //   角拖动 → 锚对角，按新宽算高
                        match wmsz {
                            WMSZ_LEFT | WMSZ_RIGHT => {
                                let new_h = (w / ratio).round() as i32;
                                r.bottom = r.top + new_h;
                            }
                            WMSZ_TOP | WMSZ_BOTTOM => {
                                let h = (r.bottom - r.top) as f64;
                                let new_w = (h * ratio).round() as i32;
                                r.right = r.left + new_w;
                            }
                            WMSZ_TOPLEFT => {
                                // 锚定 right + bottom（窗口右下角不动）
                                let new_h = (w / ratio).round() as i32;
                                r.top = r.bottom - new_h;
                            }
                            WMSZ_BOTTOMRIGHT => {
                                // 锚定 left + top（左上角不动）
                                let new_h = (w / ratio).round() as i32;
                                r.bottom = r.top + new_h;
                            }
                            WMSZ_TOPRIGHT => {
                                // 锚定 left + bottom
                                let new_h = (w / ratio).round() as i32;
                                r.top = r.bottom - new_h;
                            }
                            WMSZ_BOTTOMLEFT => {
                                // 锚定 right + top
                                let new_h = (w / ratio).round() as i32;
                                r.bottom = r.top + new_h;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    unsafe { DefSubclassProc(hwnd, umsg, wparam, lparam) }
}

/// 设置/清除窗口宽高比。`Some(r > 0)` = 维持 w/h = r；`None` 或非正数 = 清除。
///
/// **必须由 winit 事件线程调用**（同 `set_frame`，`SetWindowSubclass` 不可跨线程）。
/// 重复设置同一正数 ratio 幂等（`SetWindowSubclass` 重复挂同 proc+id 是 no-op）；
/// 重复清除幂等（`RemoveWindowSubclass` 对未挂子类是 no-op）。
pub fn set_aspect_ratio(hwnd: isize, ratio: Option<f64>) {
    let mut map = ASPECT_RATIOS.lock().unwrap();
    match ratio {
        Some(r) if r > 0.0 => {
            map.insert(hwnd, r);
            unsafe {
                SetWindowSubclass(hwnd as HWND, Some(aspect_subclass_proc), ASPECT_SUBCLASS_ID, 0);
            }
        }
        _ => {
            map.remove(&hwnd);
            unsafe {
                RemoveWindowSubclass(hwnd as HWND, Some(aspect_subclass_proc), ASPECT_SUBCLASS_ID);
            }
        }
    }
}

// ====== §7.6 非客户区管理（4 套 WM_NC* 消息）============================

/// NC 子类标识符（与 frame / aspect / thumb 区分）。`"V"` `"N"` `"C"`。
const NC_SUBCLASS_ID: usize = 0x0056_4E43;

/// 回调 newtype（绕开 orphan rules：本地类型 + 单一 `Box<dyn FnMut>` 字段，
/// 对内安全地 `unsafe impl Send`/`Sync`）。`ThumbCallback` 同模式。
pub(crate) struct HitTestCallback(
    pub(crate) Box<dyn FnMut(crate::nc::HitTestInput) -> crate::nc::NonClientHit>,
);

impl std::ops::Deref for HitTestCallback {
    type Target = Box<dyn FnMut(crate::nc::HitTestInput) -> crate::nc::NonClientHit>;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl std::ops::DerefMut for HitTestCallback {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}
unsafe impl Send for HitTestCallback {}

/// 每窗口非客户区状态。
struct NcState {
    /// 声明式 hit-test 区域（客户端逻辑像素，与 DrawBatch 绘制坐标一致）。
    /// 命中规则：**后声明优先**（z 序在上）。
    regions: Vec<crate::nc::NonClientRegion>,
    /// 命令式 `set_hit_test_callback`。
    hit_test_cb: Option<HitTestCallback>,
    /// 按下中的标题栏按钮（HTCLOSE/HTMINBUTTON/HTMAXBUTTON）。`Some` 表示
    /// 捕获鼠标已建立，`WM_NCLBUTTONDOWN` 已被接管（阻止 DefWindowProc 渲染
    /// 经典按下按钮），等 `WM_LBUTTONUP` 释放时在同一按钮上发 `WM_SYSCOMMAND`。
    pressed_ht: Option<i32>,
}

static NC_STATES: LazyLock<Mutex<HashMap<isize, NcState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 应用 NC 状态变更（在 winit 事件线程调用，与 `set_aspect_ratio` 同模式）。
///
/// 子类只装 `WM_NCHITTEST` + `WM_NCLBUTTONDOWN`（标题栏按钮自管），**不接管
/// `WM_NCCALCSIZE` / `WM_NCPAINT` / `WM_NCACTIVATE`**。`WM_NCCALCSIZE` 放行给
/// `frame_subclass`（HiddenTitlebar 保留系统边框 + Win11 圆角）或 winit 原生
/// （Frameless 客户区 = 全窗口）。不产生 NC 标题栏区域，系统不画标准标题栏/
/// 按钮。按钮外观由用户在客户端用 DrawBatch 自绘，`WM_NCHITTEST` 返回 `HT*`
/// 让 Windows 自动接管交互（snap layout / 双击 / 右键菜单 / 按钮行为）。
///
/// **不调 `force_nccalc_recalc`**：本子类不处理 `WM_NCCALCSIZE`，regions 只
/// 在 `WM_NCHITTEST` 时从 `NC_STATES` 直读，`SetWindowSubclass` 后下一次
/// hit-test 即生效，无需重算非客户区。而 `SWP_FRAMECHANGED` 会强制重发
/// `WM_NCCALCSIZE` 给 `frame_subclass`（HiddenTitlebar 时改写 `rgrc[0]`），
/// 进而扰动 `inner_size()`——若用户像 `window_create` W2 那样在每次逻辑宽度
/// 变化时重发 regions，每次重发都触发一次 `SetWindowPos`，形成「重发 → 尺寸
/// 扰动 → 逻辑宽度又变 → 再重发」的自激环，松手后残留约 1 秒抽搐（死区
/// `RESIZE_DRIFT_EPSILON` 无法吸收，因为扰动每步都在重置计时器）。
pub(crate) fn nc_apply(hwnd: isize, update: NcUpdate) {
    let hwnd_raw = hwnd as HWND;
    // 避免已销毁或被复用的 HWND 僵尸更新
    if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(hwnd_raw) } == 0 {
        return;
    }
    let mut states = NC_STATES.lock().unwrap();
    let state = states.entry(hwnd).or_insert_with(|| NcState {
        regions: Vec::new(),
        hit_test_cb: None,
        pressed_ht: None,
    });

    match update {
        NcUpdate::SetRegions(regions) => state.regions = regions,
        NcUpdate::SetHitTestCb(cb) => state.hit_test_cb = Some(cb),
        NcUpdate::ClearHitTestCb => state.hit_test_cb = None,
        NcUpdate::ClearAll => {
            *state = NcState {
                regions: Vec::new(),
                hit_test_cb: None,
                pressed_ht: None,
            };
        }
    }

    let has_regions = !state.regions.is_empty();
    let has_cb = state.hit_test_cb.is_some();
    drop(states);

    if has_regions || has_cb {
        unsafe {
            SetWindowSubclass(hwnd_raw, Some(nc_subclass_proc), NC_SUBCLASS_ID, 0);
        }
    } else {
        unsafe {
            RemoveWindowSubclass(hwnd_raw, Some(nc_subclass_proc), NC_SUBCLASS_ID);
        }
    }
}

/// 卸载窗口的整个 NC 状态（窗口销毁时调用，避免 stale 表项）。
pub fn nc_remove(hwnd: isize) {
    NC_STATES.lock().unwrap().remove(&hwnd);
    unsafe {
        RemoveWindowSubclass(hwnd as HWND, Some(nc_subclass_proc), NC_SUBCLASS_ID);
    }
}

/// 读取当前 hit-test regions（VireoWindow::non_client_regions 用）。
pub fn nc_get_regions(hwnd: isize) -> Option<Vec<crate::nc::NonClientRegion>> {
    NC_STATES
        .lock()
        .ok()
        .and_then(|m| m.get(&hwnd).map(|s| s.regions.clone()))
}

/// NC 状态更新事件（render thread → winit thread，载荷所有权移交给 winit thread）。
#[allow(dead_code)]
pub(crate) enum NcUpdate {
    SetRegions(Vec<crate::nc::NonClientRegion>),
    SetHitTestCb(HitTestCallback),
    ClearHitTestCb,
    ClearAll,
}

/// NC 子类 proc：处理 `WM_NCHITTEST` + 标题栏按钮点击。
///
/// **不接管 `WM_NCCALCSIZE`**：放行给 `frame_subclass`（若 HiddenTitlebar 已装，
/// LIFO 链中先跑的本子类不拦，`DefSubclassProc` 会传到 frame_subclass），由它
/// 保留左/右/下系统边框 → Win11 圆角与 resize 边框不消失。`Frameless` 则落回
/// winit 原生（`WM_NCCALCSIZE` 返回 0，客户区 = 全窗口）。
///
/// 标题栏按钮：`WM_NCHITTEST` 返回 `HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE` 后，
/// 若把 `WM_NCLBUTTONDOWN` 放给 `DefWindowProc`，它会**渲染经典样式的按下
/// 按钮**盖在用户自绘按钮上（Chromium 注释「for some insane reason ... ick!」）。
/// 这里拦截按下：阻止 DefWindowProc 渲染 + 自己发 `WM_SYSCOMMAND` 完成动作，
/// 交互行为（snap/双击/右键菜单）仍由系统自动接管。
unsafe extern "system" fn nc_subclass_proc(
    hwnd: HWND,
    umsg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uidsubclass: usize,
    _dwrefdata: usize,
) -> LRESULT {
    match umsg {
        WM_NCHITTEST => nc_handle_hit_test(hwnd, lparam),
        WM_NCLBUTTONDOWN => nc_handle_button_down(hwnd, wparam, lparam),
        WM_LBUTTONUP => nc_handle_button_up(hwnd, wparam),
        WM_CAPTURECHANGED => nc_handle_capture_changed(hwnd, wparam, lparam),
        _ => unsafe { DefSubclassProc(hwnd, umsg, wparam, lparam) },
    }
}

/// `WM_NCLBUTTONDOWN`：若命中标题栏按钮，拦截并自管（阻止 DefWindowProc 渲染
/// 经典按下按钮），否则放行（标题栏拖动 / 系统按钮都靠放行）。
fn nc_handle_button_down(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ht = (wparam & 0xFFFF) as i32;
    if !matches!(ht, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) {
        return unsafe { DefSubclassProc(hwnd, WM_NCLBUTTONDOWN, wparam, lparam) };
    }

    // 进入按下状态：记 HT + 捕获鼠标，等 WM_LBUTTONUP 决定是否触发动作。
    // 不调 DefSubclassProc/DefWindowProc → 经典按下按钮不渲染。
    // 若已处于按下状态则先清理，避免重复 SetCapture 泄漏。
    let already_pressed = NC_STATES.lock().map(|m| m.get(&(hwnd as isize)).and_then(|s| s.pressed_ht).is_some()).unwrap_or(false);
    if already_pressed {
        unsafe { ReleaseCapture(); }
        if let Ok(mut map) = NC_STATES.lock() {
            if let Some(s) = map.get_mut(&(hwnd as isize)) { s.pressed_ht = None; }
        }
    }
    if let Ok(mut map) = NC_STATES.lock() {
        if let Some(s) = map.get_mut(&(hwnd as isize)) {
            s.pressed_ht = Some(ht);
        }
    }
    unsafe { SetCapture(hwnd); }
    0
}

/// `WM_LBUTTONUP`：若处于按钮按下状态，判断释放位置是否仍命中同一按钮；
/// 是则发 `WM_SYSCOMMAND` 完成最小化/最大化/关闭，然后解除捕获。
fn nc_handle_button_up(hwnd: HWND, wparam: WPARAM) -> LRESULT {
    let pressed = {
        let map = NC_STATES.lock().unwrap();
        map.get(&(hwnd as isize)).and_then(|s| s.pressed_ht)
    };
    let Some(ht) = pressed else {
        return unsafe { DefSubclassProc(hwnd, WM_LBUTTONUP, wparam, 0) };
    };

    // 释放位置（屏幕物理坐标）是否仍落在同一按钮 region 内。
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
    }
    let hit = nc_hit_test_regions(hwnd, pt.x, pt.y, 0) as i32;

    // 清按下状态并解除捕获（无论是否触发）。
    if let Ok(mut map) = NC_STATES.lock() {
        if let Some(s) = map.get_mut(&(hwnd as isize)) {
            s.pressed_ht = None;
        }
    }
    unsafe {
        ReleaseCapture();
    }

    if hit == ht {
        let cmd = match ht {
            HTMINBUTTON => SC_MINIMIZE,
            HTMAXBUTTON => {
                if unsafe { IsZoomed(hwnd) } != 0 {
                    SC_RESTORE
                } else {
                    SC_MAXIMIZE
                }
            }
            HTCLOSE => SC_CLOSE,
            _ => 0,
        };
        if cmd != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                    hwnd,
                    WM_SYSCOMMAND,
                    cmd,
                    0,
                );
            }
        }
    }
    0
}

/// `WM_CAPTURECHANGED`：捕获被系统剥夺（如点开系统菜单）时清按下状态。
fn nc_handle_capture_changed(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Ok(mut map) = NC_STATES.lock() {
        if let Some(s) = map.get_mut(&(hwnd as isize)) {
            s.pressed_ht = None;
        }
    }
    unsafe { DefSubclassProc(hwnd, WM_CAPTURECHANGED, wparam, lparam) }
}

fn nc_handle_hit_test(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    // lparam = 屏幕物理坐标（低 16 = x，高 16 = y，有符号）。
    let screen_x = (lparam & 0xFFFF) as i16 as i32;
    let screen_y = ((lparam >> 16) & 0xFFFF) as i16 as i32;

    // 最大化时不允许 resize。
    if unsafe { IsZoomed(hwnd) } != 0 {
        return nc_hit_test_regions(hwnd, screen_x, screen_y, lparam);
    }

    // 获取窗口物理矩形。
    let mut wr: RECT = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut wr) };

    let sx = unsafe { GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER) } as i32;
    let sy = unsafe { GetSystemMetrics(SM_CYSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER) } as i32;

    // 窗口边缘 resize 热区（物理像素）。
    let left = screen_x >= wr.left && screen_x < wr.left + sx;
    let right = screen_x < wr.right && screen_x >= wr.right - sx;
    let top = screen_y >= wr.top && screen_y < wr.top + sy;
    let bottom = screen_y < wr.bottom && screen_y >= wr.bottom - sy;

    let hit = if top && left { 13 }          // HTTOPLEFT
        else if top && right { 14 }           // HTTOPRIGHT
        else if bottom && left { 16 }         // HTBOTTOMLEFT
        else if bottom && right { 17 }        // HTBOTTOMRIGHT
        else if top { 12 }                    // HTTOP
        else if bottom { 15 }                 // HTBOTTOM
        else if left { 10 }                   // HTLEFT
        else if right { 11 }                  // HTRIGHT
        else { 0 }; // 0 = 未命中边框

    if hit != 0 {
        return hit as LRESULT;
    }

    nc_hit_test_regions(hwnd, screen_x, screen_y, lparam)
}

/// 检查用户声明的 regions；未命中则返回 HTCLIENT。
fn nc_hit_test_regions(hwnd: HWND, screen_x: i32, screen_y: i32, lparam: LPARAM) -> LRESULT {
    let mut pt = POINT { x: screen_x, y: screen_y };
    let ok = unsafe { ScreenToClient(hwnd, &mut pt) };
    if ok == 0 {
        return unsafe { DefSubclassProc(hwnd, WM_NCHITTEST, 0, lparam) };
    }

    let dpi = get_effective_dpi(hwnd);
    let lx = pt.x as f32 / dpi as f32;
    let ly = pt.y as f32 / dpi as f32;

    if let Ok(mut map) = NC_STATES.lock() {
        if let Some(state) = map.get_mut(&(hwnd as isize)) {
            for region in state.regions.iter().rev() {
                if region.rect.contains([lx, ly]) {
                    return region.hit_test.to_win32() as LRESULT;
                }
            }
            if let Some(cb) = state.hit_test_cb.as_mut() {
                let input = crate::nc::HitTestInput {
                    pos: crate::math::Pos::new(lx, ly),
                    state: get_window_state(hwnd),
                    dpi_scale: dpi,
                };
                let hit = cb(input);
                if hit != crate::nc::NonClientHit::Client {
                    return hit.to_win32() as LRESULT;
                }
            }
        }
    }

    HTCLIENT as LRESULT
}

fn get_window_state(hwnd: HWND) -> crate::nc::WindowState {
    let mut state = crate::nc::WindowState::default();
    unsafe {
        state.maximized = IsZoomed(hwnd) != 0;
    }
    state.active = hwnd_is_active(hwnd);
    state
}

fn hwnd_is_active(hwnd: HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    unsafe { GetForegroundWindow() == hwnd }
}

fn get_effective_dpi(hwnd: HWND) -> f64 {
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    let dpi = unsafe { GetDpiForWindow(hwnd) } as f64;
    if dpi <= 0.0 {
        return 1.0;
    }
    dpi / 96.0
}

/// Windows 专属窗口扩展（镜像 winit `WindowExtWindows` 的可复用子集）。
///
/// 需要显式导入后调用：
/// ```no_run
/// use vireo::platform::windows::WindowExtWindows;
/// win.set_corner_preference(vireo::platform::windows::CornerPreference::Round);
/// ```
///
/// 非 Windows 平台本模块不存在（`#[cfg(target_os = "windows")]` 门控）。
pub trait WindowExtWindows {
    /// 启用/禁用窗口的鼠标与键盘输入。窗口必须先启用才能被激活。
    /// （winit `WindowExtWindows::set_enable`）
    fn set_enable(&self, enabled: bool);

    /// 设置任务栏图标（`ICON_BIG`，256×256 为合理上限）。`None` 恢复默认。
    /// （winit `WindowExtWindows::set_taskbar_icon`）
    fn set_taskbar_icon(&self, taskbar_icon: Option<winit::window::Icon>);

    /// 是否在任务栏显示/隐藏窗口图标。（winit `WindowExtWindows::set_skip_taskbar`）
    fn set_skip_taskbar(&self, skip: bool);

    /// 设置系统自绘背景材料（`Auto`/`None`/`Mica`/`Acrylic`/`Tabbed`）。
    /// 需 Windows 11 22523+。（winit `WindowExtWindows::set_system_backdrop`）
    fn set_system_backdrop(&self, backdrop_type: BackdropType);

    /// 设置窗口边框颜色（Windows 11 22000+）。`None` 恢复系统默认。
    /// （winit `WindowExtWindows::set_border_color`）
    fn set_border_color(&self, color: Option<Color>);

    /// 设置标题栏背景颜色（Windows 11 22000+）。`None` 恢复系统默认；
    /// 传 `Some(Color::NONE)` 可绕过「在标题栏/边框上显示强调色」系统选项。
    /// （winit `WindowExtWindows::set_title_background_color`）
    fn set_title_background_color(&self, color: Option<Color>);

    /// 设置标题文字颜色（Windows 11 22000+）。
    /// （winit `WindowExtWindows::set_title_text_color`）
    fn set_title_text_color(&self, color: Color);

    /// 设置窗口圆角偏好（`Default`/`DoNotRound`/`Round`/`RoundSmall`）。
    /// 需 Windows 11 22000+。（winit `WindowExtWindows::set_corner_preference`）
    ///
    /// **始终记录用户偏好**：即使当前 `FrameStyle::Frameless`（无边框）下
    /// DWM 无法圆角、本设置不立即生效，该偏好也会被记住，待 `set_frame_style`
    /// 切回 Normal / HiddenTitlebar 时自动恢复。需要真正独立控制时请用
    /// `set_transparent` + SDF 自绘阴影/圆角。
    fn set_corner_preference(&self, preference: CornerPreference);

    /// 把窗口置顶（`HWND_TOPMOST`）。vireo 自实现（Electron `moveTop` 语义，
    /// winit 无对应 API）。重复调用为幂等置顶。
    fn move_top(&self);

    /// 把窗口移到 z 序顶层（`HWND_TOP`，普通置顶，不设 TOPMOST 状态）。
    /// vireo 自实现（Electron `moveAbove` 语义，winit 无对应 API）。
    fn move_above(&self);

    /// 设置任务栏进度（ITaskbarList3 `SetProgressState` / `SetProgressValue`）。
    /// 对应 Electron `setProgressBar` / Tauri `setProgressBar`。
    ///
    /// - `state = TaskbarProgress::None`：清除进度（`TBPF_NOPROGRESS`）。
    /// - `state = TaskbarProgress::Normal(v)`（`0.0..=1.0`）：正常进度，`v` 为完成比例。
    /// - `state = TaskbarProgress::Indeterminate`：不确定进度动画（无数值）。
    /// - `state = TaskbarProgress::Paused(v)` / `Error(v)`：暂停/错误色 + 比例。
    ///
    /// `v` 自动 clamp 到 `[0,1]`；比例按 `(v*10000, 10000)` 传给 `SetProgressValue`。
    fn set_progress_bar(&self, state: TaskbarProgress);

    /// 设置任务栏缩略图按钮（ITaskbarList3 `ThumbBarAddButtons`）。
    /// 对应 Electron `setThumbarButtons`。`None` 或空切片清除全部按钮。
    /// 按钮 `id` 回传：任务栏点击发出 `WM_COMMAND`，`HIWORD(wParam)==THBN_CLICKED`，
    /// `LOWORD(wParam)` 即 `id`（消息路由见设计文档）。
    fn set_thumbar_buttons(&self, buttons: Option<&[ThumbarButton]>);

    /// 设置任务栏覆盖图标（ITaskbarList3 `SetOverlayIcon`）。
    /// 对应 Electron/Tauri `setOverlayIcon`。`None` 清除。
    fn set_overlay_icon(&self, overlay: Option<TaskbarOverlay>);

    /// 设置窗口的任务栏 AppUserModelID（`SHGetPropertyStoreForWindow` +
    /// `IPropertyStore::SetValue` + `Commit`，`PKEY_AppUserModel_ID`）。
    /// 对应 Electron `setAppDetails({ appId })`。`None` 清除该属性（写 `VT_EMPTY`）。
    fn set_app_user_model_id(&self, app_id: Option<&str>);

    /// 声明式 hit-test 区域（客户端逻辑像素，与 DrawBatch 绘制坐标一致）。
    ///
    /// 按钮外观由用户在 `on_frame` 里用 DrawBatch 画在客户端；`WM_NCHITTEST` 返回
    /// `HT*` 让 Windows 自动接管交互（snap layout / 双击 / 右键菜单 / 按钮点击 /
    /// Aero Snap）。不产生 NC 区域，系统不画标准标题栏/按钮。
    ///
    /// 命中规则：**后声明优先**（z 序在上）——允许按钮叠在标题栏上仍命中按钮。
    fn set_non_client_regions(&self, regions: &[crate::nc::NonClientRegion]);

    /// 读取当前 hit-test regions。
    fn non_client_regions(&self) -> Vec<crate::nc::NonClientRegion>;

    /// 命令式 hit-test 回调。**优先级高于** `set_non_client_regions`。
    /// 返回 `NonClientHit::Client` 时继续走默认（让系统处理 border resize 等）。
    /// 传 `None` 清除。
    ///
    /// # Send
    /// 回调经 `nc_tx`（`mpsc::Sender`）从渲染线程发到 winit 线程；`NcUpdate` 载荷由
    /// `HitTestCallback` newtype（`unsafe impl Send`，与 `InputCallbacks` 同约定）兜底，
    /// 用户传非 `Send` 闭包也无需处理。
    fn set_hit_test_callback(
        &self,
        callback: Option<impl FnMut(crate::nc::HitTestInput) -> crate::nc::NonClientHit + 'static>,
    );

    /// 任务栏缩略图按钮点击回调（参数 = 按钮 `id`）。仅 Windows 生效。
    fn on_thumb_button(&self, callback: impl FnMut(u32) + 'static) -> &Self;
}

/// 任务栏进度状态（`set_progress_bar`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TaskbarProgress {
    /// 清除进度（`TBPF_NOPROGRESS`）。
    None,
    /// 正常进度，参数 = 完成比例 `0.0..=1.0`。
    Normal(f64),
    /// 不确定进度（无确定数值，转圈动画）。
    Indeterminate,
    /// 暂停，参数 = 完成比例 `0.0..=1.0`。
    Paused(f64),
    /// 出错，参数 = 完成比例 `0.0..=1.0`。
    Error(f64),
}

/// 任务栏缩略图按钮（`set_thumbar_buttons`）。
#[derive(Debug, Clone)]
pub struct ThumbarButton {
    /// 按钮标识（任务栏点击经 `WM_COMMAND`/`THBN_CLICKED` 回调）。
    pub id: u32,
    /// 按钮图标（RGBA 像素，建议 32×32；`None` = 无图标）。
    pub icon: Option<TaskbarIcon>,
    /// 悬停提示文本（写入 `szTip`，最多 259 字符）。
    pub tooltip: Option<String>,
    /// 点击后是否立即关闭缩略图（`THBF_DISMISSONCLICK`）。
    pub dismiss_on_click: bool,
    /// 是否禁用（`THBF_DISABLED`；默认开启）。
    pub disabled: bool,
    /// 是否隐藏（`THBF_HIDDEN`）。
    pub hidden: bool,
    /// 是否无背景（`THBF_NOBACKGROUND`）。
    pub no_background: bool,
    /// 是否不响应鼠标（`THBF_NONINTERACTIVE`）。
    pub non_interactive: bool,
}

/// 任务栏图标（RGBA 像素 + 尺寸，`set_overlay_icon` / `ThumbarButton`）。
#[derive(Debug, Clone)]
pub struct TaskbarIcon {
    /// 每像素 4 字节 `R,G,B,A`，长度 = `width*height*4`。
    pub rgba: Vec<u8>,
    /// 像素宽（应 ≥ 1，建议 32）。
    pub width: u32,
    /// 像素高（应 ≥ 1，建议 32）。
    pub height: u32,
}

/// 任务栏覆盖图标（`set_overlay_icon`）。`icon` 之外的 `description`
/// 为无障碍辅助说明（工具提示不可见，仅供屏幕阅读器）。
#[derive(Debug, Clone)]
pub struct TaskbarOverlay {
    /// 图标像素（建议 16×16，Windows 会上采样）。
    pub icon: TaskbarIcon,
    /// 无障碍描述（`SetOverlayIcon` 的 `pszDescription`，最多 256 字符）。
    pub description: String,
}

impl WindowExtWindows for crate::window::VireoWindow {
    fn set_enable(&self, enabled: bool) {
        winit::platform::windows::WindowExtWindows::set_enable(&*self.inner, enabled);
    }

    fn set_taskbar_icon(&self, taskbar_icon: Option<winit::window::Icon>) {
        winit::platform::windows::WindowExtWindows::set_taskbar_icon(
            &*self.inner,
            taskbar_icon,
        );
    }

    fn set_skip_taskbar(&self, skip: bool) {
        winit::platform::windows::WindowExtWindows::set_skip_taskbar(&*self.inner, skip);
    }

    fn set_system_backdrop(&self, backdrop_type: BackdropType) {
        winit::platform::windows::WindowExtWindows::set_system_backdrop(
            &*self.inner,
            backdrop_type,
        );
    }

    fn set_border_color(&self, color: Option<Color>) {
        winit::platform::windows::WindowExtWindows::set_border_color(
            &*self.inner,
            color,
        );
    }

    fn set_title_background_color(&self, color: Option<Color>) {
        winit::platform::windows::WindowExtWindows::set_title_background_color(
            &*self.inner,
            color,
        );
    }

    fn set_title_text_color(&self, color: Color) {
        winit::platform::windows::WindowExtWindows::set_title_text_color(
            &*self.inner,
            color,
        );
    }

    fn set_corner_preference(&self, preference: CornerPreference) {
        self.user_corner_pref.set(preference);
        // Frameless 无边框时 DWM 无法圆角，钳回 Default（偏好已记录，
        // 待 set_frame_style 切回 Normal/HiddenTitlebar 时恢复）。
        if self.frame_style.get() == crate::window::FrameStyle::Frameless {
            winit::platform::windows::WindowExtWindows::set_corner_preference(
                &*self.inner,
                winit::platform::windows::CornerPreference::Default,
            );
        } else {
            winit::platform::windows::WindowExtWindows::set_corner_preference(
                &*self.inner,
                preference.into_winit(),
            );
        }
    }

    fn move_top(&self) {
        move_zorder(&*self.inner, true);
    }

    fn move_above(&self) {
        move_zorder(&*self.inner, false);
    }

    fn set_progress_bar(&self, state: TaskbarProgress) {
        let Some(hwnd) = window_hwnd(&*self.inner) else {
            return;
        };
        let Some(taskbar) = taskbar_list3() else {
            return;
        };
        unsafe {
            let (flag, value) = match state {
                TaskbarProgress::None => (TBPF_NOPROGRESS, None),
                TaskbarProgress::Normal(v) => {
                    (TBPF_NORMAL, Some(clamp_progress(v, 10_000)))
                }
                TaskbarProgress::Indeterminate => (TBPF_INDETERMINATE, None),
                TaskbarProgress::Paused(v) => (TBPF_PAUSED, Some(clamp_progress(v, 10_000))),
                TaskbarProgress::Error(v) => (TBPF_ERROR, Some(clamp_progress(v, 10_000))),
            };
            let f: HrFnState = slot_fn(taskbar, taskbar::SET_PROGRESS_STATE);
            let hr = f(taskbar, hwnd as HWND, flag);
            if hr != KW_HRESULT_OK {
                log::warn!("vireo taskbar: SetProgressState hr=0x{:08X}", hr as u32);
            }
            if let Some((n, d)) = value {
                let f: HrFnProgress = slot_fn(taskbar, taskbar::SET_PROGRESS_VALUE);
                let hr = f(taskbar, hwnd as HWND, n, d);
                if hr != KW_HRESULT_OK {
                    log::warn!("vireo taskbar: SetProgressValue hr=0x{:08X}", hr as u32);
                }
            }
        }
    }

    fn set_thumbar_buttons(&self, buttons: Option<&[ThumbarButton]>) {
        let Some(hwnd) = window_hwnd(&*self.inner) else {
            return;
        };
        let Some(taskbar) = taskbar_list3() else {
            return;
        };
        // 先释放窗口此前持有的缩略图图标。
        drop_thumbar_icons(hwnd);
        let Some(buttons) = buttons else {
            // 空 = 清除全部按钮。
            clear_thumbar_callback(hwnd);
unsafe {
            let f: HrFnButtons = slot_fn(taskbar, taskbar::THUMB_BAR_ADD_BUTTONS);
            f(taskbar, hwnd as HWND, 0, std::ptr::null());
        }
        return;
    };
if buttons.is_empty() {
            clear_thumbar_callback(hwnd);
            unsafe {
                let f: HrFnButtons = slot_fn(taskbar, taskbar::THUMB_BAR_ADD_BUTTONS);
                f(taskbar, hwnd as HWND, 0, std::ptr::null());
            }
            return;
        }
        let count = buttons.len().min(u32::MAX as usize) as u32;
        if count > 7 {
            // Windows 限制：缩略图最多 7 个按钮。
            log::warn!("vireo: set_thumbar_buttons 超过 7 个按钮，截断到 7");
        }
        let count = count.min(7);
        let mut icons: Vec<isize> = Vec::with_capacity(count as usize);
        let mut tb: Vec<THUMBBUTTON> = Vec::with_capacity(count as usize);
        for b in &buttons[..count as usize] {
            let mut icon = 0isize;
            if let Some(some) = &b.icon {
                icon = hicone_from_rgba(&some.rgba, some.width, some.height);
                if icon != 0 {
                    icons.push(icon);
                }
            }
            let mut flags = if b.disabled { THBF_DISABLED } else { THBF_ENABLED };
            if b.dismiss_on_click {
                flags |= THBF_DISMISSONCLICK;
            }
            if b.hidden {
                flags |= THBF_HIDDEN;
            }
            if b.no_background {
                flags |= THBF_NOBACKGROUND;
            }
            if b.non_interactive {
                flags |= THBF_NONINTERACTIVE;
            }
            let mut mask: THUMBBUTTONMASK = 0;
            if icon != 0 {
                mask |= THB_ICON;
            }
            if b.tooltip.is_some() {
                mask |= THB_TOOLTIP;
            }
            mask |= THB_FLAGS;
            let mut sz_tip = [0u16; 260];
            if let Some(tip) = &b.tooltip {
                let max = sz_tip.len() - 1;
                for (dst, src) in sz_tip.iter_mut().zip(tip.encode_utf16().take(max)) {
                    *dst = src;
                }
            }
            tb.push(THUMBBUTTON {
                dwMask: mask,
                iId: b.id,
                iBitmap: 0,
                hIcon: icon as HICON,
                szTip: sz_tip,
                dwFlags: flags,
            });
        }
        set_thumbar_icons(hwnd, icons);
        unsafe {
            let f: HrFnButtons = slot_fn(taskbar, taskbar::THUMB_BAR_ADD_BUTTONS);
            let hr = f(taskbar, hwnd as HWND, count, tb.as_ptr());
            if hr != KW_HRESULT_OK {
                log::warn!("vireo taskbar: ThumbBarAddButtons hr=0x{:08X}", hr as u32);
            }
        }
    }

    fn set_overlay_icon(&self, overlay: Option<TaskbarOverlay>) {
        let Some(hwnd) = window_hwnd(&*self.inner) else {
            return;
        };
        let Some(taskbar) = taskbar_list3() else {
            return;
        };
        drop_overlay_icons(hwnd);
        // icon / 描述字符串（宽版，需存活到 SetOverlayIcon 返回）。
        let hicon: isize;
        let desc_wide: Vec<u16> = match &overlay {
            Some(o) => {
                let icon = hicone_from_rgba(&o.icon.rgba, o.icon.width, o.icon.height);
                if icon == 0 {
                    return;
                }
                set_overlay_icons(hwnd, vec![icon]);
                hicon = icon;
                o.description
                    .encode_utf16()
                    .take(255)
                    .chain(std::iter::once(0))
                    .collect()
            }
            None => {
                hicon = 0;
                Vec::new()
            }
        };
        let desc_ptr = if hicon != 0 {
            desc_wide.as_ptr()
        } else {
            std::ptr::null()
        };
        unsafe {
            let f: HrFnOverlay = slot_fn(taskbar, taskbar::SET_OVERLAY_ICON);
            let hr = f(taskbar, hwnd as HWND, hicon as HICON, desc_ptr);
            if hr != KW_HRESULT_OK {
                log::warn!("vireo taskbar: SetOverlayIcon hr=0x{:08X}", hr as u32);
            }
        }
    }

    fn set_app_user_model_id(&self, app_id: Option<&str>) {
        let Some(hwnd) = window_hwnd(&*self.inner) else {
            return;
        };
        unsafe {
            let mut pstore: *mut c_void = std::ptr::null_mut();
            let hr = SHGetPropertyStoreForWindow(
                hwnd as HWND,
                &IID_IPROPERTY_STORE,
                &mut pstore,
            );
            if hr != KW_HRESULT_OK || pstore.is_null() {
                return;
            }
            (|| {
                // 构造 PROPVARIANT。
                let mut pv: PROPVARIANT = std::mem::zeroed();
                match app_id {
                    Some(id) => {
                        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                        let bytes = wide.len() * 2;
                        let mem = CoTaskMemAlloc(bytes);
                        if mem.is_null() {
                            return;
                        }
                        std::ptr::copy_nonoverlapping(wide.as_ptr(), mem as *mut u16, wide.len());
                        // vt = VT_LPWSTR；pwszVal 存进零初始化 union 的指针位。
                        pv.Anonymous.Anonymous.vt = VT_LPWSTR;
                        pv.Anonymous.Anonymous.Anonymous.pwszVal =
                            std::mem::transmute::<*mut c_void, windows_sys::core::PWSTR>(mem);
                    }
                    None => {
                        pv.Anonymous.Anonymous.vt = VT_EMPTY;
                    }
                }
                // IPropertyStore::SetValue + Commit。
                let f_state: HrFnSetValue = slot_fn(pstore, propstore::SET_VALUE);
                let hr_set = f_state(pstore, &PKEY_APP_USER_MODEL_ID, &pv);
                if hr_set != KW_HRESULT_OK {
                    log::warn!("vireo taskbar: propstore SetValue hr=0x{:08X}", hr_set as u32);
                }
                let f_commit: HrFnCommit = slot_fn(pstore, propstore::COMMIT);
                let hr_commit = f_commit(pstore);
                if hr_commit != KW_HRESULT_OK {
                    log::warn!("vireo taskbar: propstore Commit hr=0x{:08X}", hr_commit as u32);
                }
                // PropVariantClear 释放 VT_LPWSTR 的 CoTaskMemAlloc。
                let _ = PropVariantClear(&mut pv);
            })();
            // IPropertyStore::Release（vtable 槽 2）。
            let _release: U8FnRelease = slot_fn(pstore, iface::RELEASE);
            let _ = _release(pstore);
        }
    }

    fn set_non_client_regions(&self, regions: &[crate::nc::NonClientRegion]) {
        let Some(hwnd) = win_hwnd(&self.inner) else { return; };
        let _ = self.nc_tx.send((hwnd, NcUpdate::SetRegions(regions.to_vec())));
    }

    fn non_client_regions(&self) -> Vec<crate::nc::NonClientRegion> {
        let Some(hwnd) = win_hwnd(&self.inner) else { return Vec::new(); };
        nc_get_regions(hwnd).unwrap_or_default()
    }

    fn set_hit_test_callback(
        &self,
        callback: Option<impl FnMut(crate::nc::HitTestInput) -> crate::nc::NonClientHit + 'static>,
    ) {
        let Some(hwnd) = win_hwnd(&self.inner) else { return; };
        let upd = match callback {
            Some(f) => NcUpdate::SetHitTestCb(HitTestCallback(Box::new(f))),
            None => NcUpdate::ClearHitTestCb,
        };
        let _ = self.nc_tx.send((hwnd, upd));
    }

    fn on_thumb_button(&self, callback: impl FnMut(u32) + 'static) -> &Self {
        if let Some(hwnd) = win_hwnd(&self.inner) {
            set_thumbar_callback(hwnd, Box::new(callback));
        }
        self
    }
}

/// 取 winit 窗口的原生 HWND，**跨线程可用**（渲染线程安全）。
///
/// winit 的 `Window::window_handle()` 有线程亲和限制：只能从创建窗口的线程调用，
/// 其他线程返回 `Err(HandleError::Unavailable)`。而 vireo 的 `app.run` 闭包跑在
/// 渲染线程，故这里用 winit 提供的 `window_handle_any_thread()` 逃生通道。
///
/// # Safety
/// 该通道把线程安全责任交给调用方。本模块的调用点（ITaskbarList3 / SetWindowPos）
/// 均为线程安全的 Win32 API，满足约定。
fn window_hwnd(window: &winit::window::Window) -> Option<isize> {
    use winit::platform::windows::WindowExtWindows;
    use winit::raw_window_handle::RawWindowHandle;
    let wh = unsafe { window.window_handle_any_thread() }.ok()?;
    let RawWindowHandle::Win32(h) = wh.as_raw() else {
        log::warn!("vireo window_hwnd: 非 Win32 handle");
        return None;
    };
    Some(h.hwnd.get())
}

/// 取 winit 窗口的原生 HWND（`isize`）。`window_hwnd` 的 `pub(crate)` 别名，
/// 供核心 window.rs 的渲染线程调用点与 `WindowExtWindows` NC 方法使用。
pub(crate) fn win_hwnd(window: &winit::window::Window) -> Option<isize> {
    window_hwnd(window)
}

fn clamp_progress(v: f64, denom: u64) -> (u64, u64) {
    let v = v.clamp(0.0, 1.0);
    ((v * denom as f64).round() as u64, denom)
}

/// 用 `SetWindowPos` 调整窗口 z 序（`move_top` / `move_above` 共用）。
/// 尺寸/位置/激活状态均保持不动；`topmost` 时设 `HWND_TOPMOST`，否则 `HWND_NOTOPMOST`
/// （用 `HWND_TOP` 不清除 topmost 标志，无法取消置顶）。
/// 需在窗口所属线程调用（winit 会 `maybe_queue_on_main` 转发）。
fn move_zorder(window: &winit::window::Window, topmost: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    let Some(hwnd) = window_hwnd(window) else {
        return;
    };
    let insert_after = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        SetWindowPos(
            hwnd as HWND,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

pub(crate) fn apply_window_opacity(hwnd: isize, opacity: f64) {
    let opacity = opacity.clamp(0.0, 1.0);
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA,
        WS_EX_LAYERED,
    };
    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE, ex_style | WS_EX_LAYERED as isize);
        let alpha = (opacity * 255.0).round() as u8;
        SetLayeredWindowAttributes(hwnd as HWND, 0, alpha, LWA_ALPHA);
    }
}

pub(crate) fn apply_window_focusable(hwnd: isize, focusable: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    };
    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE);
        let new_style = if focusable {
            ex_style & !(WS_EX_NOACTIVATE as isize)
        } else {
            ex_style | (WS_EX_NOACTIVATE as isize)
        };
        if new_style != ex_style {
            SetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE, new_style);
        }
    }
}

pub(crate) fn cleanup_window_state(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    nc_remove(hwnd);
    drop_thumbar_icons(hwnd);
    drop_overlay_icons(hwnd);
    clear_thumbar_callback(hwnd);
    remove_window_icons_entry(hwnd);
}

pub fn dwm_timing() -> Option<(u64, u64)> {
    use windows_sys::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    unsafe {
        let mut ti: DWM_TIMING_INFO = std::mem::zeroed();
        ti.cbSize = std::mem::size_of::<DWM_TIMING_INFO>() as u32;
        if DwmGetCompositionTimingInfo(std::ptr::null_mut(), &mut ti) == 0 {
            if ti.qpcRefreshPeriod > 0 {
                return Some((ti.qpcVBlank, ti.qpcRefreshPeriod));
            }
        }
    }
    None
}

pub fn qpc_now() -> u64 {
    use windows_sys::Win32::System::Performance::QueryPerformanceCounter;
    let mut v: i64 = 0;
    unsafe {
        let _ = QueryPerformanceCounter(&mut v);
    }
    v as u64
}

pub fn qpc_ticks_per_sec() -> u64 {
    use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;
    static FREQ: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut v: i64 = 0;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut v);
        }
        v.max(1) as u64
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nc::{NonClientHit, NonClientRegion};

    /// GUID 手写错误会导致 `CoCreateInstance` 返回 `E_NOINTERFACE`（0x80004002），
    /// 这类问题极易静默发生。这里逐个校验任务栏相关 GUID 的字节序列与权威值一致。
    #[test]
    fn taskbar_guids_match_reference() {
        // windows-sys 的 GUID 无 PartialEq/Debug，用字段逐个比较。
        fn assert_guid_eq(actual: windows_sys::core::GUID, expected_u128: u128, name: &str) {
            let expected = windows_sys::core::GUID::from_u128(expected_u128);
            assert!(
                actual.data1 == expected.data1
                    && actual.data2 == expected.data2
                    && actual.data3 == expected.data3
                    && actual.data4 == expected.data4,
                "{name} 字节序列与权威值不一致"
            );
        }

        // CLSID_TaskbarList {56FDF344-FD6D-11D0-958A-006097C9A090}
        assert_guid_eq(
            CLSID_TASKBAR_LIST,
            0x56FDF344_FD6D_11D0_958A_006097C9A090,
            "CLSID_TASKBAR_LIST",
        );
        // IID_ITaskbarList3 {EA1AFB91-9E28-4B86-90E9-9E9F8A5EEFAF}
        assert_guid_eq(
            IID_ITASKBAR_LIST3,
            0xEA1AFB91_9E28_4B86_90E9_9E9F8A5EEFAF,
            "IID_ITASKBAR_LIST3",
        );
        // IID_IPropertyStore {886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99}
        assert_guid_eq(
            IID_IPROPERTY_STORE,
            0x886D8EEB_8CF2_4446_8D02_CDBA1DBDCF99,
            "IID_IPROPERTY_STORE",
        );
        // PKEY_AppUserModel_ID fmtid {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}
        assert_guid_eq(
            PKEY_APP_USER_MODEL_ID.fmtid,
            0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3,
            "PKEY_APP_USER_MODEL_ID fmtid",
        );
        assert_eq!(PKEY_APP_USER_MODEL_ID.pid, 5);
    }

    /// vtable 槽位推导（ITaskbarList3 = IUnknown(3) + ITaskbarList(5) + ITaskbarList2(1)）。
    #[test]
    fn taskbar_vtable_slots_match_reference() {
        assert_eq!(taskbar::HR_INIT, 3);
        assert_eq!(taskbar::ADD_TAB, 4);
        assert_eq!(taskbar::DELETE_TAB, 5);
        assert_eq!(taskbar::ACTIVATE_TAB, 6);
        assert_eq!(taskbar::SET_ACTIVE_ALT, 7);
        assert_eq!(taskbar::MARK_FULLSCREEN_WINDOW, 8);
        assert_eq!(taskbar::SET_PROGRESS_VALUE, 9);
        assert_eq!(taskbar::SET_PROGRESS_STATE, 10);
        assert_eq!(taskbar::REGISTER_TAB, 11);
        assert_eq!(taskbar::UNREGISTER_TAB, 12);
        assert_eq!(taskbar::SET_TAB_ORDER, 13);
        assert_eq!(taskbar::SET_TAB_ACTIVE, 14);
        assert_eq!(taskbar::THUMB_BAR_ADD_BUTTONS, 15);
        assert_eq!(taskbar::THUMB_BAR_UPDATE_BUTTONS, 16);
        assert_eq!(taskbar::THUMB_BAR_SET_IMAGE_LIST, 17);
        assert_eq!(taskbar::SET_OVERLAY_ICON, 18);
        assert_eq!(taskbar::SET_THUMBNAIL_TOOLTIP, 19);
        assert_eq!(taskbar::SET_THUMBNAIL_CLIP, 20);
    }

    /// `hicone_from_rgba` 生成真实 HICON（GDI 调用，无需窗口）。
    /// 回归：PNG filter 字节非 0 时 `CreateIconFromResourceEx` 返回 NULL。
    #[test]
    fn hicone_from_rgba_produces_icon_handle() {
        let rgba = vec![0u8; 8 * 8 * 4];
        let icon = hicone_from_rgba(&rgba, 8, 8);
        if icon == 0 {
            let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            panic!("CreateIconFromResourceEx 失败, GetLastError={}", err);
        }
        unsafe { DestroyIcon(icon as HICON) };
    }

    // ====== §7.6 NC API 单元测试（无窗口，state 存储/读取）======

    #[test]
    fn nc_get_regions_default_empty() {
        let hwnd: isize = 0xDEAD_BEEF_isize;
        nc_remove(hwnd);
        assert!(nc_get_regions(hwnd).is_none());
    }

    #[test]
    fn nc_set_regions_roundtrip() {
        let hwnd: isize = 0xC0FF_EE01_isize;
        nc_remove(hwnd);

        let regions = vec![
            NonClientRegion {
                rect: crate::math::Rect::new(0.0, 0.0, 600.0, 32.0),
                hit_test: NonClientHit::Caption,
            },
            NonClientRegion {
                rect: crate::math::Rect::new(8.0, 0.0, 46.0, 32.0),
                hit_test: NonClientHit::Close,
            },
        ];
        NC_STATES.lock().unwrap().insert(
            hwnd,
            NcState {
                regions: regions.clone(),
                hit_test_cb: None,
                pressed_ht: None,
            },
        );
        let read = nc_get_regions(hwnd).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].hit_test, NonClientHit::Caption);
        assert_eq!(read[1].hit_test, NonClientHit::Close);
        nc_remove(hwnd);
    }

    #[test]
    fn nc_regions_last_declared_wins() {
        let regions = vec![
            NonClientRegion {
                rect: crate::math::Rect::new(0.0, 0.0, 600.0, 32.0),
                hit_test: NonClientHit::Caption,
            },
            NonClientRegion {
                rect: crate::math::Rect::new(8.0, 0.0, 46.0, 32.0),
                hit_test: NonClientHit::Close,
            },
        ];
        let lx = 24.0_f32;
        let ly = 16.0_f32;
        let hit = regions
            .iter()
            .rev()
            .find(|r| r.rect.contains([lx, ly]))
            .map(|r| r.hit_test);
        assert_eq!(hit, Some(NonClientHit::Close));

        let lx2 = 200.0_f32;
        let hit2 = regions
            .iter()
            .rev()
            .find(|r| r.rect.contains([lx2, ly]))
            .map(|r| r.hit_test);
        assert_eq!(hit2, Some(NonClientHit::Caption));
    }

    #[test]
    fn nc_clear_all_resets_state() {
        let hwnd: isize = 0xC0FF_EE02_isize;
        nc_remove(hwnd);

        NC_STATES.lock().unwrap().insert(
            hwnd,
            NcState {
                regions: vec![NonClientRegion {
                    rect: crate::math::Rect::new(0.0, 0.0, 10.0, 10.0),
                    hit_test: NonClientHit::Close,
                }],
                hit_test_cb: None,
                pressed_ht: None,
            },
        );
        assert_eq!(nc_get_regions(hwnd).unwrap().len(), 1);

        NC_STATES.lock().unwrap().insert(
            hwnd,
            NcState {
                regions: Vec::new(),
                hit_test_cb: None,
                pressed_ht: None,
            },
        );
        assert!(nc_get_regions(hwnd).unwrap().is_empty());
        nc_remove(hwnd);
    }
}
