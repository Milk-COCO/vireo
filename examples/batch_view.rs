//! `DrawBatch.view`：整批统一视图变换（属性，非画笔状态）
//!
//! - 根 batch 设 `view` 平移/缩放，整批（几何 + 文字 + 子树）统一左乘一次。
//! - 子树自动继承父 view（子 batch 无需任何设置）。
//! - `view` 与画笔 `transform` 正交：本示例缩放 root 时子 batch 的局部变换照常生效。
//!
//! 操作：
//! - 左键拖动：平移场景
//! - 滚轮：以鼠标为中心缩放
//! - `R` 键：重置
//!
//! 对比：右侧深色板是无 view 的参照 batch，左侧是同一个 `root` 内容只挂 view。
//!
//! ```bash
//! cargo run --example batch_view
//! ```

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("DrawBatch.view — pan/zoom", 960, 540).high_dpi(true),
        None::<fn()>,
    );

    // 平移与缩放状态（逻辑坐标）
    let mut pan = Pos::new(240.0, 270.0);
    let mut zoom = 1.0f32;
    let mut mouse = Pos::new(480.0, 270.0);
    let mut mouse_was_down = false;

    let mut t: f32 = 0.0;
    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        t += 0.03;

        // 输入：左键拖动平移，滚轮缩放
        let (mox, moy) = win.mouse_pos();
        let mouse_left = win.mouse_left();
        if mouse_left {
            if !mouse_was_down {
                mouse = Pos::new(mox, moy);
            } else {
                pan.x += mox - mouse.x;
                pan.y += moy - mouse.y;
            }
            mouse = Pos::new(mox, moy);
        }
        mouse_was_down = mouse_left;

        let (_sx, sy) = win.take_scroll();
        if sy != 0.0 {
            let factor = if sy > 0.0 { 1.1 } else { 1.0 / 1.1 };
            // 以鼠标为锚点缩放：先按 factor 缩放，再平移补正锚点
            let s = factor;
            zoom *= factor;
            pan.x = mox - (mox - pan.x) * s;
            pan.y = moy - (moy - pan.y) * s;
        }
        if win.key_down(KeyCode::KeyR) {
            pan = Pos::new(240.0, 270.0);
            zoom = 1.0;
        }

        // view = 平移 * 缩放（左乘顺序：先缩放后平移）
        let view = Transform::matrix(zoom, 0.0, 0.0, zoom, pan.x, pan.y);

        // ---- root：挂 view 的整批场景（几何 + 文字 + 子树）----
        let mut root = DrawBatch::new();
        root.view = view;

        // 网格背景（局部坐标，中心在 root 原点，靠 view 平移/缩放）
        root.set_color(Color::new(0.25, 0.3, 0.4, 0.35));
        for i in -20..=20 {
            let x = i as f32 * 40.0;
            draw_line(&mut root, x, -400.0, x, 400.0, 1.0, None);
            let y = i as f32 * 40.0;
            draw_line(&mut root, -600.0, y, 600.0, y, 1.0, None);
        }

        // 一组旋转方块（子 batch，自带画笔 transform，随 view 一起变）
        let mut spins = DrawBatch::new();
        for i in 0..4 {
            let hue = (i as f32 / 4.0 + t * 0.05) % 1.0;
            let fill = Color::new(0.35 + hue * 0.55, 0.4, 0.6, 0.95);
            spins.set_color(fill);
            let a = i as f32 * 90.0 + t * 20.0;
            spins.set_deg(a);
            spins.set_position((i as f32 - 1.5) * 140.0, 0.0);
            draw_rounded_rect(&mut spins, Pos::new(-45.0, -45.0), 90.0, 90.0, 12.0, None);
        }
        root.push_child(spins);

        // 文字（直接挂 root，随 view 移动/缩放）
        draw_text(
            &mut root.texts,
            "view 作用于几何与文字",
            Pos::new(-260.0, 240.0),
            TextDef::default().font_size(18.0),
            TextOverride::from_color(Color::new(0.9, 0.95, 1.0, 1.0)),
        );
        let info = format!("pan=({:.0},{:.0}) zoom={:.2}", pan.x, pan.y, zoom);
        draw_text(
            &mut root.texts,
            &info,
            Pos::new(-260.0, 265.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.7, 0.8, 0.9, 1.0)),
        );

        // ---- 参照：同一位置静态内容（不挂 view，证明 view 只作用于挂载 batch）----
        let mut ref_batch = DrawBatch::new();
        ref_batch.set_color(Color::new(0.5, 0.55, 0.65, 0.4));
        draw_rounded_rect(
            &mut ref_batch,
            Pos::new(680.0, 40.0),
            240.0,
            160.0,
            12.0,
            None,
        );
        draw_text(
            &mut ref_batch.texts,
            "参照（无 view）",
            Pos::new(700.0, 70.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(Color::new(0.8, 0.85, 0.95, 1.0)),
        );

        // HUD
        let mut ui = DrawBatch::new();
        draw_text(
            &mut ui.texts,
            "左键拖动平移 · 滚轮缩放 · R 重置",
            Pos::new(16.0, 12.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.9, 0.95, 1.0, 1.0)),
        );

        win.draw(Color::new(0.06, 0.07, 0.1, 1.0), &[&ui, &root, &ref_batch]);
        true
    });
}
