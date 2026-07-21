//! 程序化纹理：`Texture::from_rgba` / `from_bytes`（无需 logo 文件）

use vireo::prelude::*;

fn checkerboard(w: u32, h: u32, cell: u32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let on = ((x / cell) + (y / cell)) % 2 == 0;
            let i = ((y * w + x) * 4) as usize;
            if on {
                px[i] = 40;
                px[i + 1] = 120;
                px[i + 2] = 200;
                px[i + 3] = 255;
            } else {
                px[i] = 220;
                px[i + 1] = 220;
                px[i + 2] = 230;
                px[i + 3] = 255;
            }
        }
    }
    px
}

fn gradient(w: u32, h: u32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = (x * 255 / w) as u8;
            px[i + 1] = (y * 255 / h) as u8;
            px[i + 2] = 180;
            px[i + 3] = 255;
        }
    }
    px
}

fn main() {
    let mut app = App::new();

    let check = Texture::from_rgba(64, 64, &checkerboard(64, 64, 8), &app.gpu);
    let grad = Texture::from_rgba(128, 64, &gradient(128, 64), &app.gpu);

    // from_bytes：嵌入 PNG（若仓库有 logo.png）
    let embedded = match std::fs::read("logo.png") {
        Ok(bytes) => Texture::from_bytes(&bytes, &app.gpu).ok(),
        Err(_) => None,
    };

    let idx = app.window(WindowDesc::new("Texture RGBA", 720, 420).high_dpi(true), None::<fn()>);

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mut batch = DrawBatch::new();

        draw_text(
            &mut batch.texts,
            "from_rgba / from_bytes (procedural + optional logo.png)",
            TextOptions::default()
                .x(20.0)
                .y(16.0)
                .font_size(15.0)
                .color(Color::new(0.7, 0.75, 0.85, 1.0)),
        );

        batch.set_texture(Some(&check));
        draw_rectangle(&mut batch, 40.0, 60.0, 160.0, 160.0, Some(WHITE));
        draw_text(
            &mut batch.texts,
            "from_rgba checker",
            TextOptions::default()
                .x(40.0)
                .y(230.0)
                .font_size(13.0)
                .color(Color::new(0.55, 0.55, 0.65, 1.0)),
        );

        batch.set_texture(Some(&grad));
        draw_rectangle(&mut batch, 240.0, 60.0, 200.0, 100.0, Some(WHITE));
        draw_text(
            &mut batch.texts,
            "from_rgba gradient",
            TextOptions::default()
                .x(240.0)
                .y(170.0)
                .font_size(13.0)
                .color(Color::new(0.55, 0.55, 0.65, 1.0)),
        );

        if let Some(ref tex) = embedded {
            batch.set_texture(Some(tex));
            let s = 0.12;
            draw_rectangle(&mut batch, 480.0, 60.0, tex.width as f32 * s, tex.height as f32 * s, Some(WHITE));
            draw_text(
                &mut batch.texts,
                "from_bytes logo.png",
                TextOptions::default()
                    .x(480.0)
                    .y(200.0)
                    .font_size(13.0)
                    .color(Color::new(0.55, 0.55, 0.65, 1.0)),
            );
        } else {
            draw_text(
                &mut batch.texts,
                "(no logo.png for from_bytes)",
                TextOptions::default()
                    .x(480.0)
                    .y(100.0)
                    .font_size(13.0)
                    .color(ORANGE),
            );
        }

        win.draw(Some(Color::new(0.07, 0.07, 0.1, 1.0)), &[&batch]);
        true
    });
}
