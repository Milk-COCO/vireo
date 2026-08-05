//! PresentMode 全模式：AutoVsync / Fifo / Mailbox / Immediate
//!
//! 键 1–4 切换；显示 FPS、frame_time、acquire/encode（draw）。

use vireo::prelude::*;

fn mode_name(m: PresentMode) -> &'static str {
    match m {
        PresentMode::AutoVsync => "AutoVsync",
        PresentMode::AutoNoVsync => "AutoNoVsync",
        PresentMode::Fifo => "Fifo",
        PresentMode::FifoRelaxed => "FifoRelaxed",
        PresentMode::Immediate => "Immediate",
        PresentMode::Mailbox => "Mailbox",
    }
}

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Present Mode", 640, 360).present_mode(PresentMode::AutoVsync),
        None::<fn()>,
    );

    let modes = [
        PresentMode::AutoVsync,
        PresentMode::Fifo,
        PresentMode::Mailbox,
        PresentMode::Immediate,
    ];
    let mut mode_i = 0usize;
    let mut key_was = [false; 4];
    let mut last_acq_ms = 0.0f64;
    let mut last_enc_ms = 0.0f64;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        let keys = [
            win.key_down(KeyCode::Digit1),
            win.key_down(KeyCode::Digit2),
            win.key_down(KeyCode::Digit3),
            win.key_down(KeyCode::Digit4),
        ];
        for i in 0..4 {
            if keys[i] && !key_was[i] {
                mode_i = i;
                win.set_present_mode(modes[i]);
            }
            key_was[i] = keys[i];
        }

        let mut batch = DrawBatch::new();
        let cur = win.present_mode();
        let lines = [
            format!("PresentMode: {}  (set_present_mode)", mode_name(cur)),
            "1 AutoVsync  2 Fifo  3 Mailbox  4 Immediate".into(),
            format!(
                "FPS: {:.1}   frame_time: {:.2} ms",
                app.fps,
                app.frame_time * 1000.0
            ),
            format!(
                "last acquire: {:.2} ms   encode: {:.2} ms",
                last_acq_ms, last_enc_ms
            ),
            format!("active index: {mode_i}"),
        ];
        for (i, line) in lines.iter().enumerate() {
            draw_text(
                &mut batch.texts,
                line,
                Pos::new(24.0, 28.0 + i as f32 * 30.0), TextDef::default().font_size(17.0),
                TextOverride::from_color(if i == 0 {
                    GOLD
                } else if i == 3 {
                    Color::new(0.6, 0.85, 0.7, 1.0)
                } else {
                    WHITE
                }),
            );
        }

        let t = app.frame_count as f32 * 0.05;
        let x = 80.0 + (t.sin() * 0.5 + 0.5) * 400.0;
        draw_rounded_rect(&mut batch, Pos::new(x, 220.0), 80.0, 60.0, 10.0, Some(Color::new(0.3, 0.6, 1.0, 1.0)));

        let report = win.draw(Some(Color::new(0.06, 0.07, 0.1, 1.0)), &[&batch]);
        last_acq_ms = report.timings.acquire_secs * 1000.0;
        last_enc_ms = report.timings.encode_secs * 1000.0;
        true
    });
}
