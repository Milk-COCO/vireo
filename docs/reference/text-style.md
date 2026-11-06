# 文本样式（描边 / 投影 / 发光 / 内阴影 / 浮雕）

> 面向 vireo 使用者。适用版本：0.2.0。
>
> 相关：[渐变](gradient.md)（形状渐变；文字填充的渐变排在后续版本）。

## 1. 模型：一个原语 + 一个列表

vireo 的字形现在有两种表示：

| 图集内容 | 用于 | 数据 |
|---|---|---|
| 覆盖度（`content_type = 1`） | 无样式文本（默认） | 8bit alpha |
| **有符号距离场**（`content_type = 2`） | 带样式文本 | 8bit，`d` 以设备像素计，轮廓处 0 |

样式只有一条原语：**对距离场取带软边的阈值**。

```
cov = coverage(d - threshold, soft)      // soft = 0 → 1px 解析 AA；否则 erf 软边
out = over(out, paint * cov)             // 预乘合成
```

`TextStyle` 就是「一组层 + 一个填充」，层从下往上画，填充最后画。

## 2. 用法

```rust
use vireo::prelude::*;
use std::sync::Arc;

// 外描边
let def = TextDef {
    style: Some(Arc::new(TextStyle::outline(2.0, Color::new(0.05, 0.06, 0.09, 0.9)))),
    ..TextDef::default().font_size(40.0)
};
draw_text(&mut batch.texts, "标题", Pos::new(40.0, 60.0), def, TextOverride::new());

// 软投影（顺带叠一层硬描边）
let style = TextStyle::shadow((4.0, 4.0), 3.0, BLACK)
    .with_layer(TextLayer::hard(2.0, TextPaint::solid(BLACK)));

// 发光
let glow = TextStyle::glow(6.0, Color::new(0.35, 0.75, 1.0, 0.55));

// 内描边 + 自定义填充色
let inner = TextStyle::inner_outline(2.0, ORANGE).with_fill(TextPaint::solid(Color::new(0.08,0.09,0.12,1.0)));

// 浮雕
let emboss = TextStyle::emboss(1.0, WHITE, BLACK).with_fill(TextPaint::solid(BLUE));

// 渐变字：`in_rect` 决定渐变跨满哪个页面矩形（不传 = 逐字形）
let grad = TextGradient::vertical(Color::new(1.0, 0.75, 0.3, 1.0), Color::new(0.9, 0.2, 0.5, 1.0))
    .in_rect(40.0, 60.0, 320.0, 56.0);
let def = TextDef {
    style: Some(Arc::new(TextStyle::new().with_fill(TextPaint::Gradient(grad)))),
    ..TextDef::default().font_size(48.0)
};
```

**单位是像素意图**（与窗口 API 同一套）：裸数值 / `Dp` = vireo 逻辑像素，`Px` = 物理像素。
prepare 阶段按 `dpi_scale` 折算成设备像素 ⇒ **DPI 变化时描边的视觉宽度不变**。

## 3. 层参数

| 字段 | 含义 |
|---|---|
| `threshold` | 阈值位置：`> 0` 字形外侧（外描边/投影）、`< 0` 内侧（内描边）、`0` = 轮廓 |
| `softness` | 软边 ≈ 模糊半径；`0` = 硬边（1px 解析 AA） |
| `offset` | 采样偏移（设备像素意图）—— 投影/浮雕靠它 |
| `paint` | `Solid(Color)` / `Gradient(TextGradient)` / `Texture { uv, rect }`；渐变与纹理都靠 `rect` 决定跨满哪个页面矩形，不传则逐字形 |
| `blend` | `Over`（默认）/ `Add` |

常见效果的参数组合：

| 效果 | 参数 |
|---|---|
| 外描边 | `threshold = +w, softness = 0` |
| 内描边 | `threshold = -w` |
| 硬投影 | `threshold = 0, offset = (dx,dy), softness = 0` |
| 软投影 | `threshold = 0, offset, softness = blur` |
| 发光 | `offset = 0, softness = blur` |
| 多层描边 | 多个 `threshold = +w1, +w2…`（宽的先画） |
| 浮雕 | 两个 `threshold = 0`、`offset = ±(1,1)`、一亮一暗 |
| 内阴影 | `threshold = -w, offset = +d, softness = blur` |
| **几何描边（真 miter）** | `TextStyle::vector_outline(TextStroke::new(w, c).join(TextJoin::Miter))` |

### 3.1 距离场描边 vs 几何描边

| | 距离场描边（`TextStyle::outline`） | 几何描边（`TextStyle::vector_outline`） |
|---|---|---|
| 连接方式 | 天生**圆角** | `Miter` / `Bevel` / `Round`，`miter_limit` 超过就退化 bevel |
| 宽度变化 | 免费（同一份距离场） | 每种宽度一份图集数据 |
| 开销 | 低（片元取阈） | 每条描边组合多一份光栅化 |
| 适合 | 99% 的场景：文字描边、投影、发光 | SVG 观感、尖角必须精确的标题字 |

几何描边画在所有图层与 fill **之下**，所以两者可以混用（例如几何描边 + 距离场投影）。

## 4. 为什么软边一次采样就够


距离场下，"高斯模糊过的边缘"**精确**是误差函数：`cov = Φ(-d/σ)`。所以软投影/发光
**不需要多 tap** —— 只有"模糊半径 ≫ 字形特征尺寸"时才有可见偏差。

## 5. 边界与降级

| 输入 | 行为 |
|---|---|
| `style: None` / 空图层且无 fill | 完全走旧路径（覆盖度图集），**零额外开销** |
| 图层数 > 8 | 截断到 8 + warn-once |
| 外扩 > 32px | 距离场带宽钳到 32 + warn-once（效果变淡，不崩） |
| `TextPaint::Texture` | 支持：`set_texture` 的那张批贴图 + uv 矩形 + 页面矩形；不传 rect 则逐字形 |
| `TextPaint::Gradient` | 支持：`in_rect(..)` 跨满页面矩形，不给则逐字形 |
| COLRv1 彩色字形（emoji） | **支持**：从 alpha 反推距离场（8SSEDT）+ 双 quad —— 描边/投影垫在底下，emoji 保留自己的颜色；此时 `style.fill` 被忽略 |
| 自定义材质 | 引擎图层不生效（材质替换 FS）；`material_main` 里可用基色与 UV 自绘 |
| `measure_text` | **不变**（样式不影响排版） |
| MRT / stencil / Area 裁切 | 不受影响（与普通文本同一条路径） |
| 空格 / 零轮廓字形 | 不生成距离场、不占图集 |

## 6. 性能与内存

- **只有带样式的字形**生成距离场；无样式文本一个字节都不多。
- 距离场按字形缓存，键是 `(字体, 字形, 字号, variations, band)` —— 同一 band 的
  多套样式共用一份数据（band 是"最大外扩"，不是具体样式）。
- 图集仍是**同一张 R8**（SDF 与覆盖度靠 `content_type` 区分）⇒ 零格式改动。
- 内存量级：32px 字形 + band 4 ⇒ 40×40×1B ≈ **1.6KB/字形**（1000 字形 ≈ 1.6MB）。
- **可观测**：`GpuContext::text_atlas_stats()` 返回 `TextAtlasStats`
  （`mask_size` / `color_size` / `mask_bytes` / `color_bytes` / `total_bytes()`）——
  样式文本不会进彩色图集（有 GPU 用例盯着这条不变量）。
- 片元：每层一次采样（层数 ≤ 8），只在 `content_type == 2` 的分支里。
- 生成成本：从轮廓算精确距离（线/二次/三次贝塞尔 Newton + 按段 bbox 剪枝），
  首次绘制该字形时一次性支付。

## 7. 实测（`tests/gpu_bench.rs`）

| 用例 | 结果 |
|---|---|
| 描边长在字形外 | 无样式 404 px → 带 3px 描边 967 px，其中 556 px 为描边色 |
| 同色描边不压暗填充 | 258 个深内部像素与无样式版本逐像素一致（±3） |
| 软投影方向 | 蓝色投影 958 px，重心 (47.5, 61.9)，在文字起点 (30,30) 的右下 |
| 渐变填充 | 页面矩形参照系：上部 R/B = 1.32（偏红）、下部 0.56（偏蓝） |
| 纹理填充 | 左右红蓝贴图：左 R/B = 89089/24005，右 0/35868 |
| 彩色字形（emoji）描边 | 无样式 3253 px → 4px 描边 4595 px，其中 2145 px 为描边色 |
| 图集不变量 | 样式绘制前后彩色图集不变（R8 256²=65536B，彩色 256²=262144B） |
| 几何描边 | 10px 描边让纵向跨度恰好 +10px；覆盖 675 → 1771 px（round）/ 1753 px（miter） |
| zeno stroker 连接 | 单测确认 `Miter` 确实比 `Round` 在尖角处更高 |
| MRT 中的文本 | 目标 0 正常写入、其它目标保留 |

复跑：`cargo test --release --test gpu_bench text_style -- --ignored --test-threads=1 --nocapture`

## 8. 可跑示例

`cargo run --example text_style` —— 七个示例组（描边/多层/投影/发光/内描边/浮雕/全部），
`←→` 改描边宽度，`1..7` 切组。

## 9. 尚未支持（后续版本）

- MSDF（跨尺度不重栅格）—— 当前按设备像素尺寸生成，缩放会重新生成（与无样式文本一致）。
