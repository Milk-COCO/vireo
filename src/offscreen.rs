//! 离屏画布 —— 不依赖 winit，可独立渲染。结果可贴回窗口。

use std::sync::Arc;

use crate::context::{DrawBatch, Renderer};
use crate::gpu::GpuContext;
use crate::texture::Texture;

/// 离屏画布 — 与 VireoWindow 对等，但不依赖 winit
pub struct OffscreenCanvas {
    pub texture: Texture,
    renderer: Renderer,
}

impl OffscreenCanvas {
    pub fn new(gpu: &Arc<GpuContext>, width: u32, height: u32) -> Self {
        let texture = Texture::new(&gpu.device, width, height, gpu.surface_format,
            &gpu.texture_bind_group_layout, &gpu.default_sampler);
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

    /// 获取纹理 bind group（用于贴回窗口）。
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.texture.bind_group
    }

}
