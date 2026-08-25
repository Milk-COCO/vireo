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
            VireoError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for VireoError {}
