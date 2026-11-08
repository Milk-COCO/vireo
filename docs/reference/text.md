# 文字（Text）

> 面向 vireo 使用者。适用版本：0.2.0。
>
> 这一篇是**地图与共同概念**。具体内容按主题分在几篇里，每篇都能单独读：

| 文档 | 讲什么 | 什么时候看 |
|---|---|---|
| [text-api.md](text-api.md) | **能定义什么、怎么画**：`TextDef` / `TextOverride` / `TextPart` / HUD 分段 / 绘制入口 / 裁剪与变换 / 度量 / `StableText` | 写绘制代码 |
| [text-cache.md](text-cache.md) | **自动管理 vs 自行管理**：两层缓存、默认行为、预算、自动压缩、钉子、预光栅化、跨进程持久化、观测与诊断 | 调性能、控显存、做出加载界面 |
| [text-fonts.md](text-fonts.md) | **字体**：系统字体、加载自定义字体、family/weight、fallback、Android 白名单、字体与持久化的关系 | 中文/多语言、自带字体 |
| [text-style.md](text-style.md) | **样式**：描边、多层、投影、发光、内描边、渐变字、几何描边 | 要花哨效果 |
| [text-perf.md](text-perf.md) | **性能画像与自查**：CPU/GPU 双列实测、吞吐上限、瓶颈归因、复现命令 | 文字拖慢帧率 |

## 5 分钟上手

```rust
use vireo::prelude::*;

// 1) 最简单：塞进批次（屏幕空间，不吃批次变换）
let mut batch = DrawBatch::new();
draw_text(
    &mut batch.texts,
    "Hello 世界",
    Pos::new(24.0, 32.0),
    TextDef::default().font_size(28.0),
    TextOverride::from_color(WHITE),
);
win.draw(Color::new(0.06, 0.06, 0.08, 1.0), &[&batch]);
```

需要跟随父变换、HUD 分段、大段静态文字时，分别用 `batch.text(..)` / `batch.text_parts(..)` /
`StableText` —— 对照见 [text-api.md](text-api.md) 的「绘制入口」。

## 三层模型

文字在 vireo 里分三层，**各管一件事，互不替代**：

| 层 | 管什么 | 入口 | 缓存 |
|---|---|---|---|
| **排版**（typeset） | 字体挑选、断行、字形下标与步进 | `draw_text` / `TextEntryList` / `StableText` | [`typeset_ttl` / `typeset_max_entries`](text-cache.md#1-排版缓存) |
| **样式**（style） | 描边、投影、发光、内阴影、渐变、几何描边 | `TextStyle`（见 [text-style.md](text-style.md)） | 按 `(字形, 尺寸, 带宽档)` 复用图集条目 |
| **光栅化**（glyph cache） | 字形位图 / 距离场进图集 | `gpu.text().glyphs()` | [`预光栅化 + 钉子 + 预算 + 持久化`](text-cache.md#2-字形缓存) |

一句话区分两个缓存：

- **排版缓存**管"这段字排一次"（`harfrust` 排版成本，典型 5–30 ms/长串）；
- **字形缓存**管"这个字形画一次"（`swash`/SDF 光栅化成本，典型 0.05–0.5 ms/字形）。

两者都**自动**工作：写 `draw_text` 就行。想接管其一，读
[text-cache.md](text-cache.md#3-自动还是自己管)。

## 术语表

文档和代码里这几个词是**互不混用**的：

| 词 | 中文 | 指什么 | 反例（别混） |
|---|---|---|---|
| `typeset` | 排版 | 文字的布局结果：shape 阶段产物、`Buffer`、行宽、字形序列 | 不是 UI 布局 |
| `layout` | 布局 | UI 的摆放（窗口、面板、对齐），本库只有 `layout_scale` 这类概念 | 不是文字排版 |
| `shape` | 图形/几何 | 矩形、圆、贝塞尔、SDF 图元 | 不是文字 |
| `glyph` | 字形 | 一个字符在某个字体下的一张位图/距离场 | 不是字符 |
| `band` | 带宽 | 距离场能表示的最远距离（决定能画多粗的描边） | 不是描边宽度 |
| `atlas` | 图集 | 字形位图拼成的大纹理（两张：R8 掩码/SDF、RGBA8 彩色） | 不是整帧纹理 |

历史包袱：文字相关的 API 早期叫 `shape_cache_*`，0.2.0 起改名为 `typeset_*`；旧名保留一个
版本作为兼容壳，下一版删除。

## 位置与像素

三件事容易混：

- `Pos` / `TextDef::font_size` / `TextOverride::clip` 都是**逻辑像素**；渲染时按窗口的
  `layout_scale()`（DPI）换算成物理像素。
- `TextBounds`（`TextOverride::clip` 与 `DrawBatch::text_clip`）也是逻辑像素，默认值
  `i32::MIN..i32::MAX` 表示**不裁剪**。
- 行距**固定**为 `font_size × 1.2`，当前没有可配入口（见 [text-api.md §9](text-api.md#9-已知限制)）。

## 相关示例

| 示例 | 演示 |
|---|---|
| `cargo run --example text_typeset_cache` | 排版缓存：TTL / 条数上限 / 命中率 / 动态串压力 |
| `cargo run --example text_cache` | 字形缓存：预算 / `hold` / `trim` / `compact` / `save` / `load` / 自动压缩 |
| `cargo run --example text_style` | 样式：描边、投影、发光、多层、渐变、几何描边 |
| `cargo run --example text_hud` | HUD 分段 + 数字窄路径 |
| `cargo run --example text_attrs` | 字体族 / 字重 / 斜体 |
| `cargo run --example text_font` | 加载自定义字体 + fallback |
| `cargo run --example text_clip` | 逐条裁剪 + 对齐 / `max_width` |
| `cargo run --example text_batch_clip` | 批次裁切（父 mask + `clips_children`） |
| `cargo run --example text_transform` | 文字跟随批次变换 |
| `cargo run --example text_profile` | 排版/光栅化的分项耗时 |
| `cargo run --example text_measure` | `measure_text` / `measure_text_visual` |
| `cargo run --example text_stress` | 大文本量压测 |

## 其它相关

- **文字自定义材质**（在文字上跑自己的 shader）：[material.md](material.md) 的 `VIREO_TARGET_TEXT`。
- **帧率/限流**：[throttling.md](throttling.md)。
- **渐变（形状）**：[gradient.md](gradient.md)、[line-gradient.md](line-gradient.md)。
