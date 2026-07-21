//! 多子 batch 不同 `InheritFromParent` 标志对比
//!
//! 父：`set_position` + `set_deg` + `color=ORANGE` + `sdf_feather=2.0`。
//! 四个子分别 `NONE` / `TRANSFORM` / `color+feather` / `ALL`。
//!
//! 说明：`color` / `sdf_feather` / `uv` 在绘制前写入子画笔才影响顶点；
//! `transform` 在 `push_child` 时左乘已生成的 `transform_table`。
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

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Batch InheritFromParent", 920, 420).high_dpi(true),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.02;
        let angle = t * 28.0;

        let mut ui = DrawBatch::new();
        label(
            &mut ui,
            "父：position + rotate + color=ORANGE + feather=2  |  子 inherit 各不同（默认局部色 SKYBLUE）",
            16.0,
            12.0,
        );

        let panels: [(&str, InheritFromParent); 4] = [
            ("NONE", InheritFromParent::NONE),
            ("TRANSFORM", InheritFromParent::TRANSFORM),
            ("color+feather", InheritFromParent::NONE.color().sdf_feather()),
            ("ALL", InheritFromParent::ALL),
        ];
        let centers = [(120.0, 200.0), (340.0, 200.0), (560.0, 200.0), (780.0, 200.0)];

        let mut roots: Vec<DrawBatch> = Vec::new();
        for ((title, inherit), (cx, cy)) in panels.iter().zip(centers.iter()) {
            let mut parent = DrawBatch::new();
            parent.sdf_feather = Some(2.0);
            parent.set_color(ORANGE);
            parent.set_position(*cx, *cy);
            parent.set_deg(angle);

            draw_rounded_rect(
                &mut parent,
                -55.0,
                -55.0,
                110.0,
                110.0,
                14.0,
                Some(Color::new(0.22, 0.25, 0.34, 0.95)),
            );
            draw_rect_outline(
                &mut parent,
                -55.0,
                -55.0,
                110.0,
                110.0,
                1.5,
                Some(Color::new(0.5, 0.55, 0.7, 1.0)),
            );

            let mut child = DrawBatch::new();
            // 默认：硬边 + 蓝；继承 color/feather 时在绘制前写入画笔
            child.sdf_feather = Some(0.0);
            child.set_color(SKYBLUE);
            if inherit.color {
                child.set_color(parent.color);
            }
            if inherit.sdf_feather {
                child.sdf_feather = parent.sdf_feather;
            }
            // transform 留给 push_child 左乘；不继承时用屏幕坐标摆到面板中心
            if !inherit.transform {
                child.set_position(*cx, *cy);
            }
            child.inherit = InheritFromParent {
                transform: inherit.transform,
                // 画笔已在上面写入；push 时再写一次无妨
                color: inherit.color,
                sdf_feather: inherit.sdf_feather,
                uv: inherit.uv,
            };

            draw_rounded_rect(&mut child, -28.0, -18.0, 56.0, 36.0, 6.0, None);
            draw_circle(
                &mut child,
                0.0,
                0.0,
                10.0,
                Some(Color::new(1.0, 1.0, 1.0, 0.9)),
            );

            parent.push_child(child);
            label(&mut ui, title, *cx - 36.0, *cy + 78.0);
            roots.push(parent);
        }

        label(
            &mut ui,
            "NONE: 不跟转/蓝  |  TRANSFORM: 跟转/蓝  |  color+feather: 橙+软边、不跟转  |  ALL: 跟转且橙+软边",
            16.0,
            390.0,
        );

        let mut refs: Vec<&DrawBatch> = vec![&ui];
        for r in &roots {
            refs.push(r);
        }
        win.draw(Some(Color::new(0.06, 0.07, 0.1, 1.0)), &refs);
        true
    });
}
