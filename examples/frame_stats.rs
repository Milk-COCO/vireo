//! 帧统计示例：展示 FPS / 帧耗时 / 抗锯齿模式
//!
//! 交互（仅显示当前硬件支持的 AA）：
//! - 1: AA = None
//! - 2: AA = MSAA 4x
//! - 3: AA = MSAA 8x（如不支持则忽略按键）
//! - 4: AA = SSAA 4x
//!
//! 切换 AA 时，新管线在 `set_anti_aliasing` 内同步预热，无首帧 hitch。

use std::cell::RefCell;
use std::rc::Rc;

use vireo::prelude::*;

fn aa_label(aa: AntiAliasing) -> String {
    match aa {
        AntiAliasing::None => "None".to_string(),
        AntiAliasing::Msaa { samples, alpha_to_coverage } => {
            if alpha_to_coverage {
                format!("MSAA {}x (ATC)", samples)
            } else {
                format!("MSAA {}x", samples)
            }
        }
        AntiAliasing::Ssaa { samples, alpha_to_coverage } => {
            if alpha_to_coverage {
                format!("SSAA {}x (ATC)", samples)
            } else {
                format!("SSAA {}x", samples)
            }
        }
    }
}

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Vireo Frame Stats", 600, 400)
            .anti_aliasing(AntiAliasing::None),
        None::<fn()>,
    );

    let app_init_ms = app.init_duration() * 1000.0;
    let mut history: Vec<f64> = Vec::with_capacity(60);
    let mut current_aa = AntiAliasing::None;
    let pending_aa: Rc<RefCell<Option<AntiAliasing>>> = Rc::new(RefCell::new(None));
    let mut key_registered = false;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let max_sc = win.gpu().max_sample_count();
        let win_init_ms = win.init_duration() * 1000.0;

        if !key_registered {
            key_registered = true;
            let pending = pending_aa.clone();
            win.on_key_down(move |event| {
                if !event.state.is_pressed() || event.repeat {
                    return;
                }
                let new_aa = match event.key {
                    KeyCode::Digit1 => Some(AntiAliasing::None),
                    KeyCode::Digit2 if max_sc >= 4 => {
                        Some(AntiAliasing::Msaa { samples: 4, alpha_to_coverage: false })
                    }
                    KeyCode::Digit3 if max_sc >= 8 => {
                        Some(AntiAliasing::Msaa { samples: 8, alpha_to_coverage: false })
                    }
                    KeyCode::Digit4 if max_sc >= 4 => {
                        Some(AntiAliasing::Ssaa { samples: 4, alpha_to_coverage: false })
                    }
                    _ => None,
                };
                if let Some(aa) = new_aa {
                    *pending.borrow_mut() = Some(aa);
                }
            });
        }

        if let Some(aa) = pending_aa.borrow_mut().take() {
            win.set_anti_aliasing(aa);
            current_aa = aa;
            history.clear();
        }

        let ft_ms = app.frame_time * 1000.0;
        history.push(ft_ms);
        if history.len() > 60 {
            history.remove(0);
        }

        let (min_ft, max_ft, avg_ft) = if history.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            let mut lo = f64::MAX;
            let mut hi = 0.0_f64;
            let mut sum = 0.0;
            for &v in &history {
                lo = lo.min(v);
                hi = hi.max(v);
                sum += v;
            }
            (lo, hi, sum / history.len() as f64)
        };

        let mut batch = DrawBatch::new();

        let info = format!(
            "FPS: {:.1}\nFrame time: {:.3}ms  (avg {:.3} / min {:.3} / max {:.3})\nAA: {}\nFrames: {}\nInit: app {:.0}ms + win {:.0}ms",
            app.fps,
            ft_ms,
            avg_ft,
            min_ft,
            max_ft,
            aa_label(current_aa),
            app.frame_count,
            app_init_ms,
            win_init_ms,
        );
        draw_text(
            &mut batch.texts,
            &info,
            TextOptions::default().x(20.0).y(20.0).font_size(14.0).color(WHITE),
        );

        let k_msaa8 = if max_sc >= 8 { " 3=MSAA 8x" } else { "" };
        let keys = format!(
            "Keys: 1=None  2=MSAA 4x{}  4=SSAA 4x   (hw max MSAA: {}x)",
            k_msaa8, max_sc,
        );
        draw_text(
            &mut batch.texts,
            &keys,
            TextOptions::default().x(20.0).y(135.0).font_size(12.0).color(Color::new(0.6, 0.6, 0.7, 1.0)),
        );

        let bar_width = 8.0f32;
        let bar_gap = 2.0f32;
        let max_ft_display = 33.3_f64;
        let graph_x = 20.0_f32;
        let graph_y = 250.0_f32;
        let graph_h = 100.0_f32;
        let target_ft = 1000.0 / 60.0;

        for (i, &ft) in history.iter().enumerate() {
            let h = (ft / max_ft_display * graph_h as f64).min(graph_h as f64) as f32;
            let x = graph_x + i as f32 * (bar_width + bar_gap);
            let y = graph_y + graph_h - h;
            let color = if ft <= target_ft {
                GREEN
            } else if ft <= target_ft * 2.0 {
                Color::new(1.0, 0.8, 0.0, 1.0)
            } else {
                RED
            };
            draw_rectangle(&mut batch, x, y, bar_width, h, color);
        }

        let target_y = graph_y + graph_h - (target_ft / max_ft_display * graph_h as f64) as f32;
        let total_w = 60.0 * (bar_width + bar_gap);
        draw_rectangle(
            &mut batch,
            graph_x,
            target_y,
            total_w,
            1.0,
            Color::new(0.4, 0.6, 0.9, 0.7),
        );

        draw_text(
            &mut batch.texts,
            "Frame time (ms) — green <= 16.67ms (60Hz), red > 33.3ms",
            TextOptions::default().x(20.0).y(230.0).font_size(12.0).color(Color::new(0.5, 0.5, 0.6, 1.0)),
        );

        win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        true
    });
}
