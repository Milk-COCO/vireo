//! 帧统计 + spike 诊断
//!
//! 交互：
//! - 1/2/3/4: AA = None / MSAA4 / MSAA8 / SSAA4
//! - T: 开关文字（A/B：有字 vs 无字）
//! - V: 切换 PresentMode AutoVsync ↔ Immediate
//!
//! Spike 阈值 20ms。分类（按主因，互斥）：
//! - encode >= 4ms → ENGINE（合并/上传/pass/submit）
//! - acquire >= 4ms → ACQUIRE（get_current_texture 等 swapchain）
//! - build  >= 4ms → BUILD（CPU 构图，不含 GPU）
//! - 否则 → OS/SCHED（事件循环/调度/present 后间隙）
//!
//! 注意：旧版把 acquire 算进 draw，容易把 vsync 等纹理误判成 ENGINE。
//! VIREO_QUIET=1 关闭 stderr spike 日志。

use std::cell::RefCell;
use std::rc::Rc;

use vireo::prelude::*;

const HISTORY_CAP: usize = 300;
const SPIKE_THRESHOLD_MS: f64 = 20.0;
/// 分段主因阈值：超过则优先归到该段。
const SEGMENT_MS: f64 = 4.0;

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

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let pos = q * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = pos - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

fn compute_stats(history: &[f64]) -> (f64, f64, f64, f64, f64, f64, f64) {
    if history.is_empty() {
        return (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    }
    let mut sorted = history.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    let lo = sorted[0];
    let hi = sorted[n - 1];
    let sum: f64 = sorted.iter().sum();
    let avg = sum / n as f64;
    let var: f64 = sorted.iter().map(|v| (v - avg).powi(2)).sum::<f64>() / n as f64;
    let stddev = var.sqrt();
    (
        lo,
        hi,
        avg,
        quantile(&sorted, 0.50),
        quantile(&sorted, 0.95),
        quantile(&sorted, 0.99),
        stddev,
    )
}

fn main() {
    let quiet = std::env::var("VIREO_QUIET").is_ok();

    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Vireo Frame Stats", 720, 440).anti_aliasing(AntiAliasing::None),
        None::<fn()>,
    );

    let app_init_ms = app.init_duration() * 1000.0;
    let mut history: Vec<f64> = Vec::with_capacity(HISTORY_CAP);
    let mut current_aa = AntiAliasing::None;
    let mut show_text = true;
    let mut present_immediate = false;
    let mut was_focused = true;
    let pending_aa: Rc<RefCell<Option<AntiAliasing>>> = Rc::new(RefCell::new(None));
    let toggle_text: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let toggle_present: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let mut key_registered = false;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let max_sc = win.gpu().max_sample_count();
        let win_init_ms = win.init_duration() * 1000.0;

        let focused = win.focused();
        if !quiet && focused != was_focused {
            eprintln!("[focus] F{} focused={}", app.frame_count, focused);
        }
        was_focused = focused;

        if !key_registered {
            key_registered = true;
            let pending = pending_aa.clone();
            let t_text = toggle_text.clone();
            let t_present = toggle_present.clone();
            win.on_key_down(move |event| {
                if !event.state.is_pressed() || event.repeat {
                    return;
                }
                match event.key {
                    KeyCode::Digit1 => {
                        *pending.borrow_mut() = Some(AntiAliasing::None);
                    }
                    KeyCode::Digit2 if max_sc >= 4 => {
                        *pending.borrow_mut() = Some(AntiAliasing::Msaa {
                            samples: 4,
                            alpha_to_coverage: false,
                        });
                    }
                    KeyCode::Digit3 if max_sc >= 8 => {
                        *pending.borrow_mut() = Some(AntiAliasing::Msaa {
                            samples: 8,
                            alpha_to_coverage: false,
                        });
                    }
                    KeyCode::Digit4 if max_sc >= 4 => {
                        *pending.borrow_mut() = Some(AntiAliasing::Ssaa {
                            samples: 4,
                            alpha_to_coverage: false,
                        });
                    }
                    KeyCode::KeyT => {
                        *t_text.borrow_mut() = true;
                    }
                    KeyCode::KeyV => {
                        *t_present.borrow_mut() = true;
                    }
                    _ => {}
                }
            });
        }

        if let Some(aa) = pending_aa.borrow_mut().take() {
            win.set_anti_aliasing(aa);
            current_aa = aa;
            history.clear();
        }
        if std::mem::take(&mut *toggle_text.borrow_mut()) {
            show_text = !show_text;
            history.clear();
            if !quiet {
                eprintln!("[diag] text={}", show_text);
            }
        }
        if std::mem::take(&mut *toggle_present.borrow_mut()) {
            present_immediate = !present_immediate;
            let mode = if present_immediate {
                PresentMode::Immediate
            } else {
                PresentMode::AutoVsync
            };
            win.set_present_mode(mode);
            history.clear();
            if !quiet {
                eprintln!("[diag] present={:?}", mode);
            }
        }

        let ft_ms = app.frame_time * 1000.0;
        let t_build = std::time::Instant::now();

        let mut batch = DrawBatch::new();

        history.push(ft_ms);
        if history.len() > HISTORY_CAP {
            history.remove(0);
        }
        let (lo, hi, avg, p50, p95, p99, stddev) = compute_stats(&history);
        let spike_count = history.iter().filter(|&&v| v > SPIKE_THRESHOLD_MS).count();

        let present_label = if present_immediate {
            "Immediate"
        } else {
            "AutoVsync"
        };
        let info = format!(
            "FPS: {:.1}\n\
Frame time: {:6.2}ms  avg {:5.2} / p50 {:5.2} / p95 {:5.2} / p99 {:5.2}\n\
  min {:5.2} / max {:5.2} / stddev {:4.2}  (n={})\n\
Spikes (>{:.0}ms): {} / {}\n\
AA: {}  |  Present: {}  |  Text: {}\n\
Focus: {}  |  Frames: {}\n\
Init: app {:.0}ms + win {:.0}ms",
            app.fps,
            ft_ms,
            avg,
            p50,
            p95,
            p99,
            lo,
            hi,
            stddev,
            history.len(),
            SPIKE_THRESHOLD_MS,
            spike_count,
            history.len(),
            aa_label(current_aa),
            present_label,
            if show_text { "on" } else { "off" },
            if focused { "yes" } else { "NO" },
            app.frame_count,
            app_init_ms,
            win_init_ms,
        );
        if show_text {
            draw_text(
                &mut batch.texts,
                &info,
                TextOptions::default().x(16.0).y(12.0).font_size(12.0).color(WHITE),
            );
        }

        let k_msaa8 = if max_sc >= 8 { " 3=MSAA8" } else { "" };
        let keys = format!(
            "1=None 2=MSAA4{} 4=SSAA4  |  T=text  V=vsync/imm  |  hw max {}x",
            k_msaa8, max_sc,
        );
        if show_text {
            draw_text(
                &mut batch.texts,
                &keys,
                TextOptions::default()
                    .x(16.0)
                    .y(148.0)
                    .font_size(11.0)
                    .color(Color::new(0.6, 0.6, 0.7, 1.0)),
            );
        }

        let bar_width = 6.0f32;
        let bar_gap = 1.0f32;
        let max_ft_display = 50.0_f64;
        let graph_x = 16.0_f32;
        let graph_y = 250.0_f32;
        let graph_h = 100.0_f32;
        let target_ft = 1000.0 / 60.0;
        let n = history.len();
        let total_w = (n as f32) * (bar_width + bar_gap);

        for (i, &ft) in history.iter().enumerate() {
            let h = (ft / max_ft_display * graph_h as f64).min(graph_h as f64) as f32;
            let x = graph_x + i as f32 * (bar_width + bar_gap);
            let y = graph_y + graph_h - h;
            let color = if ft <= target_ft {
                GREEN
            } else if ft <= SPIKE_THRESHOLD_MS {
                Color::new(1.0, 0.8, 0.0, 1.0)
            } else {
                RED
            };
            draw_rectangle(&mut batch, x, y, bar_width, h, color);
        }

        let target_y = graph_y + graph_h - (target_ft / max_ft_display * graph_h as f64) as f32;
        draw_rectangle(
            &mut batch,
            graph_x,
            target_y,
            total_w.max(1.0),
            1.0,
            Color::new(0.4, 0.6, 0.9, 0.7),
        );

        if show_text {
            draw_text(
                &mut batch.texts,
                "spike: encode>=4=ENGINE acquire>=4=ACQUIRE build>=4=BUILD else OS/SCHED",
                TextOptions::default()
                    .x(16.0)
                    .y(228.0)
                    .font_size(11.0)
                    .color(Color::new(0.5, 0.5, 0.6, 1.0)),
            );
        }

        let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;
        let timings = win.draw_timed(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        let acq_ms = timings.acquire_secs * 1000.0;
        let enc_ms = timings.encode_secs * 1000.0;

        if !quiet && ft_ms > SPIKE_THRESHOLD_MS {
            // 主因互斥：encode > acquire > build > OS
            let kind = if enc_ms >= SEGMENT_MS {
                "ENGINE"
            } else if acq_ms >= SEGMENT_MS {
                "ACQUIRE"
            } else if build_ms >= SEGMENT_MS {
                "BUILD"
            } else {
                "OS/SCHED"
            };
            eprintln!(
                "[spike] F{} dt={:6.2}ms build={:5.2} acq={:5.2} enc={:5.2} kind={} focus={} text={} present={}",
                app.frame_count,
                ft_ms,
                build_ms,
                acq_ms,
                enc_ms,
                kind,
                focused,
                show_text,
                present_label,
            );
        }
        true
    });
}