//! vireo Android 模板入口。
//!
//! 这是一个 **cdylib**（`crate-type = ["cdylib"]`，入口在 `src/lib.rs` 而非
//! `src/main.rs`）—— Android 侧由 AGSK 通过 `#[no_mangle] android_main` 调用，
//! 不需要 Rust 的 `fn main`。
//!
//! Android API 全部 `cfg` 门控，所以桌面上 `cargo check` 也能跑，
//! 用来在没装 Android 工具链时先把代码笔误抓出来：
//!
//! ```bash
//! cargo check                          # 桌面，只查非 Android 分支
//! cargo ndk -t arm64-v8a check --lib   # Android，查全部
//! ```
//!
//! 想看**能跑**的桌面示例，去仓库根的 `examples/`（`cargo run --example ez`）；
//! 本目录专注 Android 构建链。
//!
//! 演示三件事：
//! 1. **沉浸式 + 安全区三值**：`safe_area` / `padding` / `ime_insets` 各画一圈，
//!    直接看出刘海、状态栏、键盘各占多少。
//! 2. **文本输入**：点输入框 → `show_soft_input()` → 每帧读 `text_input_state()` 画出来。
//!    中文可用（GameActivity 提供 `InputConnection`）。
//! 3. **键盘避让**：键盘弹出时把输入框上移 `ime_insets.bottom`。

// 这个 cdylib 的**唯一**入口是 `#[vireo::main]` 展开出的 `android_main`，AGDK
// 在运行时按符号名查找它。rustc 的 dead_code 分析看不到「JNI 按字符串找符号」
// 这层间接引用，于是把 android_main 里用到的私有项全报成「未使用」。
//
// 这不是笔误 —— 真机上代码确实在跑。对纯 JNI cdylib 来说整个 crate 都是
// 「只被 JNI 到达」，所以这里整 crate 放宽。
#![allow(dead_code)]

use vireo::prelude::*;

/// 安全区四边（逻辑像素）。
///
/// vireo 的 `platform::android::Insets` 只在 Android 上编译，所以用本地结构体
/// 让同一份绘制代码在桌面也能过 `cargo check`。
#[derive(Clone, Copy, Default)]
struct Edges {
    l: f32,
    t: f32,
    r: f32,
    b: f32,
}

#[cfg(target_os = "android")]
impl From<vireo::platform::android::Insets> for Edges {
    fn from(i: vireo::platform::android::Insets) -> Self {
        Self {
            l: i.left,
            t: i.top,
            r: i.right,
            b: i.bottom,
        }
    }
}

#[vireo::main]
async fn main(app: App) {
    // 不调这个的话 vireo 的 `log::warn!`（字体加载、IME、生命周期）全被丢弃。
    vireo::init_logger();

    let idx = app.window(WindowDesc::new("Vireo Android", 1280, 720), None::<fn()>);

    // 打开就沉浸式：状态栏 + 导航栏都让给内容。
    #[cfg(target_os = "android")]
    {
        use vireo::platform::android::{SystemBars, WindowExtAndroid};
        if let Ok(win) = app.window_ref(&idx) {
            let _ = win.set_system_bars(SystemBars::ALL);
            let _ = win.set_keep_screen_on(true);
        }
    }

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(w) => w,
            Err(_) => return true,
        };

        // ---- 安全区三值 --------------------------------------------------
        #[cfg(target_os = "android")]
        let (safe, pad, ime): (Edges, Edges, Edges) = {
            use vireo::platform::android::WindowExtAndroid;
            (
                win.safe_area().into(),
                win.padding().into(),
                win.ime_insets().into(),
            )
        };
        #[cfg(not(target_os = "android"))]
        let (safe, pad, ime): (Edges, Edges, Edges) = Default::default();

        let size = win.layout_size();
        let (vw, vh) = (size.width.dp.0 as f32, size.height.dp.0 as f32);

        let mut batch = DrawBatch::new();

        // 三圈：safe_area（最大可能遮挡）/ padding（当前可见遮挡）/ ime（键盘）
        ring(&mut batch, vw, vh, safe, RED);
        ring(&mut batch, vw, vh, pad, YELLOW);
        ring(&mut batch, vw, vh, ime, GREEN);

        // ---- 文本输入框（键盘弹出时上移）----------------------------------
        let fw = vw - 160.0;
        let fx = 80.0;
        let fy = 160.0 + ime.b;
        draw_rectangle(&mut batch, Pos::new(fx, fy), fw, 72.0, Some(GRAY));

        let m = win.mouse_pos().logical();
        let (mx, my) = (m.0 as f32, m.1 as f32);
        let focused = mx >= fx && mx <= fx + fw && my >= fy && my <= fy + 72.0;

        let (typed, cursor) = {
            #[cfg(target_os = "android")]
            {
                use vireo::platform::android::WindowExtAndroid;
                match win.text_input_state() {
                    Some(s) => (s.text.clone(), s.selection.end),
                    None => (String::new(), 0),
                }
            }
            #[cfg(not(target_os = "android"))]
            {
                (String::new(), 0usize)
            }
        };

        let shown: &str = if focused {
            typed.as_str()
        } else {
            "点我聚焦"
        };
        draw_text(
            &mut batch.texts,
            shown,
            Pos::new(fx + 16.0, fy + 16.0),
            TextDef {
                font_size: 28.0,
                ..Default::default()
            },
            TextOverride::new(),
        );

        // 系统不画光标（`TextInputState` 只给文本 + selection），自己画一根。
        // 宽度是粗略估算；真要精确得用 `gpu.measure_text`。
        if focused {
            let approx = 16.0 + cursor as f32 * 17.0;
            draw_rectangle(
                &mut batch,
                Pos::new(fx + approx, fy + 14.0),
                3.0,
                30.0,
                Some(WHITE),
            );
        }

        // ---- HUD ------------------------------------------------------------
        let mut hud = format!("fps {:.0}   {vw:.0}x{vh:.0}\n", ctx.fps());
        hud.push_str(&format!(
            "safe    {:.0},{:.0}  {:.0},{:.0}\n",
            safe.l, safe.t, safe.r, safe.b
        ));
        hud.push_str(&format!(
            "padding {:.0},{:.0}  {:.0},{:.0}\n",
            pad.l, pad.t, pad.r, pad.b
        ));
        hud.push_str(&format!(
            "ime     {:.0},{:.0}  {:.0},{:.0}\n",
            ime.l, ime.t, ime.r, ime.b
        ));
        hud.push_str(&format!("text    {typed:?}"));
        // emoji：曲名/故事板要用，得确认能画出来（不是豆腐块）
        let mut emoji_probe = String::from("\nemoji");
        // 逐行测：单色 / 旗帜 / ZWJ 序列 / 肤色修饰
        for (label, s) in [
            ("fox", "🦊"),
            ("note", "🎵"),
            ("grin", "😀"),
            ("flagCN", "🇨🇳"),
            ("zwj-family", "👨‍👩‍👧"),
            ("skin", "👍🏽"),
        ] {
            emoji_probe.push_str(&format!("  {label:<10} {s}"));
            if label != "skin" {
                emoji_probe.push('\n');
            }
        }
        draw_text(
            &mut batch.texts,
            &emoji_probe,
            Pos::new(80.0, 330.0),
            TextDef {
                font_size: 20.0,
                ..Default::default()
            },
            TextOverride::new(),
        );

        // 字体加载摘要（Android 上 logcat 不可靠，看画面）
        #[cfg(target_os = "android")]
        {
            // 字体加载摘要（Android 上 logcat 不可靠，看画面）
            let mut line = String::new();
            match vireo::text::android_fonts::loaded_summary() {
                Some((files, faces, failed, fam)) => line.push_str(&format!(
                    "fonts   {files} files -> {faces} faces ({failed} bad) default={fam}\n"
                )),
                None => line.push_str("fonts   (install() 未运行)\n"),
            }
            draw_text(
                &mut batch.texts,
                &line,
                Pos::new(80.0, 500.0),
                TextDef {
                    font_size: 16.0,
                    ..Default::default()
                },
                TextOverride::new(),
            );
        }

        draw_text(
            &mut batch.texts,
            &hud,
            Pos::new(80.0, 60.0),
            TextDef {
                font_size: 20.0,
                ..Default::default()
            },
            TextOverride::new(),
        );

        win.draw(BLACK, &[&batch]);

        // ---- 焦点管理放帧末：早一帧调会被部分 ROM 吞掉 ----------------------
        #[cfg(target_os = "android")]
        {
            use vireo::platform::android::{InputType, WindowExtAndroid};
            if focused {
                let _ = win.set_input_type(InputType::Text);
                let _ = win.show_soft_input();
            } else {
                let _ = win.hide_soft_input();
            }
        }

        true
    })
    .await
    .unwrap();
}

/// 画一圈安全区：四条边各一个半透明矩形。
fn ring(batch: &mut DrawBatch, vw: f32, vh: f32, e: Edges, color: Color) {
    let c = color.with_alpha(0.28);
    if e.r > 0.0 {
        draw_rectangle(batch, Pos::new(vw - e.r, 0.0), e.r, vh, Some(c));
    }
    if e.b > 0.0 {
        draw_rectangle(batch, Pos::new(0.0, vh - e.b), vw, e.b, Some(c));
    }
    if e.l > 0.0 {
        draw_rectangle(batch, Pos::new(0.0, 0.0), e.l, vh, Some(c));
    }
    if e.t > 0.0 {
        draw_rectangle(batch, Pos::new(0.0, 0.0), vw, e.t, Some(c));
    }
}
