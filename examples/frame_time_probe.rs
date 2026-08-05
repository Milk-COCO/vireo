//! 帧间隔探针：cargo run 跑 12 帧后自动退出。
//!
//! cargo run --example frame_time_probe
//! cargo run --example frame_time_probe -- --no-text
//! cargo run --example frame_time_probe -- --immediate

use std::time::Instant;
use vireo::prelude::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let no_text = args.iter().any(|a| a == "--no-text");
    let immediate = args.iter().any(|a| a == "--immediate");

    eprintln!(
        "frame_time_probe  text={}  present={}",
        !no_text,
        if immediate { "Immediate" } else { "AutoVsync" }
    );

    let t0 = Instant::now();
    let mut app = App::new();
    let mut desc = WindowDesc::new("frame_time_probe", 600, 400)
        .anti_aliasing(AntiAliasing::None)
        .active(true);
    if immediate {
        desc = desc.present_mode(PresentMode::Immediate);
    }
    let idx = app.window(desc, None::<fn()>);
    eprintln!(
        "  App::init_duration={:.1}ms  setup={:.1}ms",
        app.init_duration() * 1000.0,
        t0.elapsed().as_secs_f64() * 1000.0
    );

    let mut focused = false;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        if !focused {
            win.focus();
            focused = true;
            eprintln!("  win.init_duration={:.1}ms", win.init_duration() * 1000.0);
        }

        let mut batch = DrawBatch::new();
        for i in 0..60 {
            let h = 10.0 + (i as f32) * 0.5;
            draw_rectangle(&mut batch, Pos::new(20.0 + i as f32 * 10.0, 250.0 - h), 8.0, h, Some(GREEN));
        }
        if !no_text {
            let info = format!(
                "FPS: {:.1}\nFrame time: {:.3}ms\nFrames: {}",
                app.fps,
                app.frame_time * 1000.0,
                app.frame_count
            );
            draw_text(
                &mut batch.texts,
                &info,
                Pos::new(20.0, 20.0),
                TextDef::default().font_size(16.0), TextOverride::from_color(WHITE),
            );
            draw_text(
                &mut batch.texts,
                "static line two",
                Pos::new(20.0, 100.0),
                TextDef::default().font_size(12.0), TextOverride::from_color(WHITE),
            );
            draw_text(
                &mut batch.texts,
                "static line three",
                Pos::new(20.0, 130.0),
                TextDef::default().font_size(12.0), TextOverride::from_color(WHITE),
            );
        }

        let report = win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        let acq_ms = report.timings.acquire_secs * 1000.0;
        let enc_ms = report.timings.encode_secs * 1000.0;

        eprintln!(
            "  F{:<2}  frame_time={:7.2}ms  acq={:6.2} enc={:6.2}  fps={:5.1}  focused={}",
            app.frame_count,
            app.frame_time * 1000.0,
            acq_ms,
            enc_ms,
            app.fps,
            win.focused(),
        );

        if app.frame_count >= 12 {
            eprintln!("done.");
            false
        } else {
            true
        }
    });
}
