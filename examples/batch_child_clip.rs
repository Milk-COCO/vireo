//! 父裁子（stencil clipping）对比：圆形裁切 vs 不裁切
//!
//! ```bash
//! cargo run --example batch_clip
//! ```

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Batch Clip — Circle Stencil", 900, 480).high_dpi(true),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.025;

        let mut ui = DrawBatch::new();
        draw_text(
            &mut ui.texts,
            "Batch clips_children = true   vs   clips_children = false (对照)",
            Pos::new(20.0, 12.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(Color::new(0.75, 0.8, 0.9, 1.0)),
        );

        // 圆心 / 半径
        let (cx_l, cy, r) = (200.0, 250.0, 160.0);
        let (cx_r, _) = (650.0, 250.0);

        // ===== 左侧：圆形裁切 =====
        let mut clip_batch = DrawBatch::new();
        clip_batch.set_sdf_feather(Some(1.0));
        clip_batch.clips_children = true;

        draw_circle(
            &mut clip_batch,
            Pos::new(cx_l, cy),
            r,
            Some(Color::new(0.98, 0.98, 1.0, 1.0)),
        );
        draw_text(
            &mut clip_batch.texts,
            "clips_children = true",
            Pos::new(cx_l - 70.0, cy - r + 12.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.3, 0.35, 0.5, 1.0)),
        );

        // 子内容：色块大幅上下左右晃动，故意越界
        let mut inner = DrawBatch::new();
        let n = 40;
        for i in 0..n {
            let phase = t * 1.8 + i as f32 * 0.35;
            let ox = phase.sin() * 90.0;
            let oy = (phase * 0.7 + 1.2).cos() * 110.0;
            let x = cx_l - 100.0 + (i % 5) as f32 * 42.0 + ox;
            let y = cy - 100.0 + (i / 5) as f32 * 36.0 + oy;
            let hue = (i as f32 / n as f32 + t * 0.05) % 1.0;
            let c = Color::new(
                0.3 + hue * 0.6,
                0.4 + (1.0 - hue) * 0.4,
                0.5 + hue * 0.3,
                0.9,
            );
            draw_rounded_rect(&mut inner, Pos::new(x, y), 48.0, 28.0, 6.0, Some(c));
        }
        clip_batch.push_child(inner);

        // ===== 右侧：不裁切（对照）=====
        let mut no_clip_batch = DrawBatch::new();
        no_clip_batch.set_sdf_feather(Some(1.0));

        draw_circle(
            &mut no_clip_batch,
            Pos::new(cx_r, cy),
            r,
            Some(Color::new(0.98, 0.98, 1.0, 1.0)),
        );
        draw_text(
            &mut no_clip_batch.texts,
            "clips_children = false",
            Pos::new(cx_r - 75.0, cy - r + 12.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.5, 0.35, 0.3, 1.0)),
        );

        let mut no_inner = DrawBatch::new();
        for i in 0..n {
            let phase = t * 1.8 + i as f32 * 0.35 + 0.8;
            let ox = phase.sin() * 90.0;
            let oy = (phase * 0.7 + 1.2).cos() * 110.0;
            let x = cx_r - 100.0 + (i % 5) as f32 * 42.0 + ox;
            let y = cy - 100.0 + (i / 5) as f32 * 36.0 + oy;
            let hue = (i as f32 / n as f32 + t * 0.05 + 0.3) % 1.0;
            let c = Color::new(
                0.5 + hue * 0.3,
                0.3 + (1.0 - hue) * 0.5,
                0.4 + hue * 0.4,
                0.9,
            );
            draw_rounded_rect(&mut no_inner, Pos::new(x, y), 48.0, 28.0, 6.0, Some(c));
        }
        no_clip_batch.push_child(no_inner);

        draw_text(
            &mut ui.texts,
            "左侧色块被圆形裁切，右侧越界可见",
            Pos::new(250.0, 455.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(Color::new(0.5, 0.55, 0.65, 1.0)),
        );

        win.draw(
            Some(Color::new(0.06, 0.07, 0.1, 1.0)),
            &[&ui, &clip_batch, &no_clip_batch],
        );
        true
    });
}
