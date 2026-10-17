//! GPU compute 示例（P5）。
//!
//! compute shader 每帧把「粒子场」写进一张 storage texture（96 个环绕粒子、
//! 颜色/角速度由 u32 hash 决定），引擎只负责把该纹理贴回窗口 —— 写入结果
//! 对后续 render pass 可见（dispatch 与 draw 各自提交，wgpu 自动插 barrier）。
//!
//! 演示的 API 面：
//! - `App::gpu()`：拿 device/queue 自建纹理、uniform buffer、BGL/BindGroup；
//! - `App::compute_pipeline(wgsl, entry, &[(group, &bgl)])`：创建 compute 管线；
//! - `App::dispatch(&kernel, &[(group, &bg)], [x, y, z])`：提交 dispatch。

use vireo::prelude::*;

const SIZE: u32 = 512;

const COMPUTE: &str = r#"
struct Params {
    time: f32,
    width: f32,
    height: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var out_tex: texture_storage_2d<rgba8unorm, write>;

fn hash(n: u32) -> f32 {
    var x = n * 747796405u + 2891336453u;
    x = ((x >> ((x >> 28u) + 4u)) ^ x) * 277803737u;
    x = (x >> 22u) ^ x;
    return f32(x) / 4294967295.0;
}

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= u32(params.width) || gid.y >= u32(params.height) {
        return;
    }
    let p = vec2<f32>(f32(gid.x), f32(gid.y));
    let center = vec2<f32>(params.width * 0.5, params.height * 0.5);
    var c = vec3<f32>(0.015, 0.02, 0.05);
    for (var i = 0u; i < 96u; i++) {
        let fi = f32(i);
        let ang = params.time * (0.4 + hash(i) * 1.6) + fi * 2.39996;
        let rad = 30.0 + hash(i + 101u) * 190.0;
        let pos = center + vec2<f32>(cos(ang), sin(ang)) * rad;
        let d = length(p - pos);
        let glow = 4.0 / (1.0 + d * d * 0.35);
        let tint = vec3<f32>(
            0.35 + 0.65 * hash(i + 7u),
            0.25 + 0.50 * hash(i + 13u),
            1.0,
        );
        c += tint * glow * 0.05;
    }
    textureStore(out_tex, vec2<i32>(gid.xy), vec4<f32>(c, 1.0));
}
"#;

fn params_bytes(time: f32) -> [u8; 16] {
    let params = [time, SIZE as f32, SIZE as f32, 0.0];
    let mut bytes = [0u8; 16];
    for (i, v) in params.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes
}

#[vireo::main]
async fn main() {
    let window = app.window(
        WindowDesc::new("compute particles", SIZE as u32, SIZE as u32)
            .anti_aliasing(AntiAliasing::None),
        None::<fn()>,
    );

    // ---- 自建 GPU 资源（compute 需要 device/queue 的直接访问）----
    let gpu = app.gpu().clone();
    let storage_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("compute particle field"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let storage_view = storage_tex.create_view(&Default::default());

    // 引擎贴图布局（group 1 binding 0/1）：让 draw_texture/ set_texture 能用该纹理。
    let display_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("compute display bind group"),
        layout: &gpu.texture_bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&storage_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&gpu.default_sampler),
            },
        ],
    });
    let field = Texture {
        texture: storage_tex,
        view: storage_view,
        bind_group: display_bg,
        width: SIZE,
        height: SIZE,
        format: wgpu::TextureFormat::Rgba8Unorm,
    };

    let params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("compute params"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let compute_bgl = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
    let compute_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("compute bind group"),
        layout: &compute_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&field.view),
            },
        ],
    });

    let kernel = app
        .compute_pipeline(COMPUTE, "cs_main", &[(0, &compute_bgl)])
        .expect("compute pipeline");

    let mut t: f32 = 0.0;
    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&window) {
            Ok(v) => v,
            Err(_) => return false,
        };
        t += 0.016;

        gpu.queue.write_buffer(&params_buf, 0, &params_bytes(t));
        ctx.app().dispatch(
            &kernel,
            &[(0, &compute_bg)],
            [SIZE.div_ceil(8), SIZE.div_ceil(8), 1],
        );

        let mut b = DrawBatch::new();
        b.set_texture(Some(&field));
        draw_rectangle(
            &mut b,
            Pos::new(0.0, 0.0),
            SIZE as f32,
            SIZE as f32,
            Some(WHITE),
        );
        win.draw(BLACK, &[&b]);
        true
    })
    .await
    .unwrap()
}
