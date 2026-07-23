[中文](README.md) | [English](README_EN.md)

# Vireo

Vireo is just a genus of bird!

Or rather — **V**ault's **I**nterface and **R**endering **E**ngine for **O**ptics.

And it's also a 2D GPU rendering library built on `wgpu` + `winit`.

```bash
cargo run --example hello
```

## Quick Start

```bash
cargo run --example ez
```

## Drawing Model

- **Immediate mode**: Build a `DrawBatch` each frame, fill shapes/text, submit to the window
- **Single pass**: Multiple batches in one `draw()`; batch trees support children, inherit, and clipping
- **Coordinates**: Top-left origin; x right, y down (logical pixels)
- **`Pos`**: Anchored shapes (circle/rect/…) take a `Pos` plus the transform table; polylines keep point coordinates as geometry

```rust
// Later batches overlay earlier ones
win.draw(Some(bg_color), &[&batch1, &batch2, &batch3]);
```

### Text API (What / Where / Override)

| Arg | Type | Role |
|-----|------|------|
| Content | `&str` / `StableText` / parts | What to draw |
| Position | `Pos` | Where |
| Shape def | `TextDef` | Font size, wrap, align, attrs |
| Per-frame | `TextOverride` | Color, clip, extra transform |

- `draw_text(&mut batch.texts, …)`: default `transform_index = 0` (identity); `pos` is logical world space  
- `batch.text(…)`: captures the brush transform (use after `set_position`)  
- Shapes: `draw_shape` **composes** `Pos` with the batch transform; `set_position(x,y)` + `Pos(0,0)` draws at the brush origin  

### Clipping & Culling

- **`clips_children`**: stencil clip for the subtree; axis-aligned rects may use **scissor** automatically  
- **`area_include` / `area_exclude`**: boolean Area masks (∪ ∩ ∖), orthogonal to clips  
- **`bounds`**: subtree AABB culling (auto by default; disable or set manually)  
- **`text_clip` / `TextOverride.clip`**: text clip in logical pixels (scaled to physical internally)  

## Features

- Filled + outlined shapes (SDF / geometry paths via `sdf_feather`)
- Affine transform table (vertex `transform_index`; slot 0 reserved as identity)
- Text: shape cache, HUD parts, `StableText`, custom fonts / attrs
- Multi-window + offscreen rendering
- Textures (file / bytes / RGBA, UV subregions)
- Window controls (fullscreen, icon, PresentMode, AA, high_dpi, …)
- Frame stats + input (polling / events / touch)

## Examples

Flat `examples/*.rs`; the file stem is the `--example` name.

```bash
# Getting started
cargo run --example hello
cargo run --example ez

# Text
cargo run --example text_attrs
cargo run --example text_hud
cargo run --example text_measure
cargo run --example text_transform
cargo run --example text_font
cargo run --example text_clip
cargo run --example text_batch_clip
cargo run --example text_shape_cache
cargo run --example text_profile

# Shapes / transform
cargo run --example shapes
cargo run --example shapes_lines
cargo run --example shapes_rotate
cargo run --example transform_stack

# Texture
cargo run --example texture
cargo run --example texture_rgba
cargo run --example texture_region
cargo run --example texture_sdf_geo

# Batch / clip
cargo run --example batch_multi
cargo run --example batch_inherit
cargo run --example batch_child_clip
cargo run --example batch_nest_clip
cargo run --example batch_area_clip
cargo run --example clip_rect_demo

# Window / input / offscreen
cargo run --example window_controls
cargo run --example window_present
cargo run --example window_aa
cargo run --example window_multi
cargo run --example input
cargo run --example input_touch
cargo run --example offscreen
cargo run --example color_palette

# Stress / diagnostics
cargo run --example bench
cargo run --example frame_stats
cargo run --example text_stress
```

## Build & Test

```bash
cargo check
cargo test --lib          # preferred (skips window-opening doc-tests)
cargo build --examples
```
