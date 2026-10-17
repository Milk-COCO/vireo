//! 自管 Bind Group Material（P3）：用户自带 BGL + group 编号，引擎不再插入 group 0/1/2。
//!
//! ```bash
//! cargo run --example custom_material_self_managed
//! ```
//!
//! 与 `custom_material_manual` 做同样的事（HSV 脉动），区别在于：
//! - WGSL 里用 `@group(0) @binding(0)`，不再是 `@group(3)` —— 证明第三方 shader
//!   可零改移植，不受引擎内置 bind group 占用。
//! - 用户自建 BGL / buffer / bind group，安装 `set_bind_group_provider`。
//! - 数据通过 `queue.write_buffer` 直接写入 buffer。
//!
//! P3 验收：第三方 wgpu shader 零改移植能跑。

use vireo::prelude::*;
use wgpu::BufferUsages;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PulseParams {
    time: f32,
    speed: f32,
    hue: f32,
    _pad: f32,
}

/// WGSL 用 @group(0) @binding(0)，不再是 @group(3)。引擎不再把 group 0 留给
/// camera uniform，用户可自由使用任意 group 索引。
const WGSL: &str = r#"
struct Pulse {
    time: f32,
    speed: f32,
    hue: f32,
};

@group(0) @binding(0) var<storage, read> u_pulse: Pulse;

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

#[vireo::main]
async fn main() {
    let idx = app.window(
        WindowDesc::new("Custom Material (self-managed BGL)", 500, 400),
        None::<fn()>,
    );

    // ── 自建 BGL、buffer、bind group（group 0，非引擎 group 3） ──
    let buf = app.gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pulse_params"),
        size: std::mem::size_of::<PulseParams>() as u64,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bgl = app
        .gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pulse_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<PulseParams>() as u64
                    ),
                },
                count: None,
            }],
        });

    // 自管材质：group 0 绑定用户自己的 BGL，引擎不插入 group 0/1/2。
    let mat = app
        .material_self_managed(WGSL, &[(0, &bgl)])
        .expect("WGSL compile");

    let bind_group = app
        .gpu
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pulse_bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });

    // provider 每帧被调用 — 返回缓存的 bind group。
    mat.set_bind_group_provider(move |_, _| bind_group.clone());

    let start = std::time::Instant::now();

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };

        // 手动写数据到 buffer（自管材质直接写 buffer）。
        let t = start.elapsed().as_secs_f32();
        let params = PulseParams {
            time: t,
            speed: 2.0,
            hue: t * 0.1,
            _pad: 0.0,
        };
        ctx.app().gpu.queue.write_buffer(
            &buf,
            0,
            bytemuck::cast_slice(std::slice::from_ref(&params)),
        );

        // ── 绘制 ──
        let mut b = DrawBatch::new();
        b.set_custom_material(Some(mat.clone()));
        for i in 0..4 {
            b.set_position(80.0 + i as f32 * 110.0, 200.0);
            b.set_rad(t * 0.3 + i as f32 * 0.4);
            draw_rectangle(&mut b, Pos::new(-40.0, -40.0), 80.0, 80.0, Some(WHITE));
        }
        b.clear_transform();

        let mut title = DrawBatch::new();
        draw_text(
            &mut title.texts,
            "Self-managed BGL @group(0)",
            Pos::new(16.0, 16.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(Color::new(0.06, 0.08, 0.12, 1.0), &[&b, &title]);
        true
    })
    .await
    .unwrap();
}
