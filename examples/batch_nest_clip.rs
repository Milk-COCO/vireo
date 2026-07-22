//! 嵌套 stencil 裁切 + transform
//!
//! 三层：
//! - root：外圆 mask + `clips_children` + 平移/旋转
//! - mid：内圆角 mask + `clips_children` + `InheritFromParent::TRANSFORM`
//! - leaf：色块 / 文字 + `TRANSFORM`（测 mid 的 mask）
//!
//! 左：两层都裁；右：root 不裁（对照，子仍可跟转）
//!
//! ```bash
//! cargo run --example batch_nest_clip
//! ```

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Nested Clip + Transform", 920, 500).high_dpi(true),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.03;

        let mut ui = DrawBatch::new();
        draw_text(
            &mut ui.texts,
            "三层嵌套：root 外圆 → mid 内圆角 → leaf 内容  |  左双裁  |  右 root 不裁",
            TextOptions::default()
                .x(16.0)
                .y(12.0)
                .font_size(15.0)
                .color(Color::new(0.75, 0.8, 0.9, 1.0)),
        );
        draw_text(
            &mut ui.texts,
            "clips×2",
            TextOptions::default()
                .x(180.0)
                .y(40.0)
                .font_size(14.0)
                .color(Color::new(0.6, 0.7, 0.85, 1.0)),
        );
        draw_text(
            &mut ui.texts,
            "root no clip",
            TextOptions::default()
                .x(620.0)
                .y(40.0)
                .font_size(14.0)
                .color(Color::new(0.6, 0.7, 0.85, 1.0)),
        );

        let left = three_level(220.0, 270.0, t, true, true);
        let right = three_level(680.0, 270.0, t + 1.0, false, true);

        win.draw(
            Some(Color::new(0.05, 0.06, 0.09, 1.0)),
            &[&ui, &left, &right],
        );
        true
    });
}

/// root → mid → leaf（与 `nested_clips_*` 单测同构）
fn three_level(
    cx: f32,
    cy: f32,
    t: f32,
    root_clip: bool,
    mid_clip: bool,
) -> DrawBatch {
    // ---- root：外圆 mask ----
    let mut root = DrawBatch::new();
    root.sdf_feather = Some(1.5);
    root.set_position(cx, cy);
    root.set_deg(t * 18.0);
    root.clips_children = root_clip;

    draw_circle(
        &mut root,
        Pos::new(0.0, 0.0),
        130.0,
        Some(Color::new(0.18, 0.22, 0.32, 1.0)),
    );
    root.text(
        "ROOT",
        TextOptions::default()
            .x(-28.0)
            .y(-118.0)
            .font_size(14.0)
            .color(Color::new(0.55, 0.65, 0.8, 1.0)),
    );

    // ---- mid：内圆角 mask（相对 root 平移 + 再转）----
    let mut mid = DrawBatch::new();
    mid.sdf_feather = Some(1.2);
    mid.inherit = InheritFromParent::TRANSFORM;
    // 幅度要大：mid 半宽 ~72，root r=130 → 中心偏移 ~70+ 才会探出外圆
    mid.set_position((t * 1.1).sin() * 85.0, (t * 0.9).cos() * 75.0);
    mid.set_deg(t * 12.0);
    mid.clips_children = mid_clip;

    draw_rounded_rect(
        &mut mid,
        Pos::new(-72.0, -72.0),
        144.0,
        144.0,
        28.0,
        Some(Color::new(0.26, 0.32, 0.44, 1.0)),
    );
    mid.text(
        "MID",
        TextOptions::default()
            .x(-18.0)
            .y(-64.0)
            .font_size(13.0)
            .color(Color::new(0.7, 0.8, 0.95, 1.0)),
    );

    // ---- leaf：色块 + 文字（故意伸出 mid）----
    let mut leaf = DrawBatch::new();
    leaf.sdf_feather = Some(1.0);
    leaf.inherit = InheritFromParent::TRANSFORM;

    for i in 0..12 {
        let phase = t * 2.2 + i as f32 * 0.55;
        let x = -55.0 + (i % 4) as f32 * 30.0 + phase.sin() * 48.0;
        let y = -50.0 + (i / 4) as f32 * 32.0 + phase.cos() * 52.0;
        let hue = (i as f32 / 12.0 + t * 0.05) % 1.0;
        draw_rounded_rect(
            &mut leaf,
            Pos::new(x, y),
            36.0,
            22.0,
            5.0,
            Some(Color::new(
                0.4 + hue * 0.5,
                0.5 + (1.0 - hue) * 0.3,
                0.55 + hue * 0.3,
                0.95,
            )),
        );
    }
    draw_circle(
        &mut leaf,
        Pos::new(0.0, 0.0),
        14.0,
        Some(Color::new(1.0, 0.95, 0.85, 1.0)),
    );
    leaf.text(
        "LEAF",
        TextOptions::default()
            .x(-28.0)
            .y(-10.0)
            .font_size(20.0)
            .color(Color::new(0.1, 0.1, 0.15, 1.0)),
    );

    // 一条伸出 mid 的条：被 mid 裁、在右图 root 不裁时仍可能被 mid 裁
    let ox = (t * 1.8).sin() * 90.0;
    draw_rounded_rect(
        &mut leaf,
        Pos::new(ox - 40.0, 50.0),
        80.0,
        18.0,
        4.0,
        Some(Color::new(1.0, 0.55, 0.35, 0.95)),
    );

    mid.push_child(leaf);
    root.push_child(mid);
    root
}
