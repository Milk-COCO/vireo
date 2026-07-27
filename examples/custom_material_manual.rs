//! 自定义资源 Material：用户自建 BGL + buffer + bind group，自管 WGSL 的 @group(3)
//!
//! ```bash
//! cargo run --example custom_material_manual
//! ```
//!
//! 与 `custom_material` 做同样的事（HSV 脉动），区别在于：
//! - WGSL 里手动写 `@group(3) @binding(0) var<storage, read> u_pulse: Pulse;`
//! - 用户自建 BGL / buffer / bind group
//! - 安装 `set_bind_group_provider` 返回 bind group
//! - 数据通过 `queue.write_buffer` 直接写入 buffer
//!
//! 存在的意义：可能有一些事是 Vireo 当前封装做不到的，保留这些让你自定义。

use vireo::prelude::*;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PulseParams {
    time: f32,
    speed: f32,
    hue: f32,
    _pad: f32,
}

/// 注意：WGSL 中手动标注 @group(3) @binding(0)，引擎不会注入。
const WGSL: &str = r#"
struct Pulse {
    time: f32,
    speed: f32,
    hue: f32,
};

@group(3) @binding(0) var<storage, read> u_pulse: Pulse;

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
    let idx = app.window(
        WindowDesc::new("Custom Material (manual BGL)", 500, 400),
        None::<fn()>,
    );

    // ── 自建 BGL、buffer、bind group ──

    let buf = app.gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pulse_params"),
        size: std::mem::size_of::<PulseParams>() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bgl = app.gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pulse_bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(
                    std::mem::size_of::<PulseParams>() as u64,
                ),
            },
            count: None,
        }],
    });

    let mat = app.material_manual(WGSL, &bgl).expect("WGSL compile");

    let bind_group = app.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pulse_bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buf.as_entire_binding(),
        }],
    });

    // provider 每帧被调用 — 返回缓存的 bind group（它只被引擎借来用一下）
    mat.set_bind_group_provider(move |_, _| bind_group.clone());

    let start = std::time::Instant::now();

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        // 手动写数据到 buffer（描述符材质走 mat.set_uniform，手动 BGL 直接写）
        let t = start.elapsed().as_secs_f32();
        let params = PulseParams {
            time: t,
            speed: 2.0,
            hue: t * 0.1,
            _pad: 0.0,
        };
        app.gpu.queue.write_buffer(
            &buf,
            0,
            bytemuck::cast_slice(std::slice::from_ref(&params)),
        );

        // ── 绘制 ──

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
            "Manual BGL / buffer / provider",
            Pos::new(16.0, 16.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(Some(Color::new(0.06, 0.08, 0.12, 1.0)), &[&b, &title]);
        true
    });
}
