//! 随包只读资源（assets）。
//!
//! # 为什么需要它
//!
//! 安卓上「随安装包发布的图片/字体/关卡」**不在文件系统上** —— 它们是 APK 这个
//! zip 包里的一项，必须经 `AssetManager` / `AAssetManager` 读取，`std::fs::read`
//! **读不到**。所以桌面能跑的 `load_texture("bg.png")` 在安卓上必然失败。
//!
//! 本模块提供**显式**入口，不改动任何既有路径 API 的语义：
//!
//! - [`App::load_texture`] / [`App::load_font_file`] / [`WindowDesc::icon_from_path`]
//!   保持**纯 `std::fs`**：给真实路径，或安卓上你自己有权限的绝对路径。
//! - [`Assets`] 是另一条路：安卓 = APK assets，桌面 = [`App::set_asset_root`]
//!   指定的目录。
//!
//! # 为什么不自动回退
//!
//! 曾经考虑「相对路径先查 assets 再查文件系统」。**否决**：那会让同一份代码在两个
//! 平台上**资源来源不同且不报错** —— 桌面调通了，安卓静默换成另一张图，排查成本
//! 极高。两条路各自显式，语义唯一。
//!
//! # 目录约定
//!
//! 桌面：把文件放 [`App::set_asset_root`] 的目录。⚠️ 该路径相对 **CWD** ——
//!       `cargo run --example x` 的 CWD 是**仓库根**，不是 `examples/`。<br/>
//! 安卓：放 `src/main/assets/`（AGP 默认打进 APK，无需改 `build.gradle`）。
//!
//! 同一份代码两端通用，这是本模块存在的主要价值 —— **不是**魔法，是把根目录
//! 显式化。
//!
//! [`App`]: crate::App
//! [`App::load_texture`]: crate::App::load_texture
//! [`App::load_font_file`]: crate::App::load_font_file
//! [`App::set_asset_root`]: crate::App::set_asset_root
//! [`WindowDesc::icon_from_path`]: crate::WindowDesc::icon_from_path

use crate::error::VireoError;

/// 随包只读资源的句柄。
///
/// 安卓 = APK 内的 `assets/`；桌面 = [`root`](Assets::root) 指定的目录。
/// 两者都**只读** —— 可写路径见 [`crate::storage`]。
///
/// 用 [`App::assets`] 取得。
///
/// [`App::assets`]: crate::App::assets
pub struct Assets {
    /// 桌面的根目录。安卓下为 `None`（资源在 APK 内，没有「根目录」概念）。
    root: Option<std::path::PathBuf>,
}

impl Assets {
    pub(crate) fn new(root: Option<std::path::PathBuf>) -> Self {
        Self { root }
    }

    /// 桌面的资产根目录；安卓下恒为 `None`。
    pub fn root(&self) -> Option<&std::path::Path> {
        self.root.as_deref()
    }

    /// 读取资源的全部字节。
    ///
    /// `name` 用 `/` 分隔（`"levels/01/chart.json"`）。`\`、`./`、重复 `/` 会被
    /// 归一化；开头的 `/` 会被去掉（安卓 assets 名恒为相对路径）。
    ///
    /// # 错误
    ///
    /// - [`VireoError::AssetNotFound`]：资源不存在
    /// - [`VireoError::NoAssetRoot`]：桌面未设 [`set_asset_root`](Self::set_asset_root)
    ///   时的其它失败
    /// - [`VireoError::Other`]：IO 失败或安卓 `AndroidApp` 不可用
    ///
    /// [`set_asset_root`]: crate::App::set_asset_root
    pub fn read(&self, name: &str) -> Result<Vec<u8>, VireoError> {
        let norm = normalize(name)
            .ok_or_else(|| VireoError::Other(format!("invalid asset name: {name:?} (rejected)")))?;
        read_impl(self.root.as_deref(), &norm)
    }

    /// 资源是否存在。
    ///
    /// 桌面下若未设资产根目录，**恒为 `false`**（不会 panic）。安卓上资产句柄不可用
    /// 时同样为 `false`。
    pub fn exists(&self, name: &str) -> bool {
        let Some(norm) = normalize(name) else {
            return false;
        };
        exists_impl(self.root.as_deref(), &norm)
    }

    /// 列出子目录下的条目名（不含子目录自身，不递归）。
    ///
    /// `dir` 用 `/` 分隔；`""` 表示根。安卓上会过滤掉 `"."` / `".."`。
    ///
    /// # 错误
    ///
    /// - [`VireoError::AssetNotFound`]：目录不存在
    /// - [`VireoError::Other`]：其它 IO / 平台失败
    ///
    /// 条目**顺序不保证** —— 桌面是 `read_dir` 顺序，安卓是 zip 内顺序。
    pub fn list(&self, dir: &str) -> Result<Vec<String>, VireoError> {
        let norm = normalize(dir).unwrap_or_default();
        list_impl(self.root.as_deref(), &norm)
    }
}

impl std::fmt::Debug for Assets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assets").field("root", &self.root).finish()
    }
}

/// 归一化资产名：反斜杠转正斜杠、去掉 `./`、压掉重复 `/`、去掉前导 `/`。
///
/// 返回 `None` 当归一化后为空（原名是空串或全是分隔符）。
///
/// # 为什么允许 `\` 和前导 `/`
///
/// 安卓 assets 名恒为相对 POSIX 路径，但用户手上可能拿到 Windows 风格的串
/// （从配置文件读来的、或从桌面代码拷过来的）。这里**接受**它们而不是报错 ——
/// 报错会让「同一个 name 字符串在两端都能用」这个目标落空。
///
/// # 为什么**拒绝** `..`
///
/// 资产名会参与路径拼接，`../` 能跳出根目录。桌面侧必须挡住（安卓侧本来就在
/// 沙箱内，但归一化统一做掉更简单，也避免两端行为分叉）。
pub(crate) fn normalize(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    for seg in name.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return None;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(seg);
    }
    if out.is_empty() { None } else { Some(out) }
}

// ---- 平台实现 ------------------------------------------------------------
// 平台实现直接返回 `Result`，这样「桌面没设根目录」（`NoAssetRoot`）与
// 「资源不存在」（`AssetNotFound`）能被调用方区分 —— 前者是配置遗漏，
// 后者是内容缺失，排查方向完全不同。

#[cfg(not(target_os = "android"))]
fn read_impl(root: Option<&std::path::Path>, name: &str) -> Result<Vec<u8>, VireoError> {
    let root = root.ok_or(VireoError::NoAssetRoot)?;
    std::fs::read(root.join(name)).map_err(|_| VireoError::AssetNotFound(name.to_string()))
}

#[cfg(not(target_os = "android"))]
fn exists_impl(root: Option<&std::path::Path>, name: &str) -> bool {
    let Some(root) = root else {
        return false;
    };
    root.join(name).is_file()
}

#[cfg(not(target_os = "android"))]
fn list_impl(root: Option<&std::path::Path>, dir: &str) -> Result<Vec<String>, VireoError> {
    let root = root.ok_or(VireoError::NoAssetRoot)?;
    let path = if dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir)
    };
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&path).map_err(|_| VireoError::AssetNotFound(dir.to_string()))? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        // Windows 上 `read_dir` 会把调用方给的 `dir` 自身列进来，别把它塞进结果
        if name != "." && name != ".." {
            out.push(name);
        }
    }
    Ok(out)
}

#[cfg(target_os = "android")]
fn read_impl(_root: Option<&std::path::Path>, name: &str) -> Result<Vec<u8>, VireoError> {
    use std::io::Read;
    let notfound = || VireoError::AssetNotFound(name.to_string());
    let app = crate::app::android_app().ok_or_else(notfound)?;
    let cname = std::ffi::CString::new(name).map_err(|_| notfound())?;
    // `Asset` 不是 `Send`，但整个调用同步完成、不跨线程持有它。
    let mut asset = app.asset_manager().open(&cname).ok_or_else(notfound)?;
    let mut buf = Vec::new();
    // 压缩 / 未压缩的资产都能读：`read` 内部按需解压。
    // 所以本模块**不依赖** build.gradle 的 `noCompress` 配置。
    asset.read_to_end(&mut buf).map_err(|_| notfound())?;
    Ok(buf)
}

#[cfg(target_os = "android")]
fn exists_impl(_root: Option<&std::path::Path>, name: &str) -> bool {
    use std::io::Read;
    let Some(app) = crate::app::android_app() else {
        return false;
    };
    let Ok(cname) = std::ffi::CString::new(name) else {
        return false;
    };
    let am = app.asset_manager();

    // ⚠️ 这里有个真实的坑：`AAsset_open` 对**目录**也返回非 None，且目录与
    // 0 字节文件读出来**都是 0 字节** —— 只靠 `read` 无法区分两者。
    //
    // 所以分两步：
    //   1. open 失败            → 不存在
    //   2. 能读出 ≥1 字节        → 一定是资源
    //   3. 读出 0 字节          → 用 open_dir 判定：能当目录打开就是目录（不是资源），
    //                            否则是 0 字节的资源（存在）
    let Some(mut asset) = am.open(&cname) else {
        return false;
    };
    let mut probe = [0u8; 1];
    if matches!(asset.read(&mut probe), Ok(n) if n > 0) {
        return true;
    }
    // 0 字节：借 open_dir 区分「目录」与「空文件」
    am.open_dir(&cname).is_none()
}

#[cfg(target_os = "android")]
fn list_impl(_root: Option<&std::path::Path>, dir: &str) -> Result<Vec<String>, VireoError> {
    let notfound = || VireoError::AssetNotFound(dir.to_string());
    let app = crate::app::android_app().ok_or_else(notfound)?;
    // 根目录在 AAssetDir 里的名字是 "."，不是 ""。
    let cdir =
        std::ffi::CString::new(if dir.is_empty() { "." } else { dir }).map_err(|_| notfound())?;
    let mut d = app.asset_manager().open_dir(&cdir).ok_or_else(notfound)?;
    let mut out = Vec::new();
    for c in d.by_ref() {
        let name = c.to_string_lossy().into_owned();
        if name != "." && name != ".." {
            out.push(name);
        }
    }
    Ok(out)
}

/// 进程级资产根（桌面上由 [`App::set_asset_root`](crate::App::set_asset_root) 写入）。
///
/// **是 `Mutex` 不是 `OnceLock`** —— 后者只能设一次，而 `set_asset_root` 的契约是
/// 「重复调用以最后一次为准」。
///
/// 进程级而非 `App` 字段的原因：[`WindowDesc::icon_from_asset`] 是**构建期**调用的
/// （`App::window(WindowDesc::new(..).icon_from_asset(..))`），那时还没有可用的
/// `App` 句柄。资产根必须是构建期就能读到的东西。
///
/// [`WindowDesc::icon_from_asset`]: crate::WindowDesc::icon_from_asset
#[cfg(not(target_os = "android"))]
pub(crate) static ASSET_ROOT: parking_lot::Mutex<Option<std::path::PathBuf>> =
    parking_lot::Mutex::new(None);

/// 当前资产根；安卓下恒为 `None`（资源在 APK 内，没有「根目录」概念）。
pub(crate) fn current_root() -> Option<std::path::PathBuf> {
    #[cfg(not(target_os = "android"))]
    {
        ASSET_ROOT.lock().clone()
    }
    #[cfg(target_os = "android")]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_posix_and_windows_separators() {
        assert_eq!(normalize("bg.png").as_deref(), Some("bg.png"));
        assert_eq!(normalize("a/b/c.png").as_deref(), Some("a/b/c.png"));
        // Windows 风格反斜杠 —— 桌面上手写这个名字不该炸
        assert_eq!(
            normalize(r"levels\01\chart.json").as_deref(),
            Some("levels/01/chart.json")
        );
    }

    #[test]
    fn normalize_strips_leading_slash_and_dot_prefix() {
        // 安卓 assets 名恒为相对路径，前导 / 被去掉而不是报错
        assert_eq!(normalize("/bg.png").as_deref(), Some("bg.png"));
        assert_eq!(normalize("./bg.png").as_deref(), Some("bg.png"));
        assert_eq!(normalize(".//./bg.png").as_deref(), Some("bg.png"));
    }

    #[test]
    fn normalize_collapses_duplicate_separators() {
        assert_eq!(normalize("a//b///c.png").as_deref(), Some("a/b/c.png"));
        assert_eq!(normalize(r"a\\b").as_deref(), Some("a/b"));
    }

    #[test]
    fn normalize_rejects_empty_and_separator_only() {
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("/"), None);
        assert_eq!(normalize("///"), None);
        assert_eq!(normalize(r"\\"), None);
        assert_eq!(normalize("./"), None);
    }

    #[test]
    fn normalize_rejects_parent_traversal() {
        // 资产名会参与路径拼接，`..` 能跳出根目录，必须挡
        assert_eq!(normalize("../secret"), None);
        assert_eq!(normalize("a/../../secret"), None);
        assert_eq!(normalize(r"a\..\..\secret"), None);
        // 名字里恰好含 ".." 但不是路径分量 —— 合法（真实存在这种文件名）
        assert_eq!(normalize("a..b.png").as_deref(), Some("a..b.png"));
        assert_eq!(normalize("..hidden.png").as_deref(), Some("..hidden.png"));
    }

    #[test]
    fn desktop_helpers_are_total_over_normalized_names() {
        // 未设 ASSET_ROOT 时：exists=false / read/list 走 None（不 panic）
        let a = Assets::new(None);
        assert!(!a.exists("bg.png"));
        assert!(!a.exists(""));
        assert!(!a.exists("../escape"));
        assert!(a.read("bg.png").is_err());
        assert!(a.list("").is_err());
        assert_eq!(a.root(), None);
    }

    #[test]
    fn read_uses_configured_root_on_desktop() {
        let dir = std::env::temp_dir().join("vireo_asset_test_read");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), b"hello").unwrap();

        // 直接给根目录构造句柄（不走全局），验证 join + 读
        let a = Assets::new(Some(dir.clone()));
        assert_eq!(a.read("hello.txt").unwrap(), b"hello");
        assert!(a.exists("hello.txt"));
        assert!(!a.exists("nope.txt"));

        // list 会看到刚写进去的文件
        let mut names = a.list("").unwrap();
        names.sort();
        assert_eq!(names, vec!["hello.txt".to_string()]);

        // 前导 / 与反斜杠在真实 IO 上同样生效
        assert_eq!(a.read("/hello.txt").unwrap(), b"hello");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_rejects_traversal_even_with_real_root() {
        let dir = std::env::temp_dir().join("vireo_asset_test_escape");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.txt"), b"x").unwrap();

        let a = Assets::new(Some(dir.clone()));
        // 越界名必须在 normalize 阶段就被拒绝，不能落到 IO 上
        assert!(a.read("../ok.txt").is_err());
        assert!(!a.exists("../ok.txt"));
        assert!(a.read("ok.txt").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_byte_asset_still_exists() {
        // 契约：0 字节的资源**存在**。
        // 安卓实现踩过这个坑 —— `AAsset_open` 对目录也返回非 None，且目录与
        // 0 字节文件读出来都是 0 字节，只靠 `read` 会把空资源误判为不存在。
        let dir = std::env::temp_dir().join("vireo_asset_test_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.txt"), b"").unwrap();

        let a = Assets::new(Some(dir.clone()));
        assert!(a.exists("empty.txt"), "0 字节资源必须报告存在");
        assert_eq!(a.read("empty.txt").unwrap(), Vec::<u8>::new());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subdirectory_is_not_an_asset() {
        // 与 0 字节资源相对：子目录本身不是资源（安卓侧要靠 open_dir 区分）
        let dir = std::env::temp_dir().join("vireo_asset_test_subdir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("levels")).unwrap();

        let a = Assets::new(Some(dir));
        assert!(!a.exists("levels"), "目录不该被当成资源");

        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("vireo_asset_test_subdir"));
    }

    #[test]
    fn list_of_missing_dir_errors_instead_of_empty() {
        let dir = std::env::temp_dir().join("vireo_asset_test_list_missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Assets::new(Some(dir.clone()));
        assert!(a.list("nope").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
