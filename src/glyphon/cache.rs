use super::{GlyphToRender, Params};
use std::{
    borrow::Cow,
    mem,
    num::NonZeroU64,
    ops::Deref,
    sync::{Arc, Mutex},
};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutEntry,
    BindingResource, BindingType, BlendState, Buffer, BufferBindingType, ColorTargetState,
    ColorWrites, DepthStencilState, Device, FilterMode, FragmentState, MipmapFilterMode,
    MultisampleState, PipelineCompilationOptions, PipelineLayout, PipelineLayoutDescriptor,
    PrimitiveState, PrimitiveTopology, RenderPipeline, RenderPipelineDescriptor, Sampler,
    SamplerBindingType, SamplerDescriptor, ShaderModule, ShaderModuleDescriptor, ShaderSource,
    ShaderStages, TextureFormat, TextureSampleType, TextureView, TextureViewDimension,
    VertexFormat, VertexState,
};

/// A cache to share common resources (e.g., pipelines, layouts, shaders) between multiple text
/// renderers.
#[derive(Debug, Clone)]
pub struct Cache(Arc<Inner>);

type InnerCache = Mutex<
    Vec<(
        TextureFormat,
        MultisampleState,
        Option<DepthStencilState>,
        RenderPipeline,
    )>,
>;

#[derive(Debug)]
struct Inner {
    sampler: Sampler,
    shader: ShaderModule,
    vertex_buffers: [Option<wgpu::VertexBufferLayout<'static>>; 1],
    atlas_layout: BindGroupLayout,
    uniforms_layout: BindGroupLayout,
    engine_storage_layout: BindGroupLayout,
    pipeline_layout: PipelineLayout,
    cache: InnerCache,
}

impl Cache {
    /// Creates a new `Cache` with the given `device` and transform bind group layout.
    pub fn new(device: &Device, engine_storage_bgl: &BindGroupLayout) -> Self {
        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("glyphon sampler"),
            min_filter: FilterMode::Nearest,
            mag_filter: FilterMode::Nearest,
            mipmap_filter: MipmapFilterMode::Nearest,
            lod_min_clamp: 0f32,
            lod_max_clamp: 0f32,
            ..Default::default()
        });

        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("glyphon shader"),
            source: ShaderSource::Wgsl(Cow::Borrowed(include_str!("shader.wgsl"))),
        });

        let vertex_buffer_layout = wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<GlyphToRender>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    format: VertexFormat::Sint32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: VertexFormat::Uint32,
                    offset: mem::size_of::<u32>() as u64 * 2,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: VertexFormat::Uint32,
                    offset: mem::size_of::<u32>() as u64 * 3,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: VertexFormat::Uint32,
                    offset: mem::size_of::<u32>() as u64 * 4,
                    shader_location: 3,
                },
                wgpu::VertexAttribute {
                    format: VertexFormat::Uint32,
                    offset: mem::size_of::<u32>() as u64 * 5,
                    shader_location: 4,
                },
                wgpu::VertexAttribute {
                    format: VertexFormat::Float32,
                    offset: mem::size_of::<u32>() as u64 * 6,
                    shader_location: 5,
                },
                wgpu::VertexAttribute {
                    format: VertexFormat::Uint32,
                    offset: mem::size_of::<u32>() as u64 * 7,
                    shader_location: 6,
                },
                wgpu::VertexAttribute {
                    format: VertexFormat::Float32x4,
                    offset: mem::size_of::<u32>() as u64 * 8,
                    shader_location: 7,
                },
            ],
        };

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        multisampled: false,
                        view_dimension: TextureViewDimension::D2,
                        sample_type: TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        multisampled: false,
                        view_dimension: TextureViewDimension::D2,
                        sample_type: TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 3,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        multisampled: false,
                        view_dimension: TextureViewDimension::D2,
                        sample_type: TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 4,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
            label: Some("glyphon atlas bind group layout"),
        });

        let uniforms_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::VERTEX,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(mem::size_of::<Params>() as u64),
                },
                count: None,
            }],
            label: Some("glyphon uniforms bind group layout"),
        });

        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            bind_group_layouts: &[
                Some(&atlas_layout),   // group 0: color/mask atlas + sampler + batch base texture
                Some(&uniforms_layout),// group 1: screen resolution
                Some(engine_storage_bgl), // group 2: transforms + polygon dummy
            ],
            ..Default::default()
        });

        Self(Arc::new(Inner {
            sampler,
            shader,
            vertex_buffers: [Some(vertex_buffer_layout)],
            uniforms_layout,
            engine_storage_layout: engine_storage_bgl.clone(),
            atlas_layout,
            pipeline_layout,
            cache: Mutex::new(Vec::new()),
        }))
    }

    pub(crate) fn create_atlas_bind_group(
        &self,
        device: &Device,
        color_atlas: &TextureView,
        mask_atlas: &TextureView,
        base_texture: &TextureView,
        base_sampler: &Sampler,
    ) -> BindGroup {
        device.create_bind_group(&BindGroupDescriptor {
            layout: &self.0.atlas_layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(color_atlas),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::TextureView(mask_atlas),
                },
                BindGroupEntry {
                    binding: 2,
                    resource: BindingResource::Sampler(&self.0.sampler),
                },
                BindGroupEntry {
                    binding: 3,
                    resource: BindingResource::TextureView(base_texture),
                },
                BindGroupEntry {
                    binding: 4,
                    resource: BindingResource::Sampler(base_sampler),
                },
            ],
            label: Some("glyphon atlas bind group"),
        })
    }

    pub(crate) fn create_uniforms_bind_group(&self, device: &Device, buffer: &Buffer) -> BindGroup {
        device.create_bind_group(&BindGroupDescriptor {
            layout: &self.0.uniforms_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
            label: Some("glyphon uniforms bind group"),
        })
    }

    pub(crate) fn get_or_create_pipeline(
        &self,
        device: &Device,
        format: TextureFormat,
        multisample: MultisampleState,
        depth_stencil: Option<DepthStencilState>,
    ) -> RenderPipeline {
        let Inner {
            cache,
            pipeline_layout,
            shader,
            vertex_buffers,
            ..
        } = self.0.deref();

        let mut cache = cache.lock().expect("Write pipeline cache");

        cache
            .iter()
            .find(|(fmt, ms, ds, _)| fmt == &format && ms == &multisample && ds == &depth_stencil)
            .map(|(_, _, _, p)| p.clone())
            .unwrap_or_else(|| {
                let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
                    label: Some("glyphon pipeline"),
                    layout: Some(pipeline_layout),
                    vertex: VertexState {
                        module: shader,
                        entry_point: Some("vs_main"),
                        buffers: vertex_buffers,
                        compilation_options: PipelineCompilationOptions::default(),
                    },
                    fragment: Some(FragmentState {
                        module: shader,
                        entry_point: Some("fs_main"),
                        targets: &[Some(ColorTargetState {
                            format,
                            blend: Some(BlendState::ALPHA_BLENDING),
                            write_mask: ColorWrites::default(),
                        })],
                        compilation_options: PipelineCompilationOptions::default(),
                    }),
                    primitive: PrimitiveState {
                        topology: PrimitiveTopology::TriangleStrip,
                        ..Default::default()
                    },
                    depth_stencil: depth_stencil.clone(),
                    multisample,
                    cache: None,
                    multiview_mask: None,
                });

                cache.push((format, multisample, depth_stencil, pipeline.clone()));

                pipeline
            })
            .clone()
    }

    pub(crate) fn create_material_pipeline(
        &self,
        device: &Device,
        material_layout: &BindGroupLayout,
        fragment_source: &str,
        format: TextureFormat,
        multisample: MultisampleState,
        depth_stencil: Option<DepthStencilState>,
    ) -> RenderPipeline {
        let fragment = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("glyphon material fragment"),
            source: ShaderSource::Wgsl(Cow::Borrowed(fragment_source)),
        });
        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("glyphon material pipeline layout"),
            bind_group_layouts: &[
                Some(&self.0.atlas_layout),
                Some(&self.0.uniforms_layout),
                Some(&self.0.engine_storage_layout),
                Some(material_layout),
            ],
            ..Default::default()
        });
        device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("glyphon material pipeline"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &self.0.shader,
                entry_point: Some("vs_main"),
                buffers: &self.0.vertex_buffers,
                compilation_options: PipelineCompilationOptions::default(),
            },
            fragment: Some(FragmentState {
                module: &fragment,
                entry_point: Some("fs_main"),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::default(),
                })],
                compilation_options: PipelineCompilationOptions::default(),
            }),
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil,
            multisample,
            cache: None,
            multiview_mask: None,
        })
    }
}
