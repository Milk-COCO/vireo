[Tap to English README](README.md)
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

- **即时模式**：每帧构建 `DrawBatch`，填充形状和文本，提交到窗口
- **单 pass 交替**：单次 `draw()` 内，多个 batch 按顺序渲染，每个 batch 先画形状再画文本
- **坐标系**：左上角原点，(0, 0) 为左上角，x 向右，y 向下

```rust
// 多 batch：后面的覆盖前面的
win.draw(Some(bg_color), &[&batch1, &batch2, &batch3]);
```

## 功能

- 9 种填充形状 + 8 种描边
- 文本渲染（shape 缓存、HUD 分段、自定义字体）
- 多窗口 + 离屏渲染
- 纹理加载（文件 / 字节 / RGBA，UV 子区域）
- 窗口控制（全屏、图标、PresentMode、AA 等）
- 帧统计 + 输入（轮询 + 事件 / 触摸）

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
cargo run --example text_font       # load_font / Family::Name
cargo run --example text_clip       # clip + align

# 形状 / 变换
cargo run --example shapes
cargo run --example shapes_lines    # line / line_chain
cargo run --example shapes_rotate
cargo run --example transform_stack # translate/rotate/scale_by

# 纹理
cargo run --example texture
cargo run --example texture_rgba    # from_rgba / from_bytes
cargo run --example texture_region

# 窗口 / 输入
cargo run --example window_controls
cargo run --example window_present  # AutoVsync/Fifo/Mailbox/Immediate
cargo run --example input
cargo run --example input_touch
cargo run --example color_palette

# 压力 / 诊断
cargo run --example text_shape_cache
cargo run --example frame_stats
cargo run --example bench
```
