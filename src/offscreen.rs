//! 离屏画布 —— 不依赖 winit，可独立渲染。结果可贴回窗口。

use std::sync::Arc;

use parking_lot::Mutex;

use crate::gpu::GpuContext;
use crate::render::{ColorTargetDesc, DepthStencilTarget, DrawBatch, RenderTarget, Renderer};
use crate::texture::Texture;
use crate::window::{AntiAliasing, OffscreenIndex};

/// 离屏画布 — 与 VireoWindow 对等，但不依赖 winit
pub struct OffscreenCanvas {
    pub texture: Mutex<Texture>,
    renderer: Renderer,
    /// 该离屏画布初始化耗时（秒）：app.offscreen() 内的 AA 管线预热。
    pub init_duration: f64,
    /// 在 `App.offscreens` 中的索引
    pub(crate) index: OffscreenIndex,
}

impl OffscreenCanvas {
    /// 创建一个不带抗锯齿的离屏画布。
    pub fn new(gpu: &Arc<GpuContext>, width: u32, height: u32) -> Self {
        Self::with_aa(gpu, width, height, AntiAliasing::None, 0.0)
    }

    /// 构造时由 `App::offscreen()` 传入 AA 预热耗时；外部直接调用时通常传 0。
    pub fn with_aa(
        gpu: &Arc<GpuContext>,
        width: u32,
        height: u32,
        aa: AntiAliasing,
        init_duration: f64,
    ) -> Self {
        assert!(
            width > 0 && height > 0,
            "OffscreenCanvas dimensions must be non-zero, got {}x{}",
            width,
            height
        );
        let texture = Texture::new(
            &gpu.device,
            width,
            height,
            gpu.surface_format(),
            &gpu.texture_bind_group_layout,
            &gpu.default_sampler,
        );
        let renderer = Renderer::new(
            gpu.clone(),
            width as f32,
            height as f32,
            width,
            height,
            1.0,
            aa,
            1.0,
        );
        Self {
            texture: Mutex::new(texture),
            renderer,
            init_duration,
            index: OffscreenIndex(0),
        }
    }

    /// 本画布在 `App.offscreens` 中的索引。
    pub fn index(&self) -> OffscreenIndex {
        self.index
    }

    /// 该离屏画布初始化耗时（秒）。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    /// 渲染并提交。
    /// 若创建后 `GpuContext::surface_format` 漂移（如 GLES 首帧由默认 sRGB
    /// 翻为线性格式），先按当前格式重建目标纹理再画，调用方无感。
    pub fn draw(&self, clear_color: Option<crate::color::Color>, batches: &[&DrawBatch]) {
        self.ensure_target_format();
        let target = self.texture.lock().target();
        let cmd_buf = target.draw(&self.renderer, &[clear_color], batches);
        self.renderer.gpu().queue.submit(Some(cmd_buf));
    }

    /// 渲染并提交，并在引擎 pass **之后**用同一个 `CommandEncoder` 追加自定义 pass。
    ///
    /// 回调在引擎 pass 结束之后、`submit` 之前执行；整帧只提交一次。
    /// `target` 的第 0 个颜色附件是本画布纹理；view 只在回调期间有效。
    /// 需要 depth-stencil 时由回调自建（引擎 DS 在引擎 pass 结束时已结束）。
    pub fn draw_with_passes<F>(
        &self,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
        mut extra: F,
    ) where
        F: FnMut(&mut wgpu::CommandEncoder, &RenderTarget),
    {
        self.ensure_target_format();
        let target = self.texture.lock().target();
        let mut encoder =
            self.renderer
                .gpu()
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("vireo offscreen encoder"),
                });
        self.renderer
            .draw_into(&mut encoder, &target, &[clear_color], batches);
        extra(&mut encoder, &target);
        self.renderer.gpu().queue.submit(Some(encoder.finish()));
    }

    /// 渲染并提交，在**引擎 pass 内部**、引擎批次之后追加自定义绘制。
    ///
    /// 回调拿到引擎配置好的 pass（颜色 / MSAA resolve / DS 一致）；整帧只提交一次。
    /// 回调里不要再次调用本画布的 `draw*`（会嵌套 prepare）。
    pub fn draw_with_inline<F>(
        &self,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
        hook: F,
    ) where
        F: FnOnce(&mut wgpu::RenderPass),
    {
        self.ensure_target_format();
        let target = self.texture.lock().target();
        let mut encoder =
            self.renderer
                .gpu()
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("vireo offscreen inline encoder"),
                });
        let frame = self
            .renderer
            .prepare_frame(&target, &[clear_color], batches);
        if frame.wants_pass() {
            let mut pass = frame.begin_engine_pass(&mut encoder);
            frame.encode(&mut pass);
            hook(&mut pass);
        }
        drop(frame);
        self.renderer.gpu().queue.submit(Some(encoder.finish()));
    }

    /// 渲染并提交，挂载本帧 depth-stencil 附件（`None` = 与 `draw` 一致）。
    pub fn draw_with_depth_stencil(
        &self,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
        ds: Option<DepthStencilTarget>,
    ) {
        self.ensure_target_format();
        let mut target = self.texture.lock().target();
        target.set_depth_stencil(ds);
        let cmd_buf = target.draw(&self.renderer, &[clear_color], batches);
        self.renderer.gpu().queue.submit(Some(cmd_buf));
    }

    /// 渲染并提交到多个颜色目标（MRT）。
    ///
    /// 第 0 个目标是本画布自身纹理（`clear_color` 清屏）；`extra_targets` 为追加的
    /// 颜色附件（load 固定 `LoadOp::Load`，blend 取 `ColorTargetDesc::blend`）。
    /// 只有 custom material（fragment shader 声明多个输出）能写多目标。
    pub fn draw_mrt(
        &self,
        clear_color: Option<crate::color::Color>,
        extra_targets: &[ColorTargetDesc],
        batches: &[&DrawBatch],
    ) {
        self.ensure_target_format();
        let prim = self.texture.lock().target();
        let mut atts = prim.color_attachments;
        atts.extend(extra_targets.iter().map(|t| ColorTargetDesc {
            view: t.view.clone(),
            resolve_target: t.resolve_target.clone(),
            blend: t.blend,
        }));
        let mut clears = vec![clear_color];
        clears.resize(atts.len(), None);
        let target = RenderTarget::from_color_attachments(atts);
        let cmd_buf = target.draw(&self.renderer, &clears, batches);
        self.renderer.gpu().queue.submit(Some(cmd_buf));
    }

    /// 目标纹理格式与当前 `surface_format` 不一致时重建（GLES 首帧 sRGB→线性）。
    fn ensure_target_format(&self) {
        let gpu = self.renderer.gpu();
        let format = gpu.surface_format();
        let tex = self.texture.lock();
        if tex.format == format {
            return;
        }
        drop(tex);
        let mut fresh = self.texture.lock();
        // 双重检查：等待锁期间可能已被别的 draw 重建。
        if fresh.format != format {
            let (w, h) = (fresh.width, fresh.height);
            log::warn!(
                "offscreen: surface format drifted ({:?} -> {:?}), recreating target",
                fresh.format,
                format
            );
            *fresh = Texture::new(
                &gpu.device,
                w,
                h,
                format,
                &gpu.texture_bind_group_layout,
                &gpu.default_sampler,
            );
            self.renderer.clear_msaa_ds();
        }
    }

    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数（渲染器真实统计）。
    pub fn last_draw_calls(&self) -> u32 {
        self.renderer.last_draw_calls()
    }

    /// 上一帧参与绘制的 batch（扁平化顺序）实际使用的裁剪路径（诊断）。
    pub fn last_clip_paths(&self) -> Vec<crate::render::ClipPath> {
        self.renderer.last_clip_paths()
    }

    /// 底层 GpuContext（供基准/诊断等待 GPU 排空）。
    pub fn gpu(&self) -> &crate::gpu::GpuContext {
        self.renderer.gpu()
    }

    /// 获取纹理视图（用于贴到窗口等）。
    pub fn view(&self) -> wgpu::TextureView {
        self.texture.lock().view.clone()
    }

    /// 获取纹理 bind group（用于贴回窗口）。
    pub fn bind_group(&self) -> wgpu::BindGroup {
        self.texture.lock().bind_group.clone()
    }

    /// 像素回读（用于 GPU 回归测试断言）。
    ///
    /// 阻塞到当前 GPU 队列清空后从内部纹理拷回 RGBA8 字节。
    /// 调用方负责保证此前已 `draw` 过。
    pub fn read_pixels(&self) -> Vec<u8> {
        let gpu = self.renderer.gpu();
        let tex = self.texture.lock();
        let (w, h) = (tex.width, tex.height);
        let bytes_per_row = w * 4;
        let padded = (bytes_per_row + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
            & !(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1);
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("offscreen readback encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
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
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit(Some(encoder.finish()));
        let slice = buf.slice(..);
        let (tx, _rx) = std::sync::mpsc::channel::<Result<(), wgpu::BufferAsyncError>>();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        // 阻塞等 map 完成：Wait 阻塞直到回调触发，无需轮询
        let _ = gpu.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let data = slice.get_mapped_range().expect("readback map").to_vec();
        buf.unmap();
        if padded == bytes_per_row {
            data
        } else {
            let mut out = Vec::with_capacity((bytes_per_row * h) as usize);
            for y in 0..h as usize {
                out.extend_from_slice(
                    &data[y * padded as usize..y * padded as usize + bytes_per_row as usize],
                );
            }
            out
        }
    }
}
