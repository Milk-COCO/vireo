//! 旋转：绕 pivot 旋转 + 平移

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("旋转", 500, 400), None::<fn()>);

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.02;

        // 三个转子绕 (180,200) 公转 + 各自自转
        let mut rotors = DrawBatch::new();
        for i in 0..3 {
            let orbit_angle = t + i as f32 * std::f32::consts::TAU / 3.0;
            rotors.orbit_transform(180.0, 200.0, 60.0, orbit_angle,
                0.0, 0.0, t * 3.0 + i as f32, 1.0, 1.0);
            draw_rectangle(&mut rotors, -20.0, -4.0, 40.0, 8.0,
                match i { 0 => RED, 1 => GREEN, _ => BLUE });
            draw_rectangle(&mut rotors, -4.0, -20.0, 8.0, 40.0,
                match i { 0 => RED, 1 => GREEN, _ => BLUE });
        }

        // 矩形绕自己中心旋转
        let mut spinning = DrawBatch::new();
        spinning.set_position(350.0, 200.0);
        spinning.set_rotation(-t * 2.0);
        draw_rect_outline(&mut spinning, -50.0, -50.0, 100.0, 100.0,
            3.0, Color::new(1.0, 0.8, 0.2, 1.0));
        // 文字不受 transform 影响
        spinning.clear_transform();
        draw_text(&mut spinning.texts, "Rotate!",
            TextOptions::default().x(300.0).y(190.0).font_size(16.0).color(WHITE));

        win.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&rotors, &spinning]);

        true
    });
}
