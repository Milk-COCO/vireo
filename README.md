[中文](README.md) | [English](README_EN.md)

# Vireo

Vireo 只是一种可爱的小鸟！

或者说 —— **V**ault's **I**nterface and **R**endering **E**ngine for **O**ptics。

当然，它也是基于 `wgpu` + `winit` 的 2D GPU 渲染库！

```bash
cargo run --example hello
```

## 快速开始

```bash
cargo run --example ez
```

## 绘制模型

- **即时模式**：每帧构建 `DrawBatch`，填充形状/文本，提交到窗口
- **单 pass**：一次 `draw()` 内多 batch 按序渲染；batch 树可嵌套（子节点、继承、裁切）
- **坐标系**：左上角原点，x 向右，y 向下（逻辑像素）
- **位置 `Pos`**：圆/矩形等锚点形状的位置走 `Pos` + 变换表；线/多边形等点列即几何坐标

```rust
// 多 batch：后面的盖住前面的
win.draw(Some(bg_color), &[&batch1, &batch2, &batch3]);
```

### 文本 API

| 参数 | 类型 | 含义 |
|------|------|------|
| 内容 | `&str` / `StableText` / parts | 画什么 |
| 位置 | `Pos` | 在哪 |
| 定型 | `TextDef` | 字号、换行、对齐、字体属性 |
| 每帧覆盖 | `TextOverride` | 颜色、clip、额外 transform |

- `draw_text(&mut batch.texts, …)`：默认 `transform_index = 0`（单位阵），`pos` 为逻辑世界坐标  
- `batch.text(…)`：捕获当前画笔 transform，适合 `set_position` 后随 batch 移动  
- 形状：`draw_shape` 将 `Pos` 与 batch transform **组合**；`set_position(x,y)` + `Pos(0,0)` 表示在画笔原点处绘制  

### 裁切与剔除

- **`clips_children`**：子树 stencil 裁切；纯轴对齐矩形可自动走 **scissor**  
- **`area_include` / `area_exclude`**：任意 Area 布尔（∪ ∩ ∖），与 clips 正交  
- **`bounds`**：子树 AABB 剔除（默认自动；可关 / 可手动）  
- **`text_clip` / `TextOverride.clip`**：文字逻辑像素裁切（内部 × scale 到物理）  

## 功能

- 填充形状 + 描边（SDF / 几何双路径，`sdf_feather`）
- 仿射变换表（顶点 `transform_index`；槽 0 固定单位阵）
- 文本：shape 缓存、HUD 分段、`StableText`、自定义字体 / attrs
- 多窗口 + 离屏渲染
- 纹理（文件 / 字节 / RGBA，UV 子区域）
- 窗口控制（全屏、图标、PresentMode、AA、high_dpi 等）
- 帧统计 + 输入（轮询 / 事件 / 触摸）

## 示例

扁平 `examples/*.rs`，文件名即 `cargo run --example` 名。

```bash
# 入门
cargo run --example hello
cargo run --example ez

# 文字
cargo run --example text_attrs
cargo run --example text_hud
cargo run --example text_measure
cargo run --example text_transform
cargo run --example text_font
cargo run --example text_clip
cargo run --example text_batch_clip
cargo run --example text_shape_cache
cargo run --example text_profile

# 形状 / 变换
cargo run --example shapes
cargo run --example shapes_lines
cargo run --example shapes_rotate
cargo run --example transform_stack

# 纹理
cargo run --example texture
cargo run --example texture_rgba
cargo run --example texture_region
cargo run --example texture_sdf_geo

# Batch / 裁切
cargo run --example batch_multi
cargo run --example batch_inherit
cargo run --example batch_child_clip
cargo run --example batch_nest_clip
cargo run --example batch_area_clip
cargo run --example clip_rect_demo

# 窗口 / 输入 / 离屏
cargo run --example window_controls
cargo run --example window_present
cargo run --example window_aa
cargo run --example window_multi
cargo run --example input
cargo run --example input_touch
cargo run --example offscreen
cargo run --example color_palette

# 压力 / 诊断
cargo run --example bench
cargo run --example frame_stats
cargo run --example text_stress
```

## 构建与测试

```bash
cargo check
cargo test --lib          # 推荐（跳过会开窗口的 doc-test）
cargo build --examples
```
