[Tap to English README](README.md)
# Vireo

Vireo只是一种可爱的小鸟！

或者说 —— Vault's Interface and Rendering Engine for Optics !

当然，它也是基于 `wgpu` + `winit 的 2D GPU 渲染库！

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
- 文本渲染
- 多窗口 + 离屏渲染
- 纹理加载（PNG/JPG/BMP，UV 子区域）
- 窗口控制（全屏、图标、光标、大小、装饰等）
- 帧统计 + 输入系统（轮询 + 事件订阅）

## 示例

所有形状、文本样式、多 batch、离屏、纹理：

```bash
cargo run --example shapes
cargo run --example text_attrs
cargo run --example multi_batch
cargo run --example offscreen
cargo run --example texture
cargo run --example texture_fun
```
还有更多好玩的请查看 `examples` 文件夹！