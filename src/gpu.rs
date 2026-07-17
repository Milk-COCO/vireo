//! GPU 上下文和顶点定义。初始化时创建，多窗口共享。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use glyphon::ColorMode;
use crate::text::TextContext;

/// 共享 GPU 资源 —— 多个窗口/离屏纹理共用同一套 device/queue/pipeline
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub render_pipeline: wgpu::RenderPipeline,
    pub camera_bind_group_layout: wgpu::BindGroupLayout,
    pub texture_bind_group_layout: wgpu::BindGroupLayout,
    pub default_sampler: wgpu::Sampler,
    pub white_texture: wgpu::Texture,
    pub white_texture_view: wgpu::TextureView,
    pub white_bind_group: Arc<wgpu::BindGroup>,
    pub surface_format: wgpu::TextureFormat,
    pub text_ctx: RefCell<TextContext>,
    pipelines: RefCell<HashMap<u32, wgpu::RenderPipeline>>,
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
}

impl GpuContext {
    /// 创建 GPU 上下文（不依赖 surface）。
    /// format 默认为 Rgba8UnormSrgb，首窗口创建时通过 ensure_pipeline_format 调整。
    pub fn new(instance: &wgpu::Instance) -> Self {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .unwrap();

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("vireo device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                experimental_features: wgpu::ExperimentalFeatures::default(),
                trace: wgpu::Trace::default(),
            }))
            .unwrap();

        Self::build_resources(
            device,
            queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            ColorMode::Accurate,
        )
    }

    fn build_resources(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        color_mode: ColorMode,
    ) -> Self {
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("camera bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("texture bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let default_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("default sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let white_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("white texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let white_texture_view = white_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let white_bind_group = Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("white bind group"),
            layout: &texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&white_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&default_sampler),
                },
            ],
        }));

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vireo shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vireo pipeline layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout), Some(&texture_bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vireo pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(Vertex::desc())],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let text_ctx = RefCell::new(TextContext::new(&device, &queue, surface_format, color_mode));

        let mut pipelines = HashMap::new();
        pipelines.insert(1, render_pipeline.clone());

        Self {
            device,
            queue,
            render_pipeline,
            camera_bind_group_layout,
            texture_bind_group_layout,
            default_sampler,
            white_texture,
            white_texture_view,
            white_bind_group,
            surface_format,
            text_ctx,
            pipelines: RefCell::new(pipelines),
            shader,
            pipeline_layout,
        }
    }

    /// 获取匹配 sample_count 的 pipeline（按需创建并缓存）
    pub fn ensure_pipeline(&self, sample_count: u32, alpha_to_coverage: bool) -> wgpu::RenderPipeline {
        let key = sample_count | ((alpha_to_coverage as u32) << 16);
        let mut pipes = self.pipelines.borrow_mut();
        if let Some(p) = pipes.get(&key) {
            return p.clone();
        }
        let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vireo pipeline"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(Vertex::desc())],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: sample_count, alpha_to_coverage_enabled: alpha_to_coverage, ..Default::default() },
            multiview_mask: None,
            cache: None,
        });
        pipes.insert(key, p.clone());
        p
    }

    /// 测量文本尺寸（逻辑像素）。参数与 draw_text 一致。
    pub fn measure_text(&self, text: &str, options: &crate::text::TextOptions) -> (f32, f32) {
        use glyphon::{Attrs, Buffer, Metrics, Shaping};

        let mut text_ctx = self.text_ctx.borrow_mut();
        let line_height = options.font_size * 1.2;
        let metrics = Metrics::new(options.font_size, line_height);
        let mut buffer = Buffer::new(&mut text_ctx.font_system, metrics);
        buffer.set_size(options.max_width, None);

        let attrs = options.attrs.as_ref()
            .map(|a| a.as_attrs())
            .unwrap_or_else(Attrs::new);

        buffer.set_text(text, &attrs, Shaping::Advanced, Some(options.align.into()));
        buffer.shape_until_scroll(&mut text_ctx.font_system, false);

        let num_lines = buffer.lines.len() as f32;
        let max_w = (0..buffer.lines.len()).fold(0.0f32, |max, i| {
            let line_w = buffer
                .line_layout(&mut text_ctx.font_system, i)
                .map(|layout| layout.iter().map(|run| run.w).sum())
                .unwrap_or(0.0);
            max.max(line_w)
        });

        (max_w, line_height * num_lines)
    }

    /// 从文件加载字体（TTF/OTF），使该字体可用于 TextOptions::with_family
    pub fn load_font_file(&self, path: impl AsRef<std::path::Path>) -> Result<(), String> {
        let data = std::fs::read(path.as_ref()).map_err(|e| format!("failed to read font file: {}", e))?;
        self.load_font(&data);
        Ok(())
    }

    /// 加载自定义字体（TTF/OTF 字节数据），使该字体可用于 TextOptions::with_family
    pub fn load_font(&self, data: &[u8]) {
        self.text_ctx.borrow_mut().font_system.db_mut().load_font_data(data.to_vec());
    }
}

/// 2D 顶点
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    pub circle: [f32; 4], // (cx, cy, r, feather) — 非 circle 时为 (0,0,0,0)
}

impl Vertex {
    pub fn new(x: f32, y: f32, color: crate::color::Color) -> Self {
        Self {
            position: [x, y],
            uv: [0.0, 0.0],
            color: [color.r, color.g, color.b, color.a],
            circle: [0.0; 4],
        }
    }

    pub fn new_uv(x: f32, y: f32, u: f32, v: f32, color: crate::color::Color) -> Self {
        Self {
            position: [x, y],
            uv: [u, v],
            color: [color.r, color.g, color.b, color.a],
            circle: [0.0; 4],
        }
    }

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        const S2: wgpu::BufferAddress = std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress;
        const S4: wgpu::BufferAddress = std::mem::size_of::<[f32; 4]>() as wgpu::BufferAddress;
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { offset: 0, format: wgpu::VertexFormat::Float32x2, shader_location: 0 },
                wgpu::VertexAttribute { offset: S2, format: wgpu::VertexFormat::Float32x2, shader_location: 1 },
                wgpu::VertexAttribute { offset: S2 * 2, format: wgpu::VertexFormat::Float32x4, shader_location: 2 },
                wgpu::VertexAttribute { offset: S2 * 2 + S4, format: wgpu::VertexFormat::Float32x4, shader_location: 3 },
            ],
        }
    }
}
