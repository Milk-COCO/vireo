//! 纹理：可从文件加载，也可作为离屏渲染目标。

use crate::gpu::GpuContext;

/// 纹理——可从文件/RGBA 加载，也可创建为离屏渲染目标。
///
/// 贴图用 `draw_texture()`；离屏用 `target()` 获取 `RenderTarget`。
pub struct Texture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub bind_group: wgpu::BindGroup,
    pub width: u32,
    pub height: u32,
}

impl Texture {
    // ---- 离屏渲染目标构造 ----

    /// 创建可渲染的纹理（离屏渲染目标）。
    pub fn new(
        device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat,
        bg_layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vireo texture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("texture bind group"),
            layout: bg_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        });
        Self { texture, view, bind_group, width, height }
    }

    /// 获取 RenderTarget 用于离屏渲染。
    pub fn target(&self) -> crate::context::RenderTarget {
        crate::context::RenderTarget::from_texture_view(self.view.clone())
    }

    // ---- 从文件/RGBA 加载 ----

    /// 从文件加载纹理（自动识别 png/jpg/bmp）。
    pub fn from_file(path: impl AsRef<std::path::Path>, gpu: &GpuContext) -> Result<Self, String> {
        let data = std::fs::read(path.as_ref()).map_err(|e| format!("failed to read file: {}", e))?;
        Self::from_bytes(&data, gpu)
    }

    /// 从内存字节加载纹理（自动识别 png/jpg/bmp）。
    pub fn from_bytes(data: &[u8], gpu: &GpuContext) -> Result<Self, String> {
        let img = image::load_from_memory(data).map_err(|e| format!("failed to decode image: {}", e))?;
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Self::from_rgba(width, height, &rgba, gpu))
    }

    /// 从 RGBA 像素数据创建纹理。
    pub fn from_rgba(width: u32, height: u32, pixels: &[u8], gpu: &GpuContext) -> Self {
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vireo texture"),
            size, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            pixels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * width), rows_per_image: Some(height) },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("texture bind group"),
            layout: &gpu.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.default_sampler) },
            ],
        });
        Self { texture, view, bind_group, width, height }
    }

    /// 像素区域转换为归一化 UV 坐标 (u0, v0, u1, v1)。
    pub fn uv(&self, px: u32, py: u32, pw: u32, ph: u32) -> (f32, f32, f32, f32) {
        let w = self.width as f32;
        let h = self.height as f32;
        (px as f32 / w, py as f32 / h, (px + pw) as f32 / w, (py + ph) as f32 / h)
    }
}
