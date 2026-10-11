//! 系统剪贴板（纯免费函数，无 App/窗口状态）。
//!
//! 剪贴板是进程级 OS 全局状态 —— 读写它不需要任何 vireo 句柄，所以 API 是
//! 免费函数（`use vireo::clipboard_text;`），不挂 `App` 上。`storage` 把方法
//! 挂 `App` 是因为要注入 `storage_name`；这里没有要注入的东西，挂上去就是
//! 纯透传，没有意义。
//!
//! # 内容
//!
//! - 纯文本：[`clipboard_text`] / [`set_clipboard_text`]。
//! - HTML：[`clipboard_html`] / [`set_clipboard_html`]。读严格 —— 剪贴板上只有
//!   纯文本时返回 `None`，不降级（调用方自己决定要不要再读文本）；写要求同时给
//!   `plain_fallback`（Windows 照惯例双格式发布，Android 的 `newHtmlText` 签名
//!   本来就要 text 参数，两端对齐）。
//!
//! # 平台行为
//!
//! - **Windows**：`CF_UNICODETEXT`（UTF-16LE）/ `CF_HTML`（字节偏移头）。
//!   `OpenClipboard` 可能被别的应用短暂占用，内部重试 5×10ms，仍失败才 `Err`。
//!   渲染线程直接调即可（`OpenClipboard(NULL)` 不依赖窗口）。
//! - **Android**：纯 JNI（`ClipboardManager`），**无需 manifest 权限、无需改 Java**
//!   （和 `cache_dir` 同款）。API 29+ 后台读不到（前台可读）；API 33+ 读会弹
//!   系统 "pasted" 提示 —— OS 行为，改不了，文档到此为止。
//! - **macOS / Linux / 其它**：[`VireoError::Unsupported`]。Linux 可行方案是
//!   `arboard`（自建隐藏窗口，无需调用方给句柄），Wayland 下只有 wlroots 系
//!   compositor 有协议（GNOME 不行）—— 留待以后，不在本轮。
//!
//! # 空剪贴板不是错误
//!
//! 读到空/无内容返回 `None`（`Option`），只在「调不通」时 `Err`（`Result` 只在
//! 写接口上有 —— Windows 锁竞争耗尽、JNI 失败属于此类）。

use crate::error::VireoError;

/// 读纯文本。`None` = 剪贴板空或当前内容不是文本（不是错误）。
pub fn clipboard_text() -> Option<String> {
    #[cfg(windows)]
    {
        return get_text_impl();
    }
    #[cfg(target_os = "android")]
    {
        return get_text_android();
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = VireoError::Unsupported("clipboard");
        return None;
    }
}

/// 写纯文本。
pub fn set_clipboard_text(text: &str) -> Result<(), VireoError> {
    #[cfg(windows)]
    {
        return set_text_impl(text);
    }
    #[cfg(target_os = "android")]
    {
        return set_text_android(text);
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = text;
        return Err(VireoError::Unsupported("clipboard"));
    }
}

/// 读 HTML 片段。`None` = 剪贴板上没有 HTML（**即使有纯文本也不降级**）。
pub fn clipboard_html() -> Option<String> {
    #[cfg(windows)]
    {
        return get_html_impl();
    }
    #[cfg(target_os = "android")]
    {
        return get_html_android();
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = VireoError::Unsupported("clipboard");
        return None;
    }
}

/// 写 HTML（同时发布纯文本 fallback，保证记事本这类只认文本的地方可粘）。
pub fn set_clipboard_html(html: &str, plain_fallback: &str) -> Result<(), VireoError> {
    #[cfg(windows)]
    {
        return set_html_impl(html, plain_fallback);
    }
    #[cfg(target_os = "android")]
    {
        return set_html_android(html, plain_fallback);
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        let _ = (html, plain_fallback);
        return Err(VireoError::Unsupported("clipboard"));
    }
}

// ---- 纯逻辑（无 cfg，全平台可测） ------------------------------------------------

/// UTF-16LE 编码 + 末尾 NUL（`CF_UNICODETEXT` 要求以 NUL 结尾）。
// Windows 生产代码 + 全平台单测用；其它平台非 test 构建下无人调用。
#[cfg(any(windows, test))]
pub(crate) fn encode_utf16_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16LE 解码，遇到首个 NUL 即停（`CF_UNICODETEXT` 按 NUL 截断；
/// 越界不用管 —— 调用方按 `GlobalSize` 切片，`end` 兜底）。
/// 非法序列按 U+FFFD 替换（剪贴板内容不可信，不值得 `Err`）。
#[cfg(any(windows, test))]
pub(crate) fn decode_utf16_bounded(words: &[u16]) -> String {
    let end = words.iter().position(|&w| w == 0).unwrap_or(words.len());
    String::from_utf16_lossy(&words[..end])
}

/// 组 CF_HTML 字节流（`Version:0.9` + 8 位十进制**字节**偏移头）。
///
/// 偏移从数据起始算起。`fragment` 按 UTF-8 原样嵌入（中文的偏移是字节数，
/// 不是字符数 —— 这是 CF_HTML 最常见的错法，单测用中文锁死）。
#[cfg(any(windows, test))]
pub(crate) fn build_cf_html(fragment: &str) -> Vec<u8> {
    let frag = fragment.as_bytes();
    let pre = b"<html>\r\n<body>\r\n<!--StartFragment-->";
    let post = b"<!--EndFragment-->\r\n</body>\r\n</html>";
    // 占位头与最终头等长（8 位数字），先算偏移再填数。
    let placeholder =
        b"Version:0.9\r\nStartHTML:00000000\r\nEndHTML:00000000\r\nStartFragment:00000000\r\nEndFragment:00000000\r\n";
    let sh = placeholder.len();
    let sf = sh + pre.len();
    let ef = sf + frag.len();
    let eh = ef + post.len();
    let header = format!(
        "Version:0.9\r\nStartHTML:{sh:08}\r\nEndHTML:{eh:08}\r\nStartFragment:{sf:08}\r\nEndFragment:{ef:08}\r\n"
    );
    debug_assert_eq!(header.len(), placeholder.len());
    [header.as_bytes(), pre, frag, post].concat()
}

/// 解析 CF_HTML，取出 `StartFragment..EndFragment` 切片。
///
/// 数字宽度不限（规范写 8 位，但 10 位的生产者真实存在，按「冒号后连续数字」解析，
/// 与换行风格无关）。越界/非法 UTF-8/缺标记一律 `None`。
#[cfg(any(windows, test))]
pub(crate) fn parse_cf_html(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    let num = |key: &str| -> Option<usize> {
        let i = text.find(key)? + key.len();
        let digits: String = text[i..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            None
        } else {
            digits.parse().ok()
        }
    };
    let sf = num("StartFragment:")?;
    let ef = num("EndFragment:")?;
    if sf > ef || ef > data.len() {
        return None;
    }
    std::str::from_utf8(&data[sf..ef])
        .ok()
        .map(|s| s.to_string())
}

// ---- Windows --------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::{build_cf_html, decode_utf16_bounded, encode_utf16_nul, parse_cf_html};
    use crate::error::VireoError;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats,
        GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
        SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    // 注意：0.61 里剪贴板函数搬到了 `System::DataExchange`（以前在
    // `UI::WindowsAndMessaging`），`CF_UNICODETEXT` 在 `System::Ole` 且类型是
    // `CLIPBOARD_FORMAT(u16)` —— 传参时 `as u32`；`HANDLE`/`HGLOBAL`/`HWND`
    // 全是裸指针（`is_null` 可用，空窗口传 `null_mut()`）。
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    const CF_TEXT: u32 = CF_UNICODETEXT as u32;

    /// `CloseClipboard` 配对守卫。`Drop` 里不判结果 —— 关失败无可挽回，
    /// 且 `Drop` 里 panic 会把展开中的栈炸成 abort。
    struct ClipGuard;
    impl Drop for ClipGuard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }

    /// 开剪贴板（`NULL` owner，不依赖窗口）。被别的应用短暂占用是常态，
    /// 重试 5×10ms，仍失败才报错。成功时返回持有关闭守卫。
    ///
    /// 另有一层校验（见 `session_coherent`）：极快速的 close→open 接力会在
    /// win32k 里留下「半开」会话 —— `OpenClipboard` 返回成功，但后续调用报
    /// `NOT_OPEN`。真机复现过（set 后紧跟 get 必现），校验不通过就关掉重开。
    fn open() -> Result<ClipGuard, VireoError> {
        for _ in 0..5 {
            let ok = unsafe { OpenClipboard(std::ptr::null_mut()) };
            if ok != 0 {
                if session_coherent() {
                    return Ok(ClipGuard);
                }
                // 半开会话：关掉（多半失败，无所谓）再重试。
                unsafe {
                    CloseClipboard();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(VireoError::Other(
            "clipboard is busy (held by another app)".to_string(),
        ))
    }

    /// 会话一致性校验：需要 open 会话的 `Enum` 与无需会话的 `Count` 互相印证。
    ///
    /// 空剪贴板（`Count == 0`）无从验证，直接信它 —— 后续 `IsAvailable` 会报 0，
    /// 本来就会走 `None`，不需要会话。
    fn session_coherent() -> bool {
        // SAFETY: 纯查询，无副作用；Enum 枚举不需要额外清理。
        unsafe {
            if CountClipboardFormats() <= 0 {
                return true;
            }
            EnumClipboardFormats(0) != 0
        }
    }

    fn wide_nul(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// 把字节拷进可移动全局内存，返回句柄（调用方负责成功时交出去/失败时释放）。
    fn to_hglobal(bytes: &[u8]) -> Result<HGLOBAL, VireoError> {
        // SAFETY: 大小非零（调用方保证至少 1 字节）；失败返回 NULL 并判空。
        let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
        if h.is_null() {
            return Err(VireoError::Other("clipboard alloc failed".to_string()));
        }
        // SAFETY: 刚分配的句柄，lock 必成功；真失败则释放后报错。
        let p = unsafe { GlobalLock(h) };
        if p.is_null() {
            unsafe {
                GlobalFree(h);
            }
            return Err(VireoError::Other("clipboard lock failed".to_string()));
        }
        // SAFETY: p 指向 bytes.len() 可写字节，copy_nonoverlapping 长度一致。
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
            GlobalUnlock(h);
        }
        Ok(h)
    }

    pub(super) fn get_text() -> Option<String> {
        let _g = open().ok()?;
        // SAFETY: 只读查询，无副作用。
        let avail = unsafe { IsClipboardFormatAvailable(CF_TEXT) };
        if avail == 0 {
            return None;
        }
        // SAFETY: 有格式才取，句柄归系统（不释放）；lock 后按 GlobalSize 有界读。
        unsafe {
            let h = GetClipboardData(CF_TEXT);
            if h.is_null() {
                return None;
            }
            let p = GlobalLock(h);
            if p.is_null() {
                return None;
            }
            let bytes = GlobalSize(h);
            let words = std::slice::from_raw_parts(p as *const u16, bytes / 2);
            let s = decode_utf16_bounded(words);
            GlobalUnlock(h);
            Some(s)
        }
    }

    pub(super) fn set_text(text: &str) -> Result<(), VireoError> {
        let wide = encode_utf16_nul(text);
        // u16 → u8 视图：小端平台上 UTF-16LE 字节序即内存序。
        // SAFETY: wide 非空（含 NUL），对齐/长度合法。
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
        set_raw(CF_TEXT, bytes)
    }

    /// 发布单个格式（`EmptyClipboard` 由调用方在批量发布前调一次，见 `set_html`）。
    fn set_raw(format: u32, bytes: &[u8]) -> Result<(), VireoError> {
        let _g = open()?;
        // SAFETY: EmptyClipboard 清空旧内容，失败直接报错（guard 关剪贴板）。
        if unsafe { EmptyClipboard() } == 0 {
            return Err(VireoError::Other("clipboard empty failed".to_string()));
        }
        // 空内容不发布空句柄：GlobalAlloc(0) 行为未定义，直接跳过（剪贴板保持清空态）。
        if bytes.is_empty() {
            return Ok(());
        }
        let h = to_hglobal(bytes)?;
        // SAFETY: 成功后句柄所有权归系统，不释放；失败则释放防泄漏。
        if unsafe { SetClipboardData(format, h) }.is_null() {
            unsafe {
                GlobalFree(h);
            }
            return Err(VireoError::Other("clipboard set failed".to_string()));
        }
        Ok(())
    }

    fn html_format() -> Result<u32, VireoError> {
        let name = wide_nul("HTML Format");
        // SAFETY: NUL 结尾宽字符串；返回 0 表示注册失败。
        let id = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        if id == 0 {
            return Err(VireoError::Other(
                "clipboard HTML format unavailable".to_string(),
            ));
        }
        Ok(id)
    }

    pub(super) fn get_html() -> Option<String> {
        let _g = open().ok()?;
        let id = html_format().ok()?;
        // SAFETY: 同 get_text，只读。
        unsafe {
            if IsClipboardFormatAvailable(id) == 0 {
                return None;
            }
            let h = GetClipboardData(id);
            if h.is_null() {
                return None;
            }
            let p = GlobalLock(h);
            if p.is_null() {
                return None;
            }
            let bytes = GlobalSize(h);
            let data = std::slice::from_raw_parts(p as *const u8, bytes);
            let s = parse_cf_html(data);
            GlobalUnlock(h);
            s
        }
    }

    pub(super) fn set_html(html: &str, plain_fallback: &str) -> Result<(), VireoError> {
        let _g = open()?;
        // SAFETY: 先清空再发布双格式（惯例：文本 fallback 让记事本也能粘）。
        if unsafe { EmptyClipboard() } == 0 {
            return Err(VireoError::Other("clipboard empty failed".to_string()));
        }
        let id = html_format()?;
        // 空 HTML 不发布空句柄（同 set_raw 的理由），但纯文本照常发布。
        if !html.is_empty() {
            let data = build_cf_html(html);
            let h = to_hglobal(&data)?;
            // SAFETY: 成功交系统，失败释放（同 set_raw）。
            if unsafe { SetClipboardData(id, h) }.is_null() {
                unsafe {
                    GlobalFree(h);
                }
                return Err(VireoError::Other("clipboard set failed".to_string()));
            }
        }
        let wide = encode_utf16_nul(plain_fallback);
        if wide.len() > 1 {
            // SAFETY: 同上；wide 含 NUL 必非空。
            let bytes: &[u8] =
                unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
            let h = to_hglobal(bytes)?;
            if unsafe { SetClipboardData(CF_TEXT, h) }.is_null() {
                unsafe {
                    GlobalFree(h);
                }
                return Err(VireoError::Other("clipboard set failed".to_string()));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn get_text_impl() -> Option<String> {
    win::get_text()
}
#[cfg(windows)]
fn set_text_impl(text: &str) -> Result<(), VireoError> {
    win::set_text(text)
}
#[cfg(windows)]
fn get_html_impl() -> Option<String> {
    win::get_html()
}
#[cfg(windows)]
fn set_html_impl(html: &str, plain_fallback: &str) -> Result<(), VireoError> {
    win::set_html(html, plain_fallback)
}

// ---- Android ----------------------------------------------------------------------

#[cfg(target_os = "android")]
fn get_text_android() -> Option<String> {
    read_android(false).unwrap_or(None)
}

#[cfg(target_os = "android")]
fn set_text_android(text: &str) -> Result<(), VireoError> {
    write_android(text, None)
}

#[cfg(target_os = "android")]
fn get_html_android() -> Option<String> {
    read_android(true).unwrap_or(None)
}

#[cfg(target_os = "android")]
fn set_html_android(html: &str, plain_fallback: &str) -> Result<(), VireoError> {
    write_android(plain_fallback, Some(html))
}

/// 读剪贴板（`want_html` 决定取 `getHtmlText` 还是 `getText`）。
/// 外层一律吞错返 `None` —— 读剪贴板失败（空/无权限/后台）都不是值得抛的错，
/// 调用方只关心「有没有拿到」。
#[cfg(target_os = "android")]
fn read_android(want_html: bool) -> Result<Option<String>, VireoError> {
    use crate::platform::android::{JniErr, with_activity};
    use jni::{jni_sig, jni_str};
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: clipboard needs the #[vireo::main] android_main entry"
                .to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        use jni::objects::JValue;
        let svc_name = env.new_string("clipboard").map_err(JniErr::from)?;
        let manager = env
            .call_method(
                activity,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&svc_name.into())],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if manager.as_raw().is_null() {
            return Err(JniErr::Null("ClipboardManager"));
        }
        let has = env
            .call_method(&manager, jni_str!("hasPrimaryClip"), jni_sig!("()Z"), &[])
            .map_err(JniErr::from)?
            .z()
            .map_err(JniErr::from)?;
        if !has {
            return Ok(None);
        }
        let clip = env
            .call_method(
                &manager,
                jni_str!("getPrimaryClip"),
                jni_sig!("()Landroid/content/ClipData;"),
                &[],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if clip.as_raw().is_null() {
            return Ok(None);
        }
        let count = env
            .call_method(&clip, jni_str!("getItemCount"), jni_sig!("()I"), &[])
            .map_err(JniErr::from)?
            .i()
            .map_err(JniErr::from)?;
        if count <= 0 {
            return Ok(None);
        }
        let item = env
            .call_method(
                &clip,
                jni_str!("getItemAt"),
                jni_sig!("(I)Landroid/content/ClipData$Item;"),
                &[JValue::Int(0)],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if item.as_raw().is_null() {
            return Ok(None);
        }
        // 方法名是运行时分支，`jni_str!` 要求字面量，两条都写死。
        let value = if want_html {
            env.call_method(
                &item,
                jni_str!("getHtmlText"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )
        } else {
            env.call_method(
                &item,
                jni_str!("getText"),
                jni_sig!("()Ljava/lang/CharSequence;"),
                &[],
            )
        }
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?;
        if value.as_raw().is_null() {
            return Ok(None);
        }
        let s = env
            .cast_local::<jni::objects::JString>(value)
            .map_err(JniErr::from)?;
        let text = s.try_to_string(env).map_err(JniErr::from)?;
        Ok(Some(text))
    })
    .map_err(|e| VireoError::Other(format!("clipboard read failed: {e:?}")))
}

/// 写剪贴板（`html` 为 `Some` 时走 `newHtmlText`，否则 `newPlainText`）。
#[cfg(target_os = "android")]
fn write_android(text: &str, html: Option<&str>) -> Result<(), VireoError> {
    use crate::platform::android::{JniErr, with_activity};
    use jni::{jni_sig, jni_str};
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: clipboard needs the #[vireo::main] android_main entry"
                .to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        use jni::objects::JValue;
        let svc_name = env.new_string("clipboard").map_err(JniErr::from)?;
        let manager = env
            .call_method(
                activity,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&svc_name.into())],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if manager.as_raw().is_null() {
            return Err(JniErr::Null("ClipboardManager"));
        }
        let label = env.new_string("vireo").map_err(JniErr::from)?;
        let body = env.new_string(text).map_err(JniErr::from)?;
        let clip_cls = env
            .find_class(jni_str!("android/content/ClipData"))
            .map_err(JniErr::from)?;
        let clip = match html {
            Some(h) => {
                let html_js = env.new_string(h).map_err(JniErr::from)?;
                env.call_static_method(
                    clip_cls,
                    jni_str!("newHtmlText"),
                    jni_sig!(
                        "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;Ljava/lang/String;)Landroid/content/ClipData;"
                    ),
                    &[
                        JValue::Object(&label.into()),
                        JValue::Object(&body.into()),
                        JValue::Object(&html_js.into()),
                    ],
                )
            }
            None => env.call_static_method(
                clip_cls,
                jni_str!("newPlainText"),
                jni_sig!(
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;"
                ),
                &[JValue::Object(&label.into()), JValue::Object(&body.into())],
            ),
        }
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?;
        if clip.as_raw().is_null() {
            return Err(JniErr::Null("ClipData"));
        }
        env.call_method(
            &manager,
            jni_str!("setPrimaryClip"),
            jni_sig!("(Landroid/content/ClipData;)V"),
            &[JValue::Object(&clip)],
        )
        .map_err(JniErr::from)?;
        Ok(())
    })
    .map_err(|e| VireoError::Other(format!("clipboard write failed: {e:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_round_trips_cjk_and_emoji() {
        // 代理对（emoji）+ CJK：按 u16 切片拼回去必须逐位一致
        for s in ["hello", "中文测试", "🦊🎵", "a🦊b中文c", ""] {
            let w = encode_utf16_nul(s);
            assert_eq!(w.last(), Some(&0));
            assert_eq!(decode_utf16_bounded(&w), s);
        }
    }

    #[test]
    fn utf16_decode_stops_at_first_nul() {
        // CF_UNICODETEXT 按首个 NUL 截断，后面跟什么都不管
        let mut w = encode_utf16_nul("abc");
        w.extend_from_slice(&[0x0058, 0x0059]);
        assert_eq!(decode_utf16_bounded(&w), "abc");
        // 无 NUL：读到头（调用方按 GlobalSize 切片保证不越界）
        assert_eq!(decode_utf16_bounded(&[0x0041, 0x0042]), "AB");
    }

    #[test]
    fn cf_html_round_trips_with_cjk() {
        // 中文锁死「字节偏移不是字符偏移」：fragment 含多字节字符时
        // 按字符数算偏移会切错位置。
        let frag = "<p>中文🦊 mixed</p>";
        let data = build_cf_html(frag);
        assert_eq!(parse_cf_html(&data).as_deref(), Some(frag));
        // 头部数字确实是 8 位且单调
        let text = std::str::from_utf8(&data).unwrap();
        assert!(text.starts_with("Version:0.9\r\nStartHTML:"));
    }

    #[test]
    fn cf_html_parse_rejects_garbage() {
        assert_eq!(parse_cf_html(b""), None);
        assert_eq!(parse_cf_html(b"no markers here"), None);
        // 越界偏移
        assert_eq!(
            parse_cf_html(b"StartFragment:999\r\nEndFragment:1000\r\nabc"),
            None
        );
        // 起止倒置
        assert_eq!(
            parse_cf_html(b"StartFragment:5\r\nEndFragment:3\r\nabcdef"),
            None
        );
        // 非法 UTF-8 片段
        let mut bad = build_cf_html("ok");
        let len = bad.len();
        bad[len - 1] = 0xFF;
        bad[len - 2] = 0xFF;
        assert_eq!(parse_cf_html(&bad), None);
    }

    #[test]
    fn cf_html_parse_accepts_wide_offsets_and_lf() {
        // 10 位偏移的生产者真实存在；换行 \n 裸写也有。
        // 偏移手工数过：12+21+19+25+23=100（头）+12（<html><body>）=112 起。
        let data = b"Version:0.9\nStartHTML:0000000100\nEndHTML:0000000136\nStartFragment:0000000112\nEndFragment:0000000122\n<html><body>0123456789</body></html>";
        assert_eq!(parse_cf_html(data).as_deref(), Some("0123456789"));
    }

    #[test]
    fn desktop_stubs_do_not_panic() {
        // 非 Windows/安卓：读 None、写 Unsupported，不 panic。
        // （本机 Windows 跑此测试会走真实剪贴板，故只在 fallback 平台断言。）
        #[cfg(not(any(windows, target_os = "android")))]
        {
            assert_eq!(clipboard_text(), None);
            assert!(matches!(
                set_clipboard_text("x"),
                Err(VireoError::Unsupported(_))
            ));
            assert_eq!(clipboard_html(), None);
            assert!(matches!(
                set_clipboard_html("<b>x</b>", "x"),
                Err(VireoError::Unsupported(_))
            ));
        }
    }

    /// Windows 真机 roundtrip：写 → 读回。会短暂覆盖系统剪贴板，
    /// 所以进出都保存/恢复（空剪贴板则恢复成空串，近似无痕）。
    #[cfg(windows)]
    #[test]
    fn windows_clipboard_roundtrip() {
        let saved = clipboard_text();
        let probe = "vireo-clipboard-probe-🦊-中文";
        set_clipboard_text(probe).expect("set text");
        assert_eq!(clipboard_text().as_deref(), Some(probe));
        let html = "<p>hi <b>there</b> 中文</p>";
        set_clipboard_html(html, "hi there").expect("set html");
        assert_eq!(clipboard_html().as_deref(), Some(html));
        assert_eq!(clipboard_text().as_deref(), Some("hi there"));
        // 恢复现场
        match saved {
            Some(s) => {
                let _ = set_clipboard_text(&s);
            }
            None => {
                let _ = set_clipboard_text("");
            }
        }
    }
}
