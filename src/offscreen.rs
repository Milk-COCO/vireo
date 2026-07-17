use std::sync::Arc;

use crate::context::{DrawBatch, Renderer};
use crate::gpu::GpuContext;
use crate::texture::RenderTexture;

/// 离屏画布 — 与 VireoWindow 对等，但不依赖 winit
pub struct OffscreenCanvas {
    pub texture: RenderTexture,
    renderer: Renderer,
}

impl OffscreenCanvas {
    pub fn new(gpu: &Arc<GpuContext>, width: u32, height: u32) -> Self {
        let texture = RenderTexture::new(&gpu.device, width, height, gpu.surface_format);
        let renderer = Renderer::new(gpu.clone(), width, height, width, height, 1.0);
        Self { texture, renderer }
    }

    /// 渲染并提交
    pub fn draw(&self, clear_color: Option<crate::color::Color>, batches: &[&DrawBatch]) {
        let target = self.texture.target();
        target.draw(&self.renderer, clear_color, batches);
    }

    /// 获取纹理视图（用于贴到窗口等）
    pub fn view(&self) -> &wgpu::TextureView {
        &self.texture.view
    }

    /// 获取纹理的 bind group
    pub fn bind_group(&self, gpu: &GpuContext) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("offscreen bind group"),
            layout: &gpu.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gpu.default_sampler),
                },
            ],
        })
    }
}
