//! 同一张纹理两种采样：左 Repeat 平铺，右 ClampToEdge 钳制。
//!
//! - `Texture::set_address_mode`：原地切换（本例 checker 出厂即 Repeat，演示单 mode 用法）。
//! - `Texture::with_address_mode`：共享 image 的新对象，同帧混用（右 quad 用它回到 Clamp）。
//! - 注意：Repeat＋拼 atlas 会 wrap 渗色（采到邻区域/对边），要 gutter 或独占整图。

use vireo::prelude::*;

fn checker_rgba(w: u32, h: u32, c0: [u8; 3], c1: [u8; 3], cell: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let on = ((x / cell) + (y / cell)).is_multiple_of(2);
            let c = if on { c0 } else { c1 };
            v.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    v
}

#[vireo::main]
async fn main() {
    let idx = app.window(
        WindowDesc::new("Texture Repeat vs Clamp", 900, 420),
        None::<fn()>,
    );

    // 同一张 64×64 棋盘：出厂 Clamp；repeat 版共享 image（零重传）。
    let mut tex = Texture::from_rgba(
        64,
        64,
        &checker_rgba(64, 64, [255, 80, 40], [40, 200, 120], 8),
        &app.gpu,
    );
    tex.set_address_mode(&app.gpu, AddressMode::Repeat);
    let clamped = tex.with_address_mode(&app.gpu, AddressMode::ClampToEdge);

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let _keep_alive = (&tex, &clamped);

        // 左：Repeat，uv 出界平铺 3×2。
        let mut left = DrawBatch::new();
        left.set_texture(Some(&tex));
        left.set_uv(0.0, 0.0, 3.0, 2.0);
        draw_rectangle(&mut left, Pos::new(40.0, 80.0), 380.0, 260.0, Some(WHITE));
        // 右：Clamp，出界钳边（同一张图，同一帧）。
        let mut right = DrawBatch::new();
        right.set_texture(Some(&clamped));
        right.set_uv(0.0, 0.0, 3.0, 2.0);
        draw_rectangle(&mut right, Pos::new(480.0, 80.0), 380.0, 260.0, Some(WHITE));

        let mut title = DrawBatch::new();
        draw_text(
            &mut title.texts,
            "left: Repeat 3x2 tiling (one image) / right: ClampToEdge (same image)",
            Pos::new(16.0, 16.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(Color::new(0.06, 0.08, 0.12, 1.0), &[&left, &right, &title]);
        true
    })
    .await
    .unwrap();
}
