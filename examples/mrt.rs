//! MRT（多目标渲染）示例（P4）。
//!
//! 一次 Pass 写入两个颜色目标：
//! - target 0（`base` 画布）：常规颜色；
//! - target 1（`aux` 画布）：棋盘格辅助通道。
//!
//! 只有 custom material 能写多目标：fragment shader 返回 `MaterialOutput`
//! （第 0 个字段必须叫 `color`，引擎在其上应用 alpha / SDF），其余 location
//! 原样输出。最后把两张画布左右贴回窗口对比（左＝颜色，右＝辅助通道）。

use vireo::prelude::*;

const FRAG: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) aux: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    let base = vireo_base_sample(in.uv);
    let cell = floor(in.local_pos.x / 24.0) + floor(in.local_pos.y / 24.0);
    let checker = cell % 2.0;
    var out: MaterialOutput;
    out.color = vec4<f32>(base.rgb * in.color.rgb, base.a * in.color.a);
    out.aux = vec4<f32>(checker, 1.0 - checker, 0.35 + 0.4 * checker, 1.0);
    return out;
}
"#;

#[vireo::main]
async fn main() {
    let window = app.window(WindowDesc::new("MRT", 760, 420), None::<fn()>);
    let base = app.offscreen(760, 420, AntiAliasing::None);
    let aux = app.offscreen(760, 420, AntiAliasing::None);
    let mat = app.material(FRAG).expect("material");

    let mut t: f32 = 0.0;

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&window) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let base = match ctx.app().offscreen_ref(&base) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let aux = match ctx.app().offscreen_ref(&aux) {
            Ok(v) => v,
            Err(_) => return false,
        };
        t += 0.016;

        // ---- MRT pass：一次提交写两个颜色目标 ----
        let mut scene = DrawBatch::new();
        scene.set_custom_material(Some(mat.clone()));
        draw_rectangle(
            &mut scene,
            Pos::new(0.0, 0.0),
            760.0,
            420.0,
            Some(Color::new(0.06, 0.07, 0.14, 1.0)),
        );
        for i in 0..5 {
            let x = 90.0 + i as f32 * 145.0;
            let y = 210.0 + (t * 2.0 + i as f32).sin() * 70.0;
            draw_circle(&mut scene, Pos::new(x, y), 52.0, Some(ORANGE));
            draw_rectangle(
                &mut scene,
                Pos::new(x - 28.0, y - 28.0),
                56.0,
                56.0,
                Some(MAGENTA),
            );
        }
        let aux_target = ColorTargetDesc {
            view: aux.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        };
        base.draw_mrt(Some(BLACK), &[aux_target], &[&scene]);

        // ---- 贴回窗口：左半 target 0，右半 target 1 ----
        let mut blit = DrawBatch::new();
        {
            let tex = base.texture.lock();
            blit.set_texture(Some(&*tex));
            draw_rectangle(&mut blit, Pos::new(0.0, 0.0), 380.0, 420.0, Some(WHITE));
        }
        {
            let tex = aux.texture.lock();
            blit.set_texture(Some(&*tex));
            draw_rectangle(&mut blit, Pos::new(380.0, 0.0), 380.0, 420.0, Some(WHITE));
        }
        win.draw(BLACK, &[&blit]);
        true
    })
    .await
    .unwrap()
}
