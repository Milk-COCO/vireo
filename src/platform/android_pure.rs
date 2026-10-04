//! Android 平台 API 的**纯逻辑**部分（不含任何 JNI）。
//!
//! # 为什么单独一个文件
//!
//! `super::android` 整个被 `#[cfg(target_os = "android")]` 门控，所以它里面的
//! `#[cfg(test)] mod tests` 在桌面上**从未被执行过** —— 那些测试验证的全是
//! 纯计算（Insets 换算、bitmask、AOSP 常量对齐），根本不需要 JVM，却跟着模块
//! 一起被门控掉了，等于白写。
//!
//! 这个模块**无条件编译**，于是那些测试进入 `cargo test --lib`，在开发机上就能跑。
//! 参照同目录的 `super::android_text_input`（同一个动机）。
//!
//! # 分界标准
//!
//! 「这个 item 需要 JNI / Android 运行时吗？」不需要的都在这里。
//!
//! | 在这里（纯逻辑） | 在 `android.rs`（JNI） |
//! |---|---|
//! | `Insets` / `SystemBars` / 各枚举 + `to_android`/`from_android` | `read_insets_logical` / `post_to_java_thread` |
//! | `mod api`（AOSP 常量） | `JniErr` / `with_activity` / 所有 `env.call_method` |
//! | `SAFETY_MASK` / `to_logical` | `WindowExtAndroid` trait 与其实impl |
//!
//! 搬移是逐字节的，唯一改动是可见性：`android.rs` 里私有的 `to_android` 等
//! 搬过来后必须 `pub(crate)`，否则 JNI 那半边调不到。
//!
//! `super::android`: super::android
//! `super::android_text_input`: super::android_text_input

use crate::math::Rect;

// ---------------------------------------------------------------------------
// Android API level 与其它硬编码常量
// ---------------------------------------------------------------------------

pub(crate) mod api {
    // `WindowInsets.Type` 是 bitflag。这里用 u32 存，与 `SystemBars` 的位掩码一致，
    // 传给 JNI（`jint`）时在调用点 `as i32`。
    /// `WindowInsets.Type.statusBars()`
    pub const STATUS_BARS: u32 = 0x0000_0001;
    /// `WindowInsets.Type.navigationBars()`
    pub const NAVIGATION_BARS: u32 = 0x0000_0002;
    /// `WindowInsets.Type.captionBar()`
    pub const CAPTION_BAR: u32 = 0x0000_0004;
    /// `WindowInsets.Type.ime()`
    pub const IME: u32 = 0x0000_0100;
    /// `WindowInsets.Type.displayCutout()`
    pub const DISPLAY_CUTOUT: u32 = 0x0000_0400;

    /// `WindowInsetsController.BEHAVIOR_DEFAULT`
    pub const BEHAVIOR_DEFAULT: i32 = 0;
    /// `WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE`（API 30+）
    pub const BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE: i32 = 2;

    /// `WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON`
    pub const FLAG_KEEP_SCREEN_ON: i32 = 0x0000_0080;

    /// `WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_DEFAULT`（API 28+）
    pub const CUTOUT_MODE_DEFAULT: i32 = 0;
    /// `LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES`
    pub const CUTOUT_MODE_SHORT_EDGES: i32 = 1;
    /// `LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS`
    pub const CUTOUT_MODE_ALWAYS: i32 = 2;

    // `WindowManager.LayoutParams.type` —— 数值取自 AOSP `WindowManager.java`：
    // TYPE_APPLICATION = 2；
    // TYPE_APPLICATION_ATTACHED_DIALOG = FIRST_SUB_WINDOW(1000) + 3；
    // TYPE_SYSTEM_ALERT = FIRST_SYSTEM_WINDOW(2000) + 3；
    // TYPE_APPLICATION_OVERLAY = FIRST_SYSTEM_WINDOW(2000) + 38（API 26 新增）。
    /// `WindowManager.LayoutParams.TYPE_APPLICATION`（普通应用窗口；系统默认值）
    pub const TYPE_APPLICATION: i32 = 2;
    /// `TYPE_APPLICATION_ATTACHED_DIALOG`（附着对话框）
    pub const TYPE_APPLICATION_ATTACHED_DIALOG: i32 = 1003;
    /// `TYPE_SYSTEM_ALERT`（系统弹窗级；**API 26 起废弃**，官方要求改用
    /// [`TYPE_APPLICATION_OVERLAY`]）
    pub const TYPE_SYSTEM_ALERT: i32 = 2003;
    /// `TYPE_APPLICATION_OVERLAY`（悬浮窗，API 26+）。
    ///
    /// 需要 manifest 的 `android.permission.SYSTEM_ALERT_WINDOW`，**且**用户已在
    /// 「显示在其他应用上层」设置里授权；未授权时 `Window.setAttributes` 会抛
    /// `WindowManager.BadTokenException`。
    pub const TYPE_APPLICATION_OVERLAY: i32 = 2038;

    /// `ActivityInfo.SCREEN_ORIENTATION_*`
    pub const REQ_UNSPECIFIED: i32 = 0;
    pub const REQ_LANDSCAPE: i32 = 1;
    pub const REQ_PORTRAIT: i32 = 2;
    pub const REQ_USER: i32 = 3;
    pub const REQ_SENSOR: i32 = 4;
    pub const REQ_NOSENSOR: i32 = 5;
    pub const REQ_SENSOR_LANDSCAPE: i32 = 6;
    pub const REQ_SENSOR_PORTRAIT: i32 = 7;

    /// `Configuration.orientation`
    pub const CONFIGURATION_PORTRAIT: i32 = 1;
    pub const CONFIGURATION_LANDSCAPE: i32 = 2;

    /// `View.SYSTEM_UI_FLAG_HIDE_NAVIGATION`（legacy 分支，API 30+ 起废弃）
    pub const LEGACY_HIDE_NAVIGATION: i32 = 0x0000_0002;
    /// `View.SYSTEM_UI_FLAG_FULLSCREEN`
    pub const LEGACY_FULLSCREEN: i32 = 0x0000_0004;
    /// `View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY`
    pub const LEGACY_IMMERSIVE_STICKY: i32 = 0x0000_1000;
    /// `View.SYSTEM_UI_FLAG_LAYOUT_STABLE`
    pub const LEGACY_LAYOUT_STABLE: i32 = 0x0000_0100;
    /// `View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION`
    pub const LEGACY_LAYOUT_HIDE_NAVIGATION: i32 = 0x0000_0200;
    /// `View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN`
    pub const LEGACY_LAYOUT_FULLSCREEN: i32 = 0x0000_0400;

    /// `WindowManager.LayoutParams.layoutInDisplayCutoutMode` 引入版本
    pub const SDK_CUTOUT: i32 = 28;
    /// `Window.getInsetsController()` / `WindowInsets.Type` 引入版本
    pub const SDK_INSETS_CONTROLLER: i32 = 30;
    /// `Window.setDecorFitsSystemWindows` 引入版本
    pub const SDK_DECOR_FITS: i32 = 30;
}

// 公开类型（不含 JNI 类型）
// ---------------------------------------------------------------------------

/// 四边内缩量。
///
/// **单位 = vireo 逻辑像素（Dp）**，与 vireo 绘制坐标系一致，可直接喂给
/// [`crate::shapes`] 系列 API。需要物理像素时用 [`Insets::scaled`] 换算。
///
/// **坐标系：原点左上、y 向下**（与 vireo 一致）。注意 Unity 的 `Screen.safeArea`
/// 原点在左下角，是个著名的踩坑点。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Insets {
    /// 全零（无遮挡）。
    pub const ZERO: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };

    /// 四边等值内缩。
    pub const fn uniform(v: f32) -> Self {
        Self {
            left: v,
            top: v,
            right: v,
            bottom: v,
        }
    }

    /// 乘一个系数（物理 px ↔ 逻辑 px 换算，或做安全边距放大）。
    pub const fn scaled(self, factor: f32) -> Self {
        Self {
            left: self.left * factor,
            top: self.top * factor,
            right: self.right * factor,
            bottom: self.bottom * factor,
        }
    }

    /// 取四边最大值（HUD 统一留边常用）。
    pub fn max_edge(self) -> f32 {
        let mut m = self.left;
        if self.top > m {
            m = self.top;
        }
        if self.right > m {
            m = self.right;
        }
        if self.bottom > m {
            m = self.bottom;
        }
        m
    }

    /// 内缩后的安全矩形（给定窗口尺寸）。
    pub fn to_rect(self, width: f32, height: f32) -> Rect {
        Rect::new(
            self.left,
            self.top,
            (width - self.left - self.right).max(0.0),
            (height - self.top - self.bottom).max(0.0),
        )
    }
}

/// **要隐藏的**系统栏集合（bitmask）。
///
/// 对应 Android `WindowInsets.Type` 的 `statusBars()` / `navigationBars()` /
/// `captionBar()`。**故意用 bitmask 而非单值枚举**：「只隐藏状态栏、保留导航栏」
/// 是真实需求（Flutter 的 `manual` + `SystemUiOverlay` 列表就是干这个的）。
///
/// # 语义方向（别搞反）
///
/// 集合里的位 = **隐藏**，集合外的位 = **保持系统默认可见**。
///
/// | 值 | 效果 |
/// |---|---|
/// | [`NONE`](Self::NONE) | 一个都不隐藏 = **全部显示** |
/// | [`STATUS`](Self::STATUS) | 只隐藏状态栏，导航栏保留 |
/// | [`NAVIGATION`](Self::NAVIGATION) | 只隐藏导航栏 |
/// | [`ALL`](Self::ALL) | 全部隐藏（沉浸式） |
///
/// 想「全部隐藏」用 [`ALL`](Self::ALL)，**不是** [`NONE`](Self::NONE)。
///
/// `displayCutout` **不在**这个集合里——刘海是遮挡区不是系统栏，用
/// `WindowExtAndroid::safe_area` / `WindowExtAndroid::cutouts` 表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemBars(u32);

impl SystemBars {
    /// 空集：**不隐藏任何栏**，即两个栏都显示。
    ///
    /// 注意：想要「全部隐藏」请用 [`ALL`](Self::ALL)。
    pub const NONE: Self = Self(0);
    /// 状态栏。
    pub const STATUS: Self = Self(api::STATUS_BARS);
    /// 导航栏。
    pub const NAVIGATION: Self = Self(api::NAVIGATION_BARS);
    /// caption bar（桌面窗口化模式；即使开沉浸式也永远可见）。
    pub const CAPTION: Self = Self(api::CAPTION_BAR);
    /// 状态栏 + 导航栏（游戏最常用）。
    pub const STATUS_NAVIGATION: Self = Self(api::STATUS_BARS | api::NAVIGATION_BARS);
    /// 全部三类。
    pub const ALL: Self = Self(api::STATUS_BARS | api::NAVIGATION_BARS | api::CAPTION_BAR);

    /// 原始位掩码。
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// 位或组合。
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// 是否包含全部给定位。
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// 是否为空集。
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// 从 Android `WindowInsets.Type` 位掩码构造，未知位（IME / displayCutout）被过滤。
    pub fn from_android_bits(bits: i32) -> Self {
        Self((bits as u32) & (api::STATUS_BARS | api::NAVIGATION_BARS | api::CAPTION_BAR))
    }
}

impl core::ops::BitOr for SystemBars {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// 隐藏系统栏后，边缘滑动唤回时的行为。
///
/// 对应 Android `WindowInsetsController.setSystemBarsBehavior(int)`。
///
/// **刻意不提供** `BEHAVIOR_SHOW_BARS_BY_TOUCH` / `BEHAVIOR_SHOW_BARS_BY_SWIPE`：
/// 前者 API 30 废弃、后者 API 31+ 完全不支持。Flutter 仍保留这两个枚举值并在文档里
/// 反复写「你必须先迁移」——vireo 不做僵尸枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SystemBarBehavior {
    /// 瞬时栏覆盖内容，短暂停留后自动隐藏。（`BEHAVIOR_DEFAULT`）
    #[default]
    Transient,
    /// 边缘滑动显示，短暂停留后自动隐藏，**应用继续收到 touch**。
    /// （`BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE`，API 30+）
    ///
    /// 游戏应当选这个：手势不丢。
    ShowTransientBarsBySwipe,
}

impl SystemBarBehavior {
    pub(crate) fn to_android(self) -> i32 {
        match self {
            Self::Transient => api::BEHAVIOR_DEFAULT,
            Self::ShowTransientBarsBySwipe => api::BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE,
        }
    }
}

/// 内容是否延伸进摄像头挖孔区。
///
/// 对应 Android `WindowManager.LayoutParams.layoutInDisplayCutoutMode`（API 28+）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayCutoutMode {
    /// 内容自动避开挖孔（顶部整条下移）。
    #[default]
    Default,
    /// 铺满短边，挖孔区留黑。
    ShortEdges,
    /// 铺满整个屏，挖孔区留黑。
    Always,
}

impl DisplayCutoutMode {
    pub(crate) fn to_android(self) -> i32 {
        match self {
            Self::Default => api::CUTOUT_MODE_DEFAULT,
            Self::ShortEdges => api::CUTOUT_MODE_SHORT_EDGES,
            Self::Always => api::CUTOUT_MODE_ALWAYS,
        }
    }

    pub(crate) fn from_android(v: i32) -> Option<Self> {
        match v {
            api::CUTOUT_MODE_DEFAULT => Some(Self::Default),
            api::CUTOUT_MODE_SHORT_EDGES => Some(Self::ShortEdges),
            api::CUTOUT_MODE_ALWAYS => Some(Self::Always),
            _ => None,
        }
    }
}

/// 请求的屏幕朝向（`Activity.setRequestedOrientation`）。
///
/// 数字定义是 `ActivityInfo.SCREEN_ORIENTATION_*`，与 [`DisplayOrientation`]
/// （来自 `Configuration.orientation`）**编号不同**，别混。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// 未指定，沿用系统/清单。
    #[default]
    Unspecified,
    /// 固定横屏。
    Landscape,
    /// 固定竖屏。
    Portrait,
    /// 跟随用户的旋转锁。
    User,
    /// 跟随传感器。
    Sensor,
    /// 跟随传感器但忽略用户锁。
    NoSensor,
    /// 跟随传感器，横向（含反向）。
    SensorLandscape,
    /// 跟随传感器，纵向（含反向）。
    SensorPortrait,
}

impl Orientation {
    pub(crate) fn to_android(self) -> i32 {
        match self {
            Self::Unspecified => api::REQ_UNSPECIFIED,
            Self::Landscape => api::REQ_LANDSCAPE,
            Self::Portrait => api::REQ_PORTRAIT,
            Self::User => api::REQ_USER,
            Self::Sensor => api::REQ_SENSOR,
            Self::NoSensor => api::REQ_NOSENSOR,
            Self::SensorLandscape => api::REQ_SENSOR_LANDSCAPE,
            Self::SensorPortrait => api::REQ_SENSOR_PORTRAIT,
        }
    }
}

/// 当前实际显示朝向（来自 `Resources.getConfiguration().orientation`）。
///
/// 与 [`Orientation`] **刻意分开**：后者是「请求」，这个是「实际」。
/// Godot 的 `screen_get_orientation()` 返回项目设置值而不是运行时方向
/// （godot#114840），vireo 不重复这个错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayOrientation {
    #[default]
    Unknown,
    Portrait,
    Landscape,
}

/// 窗口层级（`WindowManager.LayoutParams.type`）。
///
/// # 权限
///
/// [`VireoWindowLevel::Overlay`] 需要 manifest 声明
/// `android.permission.SYSTEM_ALERT_WINDOW`，**且**用户已在系统「显示在其他应用
/// 上层」里授权。未授权时 `WindowExtAndroid::set_window_level` 会失败
/// （`WindowManager.BadTokenException`）——vireo 不代用户跳授权设置页。
///
/// # 与 winit `WindowLevel` 的关系
///
/// winit 的 `WindowLevel { Normal, AlwaysOnTop, AlwaysOnBottom, AbovePro }` 是
/// X11/Win32 概念，Android 没有「置底」这一档（`type` 越小越靠下，但低于
/// `TYPE_APPLICATION` 的都是系统保留窗口）。所以这里按 Android 的 `type`
/// 重新表达，不硬套 winit 的四档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VireoWindowLevel {
    /// 普通应用窗口（`TYPE_APPLICATION`）。系统默认值。
    #[default]
    Normal,
    /// 悬浮窗：盖在所有 Activity 窗口之上，但在状态栏/输入法之下
    /// （`TYPE_APPLICATION_OVERLAY`，API 26+）。**需权限，见上。**
    Overlay,
    /// 系统弹窗级（`TYPE_SYSTEM_ALERT`）。**API 26 起已废弃**，
    /// 官方文档要求非系统应用改用 [`VireoWindowLevel::Overlay`]；保留仅为兼容老系统。
    SystemAlert,
    /// 附着对话框（`TYPE_APPLICATION_ATTACHED_DIALOG`）：按顶层窗口布局，
    /// 而不是作为容器子窗口。
    AttachedDialog,
}

impl VireoWindowLevel {
    pub(crate) fn to_android(self) -> i32 {
        match self {
            Self::Normal => api::TYPE_APPLICATION,
            Self::Overlay => api::TYPE_APPLICATION_OVERLAY,
            Self::SystemAlert => api::TYPE_SYSTEM_ALERT,
            Self::AttachedDialog => api::TYPE_APPLICATION_ATTACHED_DIALOG,
        }
    }

    /// 未知 `type`（系统保留窗口 / 被别的库改过）→ `None`，由调用方决定回落。
    pub(crate) fn from_android(v: i32) -> Option<Self> {
        match v {
            api::TYPE_APPLICATION => Some(Self::Normal),
            api::TYPE_APPLICATION_OVERLAY => Some(Self::Overlay),
            api::TYPE_SYSTEM_ALERT => Some(Self::SystemAlert),
            api::TYPE_APPLICATION_ATTACHED_DIALOG => Some(Self::AttachedDialog),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 安全区 / 缩放换算
// ---------------------------------------------------------------------------

pub(crate) const SAFETY_MASK: u32 =
    api::STATUS_BARS | api::NAVIGATION_BARS | api::CAPTION_BAR | api::DISPLAY_CUTOUT;

pub(crate) fn to_logical(scale: f32, px: Insets) -> Insets {
    if scale > 0.0 {
        px.scaled(1.0 / scale)
    } else {
        px
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insets_defaults_and_helpers() {
        assert_eq!(Insets::default(), Insets::ZERO);
        assert_eq!(Insets::uniform(4.0).max_edge(), 4.0);
        assert_eq!(Insets::uniform(4.0).scaled(0.5).left, 2.0);
        let r = Insets::uniform(10.0).to_rect(100.0, 50.0);
        assert_eq!((r.x, r.y, r.w, r.h), (10.0, 10.0, 80.0, 30.0));
    }

    #[test]
    fn insets_to_rect_clamps_negative() {
        let r = Insets {
            left: 60.0,
            top: 0.0,
            right: 60.0,
            bottom: 0.0,
        }
        .to_rect(100.0, 100.0);
        assert_eq!(r.w, 0.0);
    }

    #[test]
    fn system_bars_is_bitmask() {
        assert!(SystemBars::NONE.is_empty());
        assert!(!SystemBars::STATUS.is_empty());
        assert_eq!(SystemBars::STATUS.union(SystemBars::NAVIGATION).bits(), 0x3);
        assert_eq!(
            SystemBars::STATUS | SystemBars::NAVIGATION,
            SystemBars::STATUS_NAVIGATION
        );
        assert!(SystemBars::ALL.contains(SystemBars::STATUS));
        assert!(!SystemBars::STATUS.contains(SystemBars::NAVIGATION));
        // IME / displayCutout 不是"系统栏"，必须被过滤
        assert_eq!(
            SystemBars::from_android_bits(0x100 | 0x400),
            SystemBars::NONE
        );
        assert_eq!(
            SystemBars::from_android_bits(0x1 | 0x400),
            SystemBars::STATUS
        );
    }

    #[test]
    fn behavior_and_cutout_android_codes() {
        assert_eq!(SystemBarBehavior::Transient.to_android(), 0);
        assert_eq!(SystemBarBehavior::ShowTransientBarsBySwipe.to_android(), 2);
        for m in [
            DisplayCutoutMode::Default,
            DisplayCutoutMode::ShortEdges,
            DisplayCutoutMode::Always,
        ] {
            assert_eq!(DisplayCutoutMode::from_android(m.to_android()), Some(m));
        }
        // 未知值必须映射成 None，不能默默当成某个值
        assert_eq!(DisplayCutoutMode::from_android(99), None);
    }

    #[test]
    fn safety_mask_includes_display_cutout() {
        // Android 文档：systemBars() **不**含 displayCutout()。安全区 mask 必须显式 or，
        // 否则刘海区在 safe_area / padding 里会消失（HUD 撞刘海）。
        assert_eq!(SAFETY_MASK, 0x1 | 0x2 | 0x4 | 0x400);
        assert_ne!(SAFETY_MASK & api::DISPLAY_CUTOUT, 0);
        // ⚠️ 这里是 `assert_eq!(.., 0)` 而不是 `assert_ne!(.., 0)`。
        //
        // 原先写成了 `assert_ne!` —— 断言的是「IME **在**安全区里」，与它自己的
        // 提示语（"IME 不应混进安全区"）正好相反。因为这个测试模块跟着
        // `cfg(android)` 门控、在桌面上从未执行过，所以这个反向断言一直没被发现。
        //
        // 生产代码一直是对的（`SAFETY_MASK = STATUS|NAVIGATION|CAPTION|CUTOUT`），
        // 只有断言写反了。
        assert_eq!(
            SAFETY_MASK & api::IME,
            0,
            "IME 不应混进安全区，走独立 getter"
        );
    }

    #[test]
    fn orientation_android_codes_match_activityinfo() {
        assert_eq!(Orientation::Unspecified.to_android(), 0);
        assert_eq!(Orientation::Landscape.to_android(), 1);
        assert_eq!(Orientation::Portrait.to_android(), 2);
        assert_eq!(Orientation::User.to_android(), 3);
        assert_eq!(Orientation::Sensor.to_android(), 4);
        assert_eq!(Orientation::NoSensor.to_android(), 5);
        assert_eq!(Orientation::SensorLandscape.to_android(), 6);
        assert_eq!(Orientation::SensorPortrait.to_android(), 7);
    }

    #[test]
    fn window_insets_type_bits_match_android() {
        assert_eq!(api::STATUS_BARS, 0x1);
        assert_eq!(api::NAVIGATION_BARS, 0x2);
        assert_eq!(api::CAPTION_BAR, 0x4);
        assert_eq!(api::IME, 0x100);
        assert_eq!(api::DISPLAY_CUTOUT, 0x400);
        // systemBars() 不含 ime / displayCutout —— 引擎界著名坑，必须显式 or
        assert_eq!(
            api::STATUS_BARS | api::NAVIGATION_BARS | api::CAPTION_BAR,
            0x7
        );
        assert_eq!(api::FLAG_KEEP_SCREEN_ON, 0x80);
        assert_eq!(api::BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE, 2);
    }

    #[test]
    fn window_level_android_codes_match_aosp() {
        // 数值直接对应 AOSP `WindowManager.java`，改错会静默变成系统保留窗口。
        assert_eq!(api::TYPE_APPLICATION, 2);
        // FIRST_SUB_WINDOW(1000) + 3
        assert_eq!(api::TYPE_APPLICATION_ATTACHED_DIALOG, 1003);
        // FIRST_SYSTEM_WINDOW(2000) + 3
        assert_eq!(api::TYPE_SYSTEM_ALERT, 2003);
        // FIRST_SYSTEM_WINDOW(2000) + 38
        assert_eq!(api::TYPE_APPLICATION_OVERLAY, 2038);

        // 四个档位与 android 常数一一对应，且 round-trip 无损
        for lvl in [
            VireoWindowLevel::Normal,
            VireoWindowLevel::Overlay,
            VireoWindowLevel::SystemAlert,
            VireoWindowLevel::AttachedDialog,
        ] {
            assert_eq!(VireoWindowLevel::from_android(lvl.to_android()), Some(lvl));
        }
        assert_eq!(VireoWindowLevel::default(), VireoWindowLevel::Normal);

        // 系统保留 type（如 TYPE_PHONE=2002 / TYPE_STATUS_BAR=2000）不应被误认
        assert_eq!(VireoWindowLevel::from_android(2002), None);
        assert_eq!(VireoWindowLevel::from_android(2000), None);
        assert_eq!(VireoWindowLevel::from_android(1), None); // TYPE_BASE_APPLICATION
    }

    #[test]
    fn to_logical_divides_by_scale() {
        let px = Insets::uniform(90.0);
        assert_eq!(to_logical(3.0, px), Insets::uniform(30.0));
        // scale 非法时原样返回，不产生 inf/NaN
        assert_eq!(to_logical(0.0, px), px);
        assert_eq!(to_logical(-1.0, px), px);
    }
}
