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
    let long = "Text clipped by parent circle.";

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.02;

        let mut ui = DrawBatch::new();
        draw_text(
            &mut ui.texts,
            "左 clips=true  |  右 false  |  子文字 + TRANSFORM（父不旋转）",
            Pos::new(16.0, 12.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(Color::new(0.75, 0.8, 0.9, 1.0)),
        );
        draw_text(
            &mut ui.texts,
            "clips",
            Pos::new(190.0, 40.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.6, 0.7, 0.85, 1.0)),
        );
        draw_text(
            &mut ui.texts,
            "no clip",
            Pos::new(640.0, 40.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.6, 0.7, 0.85, 1.0)),
        );

        let left = text_clip_panel(220.0, 260.0, t, true, long);
        let right = text_clip_panel(680.0, 260.0, t + 0.8, false, long);

        win.draw(
            Color::new(0.05, 0.06, 0.09, 1.0),
            &[&ui, &left, &right],
        );
        true
    });
}

fn text_clip_panel(cx: f32, cy: f32, t: f32, clip: bool, long: &str) -> DrawBatch {
    let mut parent = DrawBatch::new();
    parent.set_sdf_feather(Some(1.5));
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
    child.set_sdf_feather(Some(1.0));
    child.inherit = InheritFromParent::TRANSFORM;

    let scroll = -400.0 + (t * 150.0) % 550.0;
    child.text(
        long,
        Pos::new(scroll, -14.0),
        TextDef::default().font_size(22.0),
        TextOverride::from_color(Color::new(0.98, 0.95, 0.88, 1.0)),
    );

    child.text(
        if clip { "CLIPPED" } else { "VISIBLE" },
        Pos::new(-52.0, 30.0),
        TextDef::default().font_size(24.0),
        TextOverride::from_color(Color::new(1.0, 0.85, 0.3, 1.0)),
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
