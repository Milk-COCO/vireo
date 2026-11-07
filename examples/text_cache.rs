//! 文字**字形缓存**（`gpu.text().glyphs()`）示例：预算 / 预光栅化 / hold / 持久化。
//!
//! 和 `text_typeset_cache` 的区别：
//! - 那个管**排版**（shape 过的 Buffer，管"排一次"）；
//! - 这个管**光栅化**（图集里的位图 / 距离场，管"画一次"）。
//!
//! 画面分三块，HUD 直接显示缓存数字（HUD 用固定 4 个字号的 `glyphs` 段，
//! 不会污染统计）：
//!
//! 1. 上：被 `hold` 钉住的标题（永远不会被淘汰，也不会重光栅化）；
//! 2. 中：每帧换新字号的滚动文本（故意制造 miss，靠保留预算回收）；
//! 3. 下：说明 + 当前预算模式 / 占用 / 纹理 / 条目 / 光栅化 / 压缩次数。
//!
//! 操作：
//! - `1` 保留预算 = 256 KiB（**不抖**，只清历史积压）
//! - `2` 保留预算 = 1 MiB
//! - `3` 无预算
//! - `H` 硬预算 = 256 KiB（<工作集，**会抖** —— 用来对比两种语义）
//! - `A` 自动压缩开关（默认开）
//! - `T` 手动按预算淘汰一次
//! - `C` 清掉未钉住的字形
//! - `K` 手动收缩图集
//! - `S` 把图集存到临时文件
//! - `L` 从临时文件装载（会先清空）
//! - 空格 暂停/继续滚动（暂停后统计应完全不动 = 跨帧零重光栅化）
//!
//! ```bash
//! cargo run --example text_cache
//! ```

use std::time::{Duration, Instant};

use vireo::prelude::*;

const TITLE: &str = "Hold 钉住：字形缓存";
const ROLLING: &[&str] = &[
    "prewarm 一次，之后每帧都是命中",
    "保留预算只清 1 秒没用的冷字形，永不掉帧",
    "hard 预算低于工作集才会每帧重光栅化（掉帧）",
    "compact 把图集收缩回去，利用率低时还会自动做",
    "save/load 跨进程复用已光栅化的图集",
];

fn cache_file() -> std::path::PathBuf {
    std::env::temp_dir().join("vireo_example_text_cache.vrtx")
}

/// 自动基准场景：60 段互不重复的 12 字文本（24px），每帧全画。
///
/// 工作集明显大于小预算，所以能把「预算 < 工作集 ⇒ 每帧重光栅化」在真实窗口里复现出来。
fn bench_grid() -> Vec<String> {
    (0..60u32)
        .map(|i| {
            (0..12u32)
                .filter_map(|j| char::from_u32(0x4E00 + i * 12 + j))
                .collect::<String>()
        })
        .collect()
}

#[vireo::main]
async fn main() {
    // `VIREO_TEXT_CACHE_NOVSYNC=1`：关掉垂直同步跑基准 —— 帧耗时才等于**真实 CPU 成本**
    // （开着 vsync 时 acquire/present 会把数字钳在 8.33 ms 附近，看不出编码成本）。
    let no_vsync = std::env::var("VIREO_TEXT_CACHE_NOVSYNC").is_ok();
    let mut desc = WindowDesc::new("Text Glyph Cache", 960, 640);
    if no_vsync {
        desc = desc.present_mode(PresentMode::AutoNoVsync).max_fps(None);
    }
    let idx = app.window(desc);
    // `VIREO_TEXT_CACHE_BENCH=1`：不等人按键，自动跑预算三档、每档打印一行到 stderr，
    // 最后一档结束就退出。用来在真实窗口（含 vsync/合成）里复跑无头测出来的结论。
    let bench = std::env::var("VIREO_TEXT_CACHE_BENCH").is_ok();
    let bench_texts = bench_grid();
    const BENCH_WARMUP: u32 = 30;
    const BENCH_FRAMES: u32 = 120;
    /// 基准三档：软预算（不抖）/ 硬预算（会抖）/ 不限制 —— 直接对比两种语义。
    #[derive(Clone, Copy)]
    enum BenchMode {
        Soft(u64),
        Hard(u64),
        Off,
    }
    const BENCH_MODES: [BenchMode; 3] = [
        BenchMode::Soft(256 * 1024),
        BenchMode::Hard(256 * 1024),
        BenchMode::Off,
    ];
    const BENCH_LABELS: [&str; 3] = [
        "软预算 256 KiB（<工作集）",
        "硬预算 256 KiB（会抖）",
        "不限制",
    ];
    let apply_mode = |glyphs: &GlyphCache<'_>, mode: BenchMode| match mode {
        BenchMode::Soft(b) => {
            glyphs.set_retention_budget(Some(b));
            glyphs.set_hard_budget(None);
        }
        BenchMode::Hard(b) => {
            glyphs.set_hard_budget(Some(b));
            glyphs.set_retention_budget(None);
        }
        BenchMode::Off => {
            glyphs.set_retention_budget(None);
            glyphs.set_hard_budget(None);
        }
    };
    let mut bench_stage = 0usize;
    let mut bench_frame = 0u32;
    let mut bench_rasters = 0u64;
    let mut bench_sum_ms = 0.0f64;
    let mut bench_encode_ms = 0.0f64;
    let mut bench_wait_ms = 0.0f64;
    let mut bench_frames_done = 0u32;

    let mut paused = false;
    let mut budget_label = "软 1 MiB（默认）";
    let mut rolling = 0usize;
    let mut last_log = Instant::now() - Duration::from_secs(1);
    let mut redraw_hud = true;
    let mut hud_line = String::new();
    let mut t0 = Instant::now();
    // 钉住的句柄必须活到进程结束；首帧拿到真实 DPI 后再创建（见下）。
    let mut hold: Option<GlyphHold> = None;

    if bench {
        eprintln!(
            "[bench] 自动基准：{} 段 × 12 字 @24px，每档 {BENCH_WARMUP} 帧预热 + {BENCH_FRAMES} 帧计数",
            bench_texts.len()
        );
    } else {
        eprintln!("[text_cache] K 收缩  S 保存  L 装载  空格 暂停");
    }

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let gpu = &ctx.app().gpu;
        let glyphs = gpu.text().glyphs();

        // 首帧才知道真实 DPI：预光栅化的 `scale` 必须与绘制一致，否则字号键对不上
        // （`physical` 会把 font_size 乘 scale），一帧之后全部变成 miss。
        if hold.is_none() {
            let scale = win.layout_scale() as f32;
            apply_mode(&glyphs, if bench { BENCH_MODES[0] } else { BenchMode::Soft(1024 * 1024) });
            hold = Some(glyphs.hold(
                &gpu.device,
                &gpu.queue,
                TITLE,
                &TextDef::default().font_size(34.0),
                scale,
            ));
            eprintln!(
                "[text_cache] hold {} 条（DPI {scale}）",
                hold.as_ref().map(|h| h.len()).unwrap_or(0)
            );
        }

        if win.key_down(KeyCode::Digit1) {
            apply_mode(&glyphs, BenchMode::Soft(256 * 1024));
            budget_label = "软 256 KiB";
            redraw_hud = true;
        }
        if win.key_down(KeyCode::Digit2) {
            apply_mode(&glyphs, BenchMode::Soft(1024 * 1024));
            budget_label = "软 1 MiB";
            redraw_hud = true;
        }
        if win.key_down(KeyCode::Digit3) {
            apply_mode(&glyphs, BenchMode::Off);
            budget_label = "无预算";
            redraw_hud = true;
        }
        if win.key_down(KeyCode::KeyH) {
            // 硬预算：故意小于工作集，演示"每帧重光栅化"的代价。
            apply_mode(&glyphs, BenchMode::Hard(256 * 1024));
            budget_label = "硬 256 KiB（会抖）";
            redraw_hud = true;
        }
        if win.key_down(KeyCode::KeyA) {
            let on = !glyphs.auto_compact();
            glyphs.set_auto_compact(on);
            eprintln!("[text_cache] auto_compact => {on}");
            redraw_hud = true;
        }
        if win.key_down(KeyCode::KeyT) {
            let n = glyphs.trim();
            eprintln!("[text_cache] trim 淘汰 {n} 条");
            redraw_hud = true;
        }
        if win.key_down(KeyCode::KeyC) {
            let n = glyphs.clear_unpinned();
            eprintln!("[text_cache] clear_unpinned 清掉 {n} 条");
            redraw_hud = true;
        }
        if win.key_down(KeyCode::KeyK) {
            let shrunk = glyphs.compact(&gpu.device, &gpu.queue);
            eprintln!("[text_cache] compact => {shrunk}");
            redraw_hud = true;
        }
        if win.key_down(KeyCode::KeyS) {
            match glyphs.save(&gpu.device, &gpu.queue, cache_file()) {
                Ok(n) => eprintln!("[text_cache] save {n} 条 -> {}", cache_file().display()),
                Err(e) => eprintln!("[text_cache] save 失败：{e}"),
            }
        }
        if win.key_down(KeyCode::KeyL) {
            glyphs.clear();
            match glyphs.load(&gpu.device, &gpu.queue, cache_file()) {
                Ok(n) => eprintln!("[text_cache] load {n} 条"),
                Err(e) => eprintln!("[text_cache] load 失败：{e}"),
            }
            redraw_hud = true;
        }
        if win.key_down(KeyCode::Space) {
            paused = !paused;
            eprintln!("[text_cache] {}", if paused { "暂停" } else { "继续" });
            redraw_hud = true;
        }

        if !paused {
            t0 = Instant::now();
            rolling = rolling.wrapping_add(1);
        }
        let t = if paused {
            0.0
        } else {
            t0.elapsed().as_secs_f32()
        };

        let mut batches: Vec<DrawBatch> = Vec::with_capacity(8);

        // ---- 自动基准分支：只画网格，按三档预算语义各测一段 ----
        if bench {
            if bench_frame == 0 && bench_stage < BENCH_MODES.len() {
                apply_mode(&glyphs, BENCH_MODES[bench_stage]);
                bench_rasters = glyphs.stats().rasterizations;
                bench_sum_ms = 0.0;
                bench_encode_ms = 0.0;
                bench_wait_ms = 0.0;
                bench_frames_done = 0;
            }
            let mut grid = DrawBatch::new();
            for (i, text) in bench_texts.iter().enumerate() {
                draw_text(
                    &mut grid.texts,
                    text,
                    Pos::new(8.0 + (i % 3) as f32 * 320.0, 8.0 + (i / 3) as f32 * 15.0),
                    TextDef::default().font_size(24.0),
                    TextOverride::from_color(WHITE),
                );
            }
            let t = Instant::now();
            let report = win.draw(Color::new(0.07, 0.07, 0.10, 1.0), &[&grid]);
            let frame_ms = t.elapsed().as_secs_f64() * 1000.0;
            // 拆分：`encode` = 记录 + prepare（含字形光栅化）+ 编码 + submit；
            // `acquire/present` = 等 swapchain / 等 vsync。
            let encode_ms = report.timings.encode_secs * 1000.0;
            let wait_ms = (report.timings.acquire_secs + report.timings.present_secs) * 1000.0;

            bench_frame += 1;
            if bench_frame > BENCH_WARMUP {
                bench_sum_ms += frame_ms;
                bench_encode_ms += encode_ms;
                bench_wait_ms += wait_ms;
                bench_frames_done += 1;
            }
            if bench_frames_done >= BENCH_FRAMES {
                let s = glyphs.stats();
                let rasters = s.rasterizations - bench_rasters;
                let n = bench_frames_done as f64;
                println!(
                    "[bench] {:<18} {:>6.2} ms/帧（{:>5.1} fps）= 编码 {:>5.2} + 等待 {:>5.2} | \
                     光栅化 {:.1} 次/帧 | 条目 {} 占用 {}KiB 淘汰 {}",
                    BENCH_LABELS[bench_stage],
                    bench_sum_ms / n,
                    1000.0 / (bench_sum_ms / n).max(0.001),
                    bench_encode_ms / n,
                    bench_wait_ms / n,
                    rasters as f64 / n,
                    s.entries(),
                    s.used_bytes() / 1024,
                    s.evictions,
                );
                bench_stage += 1;
                bench_frame = 0;
                if bench_stage >= BENCH_MODES.len() {
                    println!("[bench] 完毕");
                    return false;
                }
            }
            return true;
        }

        // 1. 钉住的标题（每个字都是恒等位置 → 与 hold 时的键一致）。
        let mut b = DrawBatch::new();
        draw_text(
            &mut b.texts,
            TITLE,
            Pos::new(24.0, 24.0),
            TextDef::default().font_size(34.0),
            TextOverride::from_color(WHITE),
        );
        batches.push(b);

        // 2. 中段：每帧换字号的滚动文本（制造 miss + 淘汰）。
        let roll = ROLLING[(rolling / 90) % ROLLING.len()];
        let mut b = DrawBatch::new();
        let size = 18.0 + 6.0 * (t * 1.7).sin().abs();
        draw_text(
            &mut b.texts,
            roll,
            Pos::new(24.0, 96.0),
            TextDef::default().font_size(size),
            TextOverride::from_color(Color::new(0.65, 0.85, 1.0, 1.0)),
        );
        batches.push(b);

        // 3. HUD：每秒重算一次字符串，数字用 `TextPart::glyphs` 拼（少造新键）。
        if last_log.elapsed() >= Duration::from_millis(250) || redraw_hud {
            let s = glyphs.stats();
            hud_line = format!(
                "预算 {} | 占用 {} KiB / 纹理 {} KiB | 条目 {} | 光栅化 {} | 淘汰 {} | 扩图 {} | 压缩 {} | 钉住 {}",
                budget_label,
                s.used_bytes() / 1024,
                s.texture_bytes() / 1024,
                s.entries(),
                s.rasterizations,
                s.evictions,
                s.grows,
                s.compacts,
                s.pinned_entries,
            );
            last_log = Instant::now();
            redraw_hud = false;
        }
        let mut hud = DrawBatch::new();
        // 帧率用小数字段拼（`TextPart::glyphs`），否则每帧一个整串会把缓存统计搅浑。
        draw_text_parts(
            &mut hud.texts,
            &[
                TextPart::normal("FPS "),
                TextPart::glyphs(format!("{:.0}", ctx.fps())),
                TextPart::normal("  "),
                TextPart::glyphs(format!("{:.2}", ctx.frame_time() * 1000.0)),
                TextPart::normal(" ms"),
            ],
            Pos::new(24.0, 126.0),
            TextDef::default().font_size(20.0),
            TextOverride::from_color(Color::new(0.95, 0.95, 0.8, 1.0)),
        );
        draw_text(
            &mut hud.texts,
            &hud_line,
            Pos::new(24.0, 150.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(Color::new(0.85, 0.85, 0.65, 1.0)),
        );
        draw_text(
            &mut hud.texts,
            "1/2/3 软预算   H 硬预算（会抖）   A 自动压缩开关   T trim   C 清未钉住   K 收缩   S/L 存取   空格 暂停",
            Pos::new(24.0, 178.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.55, 0.55, 0.6, 1.0)),
        );
        draw_text(
            &mut hud.texts,
            "软预算永不抖；硬预算 < 工作集才会让 rasterizations 每帧涨。暂停后应完全不动",
            Pos::new(24.0, 202.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.55, 0.7, 0.55, 1.0)),
        );
        batches.push(hud);

        let refs: Vec<&DrawBatch> = batches.iter().collect();
        win.draw(Color::new(0.07, 0.07, 0.10, 1.0), &refs);
        true
    })
    .await
    .unwrap();
}
