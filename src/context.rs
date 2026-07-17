//! 渲染核心：批量绘制、渲染目标和渲染器。

use std::cell::RefCell;

use wgpu::util::DeviceExt;

pub use crate::gpu::Vertex;
use crate::gpu::GpuContext;

/// 渲染目标，封装用于 render pass 的 `TextureView`。
///
/// 窗口和离屏纹理都通过此类型提交绘制。
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

/// 渲染器 —— 管理 vertex/index buffer 复用，执行单 pass 渲染。
///
/// 内部维护 GPU buffer，支持在多 batch 间以偏移量追加写入。
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
        struct ShapeSegment {
            ndx_start: u32,
            ndx_count: u32,
            bind_group: wgpu::BindGroup,
        }

        struct ShapeInfo {
            base_vertex: i32,
            segments: Vec<ShapeSegment>,
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

                // 构建 texture segments（相对于 batch 的偏移量转为全局偏移量）
                let segs: Vec<ShapeSegment> = if batch.texture_segments.is_empty() {
                    // 没有 segment：整批用 batch.texture（或 white）
                    let bg = batch.texture.clone().unwrap_or_else(|| self.gpu.white_bind_group.as_ref().clone());
                    vec![ShapeSegment { ndx_start: ndx_accum, ndx_count: batch.indices.len() as u32, bind_group: bg }]
                } else {
                    batch.texture_segments.iter().map(|s| ShapeSegment {
                        ndx_start: ndx_accum + s.ndx_start,
                        ndx_count: s.ndx_count,
                        bind_group: s.bind_group.clone(),
                    }).collect()
                };

                let info = ShapeInfo {
                    base_vertex: vertex_count as i32,
                    segments: segs,
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
                    for seg in &shape.segments {
                        pass.set_bind_group(1, &seg.bind_group, &[]);
                        pass.draw_indexed(
                            seg.ndx_start..seg.ndx_start + seg.ndx_count,
                            shape.base_vertex,
                            0..1,
                        );
                    }
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

/// 形状变换。内部使用，通过 `set_position` / `set_rotation` / `set_scale` / `set_pivot` 设置。
#[derive(Clone, Copy)]
struct Transform {
    x: f32,
    y: f32,
    px: f32,
    py: f32,
    rotation: f32,
    sx: f32,
    sy: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, px: 0.0, py: 0.0, rotation: 0.0, sx: 1.0, sy: 1.0 }
    }
}

/// 批量绘制单元 —— 容纳一组形状顶点、文本条目和可选纹理。
///
/// 每帧创建、填充后交给 `VireoWindow::draw()` 或 `OffscreenCanvas::draw()`。
/// 多个 batch 按提交顺序叠加，后面的覆盖前面的。
#[derive(Clone)]
struct TextureSegment {
    ndx_start: u32,
    ndx_count: u32,
    bind_group: wgpu::BindGroup,
}

pub struct DrawBatch {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub texts: TextEntryList,
    pub(crate) texture: Option<wgpu::BindGroup>,
    texture_segments: Vec<TextureSegment>,
    transform: Option<Transform>,
}

impl DrawBatch {
    pub fn new() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
            texts: TextEntryList::new(),
            texture: None,
            texture_segments: Vec::new(),
            transform: None,
        }
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.texts.clear();
        self.texture = None;
        self.texture_segments.clear();
        self.transform = None;
    }

    /// 设置平移（屏幕坐标）。
    pub fn set_position(&mut self, x: f32, y: f32) {
        let mut t = self.transform.unwrap_or_default();
        t.x = x;
        t.y = y;
        self.transform = Some(t);
    }

    /// 设置旋转弧度（顺时针）。默认绕 (0,0)，用 `set_pivot` 指定旋转中心。
    pub fn set_rotation(&mut self, rad: f32) {
        let mut t = self.transform.unwrap_or_default();
        t.rotation = rad;
        self.transform = Some(t);
    }

    /// 设置旋转中心（形状局部坐标）。
    pub fn set_pivot(&mut self, px: f32, py: f32) {
        let mut t = self.transform.unwrap_or_default();
        t.px = px;
        t.py = py;
        self.transform = Some(t);
    }

    /// 设置缩放（1.0 = 原始大小）。
    pub fn set_scale(&mut self, sx: f32, sy: f32) {
        let mut t = self.transform.unwrap_or_default();
        t.sx = sx;
        t.sy = sy;
        self.transform = Some(t);
    }

    /// 一次性设置完整变换。
    pub fn set_transform(&mut self, x: f32, y: f32, px: f32, py: f32, rotation: f32, sx: f32, sy: f32) {
        self.transform = Some(Transform { x, y, px, py, rotation, sx, sy });
    }

    /// 公转变换：绕轨道中心 `(cx, cy)` 的圆周上运动，同时绕自身 pivot `(px, py)` 自转。
    pub fn orbit_transform(
        &mut self, cx: f32, cy: f32, orbit_radius: f32, orbit_angle: f32,
        px: f32, py: f32, self_rotation: f32, sx: f32, sy: f32,
    ) {
        let x = cx + orbit_angle.cos() * orbit_radius;
        let y = cy + orbit_angle.sin() * orbit_radius;
        self.set_transform(x, y, px, py, self_rotation, sx, sy);
    }

    /// 清除变换，后续形状以原始坐标绘制。
    pub fn clear_transform(&mut self) {
        self.transform = None;
    }

    /// 添加单个顶点（自动应用当前 transform）。
    pub fn push_vertex(&mut self, x: f32, y: f32, color: crate::color::Color) {
        let (tx, ty) = self.transform_vertex(x, y);
        self.vertices.push(Vertex::new(tx, ty, color));
    }

    /// 添加带 UV 的顶点（自动应用当前 transform）。
    pub fn push_vertex_uv(&mut self, x: f32, y: f32, u: f32, v: f32, color: crate::color::Color) {
        let (tx, ty) = self.transform_vertex(x, y);
        self.vertices.push(Vertex::new_uv(tx, ty, u, v, color));
    }

    /// 应用当前变换到顶点：绕 pivot 旋转 → 缩放 → 平移
    fn transform_vertex(&self, vx: f32, vy: f32) -> (f32, f32) {
        let t = match self.transform {
            Some(t) => t,
            None => return (vx, vy),
        };
        let cos = t.rotation.cos();
        let sin = t.rotation.sin();
        let dx = vx - t.px;
        let dy = vy - t.py;
        let rx = dx * cos - dy * sin;
        let ry = dx * sin + dy * cos;
        (t.x + rx * t.sx, t.y + ry * t.sy)
    }

    /// 克隆 batch（vertices、indices、texts 完全复制，rasterizer 清空）
    pub fn clone_batch(&self) -> Self {
        Self {
            vertices: self.vertices.clone(),
            indices: self.indices.clone(),
            texture: self.texture.clone(),
            texture_segments: self.texture_segments.clone(),
            texts: TextEntryList::new_from_entries(&self.texts),
            transform: self.transform,
        }
    }

    /// 直接设置 bind group（高级用法，如离屏画布贴回窗口）。
    pub fn set_bind_group(&mut self, bg: wgpu::BindGroup) { self.texture = Some(bg); }

    /// 绑定纹理到整个 batch（draw_texture 内部也使用此方法记录当前纹理）。
    pub fn set_texture(&mut self, texture: &crate::texture::Texture) {
        self.texture = Some(texture.bind_group.clone());
    }

    /// 记录纹理段（由 draw_texture 调用）：自上次段以来的所有新顶点归入此 bind group。
    pub(crate) fn add_texture_segment(&mut self, bg: wgpu::BindGroup) {
        let start = self.texture_segments.last().map_or(0, |s| s.ndx_start + s.ndx_count);
        let end = self.indices.len() as u32;
        if end > start {
            self.texture_segments.push(TextureSegment { ndx_start: start, ndx_count: end - start, bind_group: bg });
        }
    }
}
