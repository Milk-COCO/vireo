//! 自定义字体：`load_font_file` / `load_font` + `Family::Name`
//!
//! 默认尝试加载系统 Consolas / Segoe UI；失败时回退到内置族名对比。

use vireo::prelude::*;

fn try_load(app: &App, path: &str) -> bool {
    match app.gpu.load_font_file(path) {
        Ok(()) => {
            eprintln!("loaded font: {path}");
            true
        }
        Err(e) => {
            eprintln!("skip {path}: {e}");
            false
        }
    }
}

fn main() {
    let mut app = App::new();

    // load_font_file：系统 TTF
    let consolas = try_load(&app, r"C:\Windows\Fonts\consola.ttf");
    let segoe = try_load(&app, r"C:\Windows\Fonts\segoeui.ttf");

    // load_font：同一文件再走字节路径（演示 API；族名已在 db 中）
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\arial.ttf") {
        app.gpu.load_font(&bytes);
        eprintln!("loaded font bytes: arial.ttf ({} bytes)", bytes.len());
    }

    let idx = app.window(
        WindowDesc::new("Text Font — load_font", 720, 360).high_dpi(true),
        None::<fn()>,
    );

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mut batch = DrawBatch::new();

        draw_text(
            &mut batch.texts,
            "Custom fonts (load_font / load_font_file)",
            Pos::new(24.0, 24.0), TextDef::default().font_size(22.0).with_weight(Weight::BOLD),
            TextOverride::from_color(GOLD),
        );

        let mut y = 80.0;
        draw_text(
            &mut batch.texts,
            "System SansSerif (default)",
            Pos::new(24.0, y), TextDef::default().font_size(20.0).with_family(Family::SansSerif),
            TextOverride::from_color(WHITE),
        );
        y += 40.0;

        if consolas {
            draw_text(
                &mut batch.texts,
                "Consolas via Family::Name — 0123456789",
                Pos::new(24.0, y), TextDef::default().font_size(20.0).with_family(Family::Name("Consolas")),
                TextOverride::from_color(Color::new(0.4, 0.9, 1.0, 1.0)),
            );
            y += 40.0;
        }

        if segoe {
            draw_text(
                &mut batch.texts,
                "Segoe UI via Family::Name — Hello 中文",
                Pos::new(24.0, y), TextDef::default().font_size(20.0).with_family(Family::Name("Segoe UI")),
                TextOverride::from_color(Color::new(1.0, 0.7, 0.4, 1.0)),
            );
            y += 40.0;
        }

        draw_text(
            &mut batch.texts,
            "Arial via Family::Name (if loaded)",
            Pos::new(24.0, y), TextDef::default().font_size(20.0).with_family(Family::Name("Arial")),
            TextOverride::from_color(Color::new(0.7, 0.85, 0.5, 1.0)),
        );
        y += 48.0;

        draw_text(
            &mut batch.texts,
            "Place/override with any TTF/OTF path in load_font_file.",
            Pos::new(24.0, y), TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.5, 0.55, 0.65, 1.0)),
        );

        win.draw(Color::new(0.06, 0.07, 0.1, 1.0), &[&batch]);
        true
    });
}
