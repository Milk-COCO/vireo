//! Android 文本输入（IME）的数据模型与单位换算。
//!
//! # 为什么单独一个无条件编译的模块
//!
//! 本模块的换算逻辑是**纯函数**（UTF-16 ↔ UTF-8 下标），必须在桌面
//! `cargo test --lib` 里真能跑到 —— 若整体挂进 `#[cfg(target_os = "android")]`
//! 的 `platform::android`，单测就永远不会执行（本平台其他 cfg 门控模块都踩过
//! 这个盲区）。代价是桌面非 test 构建下部分项无人调用，故 `allow(dead_code)`。
//!
//! 由 `platform::android` re-export，非 Android 平台那边模块不存在。
//!
//! # 单位：为什么必须换算
//!
//! Android 的 `GameTextInputSpan.start/end` 是 **UTF-16 code unit** 下标
//! （AGDK 约定，emoji 占 2 个），而 vireo 的 `text: String` 是 UTF-8。
//!
//! `android-activity` **不做换算** —— 它只是把 UTF-16 下标拿去和
//! `str.len()`（UTF-8 **字节**长度）clamp 一遍再原样透传：
//!
//! ```text
//! let len = str.len();                                        // 字节长度
//! selection.start.clamp(0, len as i32 + 1)                     // 只是 clamp，没换算
//! ```
//!
//! （好消息：UTF-16 长度 ≤ UTF-8 字节长度，所以这个 clamp 不会破坏合法值，
//! 拿到的仍是原始 UTF-16 下标。）
//!
//! 所以**换算责任在 vireo**。不做的话，中文/emoji 场景下 selection 会指到
//! 字符中间，panic 或切出乱码 —— Slint 就在这上面崩过（issue #7203）。

use std::borrow::Cow;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Android `InputType` 位常量（逐个核实自 `android-activity` `src/input.rs`）。
mod api {
    /// `TYPE_CLASS_TEXT`
    pub const CLASS_TEXT: u32 = 1;
    /// `TYPE_CLASS_NUMBER`
    pub const CLASS_NUMBER: u32 = 2;
    /// `TYPE_TEXT_VARIATION_URI`
    pub const VARIATION_URI: u32 = 0x0000_0010;
    /// `TYPE_TEXT_VARIATION_EMAIL_ADDRESS`
    pub const VARIATION_EMAIL: u32 = 0x0000_0020;
    /// `TYPE_TEXT_VARIATION_PASSWORD`
    pub const VARIATION_PASSWORD: u32 = 0x0000_0080;
    /// `TYPE_TEXT_FLAG_AUTO_CORRECT`
    pub const FLAG_AUTO_CORRECT: u32 = 0x0000_8000;
    /// `TYPE_TEXT_FLAG_MULTI_LINE`
    pub const FLAG_MULTI_LINE: u32 = 0x0002_0000;
    /// `TYPE_TEXT_FLAG_NO_SUGGESTIONS`
    pub const FLAG_NO_SUGGESTIONS: u32 = 0x0008_0000;
    /// `TYPE_TEXT_FLAG_ENABLE_TEXT_CONVERSION_SUGGESTIONS`
    ///
    /// **中文拼音的关键 flag**：告诉输入法「需要提供转写候选（拼音 → 汉字）」。
    /// 缺了它Gboard / 搜狗可能不给候选。
    pub const FLAG_ENABLE_CONVERSION_SUGGESTIONS: u32 = 0x0010_0000;

    /// `EditorInfo.IME_FLAG_NO_FULLSCREEN`（0x02000000）
    pub const FLAG_NO_FULLSCREEN: u32 = 0x0200_0000;
    /// `EditorInfo.IME_FLAG_NO_ACCESSORY_ACTION`（0x20000000）
    pub const FLAG_NO_ACCESSORY_ACTION: u32 = 0x2000_0000;
    /// `EditorInfo.IME_FLAG_NO_EXTRACT_UI`（0x10000000）
    ///
    /// AOSP 文档说它「可能被弃用，推荐改用 `NO_FULLSCREEN`」，但两者语义**不等价**：
    /// `NO_FULLSCREEN` 是「请求不要进全屏模式」，`NO_EXTRACT_UI` 是「不要显示
    /// extracted-text UI」。当输入法**无视** `NO_FULLSCREEN` 时（实测 HyperOS 搜狗
    /// 横屏就是），只有 `NO_EXTRACT_UI` 还能直接把那块白条关掉。
    pub const FLAG_NO_EXTRACT_UI: u32 = 0x1000_0000;
}

/// vireo 固定下发的 `imeOptions` 位。
///
/// # 为什么必须显式带 `NO_FULLSCREEN`（踩过的坑）
///
/// AGSK `GameActivity.getImeEditorInfo()` 的默认值是
/// `IME_ACTION_DONE | IME_FLAG_NO_FULLSCREEN`，而
/// `GameActivity_setImeEditorInfoFields` 对 `imeOptions` 是**整体替换**、不是按位或。
/// 所以一旦我们传空 options，`NO_FULLSCREEN` 就被抹掉了，后果是**三条一起发生**：
///
/// 1. 输入法在**横屏**下进入 AOSP 的 **fullscreen extract mode**，屏幕顶部弹出
///    一条全宽白条 `input_method_extract_view`（左侧 `ExtractEditText` + 右侧
///    `ExtractButton`）。
/// 2. 按钮上的「执行」是**框架字符串**
///    `com.android.internal.R.string.ime_action_default`（英文 "Execute"，
///    `values-zh-rCN` = 「执行」）—— 属于 `com.android.internal`，既不是 MIUI
///    也不是输入法画的。这正是本条 flag 缺失的**可观测特征**。
/// 3. 该模式下输入法把 IME 窗口设成 `MATCH_PARENT` 并吃掉整屏，框架注释直言
///    *"In fullscreen mode, we never resize the underlying window"* → 应用侧
///    `ime()` inset 恒为 `0`，manifest 里的 `adjustResize` 形同虚设。
///
/// # 为什么还要 `NO_ACCESSORY_ACTION`
///
/// vireo 自绘输入 UI，不需要输入法自带的功能按钮（那也是「执行」的来源）。
///
/// 注意：`IME_FLAG_NO_FULLSCREEN` 的文档明确写着「不是保证，某些输入法可能忽略」，
/// 所以它是 best-effort —— 若某个输入法无视它，仍可能出现 extract 条。
///
/// # 为什么还要 `NO_EXTRACT_UI`（真机实测后补）
///
/// HyperOS + 搜狗横屏实测：**无视** `NO_FULLSCREEN`，照样进 extract mode，
/// 顶部弹全宽白条 + 框架「完成」按钮，且 IME 窗口 `MATCH_PARENT` 吃掉整屏
/// → `ime()` inset 恒为 0、manifest 的 `adjustResize` 形同虚设
/// （可见区只剩顶部 128 物理像素）。加上 `NO_EXTRACT_UI` 后白条消失。
///
/// 两个 flag 语义不等价，不能互相替代：`NO_FULLSCREEN` 是「别进全屏模式」，
/// `NO_EXTRACT_UI` 是「别显示 extracted-text UI」。前者被无视时只有后者管用。
pub const IME_OPTIONS: u32 =
    api::FLAG_NO_FULLSCREEN | api::FLAG_NO_EXTRACT_UI | api::FLAG_NO_ACCESSORY_ACTION;

// ---------------------------------------------------------------------------
// 下标换算（纯函数）
// ---------------------------------------------------------------------------

/// UTF-16 code unit 下标 → UTF-8 字节下标。
///
/// - `utf16_idx` 超出文本 UTF-16 长度 → 返回 `text.len()`。
/// - `utf16_idx` 落在某个字符内部（含代理对中间）→ **向后**取到该字符**之前**的
///   边界。向后而非向前：向前会把一个尚未提交给 IME 的字符算进来，
///   可能把 preedit 的范围悄悄扩大。
///
/// 与 [`byte_to_utf16`] 在合法边界上互为逆运算；落在字符内部时
/// `byte_to_utf16(utf16_to_byte(i)) <= i`（只收缩，不放大）。
pub fn utf16_to_byte(text: &str, utf16_idx: usize) -> usize {
    let mut u16s = 0usize;
    let mut prev = 0usize;
    for (b, ch) in text.char_indices() {
        if u16s >= utf16_idx {
            // `u16s` 是当前字符**起点**的 UTF-16 下标。
            return if u16s == utf16_idx { b } else { prev };
        }
        prev = b;
        u16s += ch.len_utf16();
    }
    // 走完 = 目标在文本末尾之后（或恰好末尾）
    text.len()
}

/// UTF-8 字节下标 → UTF-16 code unit 下标（喂回 IME 用）。
///
/// - `byte_idx` 超出文本长度 → 返回文本的 UTF-16 总长。
/// - `byte_idx` 不在 `char` 边界上 → **向后**取到最近的不超过它的边界。
pub fn byte_to_utf16(text: &str, byte_idx: usize) -> usize {
    let mut u16s = 0usize;
    let mut last = 0usize;
    for (b, ch) in text.char_indices() {
        if b > byte_idx {
            return last;
        }
        last = u16s;
        u16s += ch.len_utf16();
    }
    // 落在最后一个字符内部 → `last`（该字符起点的下标）；
    // 超出文本长度 → 末尾的 UTF-16 下标。
    if byte_idx >= text.len() { u16s } else { last }
}

// ---------------------------------------------------------------------------
// 数据模型
// ---------------------------------------------------------------------------

/// 字节下标向下取到最近的 `char` 边界（防御 UTF-16 换算落在字符中间）。
fn floor_boundary(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// 字节下标向上取到最近的 `char` 边界。
fn ceil_boundary(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while b < text.len() && !text.is_char_boundary(b) {
        b += 1;
    }
    b
}

/// 文本区间。**下标单位是 UTF-8 字节**（可直接用于 `&str` 切片）。
///
/// 半开区间 `[start, end)`；`start == end` 表示光标（无选中）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextSpan {
    /// 起始字节下标（含）。
    pub start: usize,
    /// 结束字节下标（不含）。
    pub end: usize,
}

impl TextSpan {
    /// 构造并把两端夹到 `[0, byte_len]`；`start > end` 时**交换**两端。
    pub fn new(start: usize, end: usize, byte_len: usize) -> Self {
        let (a, b) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let a = a.min(byte_len);
        let b = b.min(byte_len);
        Self {
            start: a.min(b),
            end: b,
        }
    }

    /// 长度（字节）。
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// 是否为空（光标态或零宽组合区）。
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// 从 `text` 切出该区间；越界/非字符边界自动收缩（不会 panic）。
    pub fn slice<'a>(&self, text: &'a str) -> &'a str {
        let s = floor_boundary(text, self.start);
        let e = ceil_boundary(text, self.end).max(s);
        &text[s..e]
    }
}

/// 一次 IME 状态快照。
///
/// 由 `WindowExtAndroid::text_input_state`
/// 每帧轮询取得（**不是**事件驱动 —— Android 的 `MainEvent` 里没有文本输入通知）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInputState {
    /// 完整文本（UTF-8）。
    pub text: String,
    /// 光标 / 选中区。
    pub selection: TextSpan,
    /// **拼音等预编辑（composing）区**；`None` = 当前没有预编辑。
    ///
    /// 中文输入法打字过程中这里覆盖拼音串（如 `ni` `hao`），选词完成后会
    /// 变成 `None` 且 `text` 里已是汉字。
    pub compose: Option<TextSpan>,
}

impl TextInputState {
    /// 预编辑子串（拼音串）；无 composing 时 `None`。
    pub fn preedit(&self) -> Option<&str> {
        self.compose.map(|c| c.slice(&self.text))
    }

    /// 已提交文本（去掉预编辑区）。
    ///
    /// 无预编辑时**零拷贝**（`Cow::Borrowed`）；有预编辑时才拼一次（`Cow::Owned`）。
    pub fn committed(&self) -> Cow<'_, str> {
        match self.compose {
            None => Cow::Borrowed(&self.text),
            Some(c) if c.is_empty() => Cow::Borrowed(&self.text),
            Some(c) => {
                let head = floor_boundary(&self.text, c.start);
                let tail = ceil_boundary(&self.text, c.end);
                let mut out = String::with_capacity(self.text.len());
                out.push_str(&self.text[..head]);
                out.push_str(&self.text[tail..]);
                Cow::Owned(out)
            }
        }
    }

    /// 光标位置（UTF-8 字节下标）。无选中时取 `selection.start`。
    pub fn cursor(&self) -> usize {
        self.selection.start.min(self.text.len())
    }

    /// 内部用：把 android-activity 给的 UTF-16 下标状态转成 UTF-8 字节下标。
    ///
    /// `selection`/`compose` 是 Android 的 **UTF-16 code unit** 下标
    /// （见模块文档「单位」一节）。
    pub(crate) fn from_android_utf16(
        text: String,
        sel_start: usize,
        sel_end: usize,
        comp: Option<(usize, usize)>,
    ) -> Self {
        let byte_len = text.len();
        let selection = TextSpan::new(
            utf16_to_byte(&text, sel_start),
            utf16_to_byte(&text, sel_end),
            byte_len,
        );
        // Android 用 start<0 表达「无组合区」，android-activity 已转成 `None`。
        let compose = comp.and_then(|(s, e)| {
            let s = utf16_to_byte(&text, s);
            let e = utf16_to_byte(&text, e);
            if s >= e {
                None // 零宽组合区 == 无组合区（与 android-activity 的语义一致）
            } else {
                Some(TextSpan::new(s, e, byte_len))
            }
        });
        Self {
            text,
            selection,
            compose,
        }
    }

    /// 内部用：转回 android-activity 要的 UTF-16 下标状态。
    pub(crate) fn to_android_utf16(&self) -> (String, usize, usize, Option<(usize, usize)>) {
        (
            self.text.clone(),
            byte_to_utf16(&self.text, self.selection.start),
            byte_to_utf16(&self.text, self.selection.end),
            self.compose.map(|c| {
                (
                    byte_to_utf16(&self.text, c.start),
                    byte_to_utf16(&self.text, c.end),
                )
            }),
        )
    }
}

/// 输入法对「这里该输入什么」的期望（`EditorInfo.inputType`）。
///
/// # 为什么叫 `InputType` 而不是 `ImePurpose`
///
/// winit 已有 `ImePurpose`（且在 prelude 里，`Window::set_ime_purpose` 用它）。
/// 本类型**故意不同名**：① 同名会让 `use vireo::prelude::*` 后产生歧义；
/// ② 本模块的命名约定是**镜像 Android 自己的词汇**
/// （`SystemBars`↔`WindowInsets.Type`、`Insets`、`Orientation`…），
/// 而 Android 侧这个东西就叫 `inputType`。两者的档位也刻意不同 ——
/// Android 的 `inputType` 是位域，能表达「密码 + 不要联想」这类组合，桌面没有这种表达力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputType {
    /// 普通单行文本。默认档，**带拼音转写候选**（中文可用）。
    #[default]
    Text,
    /// 多行文本（输入法显示换行键）。
    MultiLine,
    /// 密码：隐藏输入，且关掉联想与自动纠错。
    Password,
    /// 邮箱地址。
    Email,
    /// URL。
    Url,
    /// 纯数字。
    Number,
}

impl InputType {
    /// 转成 Android `InputType` 位值。
    pub(crate) fn to_android(self) -> u32 {
        match self {
            // ENABLE_TEXT_CONVERSION_SUGGESTIONS 是中文能出候选的关键；
            // AUTO_CORRECT 让输入法对拼错的英文纠错。
            Self::Text => {
                api::CLASS_TEXT | api::FLAG_ENABLE_CONVERSION_SUGGESTIONS | api::FLAG_AUTO_CORRECT
            }
            Self::MultiLine => {
                api::CLASS_TEXT
                    | api::FLAG_ENABLE_CONVERSION_SUGGESTIONS
                    | api::FLAG_AUTO_CORRECT
                    | api::FLAG_MULTI_LINE
            }
            // 密码绝不能开联想/纠错/转写候选。
            Self::Password => api::CLASS_TEXT | api::VARIATION_PASSWORD | api::FLAG_NO_SUGGESTIONS,
            Self::Email => api::CLASS_TEXT | api::VARIATION_EMAIL,
            Self::Url => api::CLASS_TEXT | api::VARIATION_URI,
            Self::Number => api::CLASS_NUMBER,
        }
    }
}

// ---------------------------------------------------------------------------
// 软键盘显隐
// ---------------------------------------------------------------------------

// 「用户已请求显示键盘」+ 「上次请求时刻」打包在一个原子量里：
// bit0 = want，bit8.. = 毫秒时间戳（相对进程内某个固定基准）。
//
// 为什么要去重：`show_soft_input` / `hide_soft_input` 最终都要 post 到 **Java 主线程**。
// 在帧回调里每帧无条件调用会用 ImeTracker / handler 队列把主线程淹掉——
// 实测模板每帧 `hide_soft_input()` 直接 `Input dispatching timed out`
// （`Waited 5000ms for FocusEvent(hasFocus=false)`）被系统 ANR 杀掉。
static IME_STATE: AtomicU64 = AtomicU64::new(0);

/// 同一请求被抑制的时间戳窗口（仅用于诊断 / 将来做节流，当前不做周期性重发）。
pub(crate) const IME_DEDUPE_WINDOW_MS: u64 = 200;

fn now_ms() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 返回**这次请求是否需要真正下发到 Java 主线程**（纯跳变去重）。
///
/// 只在 `want` 与上次不同才返回 `true`。`show_soft_input` / `hide_soft_input` 都用
/// 这一条：帧回调里每帧无条件调用会用 ImeTracker / handler 队列把 Java 主线程淹掉
/// —— 实测模板每帧 `hide_soft_input()` 直接 `Input dispatching timed out`
/// （`Waited 5000ms for FocusEvent(hasFocus=false)`）被系统 ANR 杀掉。
///
/// # 为什么**不能**加「周期性重发」兜底
///
/// 曾为「用户按返回键收起键盘 → `want` 陈旧 → 键盘再也弹不出」加过 200ms 重发窗口，
/// 结果是**每 200ms 就重新 post 一次 `show`**。输入法每次收到 show 都会重置组合态，
/// 真机症状：① 输入字符时候选词和 preedit 一闪即消；② 按输入法的收起按钮会在
/// 200ms 内被重新拉起，用户按不掉（除非点到 vireo 其他区域让 `want` 变 false）。
///
/// 组合态是**易碎**的：显示动作本身就会清掉它。因此这里只做跳变去重，
/// 「键盘被系统收起后如何恢复」交给调用方显式重置（见 `WindowExtAndroid::show_soft_input`
/// 文档），而不是靠引擎周期性骚扰输入法。
pub(crate) fn set_ime_wanted(want: bool) -> bool {
    let now = now_ms();
    let prev = IME_STATE.load(Ordering::Acquire);
    let prev_want = prev & 1 != 0;
    if prev_want == want {
        return false;
    }
    IME_STATE.store((now << 8) | u64::from(want), Ordering::Release);
    true
}

/// 强制把「用户是否请求显示键盘」重置为 `false`。
///
/// 仅供测试构造「陈旧意图」场景；生产路径用
/// [`WindowExtAndroid::hide_soft_input`]（它会把 `want` 打成 false）代替。
#[cfg(test)]
pub(crate) fn reset_ime_wanted() {
    IME_STATE.store(now_ms() << 8, Ordering::Release);
}

/// 应用是否请求过显示软键盘（「请求值」，不代表 IME 真的在显示）。
pub fn ime_wanted() -> bool {
    IME_STATE.load(Ordering::Acquire) & 1 != 0
}

// ---------------------------------------------------------------------------
// 单测
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 下标换算 ----

    #[test]
    fn ascii_roundtrip_is_identity() {
        let t = "hello";
        for i in 0..=5 {
            assert_eq!(utf16_to_byte(t, i), i);
            assert_eq!(byte_to_utf16(t, i), i);
        }
    }

    #[test]
    fn cjk_utf16_to_byte() {
        // 「你好」= 6 UTF-8 字节、2 个 UTF-16 unit
        let t = "你好";
        assert_eq!(t.len(), 6);
        assert_eq!(utf16_to_byte(t, 0), 0);
        assert_eq!(utf16_to_byte(t, 1), 3, "第 1 个 UTF-16 unit 落在『你』之后");
        assert_eq!(utf16_to_byte(t, 2), 6);
        assert_eq!(utf16_to_byte(t, 99), 6, "越界夹到末尾");
        assert_eq!(byte_to_utf16(t, 3), 1);
        assert_eq!(byte_to_utf16(t, 6), 2);
    }

    #[test]
    fn emoji_surrogate_pair_is_two_utf16_units() {
        // "a😀b"：UTF-8 = 1+4+1 = 6 字节；UTF-16 = 1+2+1 = 4 unit
        // UTF-16 下标：0='a'前 1='😀'前 2..3='😀'占2 个 3='b'前 4=末尾
        let t = "a😀b";
        assert_eq!(t.len(), 6);
        assert_eq!(utf16_to_byte(t, 0), 0);
        assert_eq!(utf16_to_byte(t, 1), 1, "a 之后 = 😀 之前");
        assert_eq!(utf16_to_byte(t, 3), 5, "😀 之后 = b 之前（byte 5）");
        assert_eq!(utf16_to_byte(t, 4), 6, "末尾");
        assert_eq!(byte_to_utf16(t, 0), 0);
        assert_eq!(byte_to_utf16(t, 1), 1);
        assert_eq!(byte_to_utf16(t, 5), 3, "b 的起点 = UTF-16 下标 3");
        assert_eq!(byte_to_utf16(t, 6), 4);
    }

    #[test]
    fn utf16_idx_inside_surrogate_pair_snaps_backward() {
        let t = "a😀b";
        // 下标 2 落在代理对中间（😀 占 UTF-16 1..3）→ 向后取到 byte 1（不把 😀 算进来）
        let b = utf16_to_byte(t, 2);
        assert!(t.is_char_boundary(b), "结果 {b} 必须是 char 边界");
        assert_eq!(b, 1);
        // 反向同理：byte 2..4 都在 😀 内部 → 都取到 byte 1
        for byte in [2usize, 3, 4] {
            assert_eq!(byte_to_utf16(t, byte), 1, "byte {byte} 在 😀 内部");
        }
    }

    #[test]
    fn byte_idx_inside_char_snaps_down() {
        let t = "你好";
        // 字节 1、2 都在『你』(0..3) 内部 → 向下取到 0
        assert_eq!(byte_to_utf16(t, 0), 0);
        assert_eq!(byte_to_utf16(t, 1), 0);
        assert_eq!(byte_to_utf16(t, 2), 0);
        assert_eq!(byte_to_utf16(t, 3), 1);
    }

    #[test]
    fn mixed_cjk_emoji_fuzz_roundtrip() {
        // 在混合文本的每个 char 边界上，byte↔utf16 必须互为逆运算
        let t = "a你b😀中文c👨‍👩‍👧d";
        let mut u16_idx = 0usize;
        for (byte_idx, ch) in t.char_indices() {
            assert_eq!(utf16_to_byte(t, u16_idx), byte_idx, "char {ch:?}");
            assert_eq!(byte_to_utf16(t, byte_idx), u16_idx, "char {ch:?}");
            u16_idx += ch.len_utf16();
        }
        assert_eq!(utf16_to_byte(t, u16_idx), t.len());
        assert_eq!(byte_to_utf16(t, t.len()), u16_idx);
    }

    #[test]
    fn empty_text() {
        assert_eq!(utf16_to_byte("", 0), 0);
        assert_eq!(utf16_to_byte("", 5), 0);
        assert_eq!(byte_to_utf16("", 0), 0);
        assert_eq!(byte_to_utf16("", 5), 0);
    }

    // ---- TextSpan ----

    #[test]
    fn span_clamps_and_orders() {
        let s = TextSpan::new(5, 2, 10);
        assert_eq!((s.start, s.end), (2, 5), "start>end 时交换");
        let s = TextSpan::new(0, 999, 10);
        assert_eq!((s.start, s.end), (0, 10), "超出 text 长度时夹住");
        assert_eq!(s.len(), 10);
        assert!(!s.is_empty());
        assert!(TextSpan::new(3, 3, 10).is_empty());
    }

    #[test]
    fn span_slice_never_panics_on_bad_bounds() {
        let t = "你好世界";
        assert_eq!(TextSpan::new(0, 3, t.len()).slice(t), "你");
        assert_eq!(TextSpan::new(3, 6, t.len()).slice(t), "好");
        // 非 char 边界的下标：不能 panic
        let _ = TextSpan::new(1, 2, t.len()).slice(t);
        let _ = TextSpan::new(0, 9999, t.len()).slice(t);
    }

    // ---- TextInputState ----

    #[test]
    fn state_from_android_utf16_cjk() {
        // IME 说：文本「你好」，光标在第 2 个 UTF-16 unit（即末尾），无组合区
        let s = TextInputState::from_android_utf16("你好".into(), 2, 2, None);
        assert_eq!(s.cursor(), 6, "应换算成 UTF-8 字节 6");
        assert_eq!(s.preedit(), None);
        assert_eq!(s.committed(), "你好");
    }

    #[test]
    fn state_from_android_utf16_with_compose() {
        // 拼音输入中：文本「ni好」（UTF-8 5 字节 / UTF-16 3 unit），
        // 组合区覆盖 UTF-16 [0,2)（即「ni」），光标在末尾 UTF-16 3
        let s = TextInputState::from_android_utf16("ni好".into(), 3, 3, Some((0, 2)));
        assert_eq!(s.preedit(), Some("ni"));
        assert_eq!(s.committed(), "好");
        assert_eq!(s.cursor(), 5, "『ni』(2) + 『好』(3) = 5 字节");
    }

    #[test]
    fn state_from_android_utf16_compose_with_cjk_inside() {
        // 组合区本身含非 ASCII：UTF-16 [0,1) 落在 3 字节的『你』上
        let s = TextInputState::from_android_utf16("你a".into(), 2, 2, Some((0, 1)));
        assert_eq!(s.preedit(), Some("你"));
        assert_eq!(s.committed(), "a");
    }

    #[test]
    fn zero_width_compose_is_treated_as_none() {
        let s = TextInputState::from_android_utf16("abc".into(), 1, 1, Some((1, 1)));
        assert_eq!(s.preedit(), None, "零宽组合区 == 无组合区");
        assert_eq!(s.committed(), "abc");
    }

    #[test]
    fn state_roundtrip_through_utf16() {
        let s = TextInputState::from_android_utf16("a😀b".into(), 3, 3, None);
        let (text, ss, se, comp) = s.to_android_utf16();
        assert_eq!(text, "a😀b");
        assert_eq!((ss, se), (3, 3), "UTF-16 下标应还原为 3");
        assert_eq!(comp, None);
        // 再转回来应得同一状态
        let s2 = TextInputState::from_android_utf16(text, ss, se, comp);
        assert_eq!(s, s2);
    }

    #[test]
    fn committed_never_loses_characters() {
        // 「你a好」= 7 字节 / 3 个 UTF-16 unit。
        // 组合区覆盖中间的 'a'：UTF-16 1..2（你占 0..1，a 占 1..2）
        let t = "你a好";
        let s = TextInputState::from_android_utf16(t.into(), 3, 3, Some((1, 2)));
        assert_eq!(s.preedit(), Some("a"));
        assert_eq!(s.cursor(), 7, "光标在末尾 = 7 字节");
        let c = s.committed();
        assert_eq!(c, "你好");
        assert_eq!(c.len() + s.preedit().unwrap().len(), t.len(), "不能丢字符");
    }

    #[test]
    fn committed_is_borrowed_when_no_compose() {
        // 无预编辑时零拷贝
        let s = TextInputState::from_android_utf16("abc".into(), 1, 1, None);
        assert!(matches!(s.committed(), Cow::Borrowed(_)));
        let s = TextInputState::from_android_utf16("abc".into(), 1, 1, Some((1, 1)));
        assert!(
            matches!(s.committed(), Cow::Borrowed(_)),
            "零宽组合区视同无组合区，应零拷贝"
        );
        let s = TextInputState::from_android_utf16("abcd".into(), 2, 2, Some((1, 3)));
        assert!(matches!(s.committed(), Cow::Owned(_)));
    }

    // ---- InputType ----

    #[test]
    fn input_type_bits_match_android() {
        // 与 android-activity src/input.rs 的常量对齐
        assert_eq!(InputType::Text.to_android(), 1 | 0x0010_0000 | 0x0000_8000);
        assert_eq!(
            InputType::MultiLine.to_android(),
            1 | 0x0010_0000 | 0x0000_8000 | 0x0002_0000
        );
        assert_eq!(InputType::Password.to_android(), 1 | 0x80 | 0x0008_0000);
        assert_eq!(InputType::Email.to_android(), 1 | 0x20);
        assert_eq!(InputType::Url.to_android(), 1 | 0x10);
        assert_eq!(InputType::Number.to_android(), 2);
        assert_eq!(InputType::default(), InputType::Text);
    }

    #[test]
    fn ime_purpose_text_enables_cjk_conversion() {
        // 中文能出候选的前提：TEXT + FLAG_ENABLE_TEXT_CONVERSION_SUGGESTIONS
        let bits = InputType::Text.to_android();
        assert_eq!(bits & 1, 1, "TYPE_CLASS_TEXT");
        assert_ne!(bits & 0x0010_0000, 0, "必须开转写候选，否则拼音不出候选");
    }

    #[test]
    fn ime_options_keep_no_fullscreen_flag() {
        // 回归测试：曾因传 `ImeOptions::empty()` 把 AGSK 默认的
        // `IME_FLAG_NO_FULLSCREEN` 抹掉，导致输入法在横屏进入 AOSP fullscreen
        // extract mode（顶部全宽白条 + 框架字符串「执行」按钮），
        // 且 `ime()` inset 恒为 0、manifest 的 adjustResize 失效。
        // 详见 `IME_OPTIONS` 文档。
        assert_ne!(
            IME_OPTIONS & api::FLAG_NO_FULLSCREEN,
            0,
            "绝不能丢 IME_FLAG_NO_FULLSCREEN，否则横屏弹 extract 条"
        );
        assert_eq!(api::FLAG_NO_FULLSCREEN, 0x0200_0000);
        assert_ne!(
            IME_OPTIONS & api::FLAG_NO_EXTRACT_UI,
            0,
            "还必须带 IME_FLAG_NO_EXTRACT_UI —— HyperOS 搜狗横屏实测无视 \
             NO_FULLSCREEN，只认这个才关得掉 extract 白条"
        );
        assert_eq!(api::FLAG_NO_EXTRACT_UI, 0x1000_0000);
        assert_ne!(
            IME_OPTIONS & api::FLAG_NO_ACCESSORY_ACTION,
            0,
            "vireo 自绘输入 UI，不需要输入法自带的功能按钮（「执行」的来源）"
        );
        // 低 8 位是 IME_MASK_ACTION，必须留 0（actionId 走独立通道），
        // 否则会启用 extract 条右侧的 action 按钮
        assert_eq!(IME_OPTIONS & 0x0000_00ff, 0);
    }

    #[test]
    fn ime_purpose_password_has_no_suggestions() {
        // 密码绝不能开联想 / 纠错 / 转写候选
        let bits = InputType::Password.to_android();
        assert_eq!(bits & 0x80, 0x80, "TYPE_TEXT_VARIATION_PASSWORD");
        assert_ne!(bits & 0x0008_0000, 0, "NO_SUGGESTIONS");
        assert_eq!(bits & 0x0010_0000, 0, "绝不能开转写候选");
        assert_eq!(bits & 0x0000_8000, 0, "绝不能开自动纠错");
    }

    #[test]
    fn ime_requests_dedupe_on_transition() {
        // `show_soft_input` / `hide_soft_input` 靠这个返回值决定要不要 post 到
        // Java 主线程。帧回调里每帧无条件调用会淹掉主线程 → ANR
        //（实测 `Waited 5000ms for FocusEvent(hasFocus=false)`）。
        set_ime_wanted(false); // 建立基线，避免与全局状态互相影响

        // 纯跳变去重（`hide` 走这条）：相同意图永不重复 post。
        assert!(!set_ime_wanted(false), "初始已是隐藏意图，不应下发");
        assert!(set_ime_wanted(true), "false→true 是跳变");
        assert!(!set_ime_wanted(true), "重复 true（hide 语义）必须去重");
        assert!(ime_wanted(), "请求值应被记住");
        assert!(set_ime_wanted(false), "true→false 是跳变");
        assert!(!ime_wanted());
        assert!(!set_ime_wanted(false), "回到隐藏后再次抑制");
    }

    #[test]
    fn ime_request_is_never_repeated_while_state_unchanged() {
        // 回归：曾用 200ms 窗口做「周期性重发」，导致输入法每 200ms 重置一次组合态
        // ——真机症状是候选词 / preedit 一闪即消、且用户按不掉输入法收起键。
        // 组合态易碎，**任何重复 show 都是破坏**，所以必须是纯跳变去重：
        // 同状态无论过多久都返回 false。
        set_ime_wanted(false); // 基线
        assert!(set_ime_wanted(true), "false→true 下发");
        assert!(!set_ime_wanted(true), "立刻重复：必须抑制");
        assert!(!set_ime_wanted(true), "下一帧重复：必须抑制");

        // 时间流逝也不能让重复请求重新下发（不存在周期性重发）。
        std::thread::sleep(std::time::Duration::from_millis(IME_DEDUPE_WINDOW_MS + 20));
        assert!(
            !set_ime_wanted(true),
            "超过窗口也**不得**重新下发 —— 周期性重发会清掉 preedit / 候选词"
        );

        assert!(set_ime_wanted(false), "true→false 下发");
    }

    #[test]
    fn reset_ime_wanted_allows_show_again_after_user_dismissed() {
        // 用户用系统收起键把键盘收了：我们观测不到，`want` 陈旧为 true。
        // 正确恢复 = reset + show，而不是靠引擎周期性重发去骚扰输入法。
        set_ime_wanted(false);
        assert!(set_ime_wanted(true));
        assert!(!set_ime_wanted(true), "陈旧状态下重复 show 被跳过");

        reset_ime_wanted();
        assert!(!ime_wanted(), "reset 后请求值应为 false（键盘确实没显示）");
        assert!(
            set_ime_wanted(true),
            "reset 后 show 必须能真正下发，否则键盘再也弹不出来"
        );
    }
}
