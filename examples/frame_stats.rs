//! 帧统计 + spike 诊断
//!
//! 交互：
//! - 1/2/3/4: AA = None / MSAA4 / MSAA8 / SSAA4
//! - T: 开关文字（A/B：有字 vs 无字）
//! - V: 切换 PresentMode AutoVsync ↔ Immediate
//!
//! Spike 阈值 20ms。分类（按已测 CPU 阶段，互斥）：
//! - build  >= 4ms → BUILD（CPU 构图）
//! - gpu >= 4ms → GPU（queue submission completion，含 GPU/驱动排队）
//! - encode >= 4ms → ENCODE（命令编码/资源更新；不等于 GPU 执行）
//! - 否则 → WAIT/OS（present、swapchain 或线程调度等待）
//!
//! 注意：当前 draw 的 acquire 字段包含跨线程等待，不能单独证明是
//! get_current_texture；GPU 忙时的驱动等待也不能直接归因于引擎负载。
//! VIREO_QUIET=1 关闭 stderr spike 日志。

use std::sync::{Arc, Mutex};

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
    let mut last_gpu_ms: Option<f64> = None;
    let pending_aa: Arc<Mutex<Option<AntiAliasing>>> = Arc::new(Mutex::new(None));
    let toggle_text: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let toggle_present: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    let pending_aa_keys = Arc::clone(&pending_aa);
    let toggle_text_keys = Arc::clone(&toggle_text);
    let toggle_present_keys = Arc::clone(&toggle_present);
    app.on_key_down(idx, move |event| {
        if event.repeat {
            return;
        }
        match event.key {
            KeyCode::KeyT => {
                *toggle_text_keys.lock().unwrap() = true;
            }
            KeyCode::KeyV => {
                *toggle_present_keys.lock().unwrap() = true;
            }
            KeyCode::Digit1 => {
                *pending_aa_keys.lock().unwrap() = Some(AntiAliasing::None);
            }
            KeyCode::Digit2 => {
                *pending_aa_keys.lock().unwrap() = Some(AntiAliasing::Msaa {
                    samples: 4,
                    alpha_to_coverage: false,
                });
            }
            KeyCode::Digit3 => {
                *pending_aa_keys.lock().unwrap() = Some(AntiAliasing::Msaa {
                    samples: 8,
                    alpha_to_coverage: false,
                });
            }
            KeyCode::Digit4 => {
                *pending_aa_keys.lock().unwrap() = Some(AntiAliasing::Ssaa {
                    samples: 4,
                    alpha_to_coverage: false,
                });
            }
            _ => {}
        }
    });

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        win.set_gpu_timing(true);
        let max_sc = win.gpu().max_sample_count();
        let win_init_ms = win.init_duration() * 1000.0;

        let focused = win.focused();
        if !quiet && focused != was_focused {
            eprintln!("[focus] F{} focused={}", app.frame_count, focused);
        }
        was_focused = focused;

        if let Some(aa) = pending_aa.lock().unwrap().take() {
            win.set_anti_aliasing(aa);
            current_aa = aa;
            history.clear();
        }
        if std::mem::take(&mut *toggle_text.lock().unwrap()) {
            show_text = !show_text;
            history.clear();
            if !quiet {
                eprintln!("[diag] text={}", show_text);
            }
        }
        if std::mem::take(&mut *toggle_present.lock().unwrap()) {
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
        if show_text {
            let def = TextDef::default().font_size(12.0);
            let rows = [
                (vec![TextPart::normal("FPS: "), TextPart::glyphs(format!("{:.1}", app.fps))], 12.0),
                (vec![
                    TextPart::normal("Frame time: "), TextPart::glyphs(format!("{:6.2}", ft_ms)),
                    TextPart::normal("ms  avg "), TextPart::glyphs(format!("{:5.2}", avg)),
                    TextPart::normal(" / p50 "), TextPart::glyphs(format!("{:5.2}", p50)),
                    TextPart::normal(" / p95 "), TextPart::glyphs(format!("{:5.2}", p95)),
                    TextPart::normal(" / p99 "), TextPart::glyphs(format!("{:5.2}", p99)),
                ], 26.0),
                (vec![
                    TextPart::normal("GPU queue: "),
                    last_gpu_ms.map_or_else(
                        || TextPart::dynamic("n/a"),
                        |v| TextPart::glyphs(format!("{:6.2}", v)),
                    ),
                    TextPart::normal("ms (previous completed submission)"),
                ], 40.0),
                (vec![
                    TextPart::normal("min "), TextPart::glyphs(format!("{:5.2}", lo)),
                    TextPart::normal(" / max "), TextPart::glyphs(format!("{:5.2}", hi)),
                    TextPart::normal(" / stddev "), TextPart::glyphs(format!("{:4.2}", stddev)),
                    TextPart::normal("  (n="), TextPart::glyphs(history.len().to_string()), TextPart::normal(")"),
                ], 54.0),
                (vec![
                    TextPart::normal("Spikes (>"), TextPart::glyphs(format!("{:.0}", SPIKE_THRESHOLD_MS)),
                    TextPart::normal("ms): "), TextPart::glyphs(spike_count.to_string()),
                    TextPart::normal(" / "), TextPart::glyphs(history.len().to_string()),
                ], 68.0),
                (vec![
                    TextPart::normal("AA: "), TextPart::dynamic(aa_label(current_aa)),
                    TextPart::normal("  |  Present: "), TextPart::dynamic(present_label),
                    TextPart::normal("  |  Text: "), TextPart::dynamic(if show_text { "on" } else { "off" }),
                ], 82.0),
                (vec![
                    TextPart::normal("Focus: "), TextPart::dynamic(if focused { "yes" } else { "NO" }),
                    TextPart::normal("  |  Frames: "), TextPart::glyphs(app.frame_count.to_string()),
                ], 96.0),
                (vec![
                    TextPart::normal("Init: app "), TextPart::glyphs(format!("{:.0}", app_init_ms)),
                    TextPart::normal("ms + win "), TextPart::glyphs(format!("{:.0}", win_init_ms)), TextPart::normal("ms"),
                ], 110.0),
            ];
            for (row, y) in rows {
                batch.text_parts(
                    &row,
                    Pos::new(16.0, y),
                    def.clone(),
                    TextOverride::from_color(WHITE),
                );
            }
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
                Pos::new(16.0, 148.0),
                TextDef::default().font_size(11.0),
                TextOverride::from_color(Color::new(0.6, 0.6, 0.7, 1.0)),
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
            draw_rectangle(&mut batch, Pos::new(x, y), bar_width, h, Some(color));
        }

        let target_y = graph_y + graph_h - (target_ft / max_ft_display * graph_h as f64) as f32;
        draw_rectangle(&mut batch, Pos::new(graph_x, target_y), total_w.max(1.0), 1.0, Some(Color::new(0.4, 0.6, 0.9, 0.7)));

        if show_text {
            draw_text(
                &mut batch.texts,
                "spike: build>=4=BUILD encode>=4=ENCODE else WAIT/OS",
                Pos::new(16.0, 228.0),
                TextDef::default().font_size(11.0),
                TextOverride::from_color(Color::new(0.5, 0.5, 0.6, 1.0)),
            );
        }

        let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;
        let timings = win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        let acq_ms = timings.acquire_secs * 1000.0;
        let enc_ms = timings.encode_secs * 1000.0;
        let gpu_ms = timings.gpu_secs.map(|v| v * 1000.0);

        if !quiet && ft_ms > SPIKE_THRESHOLD_MS {
            // build/encode are CPU-side measurements. The remaining frame
            // interval includes present, driver and scheduler waits.
            let kind = if build_ms >= SEGMENT_MS {
                "BUILD"
            } else if gpu_ms.is_some_and(|v| v >= SEGMENT_MS) {
                "GPU"
            } else if enc_ms >= SEGMENT_MS {
                "ENCODE"
            } else {
                "WAIT/OS"
            };
            eprintln!(
                "[spike] F{} dt={:6.2}ms build={:5.2} wait={:5.2} encode={:5.2} gpu={:>5} kind={} focus={} text={} present={}",
                app.frame_count,
                ft_ms,
                build_ms,
                acq_ms,
                enc_ms,
                gpu_ms.map_or_else(|| "  n/a".to_string(), |v| format!("{:5.2}", v)),
                kind,
                focused,
                show_text,
                present_label,
            );
        }
        last_gpu_ms = gpu_ms;
        true
    });
}
