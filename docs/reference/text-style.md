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
```

**单位是像素意图**（与窗口 API 同一套）：裸数值 / `Dp` = vireo 逻辑像素，`Px` = 物理像素。
prepare 阶段按 `dpi_scale` 折算成设备像素 ⇒ **DPI 变化时描边的视觉宽度不变**。

## 3. 层参数

| 字段 | 含义 |
|---|---|
| `threshold` | 阈值位置：`> 0` 字形外侧（外描边/投影）、`< 0` 内侧（内描边）、`0` = 轮廓 |
| `softness` | 软边 ≈ 模糊半径；`0` = 硬边（1px 解析 AA） |
| `offset` | 采样偏移（设备像素意图）—— 投影/浮雕靠它 |
| `paint` | `Solid(Color)`（P1）；`Gradient`/`Texture` 已进类型但当前**降级为纯色**并 warn-once |
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

## 4. 为什么软边一次采样就够

距离场下，"高斯模糊过的边缘"**精确**是误差函数：`cov = Φ(-d/σ)`。所以软投影/发光
**不需要多 tap** —— 只有"模糊半径 ≫ 字形特征尺寸"时才有可见偏差。

## 5. 边界与降级

| 输入 | 行为 |
|---|---|
| `style: None` / 空图层且无 fill | 完全走旧路径（覆盖度图集），**零额外开销** |
| 图层数 > 8 | 截断到 8 + warn-once |
| 外扩 > 32px | 距离场带宽钳到 32 + warn-once（效果变淡，不崩） |
| `TextPaint::Gradient` / `Texture` | 当前降级为纯色（渐变取首停点、纹理取白）+ warn-once |
| COLRv1 彩色字形（emoji） | 不支持样式：按原样绘制（彩色位图没有单通道距离场） |
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
- 片元：每层一次采样（层数 ≤ 8），只在 `content_type == 2` 的分支里。
- 生成成本：从轮廓算精确距离（线/二次/三次贝塞尔 Newton + 按段 bbox 剪枝），
  首次绘制该字形时一次性支付。

## 7. 实测（`tests/gpu_bench.rs`）

| 用例 | 结果 |
|---|---|
| 描边长在字形外 | 无样式 404 px → 带 3px 描边 967 px，其中 556 px 为描边色 |
| 同色描边不压暗填充 | 258 个深内部像素与无样式版本逐像素一致（±3） |
| 软投影方向 | 蓝色投影 958 px，重心 (47.5, 61.9)，在文字起点 (30,30) 的右下 |
| MRT 中的文本 | 目标 0 正常写入、其它目标保留 |

复跑：`cargo test --release --test gpu_bench text_style -- --ignored --test-threads=1 --nocapture`

## 8. 可跑示例

`cargo run --example text_style` —— 七个示例组（描边/多层/投影/发光/内描边/浮雕/全部），
`←→` 改描边宽度，`1..7` 切组。

## 9. 尚未支持（后续版本）

- 文字**渐变 / 纹理填充**（类型已公开，当前降级为纯色）。
- 逐段样式（`TextPart::xxx_def(_, def)` 的 `def.style` 目前按行级样式处理）。
- miter/bevel 连接的几何描边（距离场描边天然是圆角连接）。
- MSDF（跨尺度不重栅格）—— 当前按设备像素尺寸生成，缩放会重新生成（与无样式文本一致）。
