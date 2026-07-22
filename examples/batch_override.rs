//! Shape / ShapeOverride / 画笔状态机
//!
//! 演示：
//! - `batch.set_color` + `draw_*(…, None)` 用笔刷色
//! - `draw_*(…, Some(c))` 仅本次覆盖颜色
//! - `Shape` + `draw_shape` + `ShapeOverride`（色 / SDF / 变换 / 贴图 / UV）
//! - `set_texture(Some|None)` 状态机
//!
//! ```bash
//! cargo run --example shape_options
//! ```

use vireo::prelude::*;

fn label(batch: &mut DrawBatch, text: &str, x: f32, y: f32) {
    draw_text(
        &mut batch.texts,
        text,
        TextOptions::default()
            .x(x)
            .y(y)
            .font_size(13.0)
            .color(Color::new(0.65, 0.68, 0.78, 1.0)),
    );
}

fn main() {
    let mut app = App::new();
    let logo = app.load_texture("logo_quad.png").ok();
    let idx = app.window(
        WindowDesc::new("ShapeOverride", 900, 520).high_dpi(true),
        None::<fn()>,
    );

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.02;

        let mut batch = DrawBatch::new();
        batch.sdf_feather = Some(1.0);

        // ---- 1) 笔刷色 + Option 颜色 ----
        label(&mut batch, "1) set_color + draw_*(None | Some)", 20.0, 16.0);

        batch.set_color(ORANGE);
        draw_circle(&mut batch, Pos::new(60.0, 80.0), 28.0, None); // 笔刷橙
        draw_circle(&mut batch, Pos::new(130.0, 80.0), 28.0, Some(SKYBLUE)); // 仅本次
        draw_circle(&mut batch, Pos::new(200.0, 80.0), 28.0, None); // 仍是橙
        label(&mut batch, "None / Some(SKYBLUE) / None", 20.0, 118.0);

        // ---- 2) Shape enum ----
        label(&mut batch, "2) Shape + draw_shape", 280.0, 16.0);
        draw_shape(
            &mut batch,
            &Shape::RoundedRect {
                pos: Pos::new(280.0, 48.0),
                w: 100.0,
                h: 60.0,
                radius: 14.0,
            },
            ShapeOverride::from_color(Some(PURPLE)),
        );
        draw_shape(
            &mut batch,
            &Shape::Triangle {
                x1: 420.0,
                y1: 108.0,
                x2: 400.0,
                y2: 48.0,
                x3: 460.0,
                y3: 48.0,
            },
            ShapeOverride::from_color(Some(YELLOW)),
        );

        // ---- 3) ShapeOverride：SDF / 几何 / 变换（不写回 batch）----
        label(
            &mut batch,
            "3) ShapeOverride: sdf / geometry / transform (no write-back)",
            20.0,
            150.0,
        );

        // batch 仍是 SDF；本次强制 geometry
        draw_shape(
            &mut batch,
            &Shape::Circle {
                pos: Pos::new(70.0, 230.0),
                r: 36.0,
            },
            ShapeOverride::new().color(GREEN).geometry(),
        );
        label(&mut batch, "geometry()", 40.0, 280.0);

        // 本次 SDF + 绝对变换（batch 的 set_position 不受影响）
        batch.set_position(0.0, 0.0);
        draw_shape(
            &mut batch,
            &Shape::Rect {
                pos: Pos::new(-30.0, -20.0),
                w: 60.0,
                h: 40.0,
            },
            ShapeOverride::new()
                .color(RED)
                .sdf(1.5)
                .transform(Transform::trs(
                    220.0,
                    230.0,
                    0.0,
                    0.0,
                    t,
                    1.0 + 0.15 * t.sin(),
                    1.0,
                )),
        );
        label(&mut batch, "transform(trs…)", 170.0, 280.0);

        // 验证：batch 状态仍是 sdf + 原点
        draw_rounded_rect(&mut batch, Pos::new(320.0, 200.0), 70.0, 50.0, 10.0, None);
        label(&mut batch, "batch state after opts", 300.0, 280.0);

        // ---- 4) 贴图状态机 + 仅本次 texture / UV ----
        label(
            &mut batch,
            "4) set_texture + ShapeOverride::texture / uv / clear",
            20.0,
            320.0,
        );

        if let Some(i) = logo {
            if let Some(tex) = app.texture(i) {
                let s = 0.08;
                let w = tex.width as f32 * s;
                let h = tex.height as f32 * s;

                // 笔刷贴图：后续白色矩形会采样 logo
                batch.set_texture(Some(tex));
                draw_rectangle(&mut batch, Pos::new(30.0, 360.0), w, h, Some(WHITE));
                label(&mut batch, "set_texture(Some)", 30.0, 360.0 + h + 4.0);

                // 仅本次 UV 子区域（不改 batch.uv）
                draw_shape(
                    &mut batch,
                    &Shape::Rect {
                        pos: Pos::new(160.0, 360.0),
                        w: w,
                        h: h,
                    },
                    ShapeOverride::new()
                        .color(WHITE)
                        .texture(tex)
                        .uv_rect(0.0, 0.0, 0.5, 0.5),
                );
                label(&mut batch, "opts.uv 半区", 160.0, 360.0 + h + 4.0);

                // 仅本次清贴图（白填充），batch 仍挂着 logo
                draw_shape(
                    &mut batch,
                    &Shape::RoundedRect {
                        pos: Pos::new(290.0, 360.0),
                        w: 80.0,
                        h: 60.0,
                        radius: 12.0,
                    },
                    ShapeOverride::new().color(PINK).clear_texture(),
                );
                label(&mut batch, "clear_texture()", 290.0, 430.0);

                // batch 仍有贴图
                draw_rectangle(&mut batch, Pos::new(400.0, 360.0), w * 0.7, h * 0.7, Some(WHITE));
                label(&mut batch, "仍 set_texture", 400.0, 360.0 + h * 0.7 + 4.0);

                batch.set_texture(None);
                draw_rectangle(&mut batch, Pos::new(520.0, 360.0), 70.0, 50.0, Some(SKYBLUE));
                label(&mut batch, "set_texture(None)", 500.0, 430.0);
            }
        } else {
            label(
                &mut batch,
                "(no logo_quad.png — texture demos skipped)",
                30.0,
                370.0,
            );
            draw_shape(
                &mut batch,
                &Shape::Circle {
                    pos: Pos::new(80.0, 400.0),
                    r: 30.0,
                },
                ShapeOverride::new().color(GOLD),
            );
        }

        // ---- 5) batch.rectangle 方法 + 状态色 ----
        label(&mut batch, "5) batch.rectangle / Shape 方法", 620.0, 16.0);
        batch.set_color(Color::new(0.3, 0.7, 1.0, 1.0));
        batch.rectangle(Pos::new(640.0, 50.0), 90.0, 50.0, None);
        batch.shape(
            &Shape::Ellipse {
                pos: Pos::new(760.0, 75.0),
                rx: 40.0,
                ry: 25.0,
            },
            ShapeOverride::new().color(LIME).sdf(1.0),
        );

        win.draw(Some(Color::new(0.06, 0.07, 0.1, 1.0)), &[&batch]);
        true
    });
}
