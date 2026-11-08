# 文字：字体

> 本文负责**字从哪来、怎么换、为什么显示成方块**。
> 绘制 API 见 [text-api.md](text-api.md)；缓存见 [text-cache.md](text-cache.md)。

## 1. 默认字体

`GpuContext` 创建时会：

1. `fontdb::Database::load_system_fonts()` —— 载入系统字体（Windows 的 Segoe UI / 宋体 /
   微软雅黑、macOS 的 PingFang、Linux 的 fontconfig 结果等）；
2. **Android**：额外按白名单扫描系统字体目录（见 §5）。

所以绝大多数机器上中文可以直接画。**没有对应字形时** cosmic-text 会在已加载字体里找
fallback；全都没有该字符 → 画不出（空白或方块）。

## 2. 加载自己的字体

```rust
// 字节
gpu.text().load_font(include_bytes!("assets/NotoSansSC-Regular.otf"));
// 或路径（失败返回 Err(String)）
gpu.text().load_font_file("assets/NotoSansSC-Regular.otf")?;
```

加载之后：

- 新字体进入 fontdb，成为 fallback 的候选；命中与否由 cosmic-text 的字体匹配决定；
- **排版缓存与逐字解析缓存会被清空**（fallback 可能变了），下一帧重新 shape；
- **`StableText` 句柄不受影响**：它们已经 shape 过并自己持有 `Buffer`，内容不会变 ——
  换字体后旧的稳定句柄不会自动改用新字体，需要重建。

要**指定**用哪个字体，用 family：

```rust
let def = TextDef::default()
    .font_size(24.0)
    .with_family(Family::Name("Noto Sans SC"));
```

`Family` 是 cosmic-text 的类型，常见写法：`Family::Name("…")`、`Family::Monospace`、
`Family::Serif`。名字用的是**字体内部声明的家族名**，不是文件名。

## 3. 字重 / 斜体

```rust
let bold = def.clone().with_weight(Weight::BOLD);
let italic = def.clone().with_style(Style::Italic);
```

`with_family` / `with_weight` / `with_style` 都写进 `TextDef.attrs`（`AttrsOwned`）。
他们参与**排版缓存的键**：换字重就是另一段排版。

没有对应字重的字体文件时，由 cosmic-text 决定是否合成（不同平台表现不同）；要严格一致就
自己加载对应的 Bold/Italic 文件。

## 4. 字体身份与跨进程持久化

`VRTX` 图集文件里存的不是 `fontdb::ID`（那是进程内 slotmap 键，跨启动必变），而是：

- **postscript 名** + **字体数据摘要**（长度 + 头尾各 64 KiB + 中间等距 64 KiB 采样）。

装载时按名字找候选面、再验摘要；**换了字体文件（哪怕同名不同版本）摘要就不匹配**，
该字体的条目整条跳过并重新光栅化 —— 这是正确行为，不是 bug。摘要采样是为了省时间：
整份哈希在中文字体上要 20 ms/个，采样后约 0.2 ms。

## 5. Android 字体白名单

Android 的系统字体目录里有大量文件（含 emoji、符号字体），全量加载既慢又可能让字体匹配
选错。默认只加载白名单，可用宏参数扩展：

```rust
#[vireo::main(fonts = |cfg| {
    cfg.extra_prefixes.push("NotoSansSC".into());          // 文件名前缀
    cfg.extra_dirs.push("/data/data/com.example/fonts".into()); // 额外扫描目录
    cfg.extra_families.push("Noto Sans SC".into());        // 优先家族名（排在默认之前）
})]
async fn main(app: App) { /* ... */ }
```

三个字段（`AndroidFontConfig`）：

| 字段 | 含义 | 说明 |
|---|---|---|
| `extra_prefixes` | 额外的**文件名前缀**白名单 | 与内置前缀、与 `FONT_EXTENSIONS` 都是 AND |
| `extra_dirs` | 额外扫描目录 | 与 `/system/fonts`、`/product/fonts` 取并集，递归 3 层，不存在就跳过 |
| `extra_families` | 额外的**优先家族名** | 排在默认优先列表之前；用于修"全大写/数字消失"这类匹配错位 |

也可以不用宏，直接调 `vireo::text::android_fonts::configure(|cfg| { .. })`，但必须在
`App` 构造**之前**。

## 6. 彩色字形（emoji / COLRv1）

彩色字形进**独立的 RGBA8 图集**，不进 R8 距离场图集。样式（描边/投影）只作用于它的"形状"
（从 alpha 反推的距离场），**不会改它自己的颜色** —— 颜色由字形自带。

COLRv1 由内置光栅化器（`skrifa` + `zeno`）实现，桌面可用环境变量 `VIREO_COLR1_FORCE=1`
强制走 v1 路径调试。

## 7. 常见问题

| 现象 | 原因 | 处理 |
|---|---|---|
| 中文/日文显示成方块 | 没有字体含这些字形 | `load_font` 一个 CJK 字体；或确认系统有 |
| 换字体后没生效 | 排版缓存/句柄已定型 | 新文字自然生效；`StableText` 要重建；必要时 `clear_typeset_cache()` |
| 数字/字母全大写、数字消失（Android） | 字体匹配选中了错误的字体 | `extra_families` 设置优先家族名 |
| 粗体不明显 | 没有对应字重的字体文件 | 加载 Bold 文件并用 `with_weight(BOLD)` |
| 加载字体后第一帧卡 | 新字体要重新 shape | 在加载界面完成 `load_font` + `prewarm` |
| emoji 是黑白的 | 字体只有单色 emoji | 换含 COLRv1 的 emoji 字体；本库不支持彩色位图 emoji（CBDT） |

## 8. 限制

- 只支持 **TrueType / OpenType 轮廓**（含 COLRv1）。位图 emoji（CBDT/sbix）不支持。
- 可变字体按默认实例渲染，不暴露轴（weight/width 连续变化）接口。
- 字体是**进程级**的：`GpuContext` 内所有窗口共享同一份 fontdb。
- `load_font` 之后旧 `StableText` 不会自动更新（见 §2）。
