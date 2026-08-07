//! 多窗口帧率验证：不同显示设置下各窗口的表现
//!
//! W0 / W1 的 PresentMode 与 frame_latency 可独立设置，每帧都调用两者的
//! `win.draw`个窗口 HUD 显示自己的 Present / Latency、上一帧
//! acquire/encode/gpu/present 耗时、presented_fps 与全局 app.fps。
//! 初始差异化：W0 = AutoVsync + latency 2，W1 = Immediate + latency 1。
//!
//! 按键（任意窗口聚焦均可）：
//!   V / C = 切 W0 / W1 的 present mode（AutoVsync ↔ Immediate）
//!   L / K = 切 W0 / W1 的 frame_latency（2 / 1）
//!   S = 半速模式：W1 隔帧跳过 draw（真正省一半提交）
//!   0 / 1 = 开关对应窗口的 draw（0 = 关闭该窗口的绘制）
//!   P = 全局暂停（画面冻结，看清数字；再按恢复）

use std::sync::{Arc, Mutex};

use vireo::prelude::*;

#[derive(Default)]
struct Ctrl {
    toggle_mode: [bool; 2],
    toggle_lat: [bool; 2],
    half: bool,
    pause: bool,
    toggle: [bool; 2],
}

struct WinCfg {
    name: &'static str,
    bg: Color,
}

const CONFIGS: [WinCfg; 2] = [
    WinCfg { name: "W0", bg: Color::new(0.06, 0.09, 0.16, 1.0) },
    WinCfg { name: "W1", bg: Color::new(0.05, 0.13, 0.09, 1.0) },
];

fn main() {
    let quiet = std::env::var("VIREO_QUIET").is_ok();

    let mut app = App::new();
    let mut idxs = Vec::new();
    for cfg in &CONFIGS {
        idxs.push(app.window(
            WindowDesc::new(cfg.name, 420, 300).anti_aliasing(AntiAliasing::None),
            None::<fn()>,
        ));
    }

    let ctrl = Arc::new(Mutex::new(Ctrl::default()));
    for idx in &idxs {
        let c = Arc::clone(&ctrl);
        app.on_key_down(*idx, move |event| {
            if event.repeat {
                return;
            }
            let mut c = c.lock().unwrap();
            match event.key {
                KeyCode::KeyV => c.toggle_mode[0] = true,
                KeyCode::KeyC => c.toggle_mode[1] = true,
                KeyCode::KeyL => c.toggle_lat[0] = true,
                KeyCode::KeyK => c.toggle_lat[1] = true,
                KeyCode::KeyS => c.half = true,
                KeyCode::KeyP => c.pause = true,
                KeyCode::Digit0 => c.toggle[0] = true,
                KeyCode::Digit1 => c.toggle[1] = true,
                _ => {}
            }
        });
    }

    // 上一帧各窗口 report timings: (acquire, encode, gpu, present) ms
    let mut prev: [Option<(f64, f64, Option<f64>, f64)>; 2] = [None; 2];
    let mut disabled = [false; 2];
    let mut half_mode = false;
    let mut paused = false;
    // 每窗口独立显示设置
    let mut modes = [false; 2]; // false=AutoVsync, true=Immediate
    let mut latencies = [2u32; 2];
    let mut applied_mode = [false; 2];
    let mut applied_latency = [2u32; 2];
    // 初始差异化
    modes[1] = true;
    latencies[1] = 1;

    app.run(move |app| {
        {
            let mut c = ctrl.lock().unwrap();
            for i in 0..2 {
                if c.toggle_mode[i] {
                    modes[i] = !modes[i];
                    c.toggle_mode[i] = false;
                    prev[i] = None;
                }
                if c.toggle_lat[i] {
                    latencies[i] = if latencies[i] == 2 { 1 } else { 2 };
                    c.toggle_lat[i] = false;
                    prev[i] = None;
                }
            }
            if c.half {
                half_mode = !half_mode;
                c.half = false;
                prev = [None; 2];
            }
            if c.pause {
                paused = !paused;
                c.pause = false;
                prev = [None; 2];
            }
            for (i, &t) in c.toggle.iter().enumerate() {
                if t {
                    disabled[i] = !disabled[i];
                    prev[i] = None;
                }
            }
            c.toggle = [false; 2];
        }

        // 全局暂停：不 draw 任何窗口 → 画面冻结在上一帧；循环仍跑以响应按键
        if paused {
            return true;
        }

        let ft_ms = app.frame_time * 1000.0;
        if !quiet && app.frame_count % 60 == 0 {
            eprintln!(
                "[fps] global fps={:.1} ft={:.2}ms | W0={} lat{} | W1={} lat{} | half={} disabled={:?}",
                app.fps,
                ft_ms,
                if modes[0] { "IMM" } else { "VSYNC" },
                latencies[0],
                if modes[1] { "IMM" } else { "VSYNC" },
                latencies[1],
                half_mode,
                disabled,
            );
        }

        for i in 0..2 {
            let win = match app.window_ref(&idxs[i]) {
                Some(w) => w,
                None => continue,
            };
            win.set_gpu_timing(true);

            // 应用该窗口自己的 present mode / latency —— 只在变化时设置一次
            //（每帧设会触发每帧 surface.configure，DX12 上每次 50-80ms 卡顿）
            if modes[i] != applied_mode[i] {
                win.set_present_mode(if modes[i] { PresentMode::Immediate } else { PresentMode::AutoVsync });
                applied_mode[i] = modes[i];
            }
            if latencies[i] != applied_latency[i] {
                win.set_frame_latency(latencies[i]);
                applied_latency[i] = latencies[i];
            }

            // 该窗口本帧是否被跳过
            let skip = disabled[i] || (half_mode && i == 1 && app.frame_count % 2 == 0);
            if skip {
                // 不调用 draw → 该窗口画面保持上一帧，且不参与本帧 acquire
                continue;
            }

            let mut batch = DrawBatch::new();
            // 动画由「该窗口自己的 presented 计数」驱动：刷新快的窗口转得快。
            let pframes = win.presented_frames() as f32;
            let cx = 150.0;
            let cy = 155.0;
            let rot = pframes * 0.08;
            batch.orbit_transform(cx, cy, 45.0, rot * 0.6, 0.0, 0.0, rot, 1.0, 1.0);
            draw_rectangle(&mut batch, Pos::new(-20.0, -3.0), 40.0, 6.0, Some(WHITE));
            batch.clear_transform();
            draw_circle(&mut batch, Pos::new(cx, cy), 48.0, Some(Color::new(0.18, 0.22, 0.32, 1.0)));

            let row = |batch: &mut DrawBatch, parts: Vec<TextPart>, y: f32, fs: f32| {
                batch.text_parts(
                    &parts,
                    Pos::new(10.0, y),
                    TextDef::default().font_size(fs),
                    TextOverride::from_color(WHITE),
                );
            };
            row(&mut batch, vec![
                TextPart::normal(CONFIGS[i].name),
                TextPart::normal("  Present: "),
                TextPart::dynamic(if modes[i] { "Immediate" } else { "AutoVsync" }.to_string()),
                TextPart::normal("  Latency: "),
                TextPart::dynamic(latencies[i].to_string()),
                TextPart::normal(if disabled[i] { "  [DISABLED]" } else { "" }),
            ], 10.0, 15.0);
            row(&mut batch, vec![
                TextPart::normal("Global FPS: "),
                TextPart::glyphs(format!("{:5.1}", app.fps)),
                TextPart::normal("  frame: "),
                TextPart::glyphs(format!("{:6.2}", ft_ms)),
                TextPart::normal("ms"),
            ], 34.0, 12.0);
            row(&mut batch, vec![
                TextPart::normal("Presented FPS: "),
                TextPart::glyphs(format!("{:5.1}", win.presented_fps())),
                TextPart::normal("  presented "),
                TextPart::glyphs(win.presented_frames().to_string()),
                TextPart::normal(" skipped "),
                TextPart::glyphs(win.skipped_frames().to_string()),
            ], 54.0, 12.0);
            match prev[i] {
                Some((acq, enc, gpu, pres)) => {
                    row(&mut batch, vec![
                        TextPart::normal("last acq: "),
                        TextPart::glyphs(format!("{:6.2}", acq)),
                        TextPart::normal("  enc: "),
                        TextPart::glyphs(format!("{:6.2}", enc)),
                        TextPart::normal("  gpu: "),
                        TextPart::glyphs(gpu.map_or(" n/a".into(), |g| format!("{:6.2}", g))),
                        TextPart::normal("  pres: "),
                        TextPart::glyphs(format!("{:6.2}", pres)),
                        TextPart::normal(" ms"),
                    ], 74.0, 12.0);
                }
                None => {
                    row(&mut batch, vec![TextPart::dynamic("(sampling...)")], 74.0, 12.0);
                }
            }
            batch.text_parts(
                &[TextPart::normal("V=W0pm C=W1pm L=W0lat K=W1lat S=half P=pause 0/1=win")],
                Pos::new(10.0, 100.0),
                TextDef::default().font_size(12.0),
                TextOverride::from_color(Color::new(1.0, 0.9, 0.3, 1.0)),
            );

            let report = win.draw(CONFIGS[i].bg, &[&batch]);
            let t = report.timings;
            prev[i] = Some((
                t.acquire_secs * 1000.0,
                t.encode_secs * 1000.0,
                t.gpu_secs.map(|v| v * 1000.0),
                t.present_secs * 1000.0,
            ));
        }

        true
    });
}
