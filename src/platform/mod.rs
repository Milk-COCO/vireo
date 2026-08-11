//! 平台专用窗口扩展。
//!
//! 只放「无法跨平台 1:1、必须按 OS 特化」的能力。模块按 OS 门控：
//! 非目标平台编译时模块**不存在**（非空壳），不进 prelude。

#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "macos")]
pub mod macos;
