use std::cell::RefCell;

use wgpu::util::DeviceExt;

pub use crate::gpu::Vertex;
use crate::gpu::GpuContext;

/// 渲染目标，持有用于 render pass 的 TextureView
pub struct RenderTarget {
    pub view: wgpu::TextureView,
}

impl RenderTarget {
    /// 从已有的 TextureView 创建
    pub fn from_texture_view(view: wgpu::TextureView) -> Self {
        Self { view }
    }

    /// 使用给定的 Renderer 渲染 batch 到此 target
    pub fn draw(&self, renderer: &Renderer, clear_color: Option<crate::color::Color>, batches: &[&DrawBatch]) {
        renderer.draw(self, clear_color, batches);
    }
}

/// 渲染器 —— 管理 vertex/index buffer 复用，提供统一的 render pass 执行
pub struct Renderer {
    gpu: std::sync::Arc<GpuContext>,
    camera_bind_group: wgpu::BindGroup,
    vertex_buf: RefCell<Option<wgpu::Buffer>>,
    vertex_cap: RefCell<u64>,
    index_buf: RefCell<Option<wgpu::Buffer>>,
    index_cap: RefCell<u64>,
    physical_width: u32,
    physical_height: u32,
    scale: f32,
}

impl Renderer {
    pub fn new(
        gpu: std::sync::Arc<GpuContext>,
        logical_width: u32,
        logical_height: u32,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
    ) -> Self {
        let proj = glam::camera::rh::proj::opengl::orthographic(0.0, logical_width as f32, logical_height as f32, 0.0, -1.0, 1.0);
        let camera_data: [[f32; 4]; 4] = proj.to_cols_array_2d();
        let camera_buf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera buffer"),
            contents: bytemuck::cast_slice(&camera_data),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let camera_bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &gpu.camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });
        Self {
            gpu,
            camera_bind_group,
            vertex_buf: RefCell::new(None),
            vertex_cap: RefCell::new(0),
            index_buf: RefCell::new(None),
            index_cap: RefCell::new(0),
            physical_width,
            physical_height,
            scale,
        }
    }

    /// 更新相机投影（窗口 resize 时调用）
    pub fn resize(&mut self, logical_width: u32, logical_height: u32, physical_width: u32, physical_height: u32, scale: f32) {
        let proj = glam::camera::rh::proj::opengl::orthographic(0.0, logical_width as f32, logical_height as f32, 0.0, -1.0, 1.0);
        let camera_data: [[f32; 4]; 4] = proj.to_cols_array_2d();
        let camera_bg = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &self.gpu.camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("camera buffer"),
                    contents: bytemuck::cast_slice(&camera_data),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                }).as_entire_binding(),
            }],
        });
        self.camera_bind_group = camera_bg;
        self.physical_width = physical_width;
        self.physical_height = physical_height;
        self.scale = scale;
    }

    /// 渲染并提交
    pub fn draw(
        &self,
        target: &RenderTarget,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) {
        let has_content = batches.iter().any(|b| {
            !b.vertices.is_empty() || !b.texts.entries.is_empty() || clear_color.is_some()
        });
        if !has_content {
            return;
        }

        let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vireo encoder"),
        });

        let load = match clear_color {
            Some(c) => wgpu::LoadOp::Clear(wgpu::Color {
                r: c.r as f64,
                g: c.g as f64,
                b: c.b as f64,
                a: c.a as f64,
            }),
            None => wgpu::LoadOp::Load,
        };

        let target_view = &target.view;

        // ---- 在 pass 外写入所有 batch 的 vertex/index 数据 ----
        // 每个 batch 的数据连续排列，记录每个 batch 的 offset
        struct ShapeInfo {
            base_vertex: i32,
            ndx_start: u32,
            ndx_count: u32,
            texture: Option<wgpu::BindGroup>,
        }

        struct BatchInfo {
            shape: Option<ShapeInfo>,
            text: Option<(u32, u32)>, // (vertex_start, vertex_count)
        }

        let mut batch_infos: Vec<BatchInfo> = Vec::new();
        let mut vertex_count: u32 = 0;
        let mut ndx_accum: u32 = 0;

        for batch in batches {
            let shape = if !batch.vertices.is_empty() {
                let vdata: &[u8] = bytemuck::cast_slice(&batch.vertices);
                let idata: &[u8] = bytemuck::cast_slice(&batch.indices);

                let total_vbytes = (vertex_count as usize * std::mem::size_of::<Vertex>()) as u64 + vdata.len() as u64;
                self.ensure_vertex_buffer(total_vbytes);
                {
                    let vbuf = self.vertex_buf.borrow();
                    self.gpu.queue.write_buffer(vbuf.as_ref().unwrap(), vertex_count as u64 * std::mem::size_of::<Vertex>() as u64, vdata);
                }

                let total_ibytes = ndx_accum as u64 * 4 + idata.len() as u64;
                self.ensure_index_buffer(total_ibytes);
                {
                    let ibuf = self.index_buf.borrow();
                    self.gpu.queue.write_buffer(ibuf.as_ref().unwrap(), ndx_accum as u64 * 4, idata);
                }

                let info = ShapeInfo {
                    base_vertex: vertex_count as i32,
                    ndx_start: ndx_accum,
                    ndx_count: batch.indices.len() as u32,
                    texture: batch.texture.clone(),
                };
                vertex_count += batch.vertices.len() as u32;
                ndx_accum += batch.indices.len() as u32;
                Some(info)
            } else {
                None
            };

            batch_infos.push(BatchInfo { shape, text: None });
        }

        // ---- 准备所有文本 ----
        // Clear 颜色代表新帧开始，此时清空旧的 glyph 数据；Load 是增量叠加，保留。
        if clear_color.is_some() {
            let mut text_ctx = self.gpu.text_ctx.borrow_mut();
            text_ctx.text_renderer.clear();
        }
        for (i, batch) in batches.iter().enumerate() {
            if !batch.texts.entries.is_empty() {
                let (start, count) = batch.texts.prepare_texts(&self.gpu, self.physical_width, self.physical_height, self.scale);
                batch_infos[i].text = Some((start, count));
            }
        }

        // ---- 单 pass：按 batch 顺序穿插 shapes → texts ----
        let has_any_content = batch_infos.iter().any(|b| b.shape.is_some() || b.text.is_some());
        if has_any_content {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vireo render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                ..Default::default()
            });

            let vbuf = self.vertex_buf.borrow();
            let ibuf = self.index_buf.borrow();
            let text_ctx = self.gpu.text_ctx.borrow();

            for info in &batch_infos {
                // 先画本 batch 的形状（重新设置 pipeline——glyphon 的 render_range 会改状态）
                if let Some(ref shape) = info.shape {
                    pass.set_pipeline(&self.gpu.render_pipeline);
                    pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    pass.set_vertex_buffer(0, vbuf.as_ref().unwrap().slice(..));
                    pass.set_index_buffer(ibuf.as_ref().unwrap().slice(..), wgpu::IndexFormat::Uint32);
                    if let Some(ref tex_bg) = shape.texture {
                        pass.set_bind_group(1, tex_bg, &[]);
                    } else {
                        pass.set_bind_group(1, self.gpu.white_bind_group.as_ref(), &[]);
                    }
                    pass.draw_indexed(
                        shape.ndx_start..shape.ndx_start + shape.ndx_count,
                        shape.base_vertex,
                        0..1,
                    );
                }

                // 再画本 batch 的文本（glyphon 有自己的 pipeline/bind groups）
                if let Some((start, count)) = info.text {
                    let _ = text_ctx.text_renderer.render_range(
                        &text_ctx.text_atlas,
                        &text_ctx.viewport,
                        &mut pass,
                        start,
                        count,
                    );
                }
            }
        }

        self.gpu.queue.submit([encoder.finish()]);
    }

    fn ensure_vertex_buffer(&self, size: u64) {
        if size == 0 { return; }
        let needs_create = {
            let buf = self.vertex_buf.borrow();
            buf.is_none() || *self.vertex_cap.borrow() < size
        };
        if needs_create {
            let new_cap = match &*self.vertex_buf.borrow() {
                None => size.next_power_of_two(),
                Some(_) => (*self.vertex_cap.borrow() * 2).max(size),
            };
            let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vertex buffer"),
                size: new_cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            *self.vertex_buf.borrow_mut() = Some(buf);
            *self.vertex_cap.borrow_mut() = new_cap;
        }
    }

    fn ensure_index_buffer(&self, size: u64) {
        if size == 0 { return; }
        let needs_create = {
            let buf = self.index_buf.borrow();
            buf.is_none() || *self.index_cap.borrow() < size
        };
        if needs_create {
            let new_cap = match &*self.index_buf.borrow() {
                None => size.next_power_of_two(),
                Some(_) => (*self.index_cap.borrow() * 2).max(size),
            };
            let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("index buffer"),
                size: new_cap,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            *self.index_buf.borrow_mut() = Some(buf);
            *self.index_cap.borrow_mut() = new_cap;
        }
    }
}

use crate::text::TextEntryList;

pub struct DrawBatch {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub texts: TextEntryList,
    pub texture: Option<wgpu::BindGroup>,
}

impl DrawBatch {
    pub fn new() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            texts: TextEntryList::new(),
            texture: None,
        }
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.texts.clear();
        self.texture = None;
    }

    /// 克隆 batch（vertices、indices、texts 完全复制，rasterizer 清空）
    pub fn clone_batch(&self) -> Self {
        Self {
            vertices: self.vertices.clone(),
            indices: self.indices.clone(),
            texture: self.texture.clone(),
            texts: TextEntryList::new_from_entries(&self.texts),
        }
    }

    /// 设置此帧批处理的纹理
    pub fn set_texture(&mut self, texture: &crate::texture::Texture) {
        self.texture = Some(texture.bind_group.clone());
    }

    pub fn add_quad(&mut self, x: f32, y: f32, w: f32, h: f32, color: crate::color::Color) {
        self.add_quad_uv(x, y, w, h, 0.0, 0.0, 1.0, 1.0, color);
    }

    pub fn add_quad_uv(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        u0: f32,
        v0: f32,
        u1: f32,
        v1: f32,
        color: crate::color::Color,
    ) {
        let base = self.vertices.len() as u32;
        let x2 = x + w;
        let y2 = y + h;

        self.vertices.push(Vertex::new_uv(x, y, u0, v0, color));
        self.vertices.push(Vertex::new_uv(x2, y, u1, v0, color));
        self.vertices.push(Vertex::new_uv(x2, y2, u1, v1, color));
        self.vertices.push(Vertex::new_uv(x, y2, u0, v1, color));

        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}
