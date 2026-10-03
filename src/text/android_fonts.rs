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
//! 非正文字体：`ComingSoon.ttf`（大写装饰体）、`CutiveMono.ttf`、`DancingScript-Regular.ttf`、
//! `AndroidClock.ttf`（只有数字）等。全量加载后 cosmic-text 的默认字体解析会挑中它们：
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
//! # 做法
//!
//! 仍然全量加载（保住 Noto/MiSans 的 CJK 与各文种覆盖 —— 对中文游戏引擎是刚需），
//! 但用 `set_sans_serif_family` 把 generic 家族**钉到 [`PREFERRED_FAMILIES`]
//! 中第一个已加载的**，让 `query` 确定性地命中。

use cosmic_text::FontSystem;

/// 优先挑选的正文字体家族（按顺序）。都是各平台常见的 UI 正文字体。
const PREFERRED_FAMILIES: &[&str] = &[
    "Roboto",          // AOSP / Pixel
    "MiSans",          // 小米 / 澎湃
    "Droid Sans",      // 老 AOSP
    "Noto Sans",       // 通用
    "HarmonyOS Sans",  // 华为
    "Source Han Sans", //思源
    "OPPO Sans",       // OPPO
    "sans-serif",      // 兜底
];

/// Android 上安装系统字体：全量加载 `/system/fonts` + `/product/fonts`，
/// 并把 cosmic-text 的默认字体家族钉到 [`PREFERRED_FAMILIES`] 中第一个已加载的。
pub(crate) fn install(font_system: &mut FontSystem) {
    let db = font_system.db_mut();
    db.load_fonts_dir("/system/fonts");
    db.load_fonts_dir("/product/fonts");

    // 先收集已加载的家族名（`faces()` 借用 db，之后要可变借用）
    let families: Vec<String> = db
        .faces()
        .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
        .collect();

    let picked = PREFERRED_FAMILIES
        .iter()
        .find(|want| families.iter().any(|have| have == *want))
        .copied();

    match picked {
        Some(family) => {
            // 必须设 **sans-serif** 这一条：cosmic-text 的 `Attrs::new()` 默认
            // `Family::SansSerif`，对应 fontdb 的 `family_sans_serif`（默认 "Arial"）。
            // Android 没有字体声明 "Arial" → query None → 回退到装饰体。
            // 桌面（Windows）恰好装了 Arial，所以这个 bug 只在 Android 暴露。
            db.set_sans_serif_family(family);
            // 其余 generic 一并钉上，用户显式 `TextDef::with_family` 时也走 good font。
            db.set_monospace_family(family);
            db.set_serif_family(family);
            log::info!(
                "vireo::text: android fonts loaded ({} faces), sans-serif default = {family:?}",
                families.len()
            );
        }
        None => {
            // 一个都没命中：保持 fontdb 默认（可能挑到装饰体），但必须说清楚，
            // 否则用户看到的「大写 + 无数字」现象无从排查。
            log::warn!(
                "vireo::text: android no known UI font family found among {} loaded faces; \
                 default font may be a decorative system font \
                 (symptom: all-caps text, missing digits)",
                families.len()
            );
        }
    }
}
