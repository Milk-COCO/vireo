# 折线沿线渐变

> 面向 vireo 使用者。目标读者：想画「一条线自己从头到尾变色（或变透明）」的人。
> 适用版本：0.2.0。

vireo 的折线有两个入口：

| 入口 | 一个折线是几次 draw | 颜色 |
|---|---|---|
| `draw_line_chain(batch, points, thickness, color)` | 1 个实例 | 单色（`color`） |
| **`draw_line_chain_colors(batch, points, thickness, colors)`** | **1 个实例** | **逐点色，沿线按弧长连续过渡** |

逐点色的版本**不是**把线切成 N 段各画一个单色：整条链仍然是一个 SDF 实例、一次 draw，
颜色在片元里逐像素求值。这一点很重要 —— 切成 N 段的话，每段的圆头会在接头处与相邻段
**重叠并被 blend 两次**，半透明时每个接头都会出现一个更亮的点。

---

## 1. 基本用法

```rust
use vireo::prelude::*;

// 200 点曲线，alpha 沿线从 0.8 淡到 0.12
let pts: Vec<(f32, f32)> = (0..200)
    .map(|i| {
        let t = i as f32 / 199.0;
        (20.0 + t * 400.0, 120.0 + (t * std::f32::consts::TAU).sin() * 60.0)
    })
    .collect();
let colors: Vec<Color> = (0..200)
    .map(|i| {
        let t = i as f32 / 199.0;
        Color::new(0.55, 0.80, 1.00, 0.8 + (0.12 - 0.8) * t)
    })
    .collect();

draw_line_chain_colors(&mut batch, &pts, 2.0, &colors);

// DrawBatch 上也有镜像方法：
// batch.line_chain_colors(&pts, 2.0, &colors);
```

## 2. 语义（三条，都是刻意的）

1. **第 i 个点就是 `colors[i]`**（`colors.len()` 必须等于 `points.len()`）。这与「mesh 顶点色」
   同一个心智模型：所见即所得，不需要猜插值发生在哪。
2. **两点之间按弧长线性过渡**：像素落在第 k 段上、段内参数 `t`，则该像素取
   `mix(colors[k], colors[k+1], t)`。所以**点距不均匀也不会让过渡速度不均匀** ——
   颜色绑在点上，插值沿弧长（这一点和"按点序号插值"不同，后者在不等距采样时会看到速度突变）。
3. **闭合链**（首尾距离 < 0.001）同样要传 `points.len()` 个颜色，**末色被忽略**：
   末点就是首点，wrap 段自然回到 `colors[0]`。

边框外的"淡出"就是第 1 条的普通用法 —— 按每个点离框的距离算 alpha：

```rust
let alpha = |y: f32| {
    let over = if y < top { top - y } else if y > bottom { y - bottom } else { 0.0 };
    if over >= FADE { 0.0 } else { 0.8 * (1.0 - over / FADE) }
};
let colors: Vec<Color> = pts.iter().map(|&(_, y)| Color::new(0.6, 0.8, 1.0, alpha(y))).collect();
```

## 3. 降级与边界（都不 panic）

| 输入 | 行为 |
|---|---|
| `colors.len() != points.len()` | 按 `colors[0]` 单色画 + 一次 `log::warn!`（`colors` 为空则用 `batch.color`） |
| `points.len() < 2` / `thickness <= 0` / 非有限值 | 跳过（与 `draw_line_chain` 一致） |
| 所有颜色的 alpha 都是 0 | 跳过（不产生实例） |
| 相邻两点重合 | 该段退化、不进边表（与 `draw_line_chain` 一致），它的终点色被丢弃 |
| `batch.clear_sdf_feather()`（几何路径） | 单色 + 一次 `log::warn!` —— geo/mesh 路径没有逐点色 |
| 自定义 vertex/fragment 材质 | 渐变**不生效**（拿到的是单色）：色带由引擎片元读取，自定义片元自行决定颜色 |

## 4. 性能

- **draw call / 实例数不变**：一条链仍是 1 个实例。
- **每帧上传**：色带表按内容去重，同一条链（同点集 + 同色）只占一份。
  简化后 200 点线性渐变只占 **2 个 stop**（≈ 80B）；"框内常量 + 框外淡出"典型 4~6 个 stop。
- **每像素**：被覆盖的像素多一次 `sqrt`（段长，循环内已有 `dot(ab,ab)`）与一次色带查表；
  单色链（`draw_line_chain`）**完全不走**这段分支，开销为 0。
- 极端输入（每个点一种色且无法简化）会被压到 **64 个 stop / 色带**，误差不再有 1/255 保证。

## 5. 什么时候不要用它

- 只想给一小段线加个渐变 → 直接用两三个点 + 两个颜色，没必要喂 200 个点。
- 想要**沿宽度**（粗细）渐变、或矩形/圆/弧/多边形的渐变 → 都不支持，见下节。
- 需要 MSAA 之外的自定义着色（辉光、描边） → 用自定义材质，渐变不适用。

## 6. 非目标（明确不做）

- 矩形 / 圆 / 圆角 / 弧 / 多边形的渐变。
- 单段渐变直线（`draw_line` 的两端异色）：走 `draw_line_chain_colors` 传两个点即可，
  引擎内的单段快速路径没做。
- 沿线**宽度**渐变。
- `LineEase` / `before` / `after` 这类"按停点给的缓动/保持"DSL：点本身就是停点，
  点距 ≈ 1px 时相邻点之间的插值形状不可观测，只有逐点色这一个概念。

## 7. 实测数据

`tests/gpu_bench.rs`（离屏 canvas 是 **sRGB** 目标，像素值都过 sRGB 编码）：

| 用例 | 结果 |
|---|---|
| 单色（全白逐点色）vs `draw_line_chain` | **逐像素完全一致**（旧路径零影响） |
| 200 点 alpha 0.8→0.12 | 沿线与线性 ramp 的最大偏差 **0.62/255**，单调、无台阶（旧做法 4 档 ≈ 6.5px 一级） |
| 同点集、不同色带（几何模板复用） | 两条各画各的：上 (255,76,76) 红 / 下 (76,255,76) 绿，不串色 |
| 预乘混合（红 a=0.9 → 蓝 a=0） | 中点 (177,0,0)，期望 sRGB(0.45)·255 = 178；直通空间混会得到明显的蓝分量 |
| PerSample(4) 档 | 正常出内容、alpha 沿线下降 |
| 退化（色数不符 / 全透明 / 全重合 / 闭合） | 全部不 panic，行为见 §3 |
