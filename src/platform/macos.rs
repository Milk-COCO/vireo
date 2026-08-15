//! macOS 窗口扩展（镜像 winit `WindowExtMacOS` 可复用子集）。
//!
//! 需要显式导入后调用：
//! ```no_run
//! use vireo::platform::macos::WindowExtMacOS;
//! win.set_document_edited(true);
//! ```
//!
//! 非 macOS 平台本模块不存在（`#[cfg(target_os = "macos")]` 门控）。
//! 本机（Windows）无法编译验证，需 macOS 实机验证。
//!
//! 注意：winit 的 `WindowExtMacOS` **运行时**只暴露本模块转发的方法；
//! 隐藏标题栏文本 / 透明标题栏 / 全尺寸内容区等**构造期**能力走
//! `FrameStyle::HiddenTitlebar`（见 `src/window.rs` `create_attrs`），
//! 不在本 trait 内。macOS 用 `with_title_hidden`（红绿灯保留、系统接管），
//! 与 Electron `titleBarStyle: 'hidden'` 一致。

pub use winit::platform::macos::OptionAsAlt;

/// macOS 专属窗口扩展（镜像 winit `WindowExtMacOS` 的可复用子集）。
pub trait WindowExtMacOS {
    /// 是否处于简单全屏（无过渡动画、不占独立空间的桌面级全屏）。
    /// （winit `WindowExtMacOS::simple_fullscreen`）
    fn simple_fullscreen(&self) -> bool;

    /// 切换简单全屏。返回是否切换成功（全屏切出失败时返回 `false`）。
    /// （winit `WindowExtMacOS::set_simple_fullscreen`）
    fn set_simple_fullscreen(&self, fullscreen: bool) -> bool;

    /// 窗口是否带阴影。（winit `WindowExtMacOS::has_shadow`）
    fn has_shadow(&self) -> bool;

    /// 设置窗口阴影。（winit `WindowExtMacOS::set_has_shadow`）
    fn set_has_shadow(&self, has_shadow: bool);

    /// 是否标记为「文档已编辑」（标题栏圆点指示）。
    /// （winit `WindowExtMacOS::is_document_edited`）
    fn is_document_edited(&self) -> bool;

    /// 设置「文档已编辑」标记。（winit `WindowExtMacOS::set_document_edited`）
    fn set_document_edited(&self, edited: bool);

    /// 设置 Option 键是否当作 Alt 键处理。
    /// （winit `WindowExtMacOS::set_option_as_alt`）
    fn set_option_as_alt(&self, option_as_alt: OptionAsAlt);

    /// 是否处于无边框游戏模式（隐藏菜单栏/停靠栏/DM 系统遮罩）。
    /// （winit `WindowExtMacOS::is_borderless_game`）
    fn is_borderless_game(&self) -> bool;

    /// 切换无边框游戏模式。（winit `WindowExtMacOS::set_borderless_game`）
    fn set_borderless_game(&self, borderless_game: bool);
}

impl WindowExtMacOS for crate::window::VireoWindow {
    fn simple_fullscreen(&self) -> bool {
        winit::platform::macos::WindowExtMacOS::simple_fullscreen(&*self.inner)
    }

    fn set_simple_fullscreen(&self, fullscreen: bool) -> bool {
        winit::platform::macos::WindowExtMacOS::set_simple_fullscreen(
            &*self.inner,
            fullscreen,
        )
    }

    fn has_shadow(&self) -> bool {
        winit::platform::macos::WindowExtMacOS::has_shadow(&*self.inner)
    }

    fn set_has_shadow(&self, has_shadow: bool) {
        winit::platform::macos::WindowExtMacOS::set_has_shadow(&*self.inner, has_shadow);
    }

    fn is_document_edited(&self) -> bool {
        winit::platform::macos::WindowExtMacOS::is_document_edited(&*self.inner)
    }

    fn set_document_edited(&self, edited: bool) {
        winit::platform::macos::WindowExtMacOS::set_document_edited(&*self.inner, edited);
    }

    fn set_option_as_alt(&self, option_as_alt: OptionAsAlt) {
        winit::platform::macos::WindowExtMacOS::set_option_as_alt(
            &*self.inner,
            option_as_alt,
        );
    }

    fn is_borderless_game(&self) -> bool {
        winit::platform::macos::WindowExtMacOS::is_borderless_game(&*self.inner)
    }

    fn set_borderless_game(&self, borderless_game: bool) {
        winit::platform::macos::WindowExtMacOS::set_borderless_game(
            &*self.inner,
            borderless_game,
        );
    }
}
