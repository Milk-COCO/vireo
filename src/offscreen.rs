//! 离屏画布 —— 不依赖 winit，可独立渲染。结果可贴回窗口。

use std::sync::Arc;

use crate::context::{DrawBatch, Renderer};
use crate::gpu::GpuContext;
use crate::texture::Texture;
use crate::window::AntiAliasing;

/// 离屏画布 — 与 VireoWindow 对等，但不依赖 winit
pub struct OffscreenCanvas {
    pub texture: Texture,
    renderer: Renderer,
    /// 该离屏画布初始化耗时（秒）：app.offscreen() 内的 AA 管线预热。
    pub init_duration: f64,
}

impl OffscreenCanvas {
    pub fn new(gpu: &Arc<GpuContext>, width: u32, height: u32) -> Self {
        Self::with_aa(gpu, width, height, AntiAliasing::None, 0.0)
    }

    /// 构造时由 `App::offscreen()` 传入 AA 预热耗时；外部直接调用时通常传 0。
    pub fn with_aa(gpu: &Arc<GpuContext>, width: u32, height: u32, aa: AntiAliasing, init_duration: f64) -> Self {
        let texture = Texture::new(&gpu.device, width, height, gpu.surface_format,
            &gpu.texture_bind_group_layout, &gpu.default_sampler);
        let renderer = Renderer::new(gpu.clone(), width, height, width, height, 1.0, aa, 1.0);
        Self { texture, renderer, init_duration }
    }

    /// 该离屏画布初始化耗时（秒）。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
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
