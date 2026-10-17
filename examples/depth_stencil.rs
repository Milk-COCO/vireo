//! 深度预通道 + 着色通道示例（P1）。
//!
//! Pass 1（离屏）：红色左半边，`depth_write=true`，写入 z=0。
//! Pass 2（离屏）：蓝色全屏，`depth_write=false`、`compare=Less`，z=0。
//!   - 左半边深度已为 0 → `0 < 0` 不通过，红色保留。
//!   - 右半边预通道清空值为 1.0 → `0 < 1.0` 通过，蓝色覆盖。
//! 最后将离屏画布贴回窗口展示：左右分色即表示深度测试生效。

use vireo::prelude::*;
use wgpu::{CompareFunction, LoadOp, Operations, StoreOp};

const FRAG: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    return vec4<f32>(in.color.rgb, in.color.a);
}
"#;

fn ds_clear() -> DepthStencilTarget {
    DepthStencilTarget::Engine(DepthStencilOps {
        depth: Some(Operations {
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        }),
        stencil: None,
    })
}

fn ds_load() -> DepthStencilTarget {
    DepthStencilTarget::Engine(DepthStencilOps {
        depth: Some(Operations {
            load: LoadOp::Load,
            store: StoreOp::Store,
        }),
        stencil: None,
    })
}

#[vireo::main]
async fn main() {
    let window = app.window(WindowDesc::new("depth_prepass", 640, 480), None::<fn()>);
    let canvas = app.offscreen(640, 480, AntiAliasing::None);

    let material_red = app.material(FRAG).expect("material");
    material_red.set_depth_write(true);
    let material_blue = app.material(FRAG).expect("material");
    material_blue.set_depth_write(false);
    material_blue.set_depth_compare(CompareFunction::Less);

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&window) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let cv = match ctx.app().offscreen_ref(&canvas) {
            Ok(v) => v,
            Err(_) => return false,
        };

        // Pass 1：深度预通道，红色左半边。
        let mut b1 = DrawBatch::new();
        b1.set_custom_material(Some(material_red.clone()));
        draw_rectangle(&mut b1, Pos::new(0.0, 0.0), 320.0, 480.0, Some(RED));
        cv.draw_with_depth_stencil(Some(BLACK), &[&b1], Some(ds_clear()));

        // Pass 2：着色通道，蓝色全屏（Less 深度测试）。
        let mut b2 = DrawBatch::new();
        b2.set_custom_material(Some(material_blue.clone()));
        draw_rectangle(&mut b2, Pos::new(0.0, 0.0), 640.0, 480.0, Some(BLUE));
        cv.draw_with_depth_stencil(None, &[&b2], Some(ds_load()));

        // 把离屏画布贴回窗口展示。
        let mut present = DrawBatch::new();
        let tex = cv.texture.lock();
        present.set_texture(Some(&*tex));
        draw_rectangle(&mut present, Pos::new(0.0, 0.0), 640.0, 480.0, Some(WHITE));
        drop(tex);

        win.draw(BLACK, &[&present]);
        true
    })
    .await
    .unwrap()
}
