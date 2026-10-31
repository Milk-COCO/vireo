[中文](README.md) | [English](README-EN.md)

# Vireo

**V**ault's **I**nterface and **R**endering **E**ngine for **O**ptics — and a genus of bird.

A **Rust 2D rendering library** built on `wgpu` + `winit`: single-pass multi-batch drawing,
SDF shapes and outlines, self-drawn text (**CJK IME works on Android**), tested on real
Windows and Android devices.

It is **not** a game engine and **not** a UI framework — no ECS, no widgets, no layout engine.
It gives you the "how to draw this frame" layer: window, event loop, GPU resources, batch
merging, text rasterization. What the UI looks like and how it is laid out is up to you.

[![crates.io](https://img.shields.io/crates/v/vireo.svg)](https://crates.io/crates/vireo)
[![docs.rs](https://docs.rs/vireo/badge.svg)](https://docs.rs/vireo)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

<img src="https://raw.githubusercontent.com/Milk-COCO/vireo/master/logo.png" width="120" alt="Vireo logo">

> ⚠️ Most of this codebase is AI-generated and validated against the author's own use cases.
> **Not production-ready by declaration** — see [Project status & risk](#project-status--risk).

## Quick start

```toml
[dependencies]
vireo = "0.1"     # crates.io has 0.1.2
# To use the APIs described in this README (0.2.0, unreleased):
# vireo = { git = "https://github.com/Milk-COCO/vireo" }
```

```rust
use vireo::prelude::*;

#[vireo::main]
async fn main(app: App) {
    let idx = app.window(WindowDesc::new("EZ Vireo", 800, 600));

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return true, // window closed -> stop the loop
        };

        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::new(100.0, 100.0), 200.0, 150.0, Some(RED));
        draw_circle(&mut batch, Pos::new(500.0, 300.0), 80.0, Some(BLUE));
        draw_line(&mut batch, 50.0, 50.0, 750.0, 550.0, 2.0, Some(GREEN));
        draw_text(
            &mut batch.texts,
            "你好，Vireo",
            Pos::new(100.0, 40.0),
            TextDef::default().font_size(32.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(BLACK, &[&batch]);
        true
    })
    .await
    .unwrap();
}
```

That is a verbatim copy of
[`examples/ez.rs`](https://github.com/Milk-COCO/vireo/blob/master/examples/ez.rs) — it draws a
red rectangle, a blue circle, a green line and one line of Chinese text on black. Next:

- Text and HUD parts → `cargo run --example text_hud`
- Custom WGSL materials → `cargo run --example custom_material`
- Android project → [`examples/android/`](https://github.com/Milk-COCO/vireo/blob/master/examples/android/README.md)

## Platforms

| Platform | Status |
|---|---|
| **Windows** (DX12 / Vulkan) | ✅ daily use + every example run |
| **Android** (Vulkan, GLES fallback on weak GPUs) | ✅ tested on device (HyperOS 16); build chain differs — needs `cargo ndk` + gradle |
| Linux (Vulkan) / macOS (Metal) | 🟡 compiles only, not tested on hardware |
| Web / wasm | ❌ not supported |

Details, verification depth and known issues (emulator compatibility, font sources, toolchain):
[`docs/platforms.md`](https://github.com/Milk-COCO/vireo/blob/master/docs/platforms.md) (Chinese).

## Core concepts

- **Immediate mode**: build a `DrawBatch` each frame, push shapes/text, submit once with
  `win.draw(clear_color, &[batches…])`. Batch trees nest (children, inheritance, clipping).
- **Single pass, many batches**: every batch inside one `draw()` lands in the same render pass —
  no per-batch pass, no forced vertex accumulation.
- **Coordinates**: top-left origin, x right, y down, units are vireo logical pixels. Size/position
  APIs take `impl ToPx`: bare numbers and `Dp` are logical, `Px` is physical — the intent is
  declared at the call site. With `dpi_override(Some(d))` vireo owns its pixels (physical =
  logical × d); with `None` logical units are winit logical units (OS scaling applies).
- **Three shape paths**: SDF instances (default, `sdf_feather` controls soft edges), geometry
  templates (after `clear_sdf_feather()`), mesh vertices (push your own). The path decides
  anti-aliasing quality and how much control you get.
- **Clipping & culling**: `clips_children` (stencil for the subtree; axis-aligned rects use
  scissor automatically), `area_include` / `area_exclude` (boolean `Area` masks),
  `text_clip` / `TextOverride.clip` (text clipping in logical pixels), `bounds` (subtree AABB
  culling, automatic by default).
- **Multi-loop threads**: each `Loop` from `app.run(..)` / `app.spawn(Thread)` runs on its own OS
  thread; `App` is `Clone` (shared GPU device/queue). Each thread owns its own `DrawBatch` —
  don't share a batch under construction across threads.
- **Text**: cosmic-text shaping + glyphon rasterization, **system fonts by default** (CJK usually
  works out of the box).

## What you get

- **SDF shapes + outlines**: 16 built-in shapes, `sdf_feather` for soft edges, `PerSample` mode
  for per-sample shading — no hand-written distance fields, no MSAA stacking for AA quality.
- **Custom WGSL materials**: one `material_main` serves both shapes and text; recolor, sample
  textures, run screen-space effects — without forking the engine.
- **MRT**: write several color attachments in one submission (frosted glass, glow, normals as a
  style field) — no multi-pass assembly.
- **Depth / stencil**: depth prepass, depth preserved across batches, boolean `Area` clipping —
  shadows and masks without building your own framework.
- **GPU compute**: `compute_pipeline` + `dispatch`, results visible to subsequent render passes.
- **Multi-window + offscreen**: `OffscreenCanvas` mirrors the window API and `read_pixels()` reads
  back (screenshots / regression tests).
- **Particles**: `ParticlePool` + a single push, positions integrated analytically in the VS —
  tens of thousands of particles without per-frame CPU writes.
- **Textures**: file / bytes / RGBA sources, `UvRect` sub-regions, Repeat / Clamp sampling —
  atlases, nine-slices and procedural textures without writing your own shader.
- **On-demand rendering & throttling**: skip the frame entirely when nothing changed (0 present/s
  while idle); all four throttling layers can be disabled individually — see
  [throttling](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/throttling.md).
- **Polyline gradient**: per-point color along one line, still 1 instance / 1 draw call — see
  [line gradient](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/line-gradient.md).
- **Text**: whole-run shape cache, HUD parts (monospace stepping), `StableText`, custom fonts and
  attrs, COLRv1 color emoji.

## Android: the most distinctive part

Desktop 2D renderers are not scarce. What is scarce is one that handles **CJK text input on
Android** properly:

- **CJK IME works out of the box** — via `AGDK GameActivity` (`InputConnection`), not
  `NativeActivity` (which simply cannot do IME).
- **Three-value safe area**: `safe_area` (physical max inset) / `padding` (current) /
  `ime_insets` (keyboard), plus `cutouts()` for the camera notch rectangle.
- **Notch / immersive mode / system-bar bitmask / orientation lock / keep-screen-on / brightness**
  all live in `platform::android`.
- **Automatic GLES fallback** on weak GPUs — a target device does not have to have Vulkan.

A copyable project template (NDK, gradle, 16 KB page-size compliance, JNI wiring) lives in
[`examples/android/`](https://github.com/Milk-COCO/vireo/blob/master/examples/android/README.md).

## Examples

| example | What it shows |
|---|---|
| `ez` | official quickstart (the code above) |
| `text_hud` | Chinese HUD / split text parts |
| `shapes` | 16 shapes with fill and outline |
| `custom_material` | custom WGSL material |
| `mrt` | several color attachments in one submission |
| `bench` | stress test + draw-call stats |

Full list (72 examples, grouped by topic):
[`docs/EXAMPLES.md`](https://github.com/Milk-COCO/vireo/blob/master/docs/EXAMPLES.md) (Chinese).

## Further reading

- [Custom materials (Material ABI)](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/material.md)
  — `material_main` contract, `MaterialInput` fields, 4-group layout, MRT outputs, self-managed
  bind groups.
- [Frame rate, vsync and throttling](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/throttling.md)
  — what the four throttling layers are, how to disable them, what you must take over afterwards.
- [Close interception and exit hooks](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/lifecycle.md)
  — veto a close with `on_close_requested`; `on_exit` / `on_exit_render` for teardown.
- [Polyline gradient](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/line-gradient.md)
  — per-point color, arc-length interpolation, degradation rules, measurements.
- [Platform support](https://github.com/Milk-COCO/vireo/blob/master/docs/platforms.md)
  — verification depth per platform, Android build chain, toolchain, fonts.

## Build & test

```bash
cargo check --lib --examples --tests   # expect 0 warnings
cargo test --lib                       # library unit tests only
cargo test --release --test gpu_bench -- --ignored --test-threads=1   # headless GPU tests (needs a GPU)
```

`cargo test` (without `--lib`) also runs the crate-level doc-test, and **that doc-test actually
opens a window** — it hangs on headless machines. Known defect; that is why only
`cargo test --lib` is listed.

## Project status & risk

**Which version this README describes**: crates.io has **0.1.2**; this README describes the
**repository master** (0.2.0, unreleased). APIs such as `on_close_requested`, `on_exit`,
`set_clock` and `draw_line_chain_colors` **do not exist in 0.1.2** — if you use the crates.io
release, follow the migration notes in
[`CHANGELOG.md`](https://github.com/Milk-COCO/vireo/blob/master/CHANGELOG.md).

**Verification depth**: Windows is used daily and every example has been run; Android is tested on
a HyperOS 16 device and an API 35 emulator; Linux / macOS only compile. Test coverage equals "the
paths the author has used", not all paths — concurrent multi-window use, extreme DPI and rare AA
combinations can break at any time.

### Preface: Why NOT Vireo

There, 95% (**or even more**) of the content in this project is generated by AI. The content that the only developer has seen is only 5% (**or even less**).

Even though the code has been repeatedly reviewed by AI, there is still a risk of exploding at any time.

## License

Vireo is dual-licensed under your choice of either:

- [Apache License 2.0](https://github.com/Milk-COCO/vireo/blob/master/LICENSE-APACHE)
- [MIT License](https://github.com/Milk-COCO/vireo/blob/master/LICENSE-MIT)
