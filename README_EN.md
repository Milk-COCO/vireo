[点击中文读我](README.md)

# Vireo
Vireo is just a genus of bird!

Or rather — Vault's Interface and Rendering Engine for Optics !

And it's also a 2D GPU rendering library built on `wgpu` + `winit` !

```bash
cargo run --example hello
```

## Quick Start

```bash
cargo run --example ez
```

## Drawing Model

- **Immediate Mode**: Build a `DrawBatch` each frame, fill it with shapes and text, submit to the window
- **Single-Pass Interleaving**: Within a single `draw()` call, multiple batches render in order — each batch draws shapes first, then text
- **Coordinate System**: Origin at top-left corner, (0, 0) is top-left, x goes right, y goes down

```rust
// Multiple batches: later batches overlay earlier ones
win.draw(Some(bg_color), &[&batch1, &batch2, &batch3]);
```

## Features

- 9 filled shapes + 8 outlined shapes
- Text rendering
- Multi-window + offscreen rendering
- Texture loading (PNG/JPG/BMP, with UV sub-region support)
- Window controls (fullscreen, icon, cursor, size, decorations, etc.)
- Frame stats + input system (polling + event subscription)

## Examples

All shapes, text styles, multi-batch, offscreen, textures:

```bash
cargo run --example shapes
cargo run --example text_attrs
cargo run --example multi_batch
cargo run --example offscreen
cargo run --example texture
cargo run --example texture_fun
```

Play out the `examples` folder for more!