//! 多子 batch：`InheritFromParent` + 部分 `unclipped` + 动态裁切
//!
//! - 左两列：整列子默认 `clipped=true`（越界裁切）
//! - 右两列：同一父下**混合** clipped / unclipped 子
//! - 色块大幅晃动，便于看出裁切边界
//!
//! ```bash
//! cargo run --example batch_inherit
//! ```

use vireo::prelude::*;

fn label(batch: &mut DrawBatch, text: &str, x: f32, y: f32) {
    draw_text(
        &mut batch.texts,
        text,
        TextOptions::default()
            .x(x)
            .y(y)
            .font_size(12.0)
            .color(Color::new(0.7, 0.75, 0.85, 1.0)),
    );
}

/// 一组晃动色块（局部坐标，中心 0,0）
fn wobble_blocks(batch: &mut DrawBatch, t: f32, phase0: f32, use_brush: bool) {
    let n = 10;
    for i in 0..n {
        let phase = t * 2.2 + i as f32 * 0.55 + phase0;
        let ox = phase.sin() * 58.0;
        let oy = (phase * 0.85 + 0.9).cos() * 62.0;
        let lx = -36.0 + (i % 5) as f32 * 18.0 + ox * 0.35;
        let ly = -30.0 + (i / 5) as f32 * 24.0 + oy * 0.35;
        // 再加一层大偏移，故意伸出 mask
        let lx = lx + ox * 0.55;
        let ly = ly + oy * 0.55;
        let hue = (i as f32 / n as f32 + t * 0.08 + phase0 * 0.1) % 1.0;
        let fill = if use_brush && i % 3 == 0 {
            None
        } else {
            Some(Color::new(
                0.35 + hue * 0.55,
                0.4 + (1.0 - hue) * 0.4,
                0.55 + hue * 0.3,
                0.92,
            ))
        };
        draw_rounded_rect(batch, Pos::new(lx, ly), 32.0, 20.0, 5.0, fill);
    }
}

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Batch Inherit + Partial Unclip", 960, 520).high_dpi(true),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.025;
        let angle = t * 18.0;

        let mut ui = DrawBatch::new();
        label(
            &mut ui,
            "父 clips_children 写 mask  |  子 inherit.clipped 可逐子 opt-out  |  色块大幅晃动",
            16.0,
            12.0,
        );

        // ---- 列 0：NONE + 全裁 ----
        // ---- 列 1：TRANSFORM + 全裁 ----
        // ---- 列 2：同父 双·子 clipped + unclipped（局部坐标，不继承 transform）----
        // ---- 列 3：同父 双·子 ALL.clipped + ALL.unclipped（跟父转）----
        let centers = [(110.0, 250.0), (340.0, 250.0), (580.0, 250.0), (820.0, 250.0)];
        let titles = [
            "NONE 全裁",
            "TRANSFORM 全裁",
            "混：clip + unclip",
            "混 ALL：clip + unclip",
        ];

        let mut roots: Vec<DrawBatch> = Vec::new();

        // 列 0
        {
            let (cx, cy) = centers[0];
            let mut parent = DrawBatch::new();
            parent.sdf_feather = Some(2.0);
            parent.set_color(ORANGE);
            parent.set_position(cx, cy);
            parent.set_deg(angle);
            parent.clips_children = true;
            draw_rounded_rect(
                &mut parent,
                Pos::new(-68.0, -68.0),
                136.0,
                136.0,
                20.0,
                Some(Color::new(0.22, 0.25, 0.34, 0.95)),
            );

            let mut child = DrawBatch::new();
            child.sdf_feather = Some(0.0);
            child.set_color(SKYBLUE);
            child.set_position(cx, cy);
            child.inherit = InheritFromParent::NONE; // clipped=true
            wobble_blocks(&mut child, t, 0.0, true);
            parent.push_child(child);
            label(&mut ui, titles[0], cx - 40.0, cy + 100.0);
            roots.push(parent);
        }

        // 列 1
        {
            let (cx, cy) = centers[1];
            let mut parent = DrawBatch::new();
            parent.sdf_feather = Some(2.0);
            parent.set_color(ORANGE);
            parent.set_position(cx, cy);
            parent.set_deg(angle + 20.0);
            parent.clips_children = true;
            draw_rounded_rect(
                &mut parent,
                Pos::new(-68.0, -68.0),
                136.0,
                136.0,
                20.0,
                Some(Color::new(0.22, 0.25, 0.34, 0.95)),
            );

            let mut child = DrawBatch::new();
            child.sdf_feather = Some(0.0);
            child.set_color(SKYBLUE);
            child.inherit = InheritFromParent::TRANSFORM;
            wobble_blocks(&mut child, t, 1.1, true);
            parent.push_child(child);
            label(&mut ui, titles[1], cx - 52.0, cy + 100.0);
            roots.push(parent);
        }

        // 列 2：同一父下两个子 — 蓝 clipped / 粉 unclipped（屏幕位姿）
        {
            let (cx, cy) = centers[2];
            let mut parent = DrawBatch::new();
            parent.sdf_feather = Some(2.0);
            parent.set_color(ORANGE);
            parent.set_position(cx, cy);
            parent.set_deg(angle * 0.5);
            parent.clips_children = true;
            draw_rounded_rect(
                &mut parent,
                Pos::new(-68.0, -68.0),
                136.0,
                136.0,
                20.0,
                Some(Color::new(0.22, 0.25, 0.34, 0.95)),
            );

            // 子 A：被裁
            let mut a = DrawBatch::new();
            a.sdf_feather = Some(0.0);
            a.set_color(SKYBLUE);
            a.set_position(cx, cy);
            a.inherit = InheritFromParent::NONE; // clipped
            for i in 0..6 {
                let phase = t * 2.0 + i as f32 * 0.7;
                let x = -50.0 + (i as f32) * 18.0 + phase.sin() * 40.0;
                let y = -20.0 + phase.cos() * 50.0;
                draw_rounded_rect(&mut a, Pos::new(x, y), 28.0, 18.0, 4.0, None);
            }
            parent.push_child(a);

            // 子 B：不裁，粉红，可越界
            let mut b = DrawBatch::new();
            b.sdf_feather = Some(0.0);
            b.set_color(Color::new(1.0, 0.45, 0.55, 0.9));
            b.set_position(cx, cy);
            b.inherit = InheritFromParent::NONE.unclipped();
            for i in 0..6 {
                let phase = t * 2.0 + i as f32 * 0.7 + 1.5;
                let x = -50.0 + (i as f32) * 18.0 + phase.sin() * 48.0;
                let y = 10.0 + phase.cos() * 55.0;
                draw_rounded_rect(&mut b, Pos::new(x, y), 28.0, 18.0, 4.0, None);
            }
            parent.push_child(b);

            label(&mut ui, titles[2], cx - 58.0, cy + 100.0);
            roots.push(parent);
        }

        // 列 3：同一父下 ALL 跟转 — 橙 clipped / 亮黄 unclipped
        {
            let (cx, cy) = centers[3];
            let mut parent = DrawBatch::new();
            parent.sdf_feather = Some(2.0);
            parent.set_color(ORANGE);
            parent.set_position(cx, cy);
            parent.set_deg(angle + 40.0);
            parent.clips_children = true;
            draw_rounded_rect(
                &mut parent,
                Pos::new(-68.0, -68.0),
                136.0,
                136.0,
                20.0,
                Some(Color::new(0.22, 0.25, 0.34, 0.95)),
            );

            let mut a = DrawBatch::new();
            a.sdf_feather = Some(0.0);
            a.set_color(ORANGE);
            a.sdf_feather = Some(2.0);
            a.inherit = InheritFromParent::ALL; // transform + color + clipped
            for i in 0..5 {
                let phase = t * 1.8 + i as f32;
                let x = -45.0 + phase.sin() * 50.0;
                let y = -30.0 + i as f32 * 14.0 + phase.cos() * 20.0;
                draw_rounded_rect(&mut a, Pos::new(x, y), 30.0, 16.0, 4.0, None);
            }
            parent.push_child(a);

            let mut b = DrawBatch::new();
            b.sdf_feather = Some(2.0);
            b.set_color(Color::new(1.0, 0.9, 0.25, 0.95));
            // 跟父转，但不测 stencil
            b.inherit = InheritFromParent::TRANSFORM.unclipped();
            // 颜色不继承，保持亮黄（上面已 set）
            for i in 0..5 {
                let phase = t * 1.8 + i as f32 + 2.0;
                let x = -20.0 + phase.cos() * 55.0;
                let y = -40.0 + i as f32 * 16.0 + phase.sin() * 25.0;
                draw_rounded_rect(&mut b, Pos::new(x, y), 30.0, 16.0, 4.0, None);
            }
            parent.push_child(b);

            label(&mut ui, titles[3], cx - 70.0, cy + 100.0);
            roots.push(parent);
        }

        label(
            &mut ui,
            "列2/3：同父多子 — 蓝·橙=clipped  |  粉·黄=unclipped（可画出圆角外）",
            16.0,
            490.0,
        );

        let mut refs: Vec<&DrawBatch> = vec![&ui];
        for r in &roots {
            refs.push(r);
        }
        win.draw(Some(Color::new(0.06, 0.07, 0.1, 1.0)), &refs);
        true
    });
}
