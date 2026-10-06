//! 可写目录（data / cache / external）。
//!
//! # 安卓上的三个世界
//!
//! 这是安卓文件系统里最容易踩坑的部分 —— 它有**三个互不相通**的「文件」世界：
//!
//! | 世界 | 路径形态 | 可写 | 用户可见 | 需权限 | 本模块 |
//! |---|---|---|---|---|---|
//! | **APK 内** | 不是路径，是 zip 里的一项 | ✗ | ✗ | — | [`crate::asset`] |
//! | **内部私有** | `/data/user/0/<包名>/files` | ✓ | ✗ | 免权限 | [`data_dir`] |
//! | **外部 app 专属** | `/storage/emulated/0/Android/data/<包名>/files` | ✓ | ✓ | 免权限 | [`external_data_dir`] |
//!
//! 关键点：
//!
//! - 两个可写目录**都不需要任何运行时权限**（app 专属目录 API 19+ 起免权限）。
//! - 两者都在**卸载时删除**。需要「卸载后还在」必须走 SAF（用户选位置），
//!   本模块**不覆盖**那个场景，见 `TODO.md`。
//! - **别直接往 `/sdcard/任意位置` 写** —— Android 11+ 分区存储会拒绝。要写
//!   共享目录只有两条路：写自己那个 `Android/data/<包名>/` 目录，或走 SAF。
//!
//! [`data_dir`]: crate::App::data_dir
//! [`external_data_dir`]: crate::App::external_data_dir

use crate::error::VireoError;
use std::path::PathBuf;

/// 目录名的默认来源：当前可执行文件的文件名（去扩展名）。
///
/// 例：`target/release/my_game.exe` → `my_game`。
///
/// 取不到（无 exe 路径）时退回 `"vireo-app"`。
pub(crate) fn default_storage_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .map(|s| sanitize_dir_name(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "vireo-app".to_string())
}

/// 把任意字符串压成安全的**单层**目录名。
///
/// 目录名会直接参与路径拼接，所以必须挡掉分隔符与 `..`：
/// - 含 `/` `\` 或变成 `.` / `..` / 空 → 整体退回 `"vireo-app"`
/// - 含路径分隔符 → 把分隔符换成 `_`（保留可读性，比整个丢弃友好）
/// - 含控制字符或 Windows 非法字符 `<>:"|?*` → 替换为 `_`
///
/// 不会因为「有非法字符」就失败 —— 退化目标是**永远返回一个可用名字**。
pub(crate) fn sanitize_dir_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '/' | '\\' => out.push('_'),
            // Windows 保留字符 + 控制字符：留着会让 create_dir 失败
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => out.push('_'),
            c if (c as u32) < 0x20 => out.push('_'),
            c => out.push(c),
        }
    }
    let trimmed = out.trim().trim_end_matches(['.', ' ']).to_string();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "vireo-app".to_string()
    } else {
        trimmed
    }
}

/// 持久化私有数据目录（存档 / 设置）。
///
/// - 安卓：`/data/user/0/<包名>/files`（免权限，卸载删除）
/// - Windows：`%APPDATA%\<name>`
/// - 其它 Unix：`$XDG_DATA_HOME/<name>`，未设则 `$HOME/.local/share/<name>`
///
/// `<name>` 来自 [`App::set_storage_name`](crate::App::set_storage_name)，
/// 默认取可执行文件名。
pub fn data_dir(app_name: &str) -> Result<PathBuf, VireoError> {
    #[cfg(target_os = "android")]
    {
        let _ = app_name; // 同 cache_dir：安卓目录名由系统从包名派生
        // 安卓的目录名由系统派生（`/data/user/0/<包名>/files`），拼不进去。
        let app = crate::app::android_app().ok_or(VireoError::Other(
            "AndroidApp unavailable: assets/storage need the #[vireo::main] android_main entry"
                .to_string(),
        ))?;
        app.internal_data_path()
            .ok_or_else(|| VireoError::Other("internal_data_path unavailable".to_string()))
    }
    #[cfg(not(target_os = "android"))]
    {
        let name = sanitize_dir_name(app_name);
        let base = env_path("APPDATA")
            .or_else(|| xdg_or_home("XDG_DATA_HOME", ".local", "share"))
            .ok_or_else(|| {
                VireoError::Other(
                    "cannot locate a data directory: set APPDATA (Windows) or \
                     XDG_DATA_HOME / HOME (Unix)"
                        .to_string(),
                )
            })?;
        Ok(base.join(name))
    }
}

/// 可再生缓存目录（可随时清空重建）。
///
/// - 安卓：`Activity.getCacheDir()`（免权限，**系统可在空间紧张时自行删除**）
/// - Windows：`%LOCALAPPDATA%\<name>\Cache`，未设则退回 `%APPDATA%\<name>\Cache`
/// - 其它 Unix：`$XDG_CACHE_HOME/<name>`，未设则 `$HOME/.cache/<name>`
///
/// ⚠️ 安卓系统**可能在你没跑的时候删掉这里的内容**。别放需要长期存在的东西。
pub fn cache_dir(app_name: &str) -> Result<PathBuf, VireoError> {
    #[cfg(target_os = "android")]
    {
        let _ = app_name; // 同 data_dir：安卓目录名由系统派生
        crate::platform::android::cache_dir()
    }
    #[cfg(not(target_os = "android"))]
    {
        let name = sanitize_dir_name(app_name);
        let base = env_path("LOCALAPPDATA")
            .map(|p| p.join(&name).join("Cache"))
            .or_else(|| env_path("APPDATA").map(|p| p.join(&name).join("Cache")))
            .or_else(|| xdg_or_home("XDG_CACHE_HOME", ".cache", "").map(|p| p.join(&name)))
            .ok_or_else(|| {
                VireoError::Other(
                    "cannot locate a cache directory: set LOCALAPPDATA / APPDATA (Windows) or \
                     XDG_CACHE_HOME / HOME (Unix)"
                        .to_string(),
                )
            })?;
        Ok(base)
    }
}

/// 用户可见的持久目录（想让用户自己拷走/放进来时用）。
///
/// - 安卓：`/storage/emulated/0/Android/data/<包名>/files`（免权限，卸载删除，
///   文件管理器可见）
/// - Windows：`%USERPROFILE%\Documents\<name>`，取不到则 `None`
/// - 其它 Unix：`$XDG_DOCUMENTS/<name>`，未设则 `$HOME/Documents/<name>`
///
/// 目录**不保证存在**（安卓下要首次写入后系统才创建；桌面下 Documents 可能没有）。
/// 需要时用 [`ensure_dir`] 创建。
///
/// ⚠️ 这**不是**「卸载后还在」的方案 —— 安卓下它同样随卸载删除，且 Android 11+
/// 分区存储不允许写 `Documents` 之外的共享位置。要真正让用户选位置，见 `TODO.md`
/// 的 SAF 条目。
pub fn external_data_dir(app_name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        let _ = app_name; // 同 data_dir：安卓目录名由系统派生
        let app = crate::app::android_app()?;
        app.external_data_path()
    }
    #[cfg(not(target_os = "android"))]
    {
        let name = sanitize_dir_name(app_name);
        xdg_or_home("XDG_DOCUMENTS", "Documents", "")
            .map(|p| p.join(&name))
            .or_else(|| home_dir().map(|h| h.join("Documents").join(name)))
    }
}

/// 读一个目录下的条目名（非递归）。不存在时返回 `Ok(vec![])`。
pub fn list_dir(dir: &std::path::Path) -> Result<Vec<String>, VireoError> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        // 目录不存在不算错误 —— 首次运行前它就是空的
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(VireoError::Other(format!("read_dir failed: {e}"))),
    };
    for entry in rd {
        let Ok(entry) = entry else { continue };
        let n = entry.file_name().to_string_lossy().into_owned();
        if n != "." && n != ".." {
            out.push(n);
        }
    }
    Ok(out)
}

/// 确保目录存在（含父目录）。
pub fn ensure_dir(dir: &std::path::Path) -> Result<(), VireoError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| VireoError::Other(format!("create_dir_all({dir:?}) failed: {e}")))
}

// ---- 桌面环境变量 ---------------------------------------------------------

#[cfg(not(target_os = "android"))]
fn env_path(key: &str) -> Option<PathBuf> {
    let v = std::env::var_os(key)?;
    if v.is_empty() {
        return None;
    }
    Some(PathBuf::from(v))
}

#[cfg(not(target_os = "android"))]
fn home_dir() -> Option<PathBuf> {
    env_path("HOME").or_else(|| env_path("USERPROFILE"))
}

/// `$XDG_<KEY>`，未设时 `$HOME/<dot><fallback_seg>/<tail_seg>/<name>`。
#[cfg(not(target_os = "android"))]
fn xdg_or_home(key: &str, dot: &str, tail: &str) -> Option<PathBuf> {
    if let Some(p) = env_path(key) {
        return Some(p);
    }
    let home = home_dir()?;
    let mut p = home.join(dot);
    if !tail.is_empty() {
        p = p.join(tail);
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_ordinary_names() {
        assert_eq!(sanitize_dir_name("my_game"), "my_game");
        assert_eq!(sanitize_dir_name("Vireo Demo"), "Vireo Demo");
        assert_eq!(sanitize_dir_name("app-1.2.3"), "app-1.2.3");
        assert_eq!(sanitize_dir_name("中文名"), "中文名");
    }

    #[test]
    fn sanitize_replaces_separators_instead_of_dropping_name() {
        // 分隔符会让人意外地写到别的目录去 —— 必须换掉
        assert_eq!(sanitize_dir_name("a/b"), "a_b");
        assert_eq!(sanitize_dir_name(r"a\b"), "a_b");
        assert_eq!(sanitize_dir_name("../etc"), ".._etc");
        // 结果必须是单层路径分量
        for raw in ["a/b", r"a\b", "../etc", "a/../b"] {
            let s = sanitize_dir_name(raw);
            assert!(!s.contains('/') && !s.contains('\\'), "{raw:?} -> {s:?}");
            assert_ne!(s, "..", "{raw:?}");
            assert_ne!(s, ".", "{raw:?}");
        }
    }

    #[test]
    fn sanitize_replaces_windows_reserved_and_control_chars() {
        assert_eq!(sanitize_dir_name("a<b>c:d\"e|f?g*h"), "a_b_c_d_e_f_g_h");
        assert_eq!(sanitize_dir_name("a\tb\nc"), "a_b_c");
    }

    #[test]
    fn sanitize_falls_back_for_empty_and_dot_names() {
        // 空 / 全空白 / "." / ".." 都不能当目录名，否则会写到意外位置
        assert_eq!(sanitize_dir_name(""), "vireo-app");
        assert_eq!(sanitize_dir_name("   "), "vireo-app");
        assert_eq!(sanitize_dir_name("."), "vireo-app");
        assert_eq!(sanitize_dir_name(".."), "vireo-app");
        // Windows 会把结尾的点/空格吃掉，预先处理掉
        assert_eq!(sanitize_dir_name("name."), "name");
        assert_eq!(sanitize_dir_name("name "), "name");
        assert_eq!(sanitize_dir_name("name. "), "name");
    }

    #[test]
    fn sanitize_never_returns_empty_or_traversal() {
        for raw in [
            "", " ", ".", "..", "/", r"\", "...", "a/", "/a", "\t", "\u{1}", "a..b",
        ] {
            let s = sanitize_dir_name(raw);
            assert!(!s.is_empty(), "{raw:?} produced empty name");
            assert!(
                !s.contains('/') && !s.contains('\\'),
                "{raw:?} -> {s:?} kept a separator"
            );
        }
    }

    #[test]
    fn default_name_is_a_single_safe_component() {
        let n = default_storage_name();
        assert!(!n.is_empty());
        assert!(!n.contains('/') && !n.contains('\\'));
        assert_ne!(n, ".");
        assert_ne!(n, "..");
    }

    #[test]
    fn list_dir_treats_missing_dir_as_empty_not_error() {
        let p = std::env::temp_dir().join("vireo_storage_missing_dir_xyz");
        let _ = std::fs::remove_dir_all(&p);
        // 首次运行前目录不存在 —— 这必须是 Ok(empty)，不是 Err
        assert_eq!(list_dir(&p).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn list_dir_reads_back_what_ensure_dir_created() {
        let root = std::env::temp_dir().join("vireo_storage_list_test");
        let _ = std::fs::remove_dir_all(&root);
        let sub = root.join("slot1");
        ensure_dir(&sub).unwrap();
        std::fs::write(sub.join("save.json"), b"{}").unwrap();

        let mut names = list_dir(&root).unwrap();
        names.sort();
        assert_eq!(names, vec!["slot1".to_string()]);

        // 幂等：已存在不报错
        ensure_dir(&sub).unwrap();

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_dirs_are_absolute_and_named() {
        // 这些函数可能返回 Err（CI 环境变量缺失），返回 Ok 时必须可用
        for (f, label) in [
            (data_dir("my_game"), "data"),
            (cache_dir("my_game"), "cache"),
        ] {
            let Ok(p) = f else { continue };
            assert!(p.is_absolute(), "{label}: {p:?} should be absolute");
            assert!(
                p.components().any(|c| c.as_os_str() == "my_game"),
                "{label}: {p:?} should contain the app name"
            );
        }
        if let Some(p) = external_data_dir("my_game") {
            assert!(p.is_absolute(), "external: {p:?} should be absolute");
            assert!(p.components().any(|c| c.as_os_str() == "my_game"));
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_dirs_sanitize_the_app_name() {
        // 名字里带分隔符时必须落在「单层子目录」而不是逃出去
        if let Ok(p) = data_dir("../escape") {
            assert!(p.is_absolute());
            let joined = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                !joined.contains('/') && !joined.contains('\\'),
                "escaped: {p:?}"
            );
        }
    }
}
