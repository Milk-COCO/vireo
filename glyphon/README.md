<h1 align="center">
  🦅 glyphon 🦁
</h1>
<div align="center">
  Fast, simple 2D text rendering for <a href="https://github.com/gfx-rs/wgpu/"><code>wgpu</code></a>
</div>
<br />
<div align="center">
  <a href="https://crates.io/crates/glyphon"><img src="https://img.shields.io/crates/v/glyphon.svg?label=glyphon" alt="crates.io"></a>
  <a href="https://docs.rs/glyphon"><img src="https://docs.rs/glyphon/badge.svg" alt="docs.rs"></a>
  <img src="https://img.shields.io/badge/min%20rust-1.92-green.svg" alt="Minimum Rust Version">
  <a href="https://github.com/grovesNL/glyphon/actions"><img src="https://github.com/grovesNL/glyphon/workflows/CI/badge.svg?branch=main" alt="Build Status" /></a>
</div>

## ⚠ API differences from upstream

This fork introduces the following API changes:

### 1. `prepare()` is now append-only

`prepare()` no longer calls `self.glyph_vertices.clear()` internally. Each call **appends** new glyph data to the vertex buffer. You must call `clear()` once per frame before the first `prepare()`:

```rust
// once per frame
text_renderer.clear();
// each call appends
text_renderer.prepare(&device, &queue, &mut font_system,
    &mut atlas, &viewport, areas_batch1, &mut cache)?;
text_renderer.prepare(&device, &queue, &mut font_system,
    &mut atlas, &viewport, areas_batch2, &mut cache)?;
// draws all accumulated glyphs
text_renderer.render(&atlas, &viewport, &mut pass)?;
```

### 2. New public method: `clear()`

Clears all prepared glyph vertices. Call once per frame. `glyph_vertices` is emptied; the vertex buffer retains its capacity.

### 3. New public method: `render_range(atlas, viewport, pass, vertex_start, vertex_count)`

Renders a sub-range of prepared glyph vertices. Use with multiple `prepare()` calls to draw text layers interleaved with shapes for full z-order control. The existing `render()` method delegates to `render_range(0, len)` and is unchanged.

### Why?

The upstream API requires passing all `TextArea`s to a single `prepare()` call. Multiple `prepare()` calls within a frame overwrite each other's GPU vertex data. This fork decouples `clear` from `prepare` so callers can accumulate text across multiple batches, then render once (or in sub-ranges).

---

## What is this?

This crate provides a simple way to render 2D text with [`wgpu`](https://github.com/gfx-rs/wgpu/) by:

- shaping/calculating layout/rasterizing glyphs (with [`cosmic-text`](https://github.com/pop-os/cosmic-text/))
- packing the glyphs into texture atlas (with [`etagere`](https://github.com/nical/etagere/))
- sampling from the texture atlas to render text (with [`wgpu`](https://github.com/gfx-rs/wgpu/))

To avoid extra render passes, rendering uses existing render passes (following the middleware pattern described in [`wgpu`'s Encapsulating Graphics Work wiki page](https://github.com/gfx-rs/wgpu/wiki/Encapsulating-Graphics-Work)).

## License

This project is licensed under either [Apache License, Version 2.0](LICENSE-APACHE), [zlib License](LICENSE-ZLIB), or [MIT License](LICENSE-MIT), at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache 2.0 license, shall be triple licensed as above, without any additional terms or conditions.
