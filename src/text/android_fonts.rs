//! Android 系统字体接入。
//!
//! # 为什么需要这一块
//!
//! fontdb 的 unix 实现整段被 `#[cfg(all(unix, not(any(target_os = "macos",
//! target_os = "android"))))]` 排除，所以 `load_system_fonts()` 在 Android 上
//! **什么都不加载** → 空字体库 → 首次 shape 时 cosmic-text
//! `expect("no default font found")` panic（真机实测）。
//!
//! # 为什么不能无差别 `load_fonts_dir("/system/fonts")`
//!
//! `/system/fonts` 在真机（Android 16 / 小米）上有 **230 个**字体文件，混着大量
//! 非正文字体：`ComingSoon.ttf`（大写装饰体）、`CutiveMono.ttf`、
//! `DancingScript-Regular.ttf`、`AndroidClock.ttf`（只有数字）等。全量加载后
//! cosmic-text 的默认字体解析会挑中它们：
//!
//! - cosmic-text `Attrs::new()` 默认 `Family::SansSerif`，对应 fontdb 的
//!   `family_sans_serif`，默认字符串 **`"Arial"`**
//! - fontdb 的 `query` 只匹配家族名**恰好等于**该字符串的 face
//! - Android 上没有字体声明 `"Arial"`（Roboto 家族名是 `Roboto`，Droid Sans 是
//!   `Droid Sans`）→ `query` 返回 `None`
//! - cosmic-text 回退到「排序后的第一个 face」→ 可能是上面那些装饰体
//!
//! 实测症状：所有文字渲染成**全大写**、**数字完全消失**（比分 `0 : 0` 画不出来）。
//!
//! 桌面（Windows）恰好装了 Arial，所以同一份代码在桌面正常 —— **这个 bug 只在
//! Android 暴露**，也是它长期没被发现的原因。
//!
//! # 做法：文件名白名单 + 钉死 generic 家族
//!
//! 两步，缺一不可：
//!
//! 1. **文件名白名单**（`is_text_font_file`）：只解析各平台已知的**正文字体**
//!    文件名，其余一概不碰。230 个文件 → 约 8~20 个，省掉 200+ 次字体解析，
//!    也从根上不让装饰体进入字体库。
//! 2. **钉死 generic 家族**（`set_sans_serif_family`）：fontdb 的
//!    `family_sans_serif` 默认是 `"Arial"`，Android 上永远命中不了。
//!
//! 第 2 步**不是补丁，是必需**：即使只加载 Roboto，generic 家族仍指向不存在的
//! `"Arial"`，`query` 照样返回 `None`。业界（Godot 的 `SystemFont`、Flutter 的
//! pubspec 声明字体、Unity 的系统字体 API）都是「显式指定默认正文字体」这一意图，
//! `set_sans_serif_family` 正是 fontdb 对该意图的表达。
//!
//! 白名单按**文件名前缀**而非完整文件名匹配 —— OEM 字体命名差异大
//! （小米 `MiSansVF.ttf`、华为 `HarmonyOS_Sans_SC_Regular.ttf`…），
//! 前缀匹配能在过滤掉装饰体的同时覆盖各家。扩充白名单只需往 [`TEXT_FONT_PREFIXES`] 加一项。
//!
//! # 白名单是编译期固定的 —— 用户怎么绕
//!
//! 三张表（[`TEXT_FONT_PREFIXES`] 文件名 / `FONT_DIRS` 目录 /
//! `PREFERRED_FAMILIES` 家族名）都是 `const`，加上 `install()` 是
//! `pub(crate)`，用户在**没听说过的 ROM** 上会同时撞到三个问题：正文字体文件
//! 不被扫描、字体文件不在系统目录、字体家族名不在优先表里。三者最终都表现为
//! 「所有文字全大写、数字消失」。
//!
//! [`configure`] 把这三张表从编译期挪到运行期（进程级，须在 `App::new()` 之前调用）。
//! 这是唯一的用户侧逃生口；`load_font_file` 那条路能加字体文件但**钉不死
//! generic 家族**，救不了这个症状。
//!
//! 模块**无条件编译**（只有 `install` 及其 Android 依赖按 target 门控），
//! 这样白名单逻辑能在桌面真跑单测 —— 若整体 gate 掉，`cargo test --lib`
//! 一个都跑不到（本平台其他 `cfg` 门控模块就有这个盲区）。
//!代价是桌面非 test 构建下这些项无人调用，故下面 `allow(dead_code)`。
#![cfg_attr(not(any(test, target_os = "android")), allow(dead_code))]

use std::path::Path;

#[cfg(target_os = "android")]
use std::path::PathBuf;

#[cfg(target_os = "android")]
use cosmic_text::FontSystem;

/// 扫描的字体目录（`/product/fonts` 是 OEM 追加位，Xiaomi/OPPO 的自研字体在这）。
#[cfg(target_os = "android")]
const FONT_DIRS: &[&str] = &["/system/fonts", "/product/fonts"];

/// 正文字体文件名前缀白名单。命中即加载，未命中一概不碰。
///
/// 覆盖：AOSP/Pixel（Roboto）、Google CJK（NotoSans/NotoSerif/DroidSansFallback）、
/// 小米澎湃（MiSans）、思源（SourceHanSans）、华为（HarmonyOS）。
pub const TEXT_FONT_PREFIXES: &[&str] = &[
    "Roboto",
    "NotoSans",
    "NotoSerif",
    "DroidSans", // 含 DroidSansFallback（旧 AOSP 的 CJK 兜底）
    "MiSans",
    "SourceHanSans",
    "HarmonyOS_Sans",
    "OPPO_Sans",
    // 彩色 emoji 字体。前缀同时命中 `NotoColorEmoji.ttf` 与
    // `NotoColorEmojiFlags.ttf`（旗帜 = 双区域指示符合成，缺后者旗帜画不出来）。
    //
    // 曲名 / 故事板需要 emoji，所以必须加载。三个要点：
    //
    // 1. **不会污染默认字体**。这些字体只含 emoji、不含拉丁字母；
    //    [`PREFERRED_FAMILIES`] 里没有它们，所以 `set_sans_serif_family`
    //    绝不会挑中 —— 这正是当初严格过滤要防的那类事故（挑中
    //    `AndroidClock.ttf` 导致「全大写 + 数字消失」）。
    // 2. **代价远小于预想**：真机实测 `NotoColorEmoji.ttf` 2.81 MB，
    //    而**已经在加载**的 `NotoSansCJK-Regular.ttc` 有 32.4 MB。
    //    此前文档写「10 MB+」是错的。
    // 3. 走 glyphon 已有的彩色通道：`SwashContent::Color` →
    //    `ContentType::Color` → `color_atlas`（RGBA8），与 `mask_atlas`
    //    （R8）分开。不是新做的子系统。
    "NotoColorEmoji",
];

/// 优先挑选的 **家族名**（按顺序），用来钉死 fontdb 的三个 generic 家族。
/// 注意与 [`TEXT_FONT_PREFIXES`]（文件名）区分：这里比的是字体内部声明的家族名。
///
/// 无条件编译（与白名单同理，靠模块级 `allow(dead_code)` 兜住非 Android 非 test 构建）：
/// 「emoji 字体绝不能进这张表」是一条需要在桌面跑的安全约束，若 gate 掉
/// `cargo test --lib` 就永远验证不了。
const PREFERRED_FAMILIES: &[&str] = &[
    "Roboto",          // AOSP / Pixel
    "MiSans",          // 小米 / 澎湃
    "HarmonyOS Sans",  // 华为
    "Noto Sans",       // 通用
    "Droid Sans",      // 老 AOSP
    "Source Han Sans", // 思源
    "OPPO Sans",       // OPPO
    "sans-serif",      // 兜底
];

/// 字体扩展名白名单（只认这三种真实字体容器）。
///
/// 与 [`TEXT_FONT_PREFIXES`] 是 **AND** 关系：光前缀命中不够，光扩展名命中也不够。
/// 后者挡掉 `Roboto-Regular.ttf.bak` 这类备份文件——前缀规则挡不住它们。
pub const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc"];

/// 用户可追加的字体扫描配置。
///
/// 三个字段各自解决 [`TEXT_FONT_PREFIXES`] / `FONT_DIRS` / `PREFERRED_FAMILIES`
/// 编译期固定带来的三类缺口。**三者都需要**——只补前缀修不好症状，见
/// [`AndroidFontConfig::extra_families`]。
///
/// 三张内置表覆盖 AOSP/Pixel、小米、华为、OPPO、通用 Noto/思源，已知的主流
/// OEM 都在内。真正的缺口是**没听说过的 ROM**：某台设备的正文字体命名既不落在
/// 前缀表里、字体文件也不在系统目录里，用户就只能自己 `adb push` 再
/// [`GpuContext::load_font_file`](crate::gpu::GpuContext::load_font_file)
/// 手工加载——而那条路**钉不死 generic 家族**，于是「所有文字全大写、数字消失」
/// 照旧。本配置就是把那三张表从编译期挪到运行期。
///
/// # 生效时机（重要）
///
/// 必须在 **`App::new()` 之前**调用 [`configure`]。`install()` 是从
/// `App::new()` → `GpuContext::new()` → `TextContext::new()` 一路急切调下来的，
/// 中间**没有**任何留给用户的时机（用户既拿不到 `FontSystem` 句柄，也没有
/// 构造器钩子）。等 `App::new()` 返回再调 = 什么都不生效，且不报错。
///
/// 配置是**进程级**的，与 [`report()`] 用的全局同源。
///
/// # Examples
///
/// ```no_run
/// use vireo::text::android_fonts;
///
/// android_fonts::configure(|c| {
///     // 自带字体打进 APK 后从 app 私有目录加载
///     c.extra_dirs.push("/data/data/com.example.app/fonts".into());
///     c.extra_prefixes.push("GameFont".to_owned());
///     // 必须显式钉家族名，否则 generic 仍指向不存在的 "Arial"
///     c.extra_families.push("Game Font".to_owned());
/// });
/// let app = vireo::App::new();
/// # let _ = app;
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AndroidFontConfig {
    /// 额外的字体**文件名**前缀，与 [`TEXT_FONT_PREFIXES`] 取并集。
    ///
    /// 比的是文件名（`path.file_name()` 的前缀），不是字体内部声明的家族名。
    /// 与 `FONT_EXTENSIONS` 仍是 AND 关系。
    pub extra_prefixes: Vec<String>,

    /// 额外的字体扫描目录，与内置的 `/system/fonts`、`/product/fonts` 取并集。
    ///
    /// 用于 app 私有目录（`/data/data/<pkg>/fonts`）或用户从 `/sdcard` 放进来的
    /// 字体。递归深度与内置目录一致（3 层）。目录不存在时静默跳过，与内置
    /// 目录同策略。
    pub extra_dirs: Vec<std::path::PathBuf>,

    /// 额外的**优先家族名**，排在 `PREFERRED_FAMILIES` **之前**。
    ///
    /// # 为什么必须能设这个（而不是只设前缀）
    ///
    /// 目标症状是「所有文字全大写、数字消失」，成因**不是**字体文件没加载，
    /// 而是 `PREFERRED_FAMILIES` 全 miss → `set_sans_serif_family` 没被调用
    /// → fontdb 的 `family_sans_serif` 仍是默认的 `"Arial"` → Android 上没有
    /// 字体声明 `"Arial"` → `query` 返回 `None` → 回退到「排序后第一个 face」
    /// → 可能是装饰体。**只加前缀不钉家族，症状一模一样。**
    ///
    /// # 为什么排在前面而不是追加到最后
    ///
    /// 追加只能服务「我的 ROM 内置表全 miss」这一种场景。而「我想把 app 自带的
    /// 字体设成默认字体」同样常见——此时 Roboto 几乎必然存在，追加的规则永远
    /// 轮不到。排在前面两种场景都成立：内置字体可用但用户显式指定时听用户的，
    /// 内置全 miss 时用户的规则就是唯一的兜底。
    ///
    /// 代价：填错成装饰体家族名会抢走默认位置。但那是**显式要求**（与
    /// `load_font_file` 后指定家族名同性质），不算引擎的责任。
    pub extra_families: Vec<String>,
}

static CONFIG: std::sync::Mutex<AndroidFontConfig> = std::sync::Mutex::new(AndroidFontConfig {
    extra_prefixes: Vec::new(),
    extra_dirs: Vec::new(),
    extra_families: Vec::new(),
});

/// 追加 Android 字体扫描配置（进程级，**必须在 `App::new()` 之前**调用）。
///
/// 闭包形式：一次调用可改多个字段，避免多次调用之间的顺序困惑。
/// 可重复调用（增量合并，不会互相覆盖）。
///
/// 幂等性：同一次进程里先 configure 再 `App::new()` 是唯一正确用法；
/// `App::new()` 之后再调用**静默无效**（见 [`AndroidFontConfig`] 的「生效时机」）。
///
/// 非 Android 平台上本函数可正常调用，只是配置永远不会被读取（`install()` 按
/// target 门控），这样 app 代码可以跨平台免 `cfg` 编译。
pub fn configure(f: impl FnOnce(&mut AndroidFontConfig)) {
    let mut cfg = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut cfg);
}

/// 当前生效的配置快照（拷贝）。
///
/// 主要给测试与诊断用：确认 [`configure`] 确实写进去了。正常代码路径下应该
/// 读 [`report`] 而不是它。
pub fn current_config() -> AndroidFontConfig {
    CONFIG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// `install()` 的结果摘要。
///
/// 无条件编译（桌面单测 / 非 Android 上 `report()` 恒 `None`）。
///
/// HUD / 日志排查 Android 字体问题时是第一手线索：env_logger 走 android_logger
/// 后端难查，`report()` 可把「加载了几个文件 / 几个 face / 有没有失败 / 默认族是谁」
/// 直接显示在屏幕上。
///
/// # Platform
///
/// 仅在 `target_os = "android"` 的 `TextContext::new()` 路径里有实际值；其它平台
/// （或 `install()` 尚未执行完）`report()` 恒返回 `None`。
///
/// # Examples
///
/// ```no_run
/// if let Some(r) = vireo::text::android_fonts::report() {
///     log::info!("android fonts: {r}");
/// }
/// ```
#[derive(Debug, Clone)]
pub struct AndroidFontsReport {
    /// 白名单命中的字体文件数（`/system/fonts` + `/product/fonts`）。
    pub files: usize,
    /// 解析成功并加载到 fontdb 的 face 数（.ttc 会拆成多个）。
    pub faces: usize,
    /// 读取失败的文件数（TTC 分片或权限问题不影响整体可用性）。
    pub failed: usize,
    /// 钉死的 generic 家族名；`None` = 一个正文字族都没命中
    /// （症状：全大写文字、数字消失）。
    pub default_family: Option<String>,
}

impl std::fmt::Display for AndroidFontsReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // key=value 便于 logcat / log 检索；default 缺失显式标 <none> 而非空。
        write!(
            f,
            "files={} faces={} failed={} default={}",
            self.files,
            self.faces,
            self.failed,
            self.default_family.as_deref().unwrap_or("<none>")
        )
    }
}

static LOADED_REPORT: std::sync::Mutex<Option<AndroidFontsReport>> = std::sync::Mutex::new(None);

/// 字体安装摘要；未初始化时 `None`（非 Android 或 `install()` 尚未执行完）。
///
/// 与 `install` 的生命周期绑定：Android 上 `TextContext::new()`
/// 完成后恒为 `Some`，即便白名单里没有命中任何正文字体（此时 `default_family`
/// 为 `None`）。
pub fn report() -> Option<AndroidFontsReport> {
    LOADED_REPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 文件名是否命中「正文字体前缀」（内置 [`TEXT_FONT_PREFIXES`] 与用户
/// `extra_prefixes` 取并集）**且**命中 [`FONT_EXTENSIONS`]。
///
/// `extra_prefixes` 显式传参而非读全局 [`CONFIG`]：本函数是纯判定逻辑，
/// 可在桌面单测里覆盖「用户前缀」这条分支而不必与其它测试抢全局锁。
fn is_text_font_file_with(path: &Path, extra_prefixes: &[String]) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|name| {
            let stem_ok = TEXT_FONT_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
                || extra_prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix.as_str()));
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|ext| {
                    let ext = ext.to_ascii_lowercase();
                    FONT_EXTENSIONS.iter().any(|allowed| ext == *allowed)
                })
                .unwrap_or(false);
            stem_ok && ext_ok
        })
        .unwrap_or(false)
}

/// 只按内置白名单判定的便捷包装 —— **仅测试用**。
///
/// 生产路径一律走 [`is_text_font_file_with`]（带用户前缀）。本包装存在的意义是
/// 让「内置表的行为」在测试里有一个不含用户配置的入口。标 `cfg(test)` 而非靠
/// 模块级 `allow(dead_code)`：Android 非 test 构建没有那个 allow，留着会真的
/// 报 dead_code。
#[cfg(test)]
fn is_text_font_file(path: &Path) -> bool {
    is_text_font_file_with(path, &[])
}

/// 递归收集白名单字体文件。`depth` 是防符号链接成环的深度上限
/// （AOSP 字体目录实际是平的，留 3 层足够）。
#[cfg(target_os = "android")]
fn collect_font_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize, extra_prefixes: &[String]) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return; // 目录不存在（老系统 / 无 product 分区 / 用户写错路径）
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_font_files(&path, out, depth - 1, extra_prefixes);
        } else if is_text_font_file_with(&path, extra_prefixes) {
            out.push(path);
        }
    }
}

/// Android 上安装系统字体：按文件名白名单加载正文字体，
/// 并把 cosmic-text 的 generic 家族钉到 [`PREFERRED_FAMILIES`] 中第一个已加载的。
///
/// 用户配置（[`configure`]）在此生效：**目录**取并集、**前缀**取并集、
/// **家族名**排在 [`PREFERRED_FAMILIES`] 之前。
#[cfg(target_os = "android")]
pub(crate) fn install(font_system: &mut FontSystem) {
    let cfg = current_config();
    let db = font_system.db_mut();

    let mut files = Vec::new();
    for dir in FONT_DIRS {
        collect_font_files(Path::new(dir), &mut files, 3, &cfg.extra_prefixes);
    }
    for dir in &cfg.extra_dirs {
        collect_font_files(dir, &mut files, 3, &cfg.extra_prefixes);
    }
    // 排序保证 face 插入顺序确定 → cosmic-text 的兜底选择也确定（否则依赖
    // readdir 顺序，跨设备不可复现）。
    files.sort();
    files.dedup(); // 目录重叠时（如用户把系统目录又填进 extra_dirs）不重复加载
    let file_count = files.len();

    let mut failed = 0usize;
    for path in &files {
        // 失败多为 TTC 分片或权限问题，跳过即可：白名单命中的是**多个**候选字体，
        // 个别失败不影响整体可用性。
        if db.load_font_file(path).is_err() {
            failed += 1;
        }
    }
    let face_count = db.len();

    // 先收集已加载的家族名（`faces()` 借用 db，之后要可变借用）
    let families: Vec<String> = db
        .faces()
        .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
        .collect();

    // 用户家族名排在内置之前（理由见 AndroidFontConfig::extra_families）。
    let picked = cfg
        .extra_families
        .iter()
        .find(|want| families.iter().any(|have| have == *want))
        .cloned()
        .or_else(|| {
            PREFERRED_FAMILIES
                .iter()
                .find(|want| families.iter().any(|have| have == *want))
                .map(|s| (*s).to_owned())
        });

    // 用户配置的实际生效情况打进日志：report() 只报结果，用户配置没生效时
    // （写错前缀 / 目录不存在 / 家族名拼错）只能靠这里判断是没设还是设错了。
    if !cfg.extra_prefixes.is_empty()
        || !cfg.extra_dirs.is_empty()
        || !cfg.extra_families.is_empty()
    {
        log::info!(
            "vireo::text: android font config: {} extra prefix(es) {:?}, \
             {} extra dir(s) {:?}, {} extra family/families {:?}",
            cfg.extra_prefixes.len(),
            cfg.extra_prefixes,
            cfg.extra_dirs.len(),
            cfg.extra_dirs,
            cfg.extra_families.len(),
            cfg.extra_families,
        );
    }

    match picked {
        Some(family) => {
            // 必须设 **sans-serif** 这一条：cosmic-text 的 `Attrs::new()` 默认
            // `Family::SansSerif`，对应 fontdb 的 `family_sans_serif`（默认 "Arial"）。
            // Android 没有字体声明 "Arial" → query None → 回退到「排序后第一个
            // face」，白名单再干净也拦不住（见模块文档「做法」第 2 步）。
            db.set_sans_serif_family(&family);
            // 其余 generic 一并钉上，用户显式指定等宽/衬线族时也走 good font。
            db.set_monospace_family(&family);
            db.set_serif_family(&family);
            log::info!(
                "vireo::text: android fonts loaded: {file_count} files -> {face_count} faces \
                 ({failed} unreadable), sans-serif default = {family:?}"
            );
            // 记下加载摘要供 `report()` 查询。**不能只靠 log**：Android 上
            // env_logger 走 android_logger 后端，tag 是进程名，logcat 过滤极易错过，
            // 而「到底加载了几个字体 / 有没有 emoji 字体」恰恰是排查豆腐块的第一线索。
            //
            // 日志放在前面正是为了让下面的 `family` 能直接 move 而不用 clone。
            LOADED_REPORT
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(AndroidFontsReport {
                    files: file_count,
                    faces: face_count,
                    failed,
                    default_family: Some(family),
                });
        }
        None => {
            // 一个都没命中：保持 fontdb 默认，但必须说清楚，否则用户看到的
            // 「大写 + 无数字」现象无从排查。即便如此也要落摘要，
            // 否则 report() 的 None 会与「install() 没跑」混成一类。
            LOADED_REPORT
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(AndroidFontsReport {
                    files: file_count,
                    faces: face_count,
                    failed,
                    default_family: None,
                });
            log::warn!(
                "vireo::text: android no known UI font family among {face_count} faces \
                 from {file_count} whitelisted files ({failed} unreadable); \
                 default font may be missing or decorative \
                 (symptom: all-caps text, missing digits)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AndroidFontConfig, FONT_EXTENSIONS, PREFERRED_FAMILIES, TEXT_FONT_PREFIXES, configure,
        current_config, is_text_font_file, is_text_font_file_with,
    };
    use std::path::Path;

    /// 触碰全局 `CONFIG` 的测试共用这把锁。
    ///
    /// cargo 默认多线程并行跑测试，而这些测试都要「保存 → 改 → 读 → 还原」，
    /// 并发时后一个测试的 `saved` 会捕到前一个测试的中间态，还原时互相覆盖。
    /// 判别逻辑（`is_text_font_file_with`）刻意做成纯函数、不读全局，正是为了
    /// 把需要这把锁的测试压到最少。
    static CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn whitelist_accepts_known_text_fonts() {
        for name in [
            "Roboto-Regular.ttf",
            "Roboto-Medium.ttf",
            "DroidSans.ttf",
            "DroidSansFallback.ttf",
            "NotoSansCJK-Regular.ttc",
            "NotoSerif-Regular.ttf",
            "MiSansVF.ttf",
            "SourceHanSans-Regular.otf",
            "HarmonyOS_Sans_SC_Regular.ttf",
            "OPPO_Sans_Regular.ttf",
        ] {
            assert!(
                is_text_font_file(Path::new(name)),
                "{name} should be whitelisted"
            );
        }
    }

    #[test]
    fn whitelist_accepts_emoji_fonts() {
        // 曲名 / 故事板需要 emoji。彩色字体必须能进白名单，否则全是豆腐块。
        for name in [
            "NotoColorEmoji.ttf",
            "NotoColorEmojiFlags.ttf", // 旗帜 emoji = 双区域指示符合成
        ] {
            assert!(
                is_text_font_file(Path::new(name)),
                "{name} should be whitelisted (emoji support)"
            );
        }
    }

    #[test]
    fn emoji_family_is_never_a_generic_default() {
        // 回归防护：彩色 emoji 字体只含 emoji、不含拉丁字母。若它被选成
        // sans-serif 默认族，症状就是当初那个「所有文字全大写、数字消失」。
        // `install()` 只在 PREFERRED_FAMILIES 里挑，所以断言它不在那张表里即可。
        const EMOJI_FAMILY: &str = "Noto Color Emoji";
        assert!(
            !PREFERRED_FAMILIES.contains(&EMOJI_FAMILY),
            "emoji 字体绝不能成为 generic 默认族（它没有拉丁字形）"
        );
        // 反向保证：拉丁正文字族仍在表里。
        assert!(PREFERRED_FAMILIES.contains(&"Roboto"));
    }

    #[test]
    fn whitelist_rejects_decorative_and_non_text_fonts() {
        // 这几个是真机上真实存在的「坑」：全量加载时 cosmic-text 会挑中它们。
        for name in [
            "ComingSoon.ttf",   // 大写装饰体 → 全大写症状
            "AndroidClock.ttf", // 只有数字 → 字母消失
            "CutiveMono.ttf",
            "DancingScript-Regular.ttf",
            "DancingScript-Bold.ttf",
            "ComingSoon-Bold.ttf",
            "GoogleDroidlogo.ttf", // logo 字形
            "Roboto-Regular.ttf.bak",
        ] {
            assert!(
                !is_text_font_file(Path::new(name)),
                "{name} must NOT be whitelisted"
            );
        }
    }

    #[test]
    fn whitelist_needs_prefix_and_extension() {
        // 前缀命中但扩展名不对（备份文件 / 目录）→ 拒绝
        for name in [
            "Roboto-Regular.ttf.bak", // 前缀命中，扩展名 .bak → 拒
            "Roboto-Regular",         // 前缀命中，无扩展名 → 拒
            "Roboto-Regular.txt",
        ] {
            assert!(
                !is_text_font_file(Path::new(name)),
                "{name} must NOT be whitelisted (extension gate)"
            );
        }
        // 扩展名命中但前缀不对 → 拒绝
        for name in ["ComingSoon.ttf", "CutiveMono.ttf"] {
            assert!(
                !is_text_font_file(Path::new(name)),
                "{name} must NOT be whitelisted (prefix gate)"
            );
        }
        // 大写扩展名也认
        assert!(is_text_font_file(Path::new("MiSansVF.TTF")));
        // 非 UTF-8 / 空文件名不炸
        assert!(!is_text_font_file(Path::new("")));
        assert!(!is_text_font_file(Path::new(".")));
    }

    #[test]
    fn whitelist_constants_are_distinct_and_non_empty() {
        assert!(!TEXT_FONT_PREFIXES.is_empty());
        assert!(!FONT_EXTENSIONS.is_empty());
        for list in [TEXT_FONT_PREFIXES, FONT_EXTENSIONS] {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), list.len(), "entries must be unique");
        }
    }

    #[test]
    fn extra_prefixes_extend_the_whitelist_without_loosening_it() {
        let extra = vec!["MyOEMFont".to_owned(), "OtherVendor".to_owned()];
        // 用户前缀让「内置表没有的正文字体」通过
        for name in [
            "MyOEMFont-Regular.ttf",
            "MyOEMFont-Bold.otf",
            "OtherVendorCJK.ttc",
        ] {
            assert!(
                is_text_font_file_with(Path::new(name), &extra),
                "{name} should pass with a user prefix"
            );
            // 不给 extra 时必须拒绝 —— 这才证明是前缀在起作用，而不是规则本来就松
            assert!(
                !is_text_font_file(Path::new(name)),
                "{name} must NOT pass without the user prefix"
            );
        }
        // 关键安全性质：用户前缀**只加不减**，装饰体仍然被拒
        for name in [
            "ComingSoon.ttf",
            "AndroidClock.ttf",
            "DancingScript-Regular.ttf",
            "CutiveMono.ttf",
            "GoogleDroidlogo.ttf",
        ] {
            assert!(
                !is_text_font_file_with(Path::new(name), &extra),
                "{name} must NOT pass even with user prefixes (decorative)"
            );
        }
        // 扩展名门槛对用户前缀同样生效
        assert!(!is_text_font_file_with(
            Path::new("MyOEMFont.ttf.bak"),
            &extra
        ));
    }

    #[test]
    fn configure_writes_all_three_tables() {
        let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = current_config();
        configure(|c| {
            c.extra_prefixes.push("GameFont".to_owned());
            c.extra_dirs.push("/data/data/com.example.app/fonts".into());
            c.extra_families.push("Game Font".to_owned());
        });
        let got = current_config();
        // 还原，避免影响同进程内其它潜在读者
        configure(|c| *c = saved.clone());

        assert_eq!(got.extra_prefixes, vec!["GameFont".to_owned()]);
        assert_eq!(
            got.extra_dirs,
            vec![std::path::PathBuf::from("/data/data/com.example.app/fonts")]
        );
        assert_eq!(got.extra_families, vec!["Game Font".to_owned()]);
        assert_eq!(current_config(), saved, "restore must be exact");
    }

    #[test]
    fn configure_is_additive_across_calls() {
        let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = current_config();
        configure(|c| c.extra_prefixes.push("Aaa".to_owned()));
        configure(|c| c.extra_prefixes.push("Bbb".to_owned()));
        let got = current_config();
        configure(|c| *c = saved.clone());
        assert_eq!(
            got.extra_prefixes,
            vec!["Aaa".to_owned(), "Bbb".to_owned()],
            "repeated configure calls must merge, not overwrite"
        );
    }

    #[test]
    fn default_config_is_empty_and_means_builtin_behavior() {
        let _guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = current_config();
        configure(|c| *c = AndroidFontConfig::default());
        let got = current_config();
        assert_eq!(got, AndroidFontConfig::default());
        // 空配置必须等价于纯内置行为：即 is_text_font_file 的无 extra 版本
        assert!(!is_text_font_file_with(
            Path::new("MyOEMFont.ttf"),
            &got.extra_prefixes
        ));
        assert!(is_text_font_file_with(
            Path::new("Roboto-Regular.ttf"),
            &got.extra_prefixes
        ));
        configure(|c| *c = saved);
    }
}
