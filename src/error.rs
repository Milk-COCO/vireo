//! Vireo 自定义错误类型。
//!
//! 与索引/句柄相关的查询 API（`App::window_ref` / `App::offscreen_ref` / `App::texture`）
//! 原本返回 `Option`，窗口关闭或索引失效时得到 `None`——调用方若 `.unwrap()` 会炸出难以
//! 定位的 panic。改为返回 `Result<_, VireoError>`，让「窗口已关闭 / 索引无效」成为有明确
//! 文案的错误，而不是靠 unwrap 在运行时偶发 panic。
//!
//! 平台能力型 getter（`is_minimized` / `is_visible` / `fullscreen` / `theme` /
//! `current_monitor` 等）仍返回 `Option`，因为那里的 `None` 语义是「平台不支持」，不是错误。

use std::fmt;

/// Vireo 操作错误。
///
/// 目前主要覆盖「按句柄/索引查找资源」类 API 的失效场景。其它路径（如启动期
/// adapter/device 创建失败）目前仍通过 wgpu 的 panic / `expect` 暴露，未来可在此统一。
#[derive(Debug, Clone, PartialEq)]
pub enum VireoError {
    /// 窗口索引无效：从未创建，或已关闭（`App::window_ref`）。
    WindowNotFound(u64),
    /// 离屏画布索引无效：从未创建，或已释放（`App::offscreen_ref`）。
    OffscreenNotFound(usize),
    /// 贴图索引无效：越界，或尚未加载完成（`App::texture`）。
    TextureNotFound(usize),
    /// 随包资源不存在（`Assets::read` / `Assets::list`）。
    ///
    /// 安卓上还包括「`AndroidApp` 句柄不可用」（未经 `android_main` 入口）——
    /// 那种情况下所有资源读取都会落到这里。
    AssetNotFound(String),
    /// 未设资产根目录（`Assets` 在桌面上的 `read`/`list`）。
    ///
    /// 桌面需要 [`App::set_asset_root`](crate::App::set_asset_root) 指向一个目录；
    /// 不设时资源一律读不到。安卓下不适用（资源在 APK 内）。
    NoAssetRoot,
    /// 用户在系统选择器里主动取消（SAF 取文件/目录时点了返回）。
    ///
    /// 不是故障 —— 调用方通常应该静默忽略，而不是报错。
    UserCancelled,
    /// 该能力只在特定平台上有（如 SAF 只在 Android 上有）。
    ///
    /// 与返回 `Option` 的平台查询不同：这里是**动作**（发起选择器、打开 fd），
    /// 失败是错误而不是「无值」，所以用 `Result` + 本变体。
    Unsupported(&'static str),
    /// 其它未分类错误。
    Other(String),
}

impl fmt::Display for VireoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VireoError::WindowNotFound(idx) => write!(
                f,
                "window not found (index {idx}); the window was never created or has been closed"
            ),
            VireoError::OffscreenNotFound(idx) => write!(
                f,
                "offscreen canvas not found (index {idx}); it was never created or has been released"
            ),
            VireoError::TextureNotFound(idx) => write!(
                f,
                "texture not found (index {idx}); out of bounds or not yet loaded"
            ),
            VireoError::AssetNotFound(name) => write!(
                f,
                "asset not found: {name:?}; on Android packaged resources live inside the APK \
                 (put them under src/main/assets/) and are not reachable through the filesystem"
            ),
            VireoError::NoAssetRoot => write!(
                f,
                "no asset root configured; call App::set_asset_root(path) on desktop \
                 (on Android packaged resources need no root)"
            ),
            VireoError::UserCancelled => write!(f, "user cancelled the picker"),
            VireoError::Unsupported(what) => write!(f, "{what} is only supported on Android"),
            VireoError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for VireoError {}
