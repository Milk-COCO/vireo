//! 文件读写三件套：随包资源（只读）+ 可写目录（存档/缓存/用户可见）。
//!
//! 这套 API 存在的理由是**安卓的文件系统有三个互不相通的世界**（详见
//! `vireo::asset` / `vireo::storage` 模块文档）。桌面和安卓用**同一份代码**：
//!
//! ```text
//! 桌面：文件放 ./examples/assets/          ← set_asset_root("assets")
//! 安卓：文件放 src/main/assets/            ← 构建期打进 APK，无需配置
//! ```
//!
//! 刻意**不做**「相对路径自动回退到 assets」：那会让同一份代码在两个平台上资源
//! 来源不同且不报错，桌面调通了安卓静默换一张图。两条路各自显式。
//!
//! 运行：`cargo run --example resource_io`

use vireo::prelude::*;

const BG: Color = Color::new(0.06, 0.07, 0.09, 1.0);
const FG: Color = Color::new(0.86, 0.88, 0.91, 1.0);
const DIM: Color = Color::new(0.45, 0.75, 1.0, 1.0);

#[vireo::main]
async fn main() {
    // 桌面的资产根；安卓这行是 no-op（资源由 src/main/assets/ 决定）
    app.set_asset_root("assets");
    // 三个可写目录的末级名字。默认取可执行文件名，这里显式设一个便于观察。
    app.set_storage_name("vireo-resource-io-demo");

    let mut lines: Vec<String> = Vec::new();

    // ---- 读：随包资源（只读）----------------------------------------------
    {
        let assets = app.assets();
        lines.push(format!("[assets] root = {:?}", assets.root()));
        lines.push(format!(
            "[assets] exists(\"notes/hello.txt\") = {}",
            assets.exists("notes/hello.txt")
        ));
        match assets.read("notes/hello.txt") {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let first = text.lines().next().unwrap_or("").to_string();
                lines.push(format!(
                    "[assets] read -> {} B，首行「{first}」",
                    bytes.len()
                ));
            }
            Err(e) => lines.push(format!("[assets] read 失败: {e}")),
        }
        match assets.list("") {
            Ok(mut names) => {
                names.sort();
                lines.push(format!("[assets] list(\"\") -> {names:?}"));
            }
            Err(e) => lines.push(format!("[assets] list 失败: {e}")),
        }
        // 刻意演示两种拒绝：目录穿越 + 不存在
        lines.push(format!(
            "[assets] read(\"../secret\") 被拒 = {}",
            assets.read("../secret").is_err()
        ));
        lines.push(format!(
            "[assets] exists(\"nope.png\") = {}",
            assets.exists("nope.png")
        ));
    }

    // 随包贴图。桌面和安卓同一行 —— 差别只在文件放哪。
    let tex = app.load_texture_asset("logo.png");

    // ---- 写：三个可写目录 --------------------------------------------------
    lines.push(format!("[dir] storage_name = {:?}", app.storage_name()));
    for (label, dir) in [("data", app.data_dir()), ("cache", app.cache_dir())] {
        match dir {
            Ok(d) => match vireo::storage::ensure_dir(&d) {
                Ok(()) => lines.push(format!("[dir:{label}] {}", d.display())),
                Err(e) => lines.push(format!("[dir:{label}] 建目录失败: {e}")),
            },
            Err(e) => lines.push(format!("[dir:{label}] 取路径失败: {e}")),
        }
    }
    match app.external_data_dir() {
        Some(d) => lines.push(format!("[dir:external] {}", d.display())),
        None => lines.push("[dir:external] 不可用（平台/环境不允许）".to_string()),
    }

    // 真写一次再读回来，证明往返可用。
    let roundtrip: Result<String, String> = (|| {
        let dir = app.data_dir().map_err(|e| e.to_string())?;
        let dir = dir.join("io-demo");
        vireo::storage::ensure_dir(&dir).map_err(|e| e.to_string())?;
        let p = dir.join("save.json");
        std::fs::write(&p, br#"{"score":12345,"stage":7}"#).map_err(|e| e.to_string())?;
        std::fs::read_to_string(&p).map_err(|e| e.to_string())
    })();
    match roundtrip {
        Ok(s) => lines.push(format!("[save] 写入并读回: {s}")),
        Err(e) => lines.push(format!("[save] 失败: {e}")),
    }

    for l in &lines {
        log::info!("{l}");
    }

    let idx = app.window(
        WindowDesc::new("文件读写：assets + 可写目录", 780, 520),
        None::<fn()>,
    );
    let hud = lines.join("\n");
    let tex = Some(tex);

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let mut batch = DrawBatch::new();

        let size = win.layout_size();
        let h = size.height.dp.0 as f32;

        // 上半：把启动时收集到的信息原样画出来
        draw_text(
            &mut batch.texts,
            &hud,
            Pos::new(14.0, 10.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(FG),
        );

        // 下半：随包贴图，证明 assets 里的图真的解码成功
        let logo_h = 132.0;
        let logo_y = h - logo_h - 16.0;
        if let Some(i) = tex {
            if let Ok(t) = ctx.app().texture(i) {
                let s = logo_h / t.height.max(1) as f32;
                batch.set_texture(Some(&*t));
                draw_rectangle(
                    &mut batch,
                    Pos::new(14.0, logo_y),
                    t.width as f32 * s,
                    logo_h,
                    Some(WHITE),
                );
                // 贴图画完要清掉画笔状态，否则后面的矩形/文字会继续采样这张图
                batch.set_texture(None);
            }
        }

        draw_text(
            &mut batch.texts,
            "assets/logo.png —— 桌面在 examples/assets/，安卓在 src/main/assets/\n\
             同一行代码：app.load_texture_asset(\"logo.png\")",
            Pos::new(160.0, logo_y + 30.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(DIM),
        );

        win.draw(BG, &[&batch]);
        true
    })
    .await
    .unwrap();
}
