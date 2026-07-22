//! Area 模板裁切：include / exclude / ∪ ∩ \，与 clips_children 正交
//!
//! 四个面板：
//! 1. include：仅在大圆内可见
//! 2. exclude：在矩形中挖掉小圆
//! 3. ∩ ：在两个圆的重叠区可见
//! 4. ∪ ：两个圆合并区可见
//!
//! ```bash
//! cargo run --example area_clip
//! ```

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Area — include/exclude/∪/∩", 920, 500).high_dpi(true),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.025;

        let mut ui = DrawBatch::new();
        draw_text(
            &mut ui.texts,
            "Area 模板裁切（per-batch，stencil 掩码）",
            TextOptions::default()
                .x(16.0)
                .y(12.0)
                .font_size(15.0)
                .color(Color::new(0.75, 0.8, 0.9, 1.0)),
        );

        let panel_y = 260.0;
        let cell_w = 220.0;
        let cell_h = 200.0;
        let x0 = 30.0;

        // ===== Panel 1: include =====
        let cx1 = x0 + cell_w * 0.5;
        let cy1 = panel_y;
        let r = 70.0;
        let mut p1 = panel_frame(cx1, cy1, cell_w, cell_h, "include: 大圆内可见");
        let include_geom = make_disk_area(cx1, cy1, r);
        p1.area_include = Some(include_geom);
        // 内容：小方块在 (cx, cy) 附近扩散
        for i in 0..30 {
            let phase = t * 1.6 + i as f32 * 0.4;
            let x = cx1 - 60.0 + (i % 6) as f32 * 24.0 + phase.sin() * 18.0;
            let y = cy1 - 60.0 + (i / 6) as f32 * 24.0 + phase.cos() * 14.0;
            let hue = (i as f32 / 30.0 + t * 0.05) % 1.0;
            let c = Color::new(0.3 + hue * 0.6, 0.4 + (1.0 - hue) * 0.4, 0.5 + hue * 0.3, 0.95);
            draw_rounded_rect(&mut p1, Pos::new(x, y), 18.0, 12.0, 3.0, Some(c));
        }
        // 大圆描边（画在 include 之外）
        let mut ring = DrawBatch::new();
        ring.sdf_feather = Some(1.0);
        draw_circle(&mut ring, Pos::new(cx1, cy1), r, Some(Color::new(0.9, 0.9, 1.0, 0.7)));
        p1.push_child(ring);

        // ===== Panel 2: exclude =====
        let cx2 = x0 + cell_w * 1.5;
        let cy2 = panel_y;
        let rw = 110.0;
        let rh = 80.0;
        let er = 26.0;
        let mut p2 = panel_frame(cx2, cy2, cell_w, cell_h, "exclude: 矩形挖掉小圆");
        let excl_geom = make_disk_area(cx2 + 16.0, cy2 - 8.0, er);
        p2.area_exclude = Some(excl_geom);
        for i in 0..40 {
            let phase = t * 1.2 + i as f32 * 0.25;
            let x = cx2 - 50.0 + (i % 8) as f32 * 14.0 + phase.sin() * 8.0;
            let y = cy2 - 40.0 + (i / 8) as f32 * 16.0 + phase.cos() * 6.0;
            let hue = (i as f32 / 40.0 + t * 0.04) % 1.0;
            let c = Color::new(0.5 + hue * 0.4, 0.3 + hue * 0.5, 0.5 + (1.0 - hue) * 0.4, 0.95);
            draw_circle(&mut p2, Pos::new(x, y), 6.0, Some(c));
        }
        // 描边矩形 + 挖空小圆
        let mut ring = DrawBatch::new();
        ring.sdf_feather = Some(1.0);
        draw_rectangle(&mut ring, Pos::new(cx2 - rw * 0.5, cy2 - rh * 0.5), rw, rh, Some(Color::new(0.9, 0.9, 1.0, 0.5)));
        draw_circle(&mut ring, Pos::new(cx2 + 16.0, cy2 - 8.0), er, Some(Color::new(0.9, 0.5, 0.5, 0.6)));
        p2.push_child(ring);

        // ===== Panel 3: ∩ =====
        let cx3 = x0 + cell_w * 2.5;
        let cy3 = panel_y;
        let off3 = 30.0;
        let mut p3 = panel_frame(cx3, cy3, cell_w, cell_h, "intersect: 两圆重叠");
        let disk_a = make_disk_area(cx3 - off3, cy3, r);
        let disk_b = make_disk_area(cx3 + off3, cy3, r);
        p3.area_include = Some(disk_a.intersect(disk_b));
        for i in 0..30 {
            let phase = t * 1.5 + i as f32 * 0.35;
            let x = cx3 - 70.0 + (i % 6) as f32 * 28.0 + phase.sin() * 14.0;
            let y = cy3 - 50.0 + (i / 6) as f32 * 22.0 + phase.cos() * 10.0;
            let hue = (i as f32 / 30.0 + t * 0.06) % 1.0;
            let c = Color::new(0.4 + hue * 0.5, 0.6 + (1.0 - hue) * 0.3, 0.5 + hue * 0.4, 0.95);
            draw_rounded_rect(&mut p3, Pos::new(x, y), 18.0, 14.0, 3.0, Some(c));
        }
        // 两圆描边
        let mut ring = DrawBatch::new();
        ring.sdf_feather = Some(1.0);
        draw_circle(&mut ring, Pos::new(cx3 - off3, cy3), r, Some(Color::new(0.9, 0.9, 1.0, 0.6)));
        draw_circle(&mut ring, Pos::new(cx3 + off3, cy3), r, Some(Color::new(0.9, 0.9, 1.0, 0.6)));
        p3.push_child(ring);

        // ===== Panel 4: ∪ =====
        let cx4 = x0 + cell_w * 3.5;
        let cy4 = panel_y;
        let off4 = 36.0;
        let mut p4 = panel_frame(cx4, cy4, cell_w, cell_h, "union: 两圆合并");
        let disk_a = make_disk_area(cx4 - off4, cy4, r * 0.85);
        let disk_b = make_disk_area(cx4 + off4, cy4, r * 0.85);
        p4.area_include = Some(disk_a.union(disk_b));
        for i in 0..32 {
            let phase = t * 1.3 + i as f32 * 0.3;
            let x = cx4 - 70.0 + (i % 7) as f32 * 22.0 + phase.sin() * 12.0;
            let y = cy4 - 50.0 + (i / 7) as f32 * 20.0 + phase.cos() * 8.0;
            let hue = (i as f32 / 32.0 + t * 0.05) % 1.0;
            let c = Color::new(0.5 + hue * 0.4, 0.4 + hue * 0.4, 0.5 + (1.0 - hue) * 0.5, 0.95);
            draw_circle(&mut p4, Pos::new(x, y), 7.0, Some(c));
        }
        // 描边
        let mut ring = DrawBatch::new();
        ring.sdf_feather = Some(1.0);
        draw_circle(&mut ring, Pos::new(cx4 - off4, cy4), r * 0.85, Some(Color::new(0.9, 0.9, 1.0, 0.6)));
        draw_circle(&mut ring, Pos::new(cx4 + off4, cy4), r * 0.85, Some(Color::new(0.9, 0.9, 1.0, 0.6)));
        p4.push_child(ring);

        win.draw(
            Some(Color::new(0.05, 0.06, 0.09, 1.0)),
            &[&ui, &p1, &p2, &p3, &p4],
        );
        true
    });
}

fn panel_frame(cx: f32, cy: f32, w: f32, h: f32, title: &str) -> DrawBatch {
    let mut b = DrawBatch::new();
    // 外框
    draw_rectangle(
        &mut b,
        Pos::new(cx - w * 0.5, cy - h * 0.5),
        w,
        h,
        Some(Color::new(0.12, 0.14, 0.2, 1.0)),
    );
    draw_text(
        &mut b.texts,
        title,
        TextOptions::default()
            .x(cx - w * 0.5 + 6.0)
            .y(cy - h * 0.5 + 4.0)
            .font_size(12.0)
            .color(Color::new(0.7, 0.75, 0.85, 1.0)),
    );
    b
}

/// 用 `DrawBatch::to_area()` 烘焙一个圆盘 Area（中心 + 半径）。
fn make_disk_area(cx: f32, cy: f32, r: f32) -> Area {
    let mut b = DrawBatch::new();
    b.sdf_feather = Some(1.0);
    draw_circle(&mut b, Pos::new(cx, cy), r, None);
    b.to_area()
}
