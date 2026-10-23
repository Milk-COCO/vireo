//! MRT（多目标渲染）：**一次 pass 写两张图**。
//!
//! 片元着色器返回 `MaterialOutput`，两个字段分别落进两个颜色附件：
//!
//! - `@location(0) color`：场景颜色（引擎会在这一个上面应用 alpha / SDF）；
//! - `@location(1) mask`：同一批片元顺手算出的掩码（这里只把圆形标 1）。
//!
//! 窗口左右摊开这两个目标，方便确认"确实是一次画出来、两张图逐像素对齐"。
//! 想让第二张图真正干点活，看 `mrt_glow.rs`（拿掩码做轮廓光晕）。
//!
//! ## 几个容易踩的点
//!
//! - 只有 **custom material** 能写多目标；引擎内置形状/文字 shader 只有单颜色输出，
//!   附件数不为 1 时会被跳过并 warn-once。
//! - **第 0 个字段必须叫 `color`**：引擎在它上面做 SDF / alpha，其余 location 原样写出。
//! - 附加目标固定 `LoadOp::Load`，**不会自动清屏**。这里靠"全写"（`blend: None`）保证
//!   每帧每个像素都由本帧最后一次写入决定——注意全写语义下重叠处"后画者赢"，
//!   所以下面的图形故意不重叠。
//! - 场景要按**面板尺寸**渲染（`PANEL_W × H`）。若按整窗宽渲染再铺进半窗，会把圆横向压扁。
//! - 颜色是**线性值**：目标格式是 sRGB 时写入会被编码，所以深底用 `0.005` 级而不是 `0.06`。

use vireo::prelude::*;

/// 每块面板的宽；整窗 = 2 × 面板宽。
const PANEL_W: u32 = 380;
const H: u32 = 420;

const FRAG: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) mask: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    let base = vireo_base_sample(in.uv);
    let sel = select(0.0, 1.0, in.sdf_type == 1u);   // 只把"圆形"标进掩码
    var out: MaterialOutput;
    out.color = base * in.color;
    out.mask = vec4<f32>(sel, sel, sel, 1.0);
    return out;
}
"#;

fn panel(batch: &mut DrawBatch, canvas: &OffscreenCanvas, x: f32) {
    let tex = canvas.texture.lock();
    batch.set_texture(Some(&*tex));
    draw_rectangle(
        batch,
        Pos::new(x, 0.0),
        PANEL_W as f32,
        H as f32,
        Some(WHITE),
    );
}

#[vireo::main]
async fn main() {
    let window = app.window(
        WindowDesc::new("MRT: albedo + mask", PANEL_W * 2, H),
        None::<fn()>,
    );
    let scene = app.offscreen(PANEL_W, H, AntiAliasing::None);
    let mask = app.offscreen(PANEL_W, H, AntiAliasing::None);
    let mat = app.material(FRAG).expect("material");

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&window) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let scene = match ctx.app().offscreen_ref(&scene) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let mask = match ctx.app().offscreen_ref(&mask) {
            Ok(v) => v,
            Err(_) => return false,
        };

        // ---- 一次 pass，两个颜色目标；图形不重叠（全写语义下后画者赢）----
        let mut geo = DrawBatch::new();
        geo.set_custom_material(Some(mat.clone()));
        draw_rectangle(
            &mut geo,
            Pos::new(0.0, 0.0),
            PANEL_W as f32,
            H as f32,
            Some(Color::new(0.005, 0.006, 0.013, 1.0)),
        );
        draw_circle(&mut geo, Pos::new(95.0, 120.0), 46.0, Some(ORANGE));
        draw_circle(&mut geo, Pos::new(95.0, 300.0), 46.0, Some(ORANGE));
        draw_rectangle(&mut geo, Pos::new(212.0, 78.0), 84.0, 84.0, Some(MAGENTA));
        draw_rectangle(&mut geo, Pos::new(212.0, 258.0), 84.0, 84.0, Some(MAGENTA));
        let mask_target = ColorTargetDesc {
            view: mask.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        };
        // 文字直接画进 MRT pass（无 custom material）：引擎文字只写 target 0，
        // 所以下面 "1) albedo" 面板会出现这行字，而 "2) mask" 面板不会有 —— 一眼可见。
        let mut label = DrawBatch::new();
        draw_text(
            &mut label.texts,
            "text drawn in the MRT pass -> albedo only",
            Pos::new(12.0, H as f32 - 28.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.85, 0.88, 0.95, 1.0)),
        );
        scene.draw_mrt(Some(BLACK), &[mask_target], &[&geo, &label]);

        // ---- 摊开看两个目标 ----
        let mut blit = DrawBatch::new();
        panel(&mut blit, &scene, 0.0);
        panel(&mut blit, &mask, PANEL_W as f32);
        draw_text(
            &mut blit.texts,
            "1) albedo (target 0)",
            Pos::new(12.0, 10.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.75, 0.78, 0.85, 1.0)),
        );
        draw_text(
            &mut blit.texts,
            "2) mask (target 1)",
            Pos::new(PANEL_W as f32 + 12.0, 10.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.75, 0.78, 0.85, 1.0)),
        );

        win.draw(BLACK, &[&blit]);
        true
    })
    .await
    .unwrap();
}
