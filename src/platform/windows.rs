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
//! 注意：这里 `windows-sys` 与 winit 各自独立的 windows-sys 版本类型不互通，
//! 但作为普通函数调用（传 `HWND = isize`）无碍。

use std::ffi::c_void;
use std::sync::{LazyLock, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONULL,
};
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows_sys::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
use windows_sys::Win32::System::Variant::{VT_EMPTY, VT_LPWSTR};
use windows_sys::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, TBPF_ERROR, TBPF_INDETERMINATE,
    TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, THB_FLAGS, THB_ICON, THB_TOOLTIP,
    THBF_DISABLED, THBF_DISMISSONCLICK, THBF_ENABLED, THBF_HIDDEN, THBF_NOBACKGROUND,
    THBF_NONINTERACTIVE, THBN_CLICKED, THUMBBUTTON, THUMBBUTTONMASK,
};
use windows_sys::Win32::UI::Shell::PropertiesSystem::{
    SHGetPropertyStoreForWindow, PROPERTYKEY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DefWindowProcW, DestroyIcon, GetSystemMetrics, IsZoomed,
    NCCALCSIZE_PARAMS, SM_CXPADDEDBORDER, SM_CXSIZEFRAME, SM_CYSIZEFRAME, WM_COMMAND,
    IMAGE_FLAGS, LR_DEFAULTCOLOR,
};

pub use winit::platform::windows::BackdropType;
pub use winit::platform::windows::Color;

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
    pub const GET_AT: usize = 3;
    pub const GET_COUNT: usize = 4;
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
        eprintln!(
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
        if icon == 0 {
            eprintln!("vireo taskbar: CreateIconFromResourceEx 失败 ({}x{})", width, height);
        }
        icon
    }
}

fn create_taskbar_list3() -> *mut c_void {
    // MTA：确保跨线程调用（渲染线程 + winit 线程）安全。
    let hr_co = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
    if hr_co != KW_HRESULT_OK && hr_co != 1 {
        // 1 = S_FALSE（本线程已初始化，合法）；RPC_E_CHANGED_MODE 等才是问题。
        eprintln!(
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
        eprintln!("vireo taskbar: CoCreateInstance ITaskbarList3 hr=0x{:08X}", hr as u32);
        return std::ptr::null_mut();
    }
    // HrInit 失败也保留对象（SetProgressValue 等仍可用）。
    unsafe {
        let f: HrFn0 = slot_fn(obj, taskbar::HR_INIT);
        let hr_init = f(obj);
        if hr_init != KW_HRESULT_OK {
            eprintln!("vireo taskbar: HrInit hr=0x{:08X}", hr_init as u32);
        }
    }
    eprintln!("vireo taskbar: ITaskbarList3 ready, obj={:p}", obj);
    obj
}

/// COM 返回值 `HRESULT`（windows-sys `HRESULT` 即 i32）。
type Hr = i32;

type HrFn0 = unsafe extern "system" fn(*mut c_void) -> Hr;
type HrFnProgress = unsafe extern "system" fn(*mut c_void, HWND, u64, u64) -> Hr;
type HrFnState = unsafe extern "system" fn(*mut c_void, HWND, i32) -> Hr;
type HrFnButtons = unsafe extern "system" fn(*mut c_void, HWND, u32, *const THUMBBUTTON) -> Hr;
type HrFnOverlay = unsafe extern "system" fn(*mut c_void, HWND, isize, *const u16) -> Hr;
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

static WINDOW_ICONS: LazyLock<Mutex<std::collections::HashMap<HWND, WindowIcons>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 记录窗口持有的一组缩略图按钮 HICON。
fn set_thumbar_icons(hwnd: HWND, icons: Vec<isize>) {
    let mut w = WINDOW_ICONS.lock().unwrap();
    w.entry(hwnd).or_default().thumb_bar = icons;
}

/// 记录窗口持有的 overlay HICON。
fn set_overlay_icons(hwnd: HWND, icons: Vec<isize>) {
    let mut w = WINDOW_ICONS.lock().unwrap();
    w.entry(hwnd).or_default().overlay = icons;
}

/// 销毁窗口此前的缩略图按钮 HICON。
fn drop_thumbar_icons(hwnd: HWND) {
    let mut w = WINDOW_ICONS.lock().unwrap();
    if let Some(icons) = w.get_mut(&hwnd) {
        for icon in icons.thumb_bar.drain(..) {
            unsafe { DestroyIcon(icon) };
        }
    }
}

/// 销毁窗口此前的 overlay HICON。
fn drop_overlay_icons(hwnd: HWND) {
    let mut w = WINDOW_ICONS.lock().unwrap();
    if let Some(icons) = w.get_mut(&hwnd) {
        for icon in icons.overlay.drain(..) {
            unsafe { DestroyIcon(icon) };
        }
    }
}

const WM_NCCALCSIZE: u32 = 131;
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

static THUMB_CALLBACKS: LazyLock<Mutex<std::collections::HashMap<HWND, Vec<ThumbCallback>>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 注册/覆盖窗口的缩略图按钮点击回调（追加；`None` 不清）。
/// 幂等：重复调用只追加；无回调时卸载点击子类。
pub(crate) fn set_thumbar_callback(hwnd: HWND, cb: Box<dyn FnMut(u32)>) {
    let mut map = THUMB_CALLBACKS.lock().unwrap();
    map.entry(hwnd).or_default().push(ThumbCallback(cb));
    unsafe {
        SetWindowSubclass(hwnd, Some(thumb_subclass_proc), THUMB_SUBCLASS_ID, 0);
    }
}

/// 卸载窗口的全部缩略图点击回调（`set_thumbar_buttons(None)` 时调用）。
fn clear_thumbar_callback(hwnd: HWND) {
    let mut map = THUMB_CALLBACKS.lock().unwrap();
    if map.remove(&hwnd).is_some() {
        unsafe {
            RemoveWindowSubclass(hwnd, Some(thumb_subclass_proc), THUMB_SUBCLASS_ID);
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
                if let Some(cbs) = map.get_mut(&hwnd) {
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
pub fn install(hwnd: HWND, titlebar: bool, border: bool) {
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
            let hr = f(taskbar, hwnd, flag);
            if hr != KW_HRESULT_OK {
                eprintln!("vireo taskbar: SetProgressState hr=0x{:08X}", hr as u32);
            }
            if let Some((n, d)) = value {
                let f: HrFnProgress = slot_fn(taskbar, taskbar::SET_PROGRESS_VALUE);
                let hr = f(taskbar, hwnd, n, d);
                if hr != KW_HRESULT_OK {
                    eprintln!("vireo taskbar: SetProgressValue hr=0x{:08X}", hr as u32);
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
                f(taskbar, hwnd, 0, std::ptr::null());
            }
            return;
        };
        if buttons.is_empty() {
            clear_thumbar_callback(hwnd);
            unsafe {
                let f: HrFnButtons = slot_fn(taskbar, taskbar::THUMB_BAR_ADD_BUTTONS);
                f(taskbar, hwnd, 0, std::ptr::null());
            }
            return;
        }
        let count = buttons.len().min(u32::MAX as usize) as u32;
        if count > 7 {
            // Windows 限制：缩略图最多 7 个按钮。
            eprintln!("vireo: set_thumbar_buttons 超过 7 个按钮，截断到 7");
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
                hIcon: icon,
                szTip: sz_tip,
                dwFlags: flags,
            });
        }
        set_thumbar_icons(hwnd, icons);
        unsafe {
            let f: HrFnButtons = slot_fn(taskbar, taskbar::THUMB_BAR_ADD_BUTTONS);
            let hr = f(taskbar, hwnd, count, tb.as_ptr());
            if hr != KW_HRESULT_OK {
                eprintln!("vireo taskbar: ThumbBarAddButtons hr=0x{:08X}", hr as u32);
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
            let hr = f(taskbar, hwnd, hicon, desc_ptr);
            if hr != KW_HRESULT_OK {
                eprintln!("vireo taskbar: SetOverlayIcon hr=0x{:08X}", hr as u32);
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
                hwnd,
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
                    eprintln!("vireo taskbar: propstore SetValue hr=0x{:08X}", hr_set as u32);
                }
                let f_commit: HrFnCommit = slot_fn(pstore, propstore::COMMIT);
                let hr_commit = f_commit(pstore);
                if hr_commit != KW_HRESULT_OK {
                    eprintln!("vireo taskbar: propstore Commit hr=0x{:08X}", hr_commit as u32);
                }
                // PropVariantClear 释放 VT_LPWSTR 的 CoTaskMemAlloc。
                let _ = PropVariantClear(&mut pv);
            })();
            // IPropertyStore::Release（vtable 槽 2）。
            let _release: U8FnRelease = slot_fn(pstore, iface::RELEASE);
            let _ = _release(pstore);
        }
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
fn window_hwnd(window: &winit::window::Window) -> Option<HWND> {
    use winit::platform::windows::WindowExtWindows;
    use winit::raw_window_handle::RawWindowHandle;
    let wh = unsafe { window.window_handle_any_thread() }.ok()?;
    let RawWindowHandle::Win32(h) = wh.as_raw() else {
        eprintln!("vireo window_hwnd: 非 Win32 handle");
        return None;
    };
    Some(h.hwnd.get())
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
            hwnd,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        unsafe { DestroyIcon(icon) };
    }
}
