//! 复现 / 验证：请求 MSAA 8x 时是否 panic，还是被 clamp。
//!
//! ```text
//! cargo run --example msaa_clamp
//! ```
//!
//! 输出：
//! - 硬件 `supported_sample_counts` / `max_sample_count`
//! - 建窗请求 8x 后实际 sample_count
//! - 运行时 `set_anti_aliasing(8x)` 是否崩

use vireo::prelude::*;

fn main() {
    let mut app = App::new();

    let supported = app.gpu.supported_sample_counts().to_vec();
    let max_sc = app.gpu.max_sample_count();
    let snapped = app.gpu.clamp_sample_count(8);
    let has_adapter_fmt = app
        .gpu
        .device
        .features()
        .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
    eprintln!("=== MSAA capability ===");
    eprintln!("  supported_sample_counts = {:?}", supported);
    eprintln!("  max_sample_count        = {}", max_sc);
    eprintln!("  TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES = {}", has_adapter_fmt);
    eprintln!("  request 8x → clamp_sample_count = {}", snapped);
    eprintln!();

    // 路径 1：建窗时直接要 8x（window() 内应 snap，不应 panic）
    eprintln!("[1] App::window(Msaa 8x) ...");
    let idx = app.window(
        WindowDesc::new("msaa_clamp", 480, 320).anti_aliasing(AntiAliasing::Msaa {
            samples: 8,
            alpha_to_coverage: false,
        }),
        None::<fn()>,
    );
    eprintln!("[1] window() returned (pipeline preheat OK if we got here)");

    let mut tried_runtime = false;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        if !tried_runtime {
            tried_runtime = true;
            eprintln!();
            eprintln!("[2] after create: max_sample_count still {}", win.gpu().max_sample_count());
            eprintln!("[2] set_anti_aliasing(Msaa 8x) ...");
            // 路径 2：运行时再要一次 8x（应 clamp + eprintln，不应 panic）
            win.set_anti_aliasing(AntiAliasing::Msaa {
                samples: 8,
                alpha_to_coverage: false,
            });
            eprintln!("[2] set_anti_aliasing returned OK");

            // 路径 3：若硬件 max < 8，再画一帧强制走 msaa 纹理创建
            eprintln!("[3] draw one frame (creates MSAA texture if samples>1) ...");
        }

        let mut batch = DrawBatch::new();
        draw_circle(&mut batch, Pos::new(240.0, 160.0), 80.0, Some(Color::new(0.2, 0.6, 1.0, 1.0)));
        draw_rounded_rect(&mut batch, Pos::new(40.0, 40.0), 120.0, 80.0, 16.0, Some(Color::new(0.15, 0.5, 0.3, 1.0)));
            draw_text(
                &mut batch.texts,
                &format!(
                    "request 8x | hw max {} | supported {:?}",
                    win.gpu().max_sample_count(),
                    win.gpu().supported_sample_counts()
                ),
                Pos::new(12.0, 12.0), TextDef::default().font_size(14.0),
                TextOverride::from_color(WHITE),
            );
        win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);

        if app.frame_count >= 3 {
            eprintln!("[3] draw OK — no panic. exit.");
            false
        } else {
            true
        }
    });
}
