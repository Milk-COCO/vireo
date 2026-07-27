//! 自定义 Material（描述符驱动）：4 个矩形，时间驱动脉动颜色
//!
//! ```bash
//! cargo run --example custom_material
//! set VIREO_AA=msaa4; cargo run --example custom_material
//! set VIREO_AA=ssaa4; cargo run --example custom_material
//! ```

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

fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0/3.0, 1.0/3.0);
    let p = abs(fract(vec3<f32>(h) + k) * 6.0 - 3.0);
    return v * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}

fn material_main(in: MaterialInput) -> vec4<f32> {
    let t = u_pulse.time;
    let wave = 0.5 + 0.5 * sin(t * u_pulse.speed + in.base_uv.x * 6.2831);
    let h = fract(u_pulse.hue + wave * 0.3);
    let rgb = hsv2rgb(h, 0.7, 0.9);
    let r = length(in.local_pos) / 55.0;
    let vignette = clamp(1.2 - r, 0.25, 1.0);
    let cell = floor(in.base_uv * 10.0);
    let check = (cell.x + cell.y) % 2.0;
    let darken = 0.55 + 0.45 * check;
    let base = vireo_base_sample(in.base_uv);
    return vec4<f32>(base.rgb * rgb * darken * vignette, 0.95);
}
"#;

fn main() {
    let mut app = App::new();
    let aa = match std::env::var("VIREO_AA").as_deref() {
        Ok("msaa4") => AntiAliasing::Msaa { samples: 4, alpha_to_coverage: false },
        Ok("ssaa4") => AntiAliasing::Ssaa { samples: 4, alpha_to_coverage: false },
        _ => AntiAliasing::None,
    };
    let title = match aa {
        AntiAliasing::None => "Custom Material (no AA)",
        AntiAliasing::Msaa { .. } => "Custom Material (MSAA x4)",
        AntiAliasing::Ssaa { .. } => "Custom Material (SSAA x4)",
    };
    let idx = app.window(WindowDesc::new(title, 500, 400).anti_aliasing(aa), None::<fn()>);

    let mat = app.material_with_resources(WGSL, MaterialResources(&[
        MaterialResource {
            name: "u_pulse",
            kind: MaterialResourceKind::Storage {
                read_only: true,
                size: std::mem::size_of::<PulseParams>() as u64,
                type_name: "Pulse",
                dynamic: false,
            },
        },
    ])).expect("WGSL compile");

    let start = std::time::Instant::now();

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        let t = start.elapsed().as_secs_f32();
        let params = PulseParams {
            time: t,
            speed: 2.0,
            hue: t * 0.1,
            _pad: 0.0,
        };
        mat.set_uniform(&app.gpu.queue, "u_pulse", &params);

        let mut b = DrawBatch::new();
        b.custom_material = Some(mat.clone());
        for i in 0..4 {
            b.set_position(80.0 + i as f32 * 110.0, 200.0);
            b.set_rad(t * 0.3 + i as f32 * 0.4);
            draw_rectangle(&mut b, Pos::new(-40.0, -40.0), 80.0, 80.0, Some(WHITE));
        }
        b.clear_transform();

        let mut title = DrawBatch::new();
        draw_text(
            &mut title.texts,
            "Custom material: HSV pulse",
            Pos::new(16.0, 16.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&b, &title]);
        true
    });
}
