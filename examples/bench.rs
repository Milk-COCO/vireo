//! 性能基准：多维度压测 SDF/几何/文字/变换/多边形。
//! 按 1-7 数字键切换场景，左上角显示实时帧率与帧时间。

use vireo::prelude::*;

const SCENES: &[(&str, fn(&mut DrawBatch))] = &[
    ("1: SDF feather shapes x2000", scene_sdf_shapes),
    ("2: Geometry shapes x500", scene_geo_shapes),
    ("3: Mixed SDF+Geo x1000", scene_mixed),
    ("4: Text dynamic x200", scene_text_dynamic),
    ("5: Unique transforms x1000", scene_transforms),
    ("6: Polygons x200", scene_polygons),
    ("7: Full load (dyn text)", scene_full),
    ("8: Text static x200", scene_text_static),
];

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Vireo Performance Benchmark", 900, 700),
        None::<fn()>,
    );

    let mut scene: usize = 0;
    let mut frame_times: Vec<f64> = Vec::new();
    let mut min_frame = f64::MAX;
    let mut max_frame = 0.0f64;
    let mut preserve_order = true;

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mut batch = DrawBatch::new();

        // ---- 场景切换 ----
        let next = if win.key_down(KeyCode::Digit1) { Some(0) }
        else if win.key_down(KeyCode::Digit2) { Some(1) }
        else if win.key_down(KeyCode::Digit3) { Some(2) }
        else if win.key_down(KeyCode::Digit4) { Some(3) }
        else if win.key_down(KeyCode::Digit5) { Some(4) }
        else if win.key_down(KeyCode::Digit6) { Some(5) }
        else if win.key_down(KeyCode::Digit7) { Some(6) }
        else if win.key_down(KeyCode::Digit8) { Some(7) }
        else { None };
        if win.key_down(KeyCode::KeyP) {
            preserve_order = !preserve_order;
        }

        if let Some(s) = next {
            scene = s;
            frame_times.clear();
            min_frame = f64::MAX;
            max_frame = 0.0;
        }

        // ---- 绘制当前场景 ----
        let (name, func) = SCENES[scene];
        func(&mut batch);
        batch.preserve_order = preserve_order;

        // ---- 帧时间统计 ----
        let ft_ms = app.frame_time * 1000.0;
        frame_times.push(ft_ms);
        if frame_times.len() > 300 { frame_times.remove(0); }
        min_frame = min_frame.min(ft_ms);
        max_frame = max_frame.max(ft_ms);
        let avg = if frame_times.is_empty() { 0.0 } else {
            frame_times.iter().sum::<f64>() / frame_times.len() as f64
        };

        // ---- UI 叠加层 ----
        let stats = batch.shape_stats();
        let overlay = format!(
            "{}\n\
             ─────────────────\n\
             FPS:         {:>8.1}\n\
             Frame:       {:>8.3} ms\n\
             Avg (300):   {:>8.3} ms\n\
             Min:         {:>8.3} ms\n\
             Max:         {:>8.3} ms\n\
             Total frames:{:>8}\n\
             Mesh V:      {:>8}\n\
             SDF Inst:    {:>8}\n\
             Geo Inst:    {:>8}\n\
             Geo Tmpl:    {:>8}\n\
             Geo Tmpl V:  {:>8}\n\
             Draw calls:  {:>8}\n\
             Order:       {:>8}",
            name,
            app.fps,
            ft_ms,
            avg,
            min_frame,
            max_frame,
            app.frame_count,
            stats.mesh_vertices,
            stats.sdf_instances,
            stats.geo_instances,
            stats.geo_templates,
            stats.geo_template_vertices,
            win.last_draw_calls(),
            if preserve_order { "preserve" } else { "reorder" },
        );
        draw_text(
            &mut batch.texts,
            &overlay,
            Pos::new(12.0, 8.0),
            TextDef::default().font_size(16.0), TextOverride::from_color(WHITE),
        );

        // 场景切换提示
        draw_text(
            &mut batch.texts,
            "Press 1-8 switch scene | P toggle preserve_order",
            Pos::new(12.0, 670.0),
            TextDef::default().font_size(12.0), TextOverride::from_color(Color::new(0.4, 0.4, 0.5, 1.0)),
        );

        win.draw(Some(Color::new(0.08, 0.08, 0.12, 1.0)), &[&batch]);
        true
    });
}

// ─── 场景定义 ─────────────────────────────────────────────

const COLORS: &[Color] = &[RED, GREEN, BLUE, YELLOW, SKYBLUE, MAGENTA, WHITE, ORANGE, PINK, GOLD];

/// SDF 柔边形状 ×2000（纯 SDF 最轻量路径）
fn scene_sdf_shapes(b: &mut DrawBatch) {
    for i in 0..2000 {
        let x = (i % 50) as f32 * 17.0 + 10.0;
        let y = (i / 50) as f32 * 17.0 + 10.0;
        let c = COLORS[i % COLORS.len()];
        b.set_position(x, y);
        b.set_sdf_feather(Some(1.0));
        match i % 5 {
            0 => draw_rectangle(b, Pos::new(0.0, 0.0), 14.0, 14.0, Some(c)),
            1 => draw_circle(b, Pos::new(7.0, 7.0), 6.0, Some(c)),
            2 => draw_rounded_rect(b, Pos::new(0.0, 0.0), 14.0, 14.0, 3.0, Some(c)),
            3 => draw_ellipse(b, Pos::new(7.0, 7.0), 6.0, 4.0, Some(c)),
            4 => draw_triangle(b, 0.0, 0.0, 14.0, 0.0, 7.0, 14.0, Some(c)),
            _ => {}
        }
    }
}

/// 几何路径形状 ×500（顶点膨胀，无 SDF）
fn scene_geo_shapes(b: &mut DrawBatch) {
    b.clear_sdf_feather(); // 强制几何模式
    for i in 0..500 {
        let x = (i % 25) as f32 * 35.0 + 15.0;
        let y = (i / 25) as f32 * 35.0 + 15.0;
        let c = COLORS[i % COLORS.len()];
        b.set_position(x, y);
        match i % 4 {
            0 => draw_circle(b, Pos::new(14.0, 14.0), 12.0, Some(c)),
            1 => draw_ellipse(b, Pos::new(14.0, 14.0), 12.0, 8.0, Some(c)),
            2 => draw_rounded_rect(b, Pos::new(0.0, 0.0), 28.0, 28.0, 6.0, Some(c)),
            3 => {
                let pts = [(0.0, 0.0), (28.0, 4.0), (24.0, 28.0), (4.0, 22.0)];
                draw_polygon(b, &pts, Some(c));
            }
            _ => {}
        }
    }
}

/// SDF + 几何混合 ×1000
fn scene_mixed(b: &mut DrawBatch) {
    for i in 0..1000 {
        let x = (i % 40) as f32 * 22.0 + 10.0;
        let y = (i / 40) as f32 * 22.0 + 10.0;
        let c = COLORS[i % COLORS.len()];
        b.set_position(x, y);
        if i % 2 == 0 {
            b.set_sdf_feather(Some(0.8));
            draw_rounded_rect(b, Pos::new(0.0, 0.0), 18.0, 18.0, 4.0, Some(c));
        } else {
            b.clear_sdf_feather();
            draw_circle(b, Pos::new(9.0, 9.0), 8.0, Some(c));
        }
    }
}

/// 文本 ×200：每帧 format 新字符串（shape 缓存几乎全 miss）
fn scene_text_dynamic(b: &mut DrawBatch) {
    for i in 0..200 {
        let x = (i % 20) as f32 * 44.0 + 5.0;
        let y = (i / 20) as f32 * 28.0 + 5.0;
        let msg = match i % 6 {
            0 => format!("ABC {i}"),
            1 => format!("你好 {i}"),
            2 => "Test 测试".to_string(),
            3 => format!("Vireo {i}"),
            4 => "wgpu 🎨".to_string(),
            _ => format!("SDF #{i}"),
        };
        let sz = 12.0 + (i % 5) as f32 * 2.0;
        draw_text(
            &mut b.texts,
            &msg,
            Pos::new(x, y),
            TextDef::default().font_size(sz), TextOverride::from_color(WHITE),
        );
    }
}

/// 文本 ×200：固定字符串（测 shape 缓存命中）
fn scene_text_static(b: &mut DrawBatch) {
    const LABELS: &[&str] = &["ABC", "你好", "Test 测试", "Vireo", "wgpu 🎨", "SDF #"];
    for i in 0..200 {
        let x = (i % 20) as f32 * 44.0 + 5.0;
        let y = (i / 20) as f32 * 28.0 + 5.0;
        let msg = LABELS[i % LABELS.len()];
        let sz = 12.0 + (i % 5) as f32 * 2.0;
        draw_text(
            &mut b.texts,
            msg,
            Pos::new(x, y),
            TextDef::default().font_size(sz), TextOverride::from_color(WHITE),
        );
    }
}

/// 每形状独立变换 ×1000（压力测试 transform_map 去重）
fn scene_transforms(b: &mut DrawBatch) {
    b.set_sdf_feather(Some(0.5));
    for i in 0..1000 {
        let x = (i % 40) as f32 * 22.0 + 15.0;
        let y = (i / 40) as f32 * 22.0 + 15.0;
        let c = COLORS[i % COLORS.len()];
        b.set_position(x, y);
        b.set_deg((i as f32) * 7.0);
        b.set_scale(1.0 + (i % 3) as f32 * 0.3, 1.0 + (i % 2) as f32 * 0.2);
        draw_rounded_rect(b, Pos::new(-6.0, -6.0), 6.0, 6.0, 2.0, Some(c));
        b.clear_transform();
    }
}

/// SDF 多边形 ×200
fn scene_polygons(b: &mut DrawBatch) {
    b.set_sdf_feather(Some(1.0));
    for i in 0..200 {
        let x = (i % 20) as f32 * 44.0 + 30.0;
        let y = (i / 20) as f32 * 50.0 + 30.0;
        b.set_position(x, y);
        let sides = 5 + (i % 7); // 5~11 边形
        let r = 16.0;
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(sides);
        for j in 0..sides {
            let angle = std::f32::consts::TAU * j as f32 / sides as f32 - std::f32::consts::FRAC_PI_2;
            pts.push((r * angle.cos(), r * angle.sin()));
        }
        let t = i as f32 / 200.0;
        let color = Color::new(0.3 + t * 0.7, 0.2 + (1.0 - t) * 0.6, 0.5 + (t * 2.0 % 0.5), 1.0);
        draw_polygon(b, &pts, Some(color));
    }
}

/// 综合全负载（含动态文字）
fn scene_full(b: &mut DrawBatch) {
    scene_sdf_shapes(b);
    scene_transforms(b);
    scene_text_dynamic(b);
}
