//! 自定义 Material + Stencil（clips_children）演示
//!
//! 父 batch 开 clips_children，子 batch 用 custom shader 画出界内容；
//! 证明 stencil 路径下自定义 FS 仍生效（不再回退 builtin）。

use std::cell::RefCell;
use vireo::prelude::*;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PulseParams {
    time: f32,
    speed: f32,
    hue: f32,
    _pad: f32,
}

const WGSL: &str = r#"
struct Pulse {
    time: f32,
    speed: f32,
    hue: f32,
};

@group(3) @binding(0) var<storage> u_pulse: Pulse;

fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0/3.0, 1.0/3.0);
    let p = abs(fract(vec3<f32>(h) + k) * 6.0 - 3.0);
    return v * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}

fn material_main(in: MaterialInput) -> vec4<f32> {
    let t = u_pulse.time;
    let wave = 0.5 + 0.5 * sin(t * u_pulse.speed + in.uv.x * 6.2831);
    let h = fract(u_pulse.hue + wave * 0.3);
    let rgb = hsv2rgb(h, 0.8, 0.95);
    let cell = floor(in.uv * 12.0);
    let check = (cell.x + cell.y) % 2.0;
    let intensity = 0.55 + 0.45 * check;
    // in.color 已经是 texture * in.color（白色），直接叠加 HSV 脉动
    return vec4<f32>(rgb * intensity, 0.95);
}
"#;

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Custom Material + Stencil", 640, 480),
        None::<fn()>,
    );

    let mat: RefCell<Option<std::sync::Arc<Material>>> = RefCell::new(None);
    let start = std::time::Instant::now();

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();
        let mat = if mat.borrow().is_none() {
            let m = win
                .gpu()
                .create_material(WGSL)
                .expect("WGSL compile");
            *mat.borrow_mut() = Some(m.clone());
            m
        } else {
            mat.borrow().as_ref().unwrap().clone()
        };

        let t = start.elapsed().as_secs_f32();
        mat.set_uniform(
            &win.gpu().queue,
            &PulseParams {
                time: t,
                speed: 3.0,
                hue: t * 0.15,
                _pad: 0.0,
            },
        );

        // 父：clip 形状（圆角矩形区域）+ clips_children
        let mut parent = DrawBatch::new();
        parent.set_position(120.0, 100.0);
        parent.clips_children = true;
        draw_rounded_rect(
            &mut parent,
            Pos::new(0.0, 0.0),
            280.0,
            280.0,
            40.0,
            Some(Color::new(0.12, 0.14, 0.2, 0.35)),
        );

        // 子：custom material，故意画出 clip 区域外
        let mut child = DrawBatch::new();
        child.custom_material = Some(mat.clone());
        for i in 0..6 {
            child.set_position(40.0 + i as f32 * 50.0, 40.0 + (i % 3) as f32 * 70.0);
            child.set_rad(t * 1.1 + i as f32 * 0.7);
            draw_rectangle(&mut child, Pos::new(-28.0, -28.0), 56.0, 56.0, Some(WHITE));
        }
        parent.push_child(child);

        // 右侧：无 clip，同样 custom material
        let mut free = DrawBatch::new();
        free.custom_material = Some(mat);
        for i in 0..3 {
            free.set_position(480.0, 120.0 + i as f32 * 100.0);
            free.set_rad(t * 0.6 - i as f32 * 0.4);
            draw_rectangle(&mut free, Pos::new(-40.0, -40.0), 80.0, 80.0, Some(WHITE));
        }

        let mut title = DrawBatch::new();
        draw_text(
            &mut title.texts,
            "clips_children + custom FS (stencil)",
            Pos::new(16.0, 16.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(WHITE),
        );
        draw_text(
            &mut title.texts,
            "no clip",
            Pos::new(440.0, 16.0),
            TextDef::default().font_size(15.0),
            TextOverride::from_color(WHITE),
        );
        win.draw(
            Some(Color::new(0.05, 0.07, 0.11, 1.0)),
            &[&parent, &free, &title],
        );
        true
    });
}
