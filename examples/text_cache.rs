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
//! 2. 中：每帧换新字号的滚动文本（故意制造 miss，靠预算回收）；
//! 3. 下：说明 + 当前预算 / 占用 / 条目 / 命中数。
//!
//! 操作：
//! - `1` 预算 = 256 KiB
//! - `2` 预算 = 1 MiB
//! - `3` 预算 = None（不限制）
//! - `T` 手动按预算淘汰一次
//! - `C` 清掉未钉住的字形
//! - `K` 收缩图集（把显存还回去）
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
    "预算到了就按 LRU 淘汰未钉住的字形",
    "钉住的字形不参与淘汰，超预算只 warn 一次",
    "compact 把图集收缩回去，显存不再按峰值常驻",
    "save/load 跨进程复用已光栅化的图集",
];

fn cache_file() -> std::path::PathBuf {
    std::env::temp_dir().join("vireo_example_text_cache.vrtx")
}

#[vireo::main]
async fn main() {
    let idx = app.window(WindowDesc::new("Text Glyph Cache", 960, 640));

    let mut paused = false;
    let mut budget_label = "1 MiB";
    let mut rolling = 0usize;
    let mut last_log = Instant::now() - Duration::from_secs(1);
    let mut redraw_hud = true;
    let mut hud_line = String::new();
    let mut t0 = Instant::now();
    // 钉住的句柄必须活到进程结束；首帧拿到真实 DPI 后再创建（见下）。
    let mut hold: Option<GlyphHold> = None;

    eprintln!("[text_cache] K 收缩  S 保存  L 装载  空格 暂停");

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
            glyphs.set_budget(Some(1024 * 1024));
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
            glyphs.set_budget(Some(256 * 1024));
            budget_label = "256 KiB";
            redraw_hud = true;
        }
        if win.key_down(KeyCode::Digit2) {
            glyphs.set_budget(Some(1024 * 1024));
            budget_label = "1 MiB";
            redraw_hud = true;
        }
        if win.key_down(KeyCode::Digit3) {
            glyphs.set_budget(None);
            budget_label = "无限制";
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
                "预算 {} | 占用 {} KiB | 条目 {} | 光栅化 {} | 淘汰 {} | 扩图 {} | 钉住 {}",
                budget_label,
                s.used_bytes() / 1024,
                s.entries(),
                s.rasterizations,
                s.evictions,
                s.grows,
                s.pinned_entries,
            );
            last_log = Instant::now();
            redraw_hud = false;
        }
        let mut hud = DrawBatch::new();
        draw_text(
            &mut hud.texts,
            &hud_line,
            Pos::new(24.0, 150.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(Color::new(0.85, 0.85, 0.65, 1.0)),
        );
        draw_text(
            &mut hud.texts,
            "1/2/3 预算   T trim   C 清未钉住   K 收缩   S 保存   L 装载   空格 暂停",
            Pos::new(24.0, 178.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.55, 0.55, 0.6, 1.0)),
        );
        draw_text(
            &mut hud.texts,
            "暂停后 rasterizations 应完全不动（= 跨帧零重光栅化）",
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
