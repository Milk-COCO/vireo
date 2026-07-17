//! 帧统计示例：展示 FPS 和帧耗时

use vireo::prelude::*;

fn main() {
    let mut app = App::new();
    let idx = app.window(WindowDesc::new("Vireo Frame Stats", 600, 400), None::<fn()>);

    // FPS 历史（render 为简单的柱状图）
    let mut history: Vec<f64> = Vec::new();

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mut batch = DrawBatch::new();

        // 更新 FPS 历史（最近 60 帧）
        history.push(app.fps);
        if history.len() > 60 {
            history.remove(0);
        }

        // FPS 柱状图
        let bar_width = 8.0f32;
        let bar_gap = 2.0f32;
        let max_fps = 120.0f64;
        let graph_y = 150.0f32;
        let graph_h = 100.0f32;

        for (i, &fps) in history.iter().enumerate() {
            let h = (fps / max_fps * graph_h as f64).min(graph_h as f64) as f32;
            let x = 20.0 + i as f32 * (bar_width + bar_gap);
            let y = graph_y + graph_h - h;
            let color = if fps >= 60.0 {
                GREEN
            } else if fps >= 30.0 {
                Color::new(1.0, 0.8, 0.0, 1.0)
            } else {
                RED
            };
            draw_rectangle(&mut batch, x, y, bar_width, h, color);
        }

        // 统计文本
        let info = format!(
            "FPS: {:.1}\nFrame time: {:.3}ms\nTotal frames: {}",
            app.fps,
            app.frame_time * 1000.0,
            app.frame_count,
        );
        draw_text(
            &mut batch.texts,
            &info,
            TextOptions::default().x(20.0).y(20.0).font_size(22.0).color(WHITE),
        );

        draw_text(
            &mut batch.texts,
            "FPS history (last 60 frames)",
            TextOptions::default().x(20.0).y(130.0).font_size(12.0).color(Color::new(0.5, 0.5, 0.6, 1.0)),
        );

        win.draw(Some(Color::new(0.05, 0.05, 0.08, 1.0)), &[&batch]);
        true
    });
}
