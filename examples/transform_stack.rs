//! 叠加变换：translate / rotate_* / scale_by / apply_matrix（右乘局部空间）
//!
//! 对比：
//! - 左：绝对 set_position + set_rad
//! - 中：叠加链（机械臂）
//! - 右：apply_matrix 剪切

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Transform Stack", 900, 420), None::<fn()>);

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.02;
        let mut batch = DrawBatch::new();
        batch.sdf_feather = Some(1.0);

        draw_text(
            &mut batch.texts,
            "Absolute set_*  |  Incremental stack  |  apply_matrix",
            TextOptions::default()
                .x(20.0)
                .y(16.0)
                .font_size(16.0)
                .color(Color::new(0.7, 0.75, 0.85, 1.0)),
        );

        // ---- 绝对：set_position + set_rad ----
        batch.clear_transform();
        batch.set_position(150.0, 220.0);
        batch.set_rad(t);
        batch.set_pivot(0.0, 0.0);
        draw_rectangle(&mut batch, -40.0, -12.0, 80.0, 24.0, Some(RED));
        draw_circle(&mut batch, 40.0, 0.0, 8.0, Some(YELLOW));
        batch.clear_transform();
        draw_text(
            &mut batch.texts,
            "set_position + set_rad",
            TextOptions::default()
                .x(70.0)
                .y(320.0)
                .font_size(13.0)
                .color(Color::new(0.55, 0.55, 0.65, 1.0)),
        );

        // ---- 叠加：基座 → 大臂 → 小臂（右乘局部）----
        batch.clear_transform();
        batch.set_position(450.0, 280.0);
        // 大臂
        batch.rotate_rad(t * 0.7);
        draw_rectangle(&mut batch, 0.0, -10.0, 100.0, 20.0, Some(GREEN));
        draw_circle(&mut batch, 0.0, 0.0, 12.0, Some(Color::new(0.2, 0.8, 0.4, 1.0)));
        // 小臂：在大臂末端局部空间继续叠加
        batch.translate(100.0, 0.0);
        batch.rotate_rad(t * 1.4);
        batch.scale_by(0.85 + 0.15 * t.sin(), 1.0);
        draw_rectangle(&mut batch, 0.0, -8.0, 70.0, 16.0, Some(Color::new(0.3, 0.9, 0.7, 1.0)));
        draw_circle(&mut batch, 70.0, 0.0, 10.0, Some(Color::new(0.2, 0.9, 0.95, 1.0)));
        batch.clear_transform();
        draw_text(
            &mut batch.texts,
            "translate → rotate → scale_by",
            TextOptions::default()
                .x(360.0)
                .y(320.0)
                .font_size(13.0)
                .color(Color::new(0.55, 0.55, 0.65, 1.0)),
        );

        // ---- apply_matrix：剪切 + 平移 ----
        batch.clear_transform();
        let shear = 0.35 * (t * 0.5).sin();
        // | 1  s |   局部 x 沿 y 剪切
        // | 0  1 |
        batch.set_position(720.0, 220.0);
        batch.apply_matrix(1.0, 0.0, shear, 1.0, 0.0, 0.0);
        batch.rotate_deg((t * 40.0) % 360.0);
        draw_rounded_rect(&mut batch, -50.0, -40.0, 100.0, 80.0, 12.0, Some(PURPLE));
        draw_rect_outline(&mut batch, -50.0, -40.0, 100.0, 80.0, 2.0, Some(WHITE));
        batch.clear_transform();
        draw_text(
            &mut batch.texts,
            "apply_matrix (shear)",
            TextOptions::default()
                .x(650.0)
                .y(320.0)
                .font_size(13.0)
                .color(Color::new(0.55, 0.55, 0.65, 1.0)),
        );

        // 参考点
        for (x, y) in [(150.0, 220.0), (450.0, 280.0), (720.0, 220.0)] {
            draw_circle_outline(&mut batch, x, y, 4.0, 1.5, Some(Color::new(1.0, 1.0, 1.0, 0.35)), 16);
        }

        win.draw(Some(Color::new(0.05, 0.06, 0.09, 1.0)), &[&batch]);
        true
    });
}
