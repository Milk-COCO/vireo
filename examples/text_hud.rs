//! HUD 分段：Normal / Dynamic / Glyphs + 自动切分。
//!
//! **核心**：固定文案与会变内容分开；Glyphs 是任意字符的按字 direct path。
//!
//! | API | 作用 |
//! |-----|------|
//! | `HudLine` | 跨帧 `Vec<TextPart>`，只改数字/动态槽 |
//! | `draw_text_parts` | 一帧内手写 Normal/Dynamic/Glyphs（`&[TextPart]`） |
//! | `draw_text_hud` / `draw_text_hud!` | `format!` 后 `split_hud` 自动切 |
//!
//! 键：`Space` 切换「自动切分」对照行开/关。
//!
//! ```bash
//! cargo run --example text_hud
//! ```

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("text/hud — Normal · Dynamic · Glyphs", 720, 360), None::<fn()>);

    // 跨帧 HUD 行（Bevy span 思路）：标签 Normal，分数 Glyphs
    let mut score_line = HudLine::new().text("分数: ").glyphs("0");
    let mode_line = HudLine::new().text("模式: ").dynamic("Parts");

    let mut score: u32 = 0;
    let mut tick: u32 = 0;
    let mut show_auto = true;
    let mut score_s = String::new();
    let mut fps_s = String::new();

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        if win.key_down(KeyCode::Space) {
            show_auto = !show_auto;
        }

        tick = tick.wrapping_add(1);
        if tick % 8 == 0 {
            score = score.wrapping_add(1);
        }
        score_s.clear();
        score_s.push_str(&score.to_string());
        score_line.write_slot(1, &score_s);

        fps_s.clear();
        // 一位小数：拆成 Glyphs 友好串（可选；也可用 format 后 draw_text_hud!）
        let fps = app.fps;
        let w = fps.floor() as i64;
        let f = ((fps - w as f64) * 10.0).round() as i64;
        let (w, f) = if f >= 10 { (w + 1, 0) } else { (w, f) };
        fps_s.push_str(&w.to_string());
        fps_s.push('.');
        fps_s.push_str(&f.to_string());

        let mut batch = DrawBatch::new();
        let bg = Color::new(0.06, 0.06, 0.09, 1.0);
        let white = WHITE;
        let dim = Color::new(0.55, 0.55, 0.65, 1.0);
        let ov = |c: Color| TextOverride::from_color(c);
        let def = |sz: f32| TextDef::default().font_size(sz);

        draw_text(
            &mut batch.texts,
            "HudLine / parts / auto-split (Space = toggle auto)",
            Pos::new(16.0, 12.0),
            def(13.0),
            ov(dim),
        );

        // 1) HudLine：只改 Glyphs 槽
        score_line.draw(&mut batch.texts, Pos::new(16.0, 48.0), def(22.0), ov(white));
        mode_line.draw(&mut batch.texts, Pos::new(16.0, 84.0), def(16.0), ov(Color::new(0.75, 0.85, 1.0, 1.0)));

        // 2) 手写 parts：Normal + Glyphs（FPS）
        draw_text_parts(
            &mut batch.texts,
            &[
                TextPart::normal("FPS "),
                TextPart::glyphs(fps_s.clone()),
            ],
            Pos::new(16.0, 120.0),
            def(16.0),
            ov(Color::new(0.9, 0.9, 0.5, 1.0)),
        );

        // 3) 自动切分对照
        if show_auto {
            draw_text(
                &mut batch.texts,
                "auto (draw_text_hud!):",
                Pos::new(16.0, 168.0),
                def(14.0),
                ov(dim),
            );
            draw_text_hud!(
                &mut batch.texts,
                Pos::new(16.0, 192.0),
                def(18.0),
                ov(Color::new(0.5, 1.0, 0.7, 1.0));
                "击杀 {}  金币 {}",
                score,
                score * 3,
            );
            draw_text(
                &mut batch.texts,
                "(split_hud → Normal 标签 + Glyphs 数值段)",
                Pos::new(16.0, 224.0),
                def(12.0),
                ov(dim),
            );
        } else {
            draw_text(
                &mut batch.texts,
                "auto line hidden (Space to show)",
                Pos::new(16.0, 168.0),
                def(14.0),
                ov(dim),
            );
        }

        // 反例说明
        draw_text(
            &mut batch.texts,
            "avoid: draw_text(&format!(\"分数: {n}\")) every frame → shape miss",
            Pos::new(16.0, 280.0),
            def(12.0),
            ov(Color::new(0.7, 0.4, 0.4, 1.0)),
        );

        let st = app.gpu.shape_cache_stats();
        let tot = st.hits + st.misses;
        let hit = if tot > 0 {
            100.0 * st.hits as f64 / tot as f64
        } else {
            0.0
        };
        draw_text_parts(
            &mut batch.texts,
            &[
                TextPart::normal("shape hit~"),
                TextPart::glyphs(format!("{hit:.0}")),
                TextPart::normal("%  entries "),
                TextPart::glyphs(app.gpu.shape_cache_len().to_string()),
            ],
            Pos::new(16.0, 312.0),
            def(13.0),
            ov(dim),
        );

        win.draw(bg, &[&batch]);
        true
    });
}
