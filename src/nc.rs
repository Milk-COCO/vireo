//! 非客户区 hit-test 类型（`WM_NCHITTEST` 声明式 + 命令式）。
//!
//! 设计参考 Electron WCO：按钮外观画在**客户端**（wgpu 渲染），`WM_NCHITTEST`
//! 返回 `HT*` 让 Windows 自动接管**交互行为**（snap layout / 双击最大化 /
//! 右键系统菜单 / 按钮点击 / Aero Snap）。
//!
//! `WM_NCCALCSIZE` / `WM_NCPAINT` / `WM_NCACTIVATE` **不接管**——不产生 NC 区域，
//! 系统不画标准标题栏/按钮，全部外观由用户在客户端用 DrawBatch 自绘。

use crate::math::Rect;

/// 非客户区命中测试结果。镜像 Win32 `HT*` 枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NonClientHit {
    /// `HTNOWHERE` = 0
    NoWhere,
    /// `HTCLIENT` = 1 — 客户区，vireo 正常处理鼠标事件。
    Client,
    /// `HTCAPTION` = 2 — 让 Windows 自动接管标题栏拖动、snap layout、右键系统菜单、
    /// 双击最大化、Aero Snap。
    Caption,
    /// `HTMENU` = 5
    Menu,
    /// `HTHELP` = 21
    Help,
    /// `HTMINBUTTON` = 8 — Windows 自动处理最小化（含点击/键盘/动画）。
    MinButton,
    /// `HTMAXBUTTON` = 9 — 自动处理最大化/还原（双击标题栏也走这条）。
    MaxButton,
    /// `HTCLOSE` = 20 — 自动处理关闭（含 `Alt+F4`）。
    Close,

    /// `HTTOP` = 12
    TopBorder,
    /// `HTBOTTOM` = 15
    BottomBorder,
    /// `HTLEFT` = 10
    LeftBorder,
    /// `HTRIGHT` = 11
    RightBorder,
    /// `HTTOPLEFT` = 13
    TopLeftBorder,
    /// `HTTOPRIGHT` = 14
    TopRightBorder,
    /// `HTBOTTOMLEFT` = 16
    BottomLeftBorder,
    /// `HTBOTTOMRIGHT` = 17
    BottomRightBorder,

    /// `HTTRANSPARENT` = -1 — 让消息传给下层窗口。
    Transparent,
    /// 自定义 HT 值。
    Custom(u32),
}

impl NonClientHit {
    /// 转 Win32 HT* 字面值（Windows 子类用）。
    pub fn to_win32(self) -> i32 {
        match self {
            NonClientHit::NoWhere => 0,
            NonClientHit::Client => 1,
            NonClientHit::Caption => 2,
            NonClientHit::Menu => 5,
            NonClientHit::MinButton => 8,
            NonClientHit::MaxButton => 9,
            NonClientHit::Close => 20,
            NonClientHit::Help => 21,
            NonClientHit::TopBorder => 12,
            NonClientHit::BottomBorder => 15,
            NonClientHit::LeftBorder => 10,
            NonClientHit::RightBorder => 11,
            NonClientHit::TopLeftBorder => 13,
            NonClientHit::TopRightBorder => 14,
            NonClientHit::BottomLeftBorder => 16,
            NonClientHit::BottomRightBorder => 17,
            NonClientHit::Transparent => -1,
            NonClientHit::Custom(v) => v as i32,
        }
    }

    /// 从 Win32 `HT*` 字面值转回（`to_win32` 的逆运算）。
    ///
    /// 标准值映射到对应变体；未识别的值（含负值，如 `HTERROR` = -2）
    /// 包进 [`NonClientHit::Custom`]，保证 `from_win32(x).to_win32() == x`。
    pub fn from_win32(v: i32) -> Self {
        match v {
            0 => NonClientHit::NoWhere,
            1 => NonClientHit::Client,
            2 => NonClientHit::Caption,
            5 => NonClientHit::Menu,
            21 => NonClientHit::Help,
            8 => NonClientHit::MinButton,
            9 => NonClientHit::MaxButton,
            20 => NonClientHit::Close,
            12 => NonClientHit::TopBorder,
            15 => NonClientHit::BottomBorder,
            10 => NonClientHit::LeftBorder,
            11 => NonClientHit::RightBorder,
            13 => NonClientHit::TopLeftBorder,
            14 => NonClientHit::TopRightBorder,
            16 => NonClientHit::BottomLeftBorder,
            17 => NonClientHit::BottomRightBorder,
            -1 => NonClientHit::Transparent,
            other => NonClientHit::Custom(other as u32),
        }
    }
}

/// 窗口状态（传给 hit-test 回调）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WindowState {
    pub maximized: bool,
    pub fullscreen: bool,
    pub active: bool,
}

/// `WM_NCHITTEST` 命令式回调输入。
#[derive(Debug, Clone, Copy)]
pub struct HitTestInput {
    /// 客户端逻辑像素坐标（已从屏幕物理坐标转换）。
    pub pos: crate::math::Pos,
    pub state: WindowState,
    /// 当前有效 dpi scale（与 `metrics().scale_factor` 一致）。
    pub dpi_scale: f64,
}

/// 声明式非客户区区域（`set_non_client_regions` 入参）。
///
/// `rect` 用**客户端逻辑像素**坐标（与 DrawBatch 绘制坐标一致）。
/// 命中规则：**后声明的优先**（z 序在上）——允许按钮叠在标题栏上仍命中按钮。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NonClientRegion {
    pub rect: Rect,
    pub hit_test: NonClientHit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn non_client_hit_to_win32_matches_windows_constants() {
        assert_eq!(NonClientHit::NoWhere.to_win32(), 0);
        assert_eq!(NonClientHit::Client.to_win32(), 1);
        assert_eq!(NonClientHit::Caption.to_win32(), 2);
        assert_eq!(NonClientHit::MinButton.to_win32(), 8);
        assert_eq!(NonClientHit::MaxButton.to_win32(), 9);
        assert_eq!(NonClientHit::Close.to_win32(), 20);
        assert_eq!(NonClientHit::LeftBorder.to_win32(), 10);
        assert_eq!(NonClientHit::RightBorder.to_win32(), 11);
        assert_eq!(NonClientHit::TopBorder.to_win32(), 12);
        assert_eq!(NonClientHit::BottomBorder.to_win32(), 15);
        assert_eq!(NonClientHit::Transparent.to_win32(), -1);
        assert_eq!(NonClientHit::Custom(100).to_win32(), 100);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn non_client_hit_from_win32_round_trips() {
        for v in [0, 1, 2, 5, 21, 8, 9, 20, 12, 15, 10, 11, 13, 14, 16, 17, -1] {
            assert_eq!(NonClientHit::from_win32(v).to_win32(), v);
        }
        assert_eq!(NonClientHit::from_win32(0), NonClientHit::NoWhere);
        assert_eq!(NonClientHit::from_win32(1), NonClientHit::Client);
        assert_eq!(NonClientHit::from_win32(-1), NonClientHit::Transparent);
        assert_eq!(NonClientHit::from_win32(2), NonClientHit::Caption);
        assert_eq!(NonClientHit::from_win32(100), NonClientHit::Custom(100));
    }

}
