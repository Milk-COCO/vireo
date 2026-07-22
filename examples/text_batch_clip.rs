//! 文字 + Batch 裁切（单层，无嵌套）
//!
//! 父：圆 mask + `clips_children`（不旋转，便于看裁切）  
//! 子：长文横向滚动 + 标签，`InheritFromParent::TRANSFORM`
//!
//! ```bash
//! cargo run --example text_batch_clip
//! ```

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Text + Batch Clip", 920, 480).high_dpi(true),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    let long = "文字会被圆形裁切 — Text clipped by parent circle. 越界部分不可见。";

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.02;

        let mut ui = DrawBatch::new();
        draw_text(
            &mut ui.texts,
            "左 clips=true  |  右 false  |  子文字 + TRANSFORM（父不旋转）",
            TextOptions::default()
                .x(16.0)
                .y(12.0)
                .font_size(15.0)
                .color(Color::new(0.75, 0.8, 0.9, 1.0)),
        );
        draw_text(
            &mut ui.texts,
            "clips",
            TextOptions::default()
                .x(190.0)
                .y(40.0)
                .font_size(14.0)
                .color(Color::new(0.6, 0.7, 0.85, 1.0)),
        );
        draw_text(
            &mut ui.texts,
            "no clip",
            TextOptions::default()
                .x(640.0)
                .y(40.0)
                .font_size(14.0)
                .color(Color::new(0.6, 0.7, 0.85, 1.0)),
        );

        let left = text_clip_panel(220.0, 260.0, t, true, long);
        let right = text_clip_panel(680.0, 260.0, t + 0.8, false, long);

        win.draw(
            Some(Color::new(0.05, 0.06, 0.09, 1.0)),
            &[&ui, &left, &right],
        );
        true
    });
}

fn text_clip_panel(cx: f32, cy: f32, t: f32, clip: bool, long: &str) -> DrawBatch {
    let mut parent = DrawBatch::new();
    parent.sdf_feather = Some(1.5);
    parent.set_position(cx, cy);
    // 不旋转：避免和 Y 裁切问题混淆
    parent.clips_children = clip;

    draw_circle(
        &mut parent,
        Pos::new(0.0, 0.0),
        120.0,
        Some(Color::new(0.2, 0.24, 0.34, 1.0)),
    );

    let mut child = DrawBatch::new();
    child.sdf_feather = Some(1.0);
    child.inherit = InheritFromParent::TRANSFORM;

    // 长文：基线在圆心附近，整行字应完整显示在圆内中部
    let scroll = -100.0 + (t * 50.0) % 220.0;
    child.text(
        long,
        TextOptions::default()
            .x(scroll)
            .y(-14.0)
            .font_size(22.0)
            .color(Color::new(0.98, 0.95, 0.88, 1.0)),
    );

    child.text(
        if clip { "CLIPPED" } else { "VISIBLE" },
        TextOptions::default()
            .x(-52.0)
            .y(30.0)
            .font_size(24.0)
            .color(Color::new(1.0, 0.85, 0.3, 1.0)),
    );

    // 对照：形状裁切
    let ox = (t * 2.2).sin() * 80.0;
    draw_rounded_rect(
        &mut child,
        Pos::new(ox - 22.0, 55.0),
        44.0,
        20.0,
        5.0,
        Some(Color::new(0.35, 0.75, 1.0, 0.95)),
    );

    parent.push_child(child);
    parent
}
