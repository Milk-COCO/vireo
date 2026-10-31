//! 官方 quickstart：**README 里贴的就是这个文件，逐字不改**。
//!
//! 跑法：`cargo run --example ez`
//!
//! 一个窗口、一帧画完就返回 `true` 继续下一帧。等宽等高的 `DrawBatch` 每帧重建
//! （即时模式），`win.draw(清屏色, &[批次...])` 一次提交。
//!
//! 想继续往下看：
//! - 文字与 HUD 分段：`examples/text_hud.rs`
//! - 自定义 WGSL 材质：`examples/custom_material.rs`
//! - 形状与描边全览：`examples/shapes.rs`

use vireo::prelude::*;

#[vireo::main]
async fn main(app: App) {
    let idx = app.window(WindowDesc::new("EZ Vireo", 800, 600));

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return true, // 窗口已经关了 → 结束循环
        };

        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::new(100.0, 100.0), 200.0, 150.0, Some(RED));
        draw_circle(&mut batch, Pos::new(500.0, 300.0), 80.0, Some(BLUE));
        draw_line(&mut batch, 50.0, 50.0, 750.0, 550.0, 2.0, Some(GREEN));
        draw_text(
            &mut batch.texts,
            "你好，Vireo",
            Pos::new(100.0, 40.0),
            TextDef::default().font_size(32.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(BLACK, &[&batch]);
        true
    })
    .await
    .unwrap();
}
