//! 离屏画布 —— 不依赖 winit，可独立渲染。结果可贴回窗口。

use std::sync::Arc;

use crate::context::{DrawBatch, Renderer};
use crate::gpu::GpuContext;
use crate::texture::Texture;
use crate::window::{AntiAliasing, OffscreenIndex};

/// 离屏画布 — 与 VireoWindow 对等，但不依赖 winit
pub struct OffscreenCanvas {
    pub texture: Texture,
    renderer: Renderer,
    /// 该离屏画布初始化耗时（秒）：app.offscreen() 内的 AA 管线预热。
    pub init_duration: f64,
    /// 在 `App.offscreens` 中的索引
    pub(crate) index: OffscreenIndex,
}

impl OffscreenCanvas {
    pub fn new(gpu: &Arc<GpuContext>, width: u32, height: u32) -> Self {
        Self::with_aa(gpu, width, height, AntiAliasing::None, 0.0)
    }

    /// 构造时由 `App::offscreen()` 传入 AA 预热耗时；外部直接调用时通常传 0。
    pub fn with_aa(gpu: &Arc<GpuContext>, width: u32, height: u32, aa: AntiAliasing, init_duration: f64) -> Self {
        assert!(
            width > 0 && height > 0,
            "OffscreenCanvas dimensions must be non-zero, got {}x{}",
            width,
            height
        );
        let texture = Texture::new(&gpu.device, width, height, gpu.surface_format,
            &gpu.texture_bind_group_layout, &gpu.default_sampler);
        let renderer = Renderer::new(gpu.clone(), width, height, width, height, 1.0, aa, 1.0);
        Self { texture, renderer, init_duration, index: OffscreenIndex(0) }
    }

    /// 本画布在 `App.offscreens` 中的索引。
    pub fn index(&self) -> OffscreenIndex {
        self.index
    }

    /// 该离屏画布初始化耗时（秒）。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    /// 渲染并提交
    pub fn draw(&self, clear_color: Option<crate::color::Color>, batches: &[&DrawBatch]) {
        let target = self.texture.target();
        let cmd_buf = target.draw(&self.renderer, clear_color, batches);
        self.renderer.gpu().queue.submit(Some(cmd_buf));
    }

    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数（渲染器真实统计）。
    pub fn last_draw_calls(&self) -> u32 {
        self.renderer.last_draw_calls()
    }

    /// 底层 GpuContext（供基准/诊断等待 GPU 排空）。
    pub fn gpu(&self) -> &crate::gpu::GpuContext {
        self.renderer.gpu()
    }

    /// 获取纹理视图（用于贴到窗口等）
    pub fn view(&self) -> &wgpu::TextureView {
        &self.texture.view
    }

    /// 获取纹理 bind group（用于贴回窗口）。
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.texture.bind_group
    }

    /// 像素回读（用于 GPU 回归测试断言）。
    ///
    /// 阻塞到当前 GPU 队列清空后从内部纹理拷回 RGBA8 字节。
    /// 调用方负责保证此前已 `draw` 过。
    pub fn read_pixels(&self) -> Vec<u8> {
        let gpu = self.renderer.gpu();
        let (w, h) = (self.texture.width, self.texture.height);
        let bytes_per_row = w * 4;
        let padded = (bytes_per_row + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
            & !(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1);
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("offscreen readback encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        gpu.queue.submit(Some(encoder.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), wgpu::BufferAsyncError>>();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        // 阻塞等 map 完成：loop 调 device.poll 直到 callback 触发
        loop {
            if rx.try_recv().is_ok() {
                break;
            }
            let _ = gpu.device.poll(wgpu::PollType::Poll);
        }
        let data = slice.get_mapped_range().expect("readback map").to_vec();
        buf.unmap();
        if padded == bytes_per_row {
            data
        } else {
            let mut out = Vec::with_capacity((bytes_per_row * h) as usize);
            for y in 0..h as usize {
                out.extend_from_slice(&data[y * padded as usize..y * padded as usize + bytes_per_row as usize]);
            }
            out
        }
    }

}
