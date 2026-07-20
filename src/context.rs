//! 渲染核心：批量绘制、渲染目标和渲染器。

use std::cell::RefCell;
use rustc_hash::FxHashMap;

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
    camera_buf: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    vertex_buf: RefCell<Option<wgpu::Buffer>>,
    vertex_cap: RefCell<u64>,
    index_buf: RefCell<Option<wgpu::Buffer>>,
    index_cap: RefCell<u64>,
    physical_width: u32,
    physical_height: u32,
    scale: f32,
    dpi_scale: f32,
    sample_count: u32,
    alpha_to_coverage: bool,
    ssaa: bool,
    msaa_tex: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
    polygon_edge_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    polygon_bind_group_cache: RefCell<Option<wgpu::BindGroup>>,
    transform_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    transform_bind_group_cache: RefCell<Option<wgpu::BindGroup>>,
    /// 帧间复用的 CPU 暂存，避免每帧大块分配
    scratch_vdata: RefCell<Vec<u8>>,
    scratch_idata: RefCell<Vec<u8>>,
    scratch_transforms: RefCell<Vec<f32>>,
    scratch_poly_edges: RefCell<Vec<f32>>,
}

impl Renderer {
    pub fn new(
        gpu: std::sync::Arc<GpuContext>,
        logical_width: u32,
        logical_height: u32,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
        aa: crate::window::AntiAliasing,
        dpi_scale: f32,
    ) -> Self {
        let proj = glam::camera::rh::proj::opengl::orthographic(0.0, logical_width as f32, logical_height as f32, 0.0, -1.0, 1.0);
        let camera_data: [[f32; 4]; 4] = proj.to_cols_array_2d();
        let mut camera_raw = [0u8; 80];
        camera_raw[..64].copy_from_slice(bytemuck::cast_slice(&camera_data));
        camera_raw[64..68].copy_from_slice(&dpi_scale.to_le_bytes());
        let camera_buf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera buffer"),
            contents: &camera_raw,
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
            camera_buf,
            camera_bind_group,
            vertex_buf: RefCell::new(None),
            vertex_cap: RefCell::new(0),
            index_buf: RefCell::new(None),
            index_cap: RefCell::new(0),
            physical_width,
            physical_height,
            scale,
            dpi_scale,
            sample_count: aa.sample_count(),
            alpha_to_coverage: aa.alpha_to_coverage(),
            ssaa: aa.is_ssaa(),
            msaa_tex: RefCell::new(None),
            polygon_edge_buf: RefCell::new(None),
            polygon_bind_group_cache: RefCell::new(None),
            transform_buf: RefCell::new(None),
            transform_bind_group_cache: RefCell::new(None),
            scratch_vdata: RefCell::new(Vec::new()),
            scratch_idata: RefCell::new(Vec::new()),
            scratch_transforms: RefCell::new(Vec::new()),
            scratch_poly_edges: RefCell::new(Vec::new()),
        }
    }

    /// 更新抗锯齿设置。
    pub fn update_aa(&mut self, aa: crate::window::AntiAliasing) {
        self.sample_count = aa.sample_count();
        self.alpha_to_coverage = aa.alpha_to_coverage();
        self.ssaa = aa.is_ssaa();
        *self.msaa_tex.borrow_mut() = None;
    }

    /// 获取匹配当前 sample_count 的 pipeline

    /// 获取 multisampled 视图（必要时创建），无 MSAA 返回 None
    fn msaa_view(&self, format: wgpu::TextureFormat) -> Option<wgpu::TextureView> {
        if self.sample_count <= 1 { return None; }
        let mut mt = self.msaa_tex.borrow_mut();
        if mt.is_none() || mt.as_ref().unwrap().0.width() != self.physical_width {
            let tex = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("msaa"),
                size: wgpu::Extent3d { width: self.physical_width, height: self.physical_height, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: self.sample_count,
                dimension: wgpu::TextureDimension::D2, format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            *mt = Some((tex, view));
        }
        Some(mt.as_ref().unwrap().1.clone())
    }

    /// 更新相机投影（窗口 resize 时调用）
    pub fn resize(&mut self, logical_width: u32, logical_height: u32, physical_width: u32, physical_height: u32, scale: f32) {
        let proj = glam::camera::rh::proj::opengl::orthographic(0.0, logical_width as f32, logical_height as f32, 0.0, -1.0, 1.0);
        let camera_data: [[f32; 4]; 4] = proj.to_cols_array_2d();
        let mut camera_raw = [0u8; 80];
        camera_raw[..64].copy_from_slice(bytemuck::cast_slice(&camera_data));
        camera_raw[64..68].copy_from_slice(&self.dpi_scale.to_le_bytes());
        self.gpu.queue.write_buffer(&self.camera_buf, 0, &camera_raw);
        self.physical_width = physical_width;
        self.physical_height = physical_height;
        self.scale = scale;
        *self.msaa_tex.borrow_mut() = None;
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
            geometry: bool,
        }

        struct BatchInfo {
            shape: Option<ShapeInfo>,
            text: Option<(u32, u32)>, // (vertex_start, vertex_count)
        }

        let mut batch_infos: Vec<BatchInfo> = Vec::with_capacity(batches.len());
        let mut vertex_count: u32 = 0;
        let mut ndx_accum: u32 = 0;

        // ---- 单次扫描：合并 transform/poly + 统计顶点数（复用 scratch）----
        let mut global_transforms = self.scratch_transforms.borrow_mut();
        global_transforms.clear();
        let mut polygon_edges_global = self.scratch_poly_edges.borrow_mut();
        polygon_edges_global.clear();
        let mut batch_transform_bases: Vec<u32> = Vec::with_capacity(batches.len());
        let mut batch_poly_base: Vec<u32> = Vec::with_capacity(batches.len());
        let mut total_vcount: u32 = 0;
        let mut total_icount: u32 = 0;
        let mut poly_offset: u32 = 0;
        for batch in batches {
            batch_transform_bases.push((global_transforms.len() / 12) as u32);
            global_transforms.extend_from_slice(&batch.transform_table);
            batch_poly_base.push(poly_offset);
            poly_offset += batch.polygon_edges.len() as u32 / 4;
            polygon_edges_global.extend_from_slice(&batch.polygon_edges);
            total_vcount += batch.vertices.len() as u32;
            total_icount += batch.indices.len() as u32;
        }

        let total_vbytes = total_vcount as u64 * std::mem::size_of::<Vertex>() as u64;
        let total_ibytes = total_icount as u64 * 4;
        self.ensure_vertex_buffer(total_vbytes);
        self.ensure_index_buffer(total_ibytes);
        let mut combined_vdata = self.scratch_vdata.borrow_mut();
        let mut combined_idata = self.scratch_idata.borrow_mut();
        combined_vdata.clear();
        combined_idata.clear();
        if combined_vdata.capacity() < total_vbytes as usize {
            combined_vdata.reserve(total_vbytes as usize);
        }
        if combined_idata.capacity() < total_ibytes as usize {
            combined_idata.reserve(total_ibytes as usize);
        }

        for (bi, batch) in batches.iter().enumerate() {
            let shape = if !batch.vertices.is_empty() {
                let transform_base = batch_transform_bases[bi];
                let poly_base = batch_poly_base[bi] as f32;
                let needs_patch = !batch.polygon_edges.is_empty() || transform_base > 0;

                let v_start = combined_vdata.len();
                combined_vdata.extend_from_slice(bytemuck::cast_slice(&batch.vertices));
                if needs_patch {
                    let has_poly = !batch.polygon_edges.is_empty();
                    let verts: &mut [Vertex] =
                        bytemuck::cast_slice_mut(&mut combined_vdata[v_start..]);
                    for v in verts.iter_mut() {
                        if transform_base > 0 {
                            v.transform_index += transform_base;
                        }
                        if has_poly && (v.sdf_type == 6 || v.sdf_type == 7) {
                            v.sdf_params[0] += poly_base;
                        }
                    }
                }
                combined_idata.extend_from_slice(bytemuck::cast_slice(&batch.indices));

                // 构建 texture segments（相对于 batch 的偏移量转为全局偏移量）
                let segs: Vec<ShapeSegment> = if batch.texture_segments.is_empty() {
                    let bg = batch.texture.clone().unwrap_or_else(|| self.gpu.white_bind_group.as_ref().clone());
                    vec![ShapeSegment { ndx_start: ndx_accum, ndx_count: batch.indices.len() as u32, bind_group: bg }]
                } else {
                    let mut v: Vec<ShapeSegment> = batch.texture_segments.iter().map(|s| ShapeSegment {
                        ndx_start: ndx_accum + s.ndx_start,
                        ndx_count: s.ndx_count,
                        bind_group: s.bind_group.clone(),
                    }).collect();
                    // 补 trailing segment：最后一段之后的新顶点用当前 batch.texture
                    let last_end = v.last().map(|s| s.ndx_start + s.ndx_count).unwrap_or(ndx_accum);
                    let total_end = ndx_accum + batch.indices.len() as u32;
                    if last_end < total_end {
                        let bg = batch.texture.clone().unwrap_or_else(|| self.gpu.white_bind_group.as_ref().clone());
                        v.push(ShapeSegment { ndx_start: last_end, ndx_count: total_end - last_end, bind_group: bg });
                    }
                    v
                };

                // has_sdf 在 push 时维护，避免每帧 O(V) 扫描
                let info = ShapeInfo {
                    base_vertex: vertex_count as i32,
                    segments: segs,
                    geometry: !batch.has_sdf && batch.sdf_feather.is_none(),
                };
                vertex_count += batch.vertices.len() as u32;
                ndx_accum += batch.indices.len() as u32;
                Some(info)
            } else {
                None
            };

            batch_infos.push(BatchInfo { shape, text: None });
        }

        // ---- 合并上传：顶点+索引各一次 write_buffer ----
        if !combined_vdata.is_empty() {
            let vbuf = self.vertex_buf.borrow();
            self.gpu.queue.write_buffer(vbuf.as_ref().unwrap(), 0, &combined_vdata);
        }
        if !combined_idata.is_empty() {
            let ibuf = self.index_buf.borrow();
            self.gpu.queue.write_buffer(ibuf.as_ref().unwrap(), 0, &combined_idata);
        }

        // ---- 上传多边形边数据到 storage buffer ----
        let polygon_bind_group: Option<wgpu::BindGroup> = if !polygon_edges_global.is_empty() {
            let size = (polygon_edges_global.len() * 4) as u64;
            self.ensure_polygon_edge_buffer(size);
            {
                let buf = self.polygon_edge_buf.borrow();
                let buf_ref = buf.as_ref().unwrap();
                self.gpu.queue.write_buffer(&buf_ref.0, 0, bytemuck::cast_slice(&polygon_edges_global));
            }
            // 复用缓存的 bind group，仅当 buffer 重建时重新创建
            let mut cache = self.polygon_bind_group_cache.borrow_mut();
            if cache.is_none() {
                let bg = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("polygon bind group"),
                    layout: &self.gpu.polygon_bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.polygon_edge_buf.borrow().as_ref().unwrap().0.as_entire_binding(),
                    }],
                });
                *cache = Some(bg);
            }
            cache.clone()
        } else {
            None
        };

        // ---- 准备所有文本（在 transform 合并之后，可能追加物理变换矩阵） ----
        // 确保 glyphon TextRenderer 匹配当前 MSAA sample_count
        self.gpu.text_ctx.borrow_mut().ensure_sample_count(&self.gpu.device, self.sample_count);
        // Clear 颜色代表新帧开始，此时清空旧的 glyph 数据；Load 是增量叠加，保留。
        if clear_color.is_some() {
            let mut text_ctx = self.gpu.text_ctx.borrow_mut();
            text_ctx.text_renderer.clear();
            text_ctx.advance_frame();
        }
        for (i, batch) in batches.iter().enumerate() {
            if !batch.texts.entries.is_empty() {
                let (start, count) = batch.texts.prepare_texts(
                    &self.gpu,
                    self.physical_width,
                    self.physical_height,
                    self.scale,
                    &batch.transform_table,
                    &mut global_transforms,
                );
                batch_infos[i].text = Some((start, count));
            }
        }

        // ---- 上传 transform 数据到 storage buffer ----
        let transform_bind_group: Option<wgpu::BindGroup> = if !global_transforms.is_empty() {
            let size = (global_transforms.len() * 4) as u64;
            self.ensure_transform_buffer(size);
            {
                let buf = self.transform_buf.borrow();
                let buf_ref = buf.as_ref().unwrap();
                self.gpu.queue.write_buffer(&buf_ref.0, 0, bytemuck::cast_slice(&global_transforms));
            }
            // 复用缓存的 bind group，仅当 buffer 重建时重新创建
            let mut cache = self.transform_bind_group_cache.borrow_mut();
            if cache.is_none() {
                let bg = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("transform bind group"),
                    layout: &self.gpu.transform_bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.transform_buf.borrow().as_ref().unwrap().0.as_entire_binding(),
                    }],
                });
                *cache = Some(bg);
            }
            cache.clone()
        } else {
            None
        };

        // ---- 单 pass：按 batch 顺序穿插 shapes → texts ----
        let has_any_content = batch_infos.iter().any(|b| b.shape.is_some() || b.text.is_some());
        if has_any_content {
            let msaa_view = self.msaa_view(self.gpu.surface_format); // offscreen uses same format via Texture::new
            let (color_view, resolve): (&wgpu::TextureView, Option<&wgpu::TextureView>) = match &msaa_view {
                Some(msaa) => (msaa, Some(target_view)),
                None => (target_view, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vireo render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target: resolve,
                    ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                ..Default::default()
            });

            let vbuf = self.vertex_buf.borrow();
            let ibuf = self.index_buf.borrow();
            let text_ctx = self.gpu.text_ctx.borrow();
            let poly_bg = polygon_bind_group.as_ref().unwrap_or(&self.gpu.polygon_dummy_bind_group);
            let xform_bg = transform_bind_group.as_ref().unwrap_or(&self.gpu.transform_dummy_bind_group);
            // glyphon render_range 会改 pass 状态；文本后或 geometry 切换时需重绑
            let mut shapes_bound = false;
            let mut last_geometry: Option<bool> = None;

            for info in &batch_infos {
                if let Some(ref shape) = info.shape {
                    let need_rebind = !shapes_bound || last_geometry != Some(shape.geometry);
                    if need_rebind {
                        pass.set_pipeline(&self.gpu.ensure_pipeline(
                            self.sample_count,
                            self.alpha_to_coverage,
                            self.ssaa,
                            shape.geometry,
                        ));
                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                        pass.set_bind_group(2, poly_bg, &[]);
                        pass.set_bind_group(3, xform_bg, &[]);
                        pass.set_vertex_buffer(0, vbuf.as_ref().unwrap().slice(..));
                        pass.set_index_buffer(ibuf.as_ref().unwrap().slice(..), wgpu::IndexFormat::Uint32);
                        shapes_bound = true;
                        last_geometry = Some(shape.geometry);
                    }
                    for seg in &shape.segments {
                        pass.set_bind_group(1, &seg.bind_group, &[]);
                        pass.draw_indexed(
                            seg.ndx_start..seg.ndx_start + seg.ndx_count,
                            shape.base_vertex,
                            0..1,
                        );
                    }
                }

                if let Some((start, count)) = info.text {
                    let _ = text_ctx.text_renderer.render_range(
                        &text_ctx.text_atlas,
                        &text_ctx.viewport,
                        &mut pass,
                        xform_bg,
                        start,
                        count,
                    );
                    // 文字管线污染 pass，下一 shape 必须重绑
                    shapes_bound = false;
                    last_geometry = None;
                }
            }
        }

        self.gpu.queue.submit([encoder.finish()]);
    }

    /// 强制 GPU 端 PSO 编译（DX12 懒编译需要）。在 `resumed()` 创建窗口后调用。
    /// 用 SDF + geo 管线各画一个 dummy 三角形，触发 PSO 编译；
    /// 同时预热文字管线（cosmic_text shape + swash 光栅化 + atlas 上传）。
    pub fn preheat(&self, target: &RenderTarget, clear_color: crate::color::Color) {
        // 1. 文字预热：cosmic_text shape + swash 光栅化 + atlas GPU 上传。
        // 首帧 ~33ms 的 text prepare 在这里完成。
        self.gpu.text_ctx.borrow_mut().preheat(
            &self.gpu.device,
            &self.gpu.queue,
            self.physical_width,
            self.physical_height,
        );

        // 2. PSO 预热：SDF + geo 管线各画一个 dummy 三角形。
        // Geo 路径（sdf_feather: None）
        let mut geo_batch = crate::context::DrawBatch::new();
        geo_batch.sdf_feather = None;
        geo_batch.vertices.push(crate::gpu::Vertex::new(0.0, 0.0, clear_color));
        geo_batch.vertices.push(crate::gpu::Vertex::new(1.0, 0.0, clear_color));
        geo_batch.vertices.push(crate::gpu::Vertex::new(0.0, 1.0, clear_color));
        geo_batch.indices.push(0);
        geo_batch.indices.push(1);
        geo_batch.indices.push(2);

        // SDF 路径（sdf_feather: Some(0.0)）
        let mut sdf_batch = crate::context::DrawBatch::new();
        sdf_batch.sdf_feather = Some(0.0);
        sdf_batch.vertices.push(crate::gpu::Vertex::new(0.0, 0.0, clear_color));
        sdf_batch.vertices.push(crate::gpu::Vertex::new(1.0, 0.0, clear_color));
        sdf_batch.vertices.push(crate::gpu::Vertex::new(0.0, 1.0, clear_color));
        sdf_batch.indices.push(0);
        sdf_batch.indices.push(1);
        sdf_batch.indices.push(2);

        self.draw(target, Some(clear_color), &[&geo_batch, &sdf_batch]);
    }

    fn ensure_vertex_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut cap = self.vertex_cap.borrow_mut();
        if *cap >= size { return; }
        let new_cap = if *cap == 0 { size.next_power_of_two() } else { (*cap * 2).max(size) };
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *self.vertex_buf.borrow_mut() = Some(buf);
        *cap = new_cap;
    }

    fn ensure_index_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut cap = self.index_cap.borrow_mut();
        if *cap >= size { return; }
        let new_cap = if *cap == 0 { size.next_power_of_two() } else { (*cap * 2).max(size) };
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("index buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *self.index_buf.borrow_mut() = Some(buf);
        *cap = new_cap;
    }

    fn ensure_polygon_edge_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.polygon_edge_buf.borrow_mut();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size { return; }
        let new_cap = if cur == 0 { size.next_power_of_two().max(64) } else { (cur * 2).max(size) };
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("polygon edge buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buf, new_cap));
        *self.polygon_bind_group_cache.borrow_mut() = None;
    }

    fn ensure_transform_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.transform_buf.borrow_mut();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size { return; }
        let new_cap = if cur == 0 { size.next_power_of_two().max(48) } else { (cur * 2).max(size) };
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("transform buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buf, new_cap));
        *self.transform_bind_group_cache.borrow_mut() = None;
    }
}

use crate::text::{TextEntryList, TextOptions};

/// 形状变换。内部使用，直接存储 3x3 仿射变换矩阵 + pivot。
#[derive(Clone, Copy)]
struct Transform {
    /// 线性部分：[a b; c d] = [sx*cos  -sx*sin; sy*sin  sy*cos]
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    /// 世界坐标位置（局部坐标系原点）
    x: f32,
    y: f32,
    /// 局部空间旋转/缩放中心
    px: f32,
    py: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, x: 0.0, y: 0.0, px: 0.0, py: 0.0 }
    }
}

impl Transform {
    /// 返回 3x3 仿射变换矩阵的 3 个列（WGSL 列主序）。
    /// 变换顺序：T(x,y) * [a b; c d] * T(-pivot)，即 pivot → 线性 → 平移。
    fn to_cols(&self) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let tx = self.x - self.px * self.a - self.py * self.b;
        let ty = self.y - self.px * self.c - self.py * self.d;
        ([self.a, self.c, 0.0], [self.b, self.d, 0.0], [tx, ty, 1.0])
    }
}

/// 6 个 f32 bit pattern 的 hash（batch 内去重用）。
/// 使用乘法混合降低对称碰撞（旧 XOR 旋转在对称矩阵上较易冲突）。
fn transform_key(c0: [f32; 3], c1: [f32; 3], c2: [f32; 3]) -> u64 {
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h = c0[0].to_bits() as u64;
    h = h.wrapping_mul(K).wrapping_add(c0[1].to_bits() as u64);
    h = h.wrapping_mul(K).wrapping_add(c1[0].to_bits() as u64);
    h = h.wrapping_mul(K).wrapping_add(c1[1].to_bits() as u64);
    h = h.wrapping_mul(K).wrapping_add(c2[0].to_bits() as u64);
    h = h.wrapping_mul(K).wrapping_add(c2[1].to_bits() as u64);
    h ^ (h >> 32)
}

/// 纹理坐标子区域，控制形状内部 UV 映射范围。
#[derive(Clone, Copy, Debug)]
pub struct UvRect {
    pub u0: f32, pub v0: f32,
    pub u1: f32, pub v1: f32,
}

impl Default for UvRect {
    fn default() -> Self { Self { u0: 0.0, v0: 0.0, u1: 1.0, v1: 1.0 } }
}

impl UvRect {
    /// 四角 UV：(左上, 右上, 右下, 左下)，对应包围盒四元组 (-1,-1)/(1,-1)/(1,1)/(-1,1)。
    pub fn corners(&self) -> [(f32, f32); 4] {
        [
            (self.u0, self.v0),
            (self.u1, self.v0),
            (self.u1, self.v1),
            (self.u0, self.v1),
        ]
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
    /// SDF 柔边宽度（逻辑像素，`None` = 几何光栅化模式，不走 SDF）。
    ///
    /// 注意：SDF 图形不受 MSAA 影响。
    pub sdf_feather: Option<f32>,
    /// 纹理坐标子区域，后续形状的 UV 在此范围内映射。
    pub uv: UvRect,
    /// 多边形的边数据：每条边 4 个 f32 (nx, ny, dot(vi,n), 0)
    /// 由 draw_polygon 填充，渲染时合并到 storage buffer。
    pub polygon_edges: Vec<f32>,
    /// 变换矩阵表（batch 内去重）。每个矩阵 12 f32（mat3x3，列 vec4-padded）。
    pub(crate) transform_table: Vec<f32>,
    /// hash → local index 映射（batch 内去重）。
    transform_map: FxHashMap<u64, u32>,
    /// 是否含 SDF 顶点（避免 draw 时全表扫描）。
    pub(crate) has_sdf: bool,
    /// 当前 transform 的已注册 index 缓存；transform 变更时失效。
    cached_transform_index: Option<u32>,
}

impl DrawBatch {
    pub fn new() -> Self {
        Self {
            vertices: Vec::with_capacity(64),
            indices: Vec::with_capacity(96),
            texts: TextEntryList::new(),
            texture: None,
            texture_segments: Vec::with_capacity(2),
            transform: None,
            sdf_feather: None,
            uv: UvRect::default(),
            polygon_edges: Vec::with_capacity(16),
            transform_table: Vec::with_capacity(48),
            transform_map: FxHashMap::default(),
            has_sdf: false,
            cached_transform_index: None,
        }
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.texts.clear();
        self.texture = None;
        self.texture_segments.clear();
        self.transform = None;
        self.sdf_feather = Some(0.0);
        self.uv = UvRect::default();
        self.polygon_edges.clear();
        self.transform_table.clear();
        self.transform_map.clear();
        self.has_sdf = false;
        self.cached_transform_index = None;
    }

    #[inline]
    fn invalidate_transform_cache(&mut self) {
        self.cached_transform_index = None;
    }

    /// 设置平移（屏幕坐标）。
    pub fn set_position(&mut self, x: f32, y: f32) {
        let mut t = self.transform.unwrap_or_default();
        t.x = x;
        t.y = y;
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 设置旋转弧度（顺时针）。保留当前 scale，默认绕 (0,0)，用 `set_pivot` 指定旋转中心。
    pub fn set_rad(&mut self, rad: f32) {
        let mut t = self.transform.unwrap_or_default();
        let old_sx = (t.a * t.a + t.b * t.b).sqrt();
        let old_sy = (t.c * t.c + t.d * t.d).sqrt();
        let (c, s) = (rad.cos(), rad.sin());
        t.a = old_sx * c;
        t.b = -old_sx * s;
        t.c = old_sy * s;
        t.d = old_sy * c;
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 设置旋转角度（度，顺时针）。等价于 `set_rad(deg.to_radians())`。
    pub fn set_deg(&mut self, deg: f32) {
        self.set_rad(deg.to_radians());
    }

    /// 设置旋转中心（形状局部坐标）。
    pub fn set_pivot(&mut self, px: f32, py: f32) {
        let mut t = self.transform.unwrap_or_default();
        t.px = px;
        t.py = py;
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 设置缩放（1.0 = 原始大小）。保留当前旋转角度。
    pub fn set_scale(&mut self, sx: f32, sy: f32) {
        let mut t = self.transform.unwrap_or_default();
        let old_sx = (t.a * t.a + t.b * t.b).sqrt();
        let old_sy = (t.c * t.c + t.d * t.d).sqrt();
        if old_sx > 0.0 { let k = sx / old_sx; t.a *= k; t.b *= k; }
        if old_sy > 0.0 { let k = sy / old_sy; t.c *= k; t.d *= k; }
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 一次性设置完整变换（从分解参数构建矩阵）。
    pub fn set_transform(&mut self, x: f32, y: f32, px: f32, py: f32, rotation: f32, sx: f32, sy: f32) {
        let (c, s) = (rotation.cos(), rotation.sin());
        self.transform = Some(Transform {
            a: sx * c, b: -sx * s,
            c: sy * s, d: sy * c,
            x, y, px, py,
        });
        self.invalidate_transform_cache();
    }

    /// 直接设置原始 3x3 仿射矩阵（6 个有效分量），pivot 归零。
    /// 矩阵列主序：`[a b tx; c d ty; 0 0 1]`。
    pub fn set_matrix(&mut self, a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) {
        self.transform = Some(Transform { a, b, c, d, x: tx, y: ty, px: 0.0, py: 0.0 });
        self.invalidate_transform_cache();
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
        self.invalidate_transform_cache();
    }

    /// 叠加平移（世界空间），在当前变换基础上移动 (dx, dy)。
    pub fn translate(&mut self, dx: f32, dy: f32) {
        let mut t = self.transform.unwrap_or_default();
        t.x += dx;
        t.y += dy;
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 叠加旋转（弧度，顺时针）。在当前变换基础上右乘 R(delta)，即绕局部原点旋转。
    pub fn rotate_rad(&mut self, rad: f32) {
        let mut t = self.transform.unwrap_or_default();
        let (c, s) = (rad.cos(), rad.sin());
        // M' = M * R(delta)，右乘旋转（局部空间）
        let a = t.a * c + t.b * s;
        let b = -t.a * s + t.b * c;
        let c2 = t.c * c + t.d * s;
        let d = -t.c * s + t.d * c;
        t.a = a;
        t.b = b;
        t.c = c2;
        t.d = d;
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 叠加旋转（度，顺时针）。等价于 `rotate_rad(deg.to_radians())`。
    pub fn rotate_deg(&mut self, deg: f32) {
        self.rotate_rad(deg.to_radians());
    }

    /// 叠加缩放（局部空间）。在当前变换基础上右乘 S(sx, sy)。
    pub fn scale_by(&mut self, sx: f32, sy: f32) {
        let mut t = self.transform.unwrap_or_default();
        // M' = M * S(sx, sy)，右乘缩放（局部空间）
        t.a *= sx;
        t.b *= sy;
        t.c *= sx;
        t.d *= sy;
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 叠加任意仿射矩阵（局部空间右乘），pivot 归零。
    /// 矩阵列主序：`[a b tx; c d ty; 0 0 1]`。
    pub fn apply_matrix(&mut self, a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) {
        let mut t = self.transform.unwrap_or_default();
        // M' = M * N，右乘（局部空间）
        let new_a = t.a * a + t.b * c;
        let new_b = t.a * b + t.b * d;
        let new_x = t.a * tx + t.b * ty + t.x;
        let new_c = t.c * a + t.d * c;
        let new_d = t.c * b + t.d * d;
        let new_y = t.c * tx + t.d * ty + t.y;
        t.a = new_a;
        t.b = new_b;
        t.c = new_c;
        t.d = new_d;
        t.x = new_x;
        t.y = new_y;
        t.px = 0.0;
        t.py = 0.0;
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 获取当前变换矩阵列（无 transform 时返回恒等矩阵）。
    pub(crate) fn current_matrix(&self) -> ([f32; 3], [f32; 3], [f32; 3]) {
        match self.transform {
            Some(t) => t.to_cols(),
            None => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        }
    }

    /// 将矩阵注册到 transform_table 并返回 local index（batch 内去重）。
    pub(crate) fn register_transform(&mut self, c0: [f32; 3], c1: [f32; 3], c2: [f32; 3]) -> u32 {
        // 6 个有意义的 f32 构成 key：col0.xy, col1.xy, col2.xy
        // col0.z=0, col1.z=0, col2.z=1 恒不变
        let key = transform_key(c0, c1, c2);
        let next_idx = (self.transform_table.len() / 12) as u32;
        let idx = *self.transform_map.entry(key).or_insert_with(|| {
            // mat3x3 在 storage buffer 中每列 vec4-padded（16 字节对齐）
            self.transform_table.extend_from_slice(&[
                c0[0], c0[1], 0.0, 0.0,  // col0 (a, c, 0, _pad)
                c1[0], c1[1], 0.0, 0.0,  // col1 (b, d, 0, _pad)
                c2[0], c2[1], 1.0, 0.0,  // col2 (tx, ty, 1, _pad)
            ]);
            next_idx
        });
        self.cached_transform_index = Some(idx);
        idx
    }

    /// 当前 transform 的 local index（同一形状的多顶点应共用一次调用）。
    /// 连续绘制且 transform 未变时命中缓存，跳过 hash。
    pub(crate) fn current_transform_index(&mut self) -> u32 {
        if let Some(idx) = self.cached_transform_index {
            return idx;
        }
        let (c0, c1, c2) = self.current_matrix();
        let idx = self.register_transform(c0, c1, c2);
        self.cached_transform_index = Some(idx);
        idx
    }

    /// 标记 batch 含 SDF 顶点（走 SDF pipeline）。
    pub(crate) fn note_sdf(&mut self) {
        self.has_sdf = true;
    }

    /// 添加单个顶点（自动应用当前 transform，索引查表）。
    pub fn push_vertex(&mut self, x: f32, y: f32, color: crate::color::Color) {
        let idx = self.current_transform_index();
        self.vertices.push(Vertex::new_uv_xform(x, y, 0.0, 0.0, color, idx));
    }

    /// 添加 SDF 顶点（自动应用当前 transform，索引查表）。
    /// 坐标和 SDF 参数应处于同一局部空间。
    pub fn push_sdf_vertex(&mut self, x: f32, y: f32, u: f32, v: f32, color: crate::color::Color, params: [f32;4], ty: u32, feather: f32) {
        let idx = self.current_transform_index();
        let mut vert = Vertex::new_uv_xform(x, y, u, v, color, idx);
        vert.sdf_params = params;
        vert.sdf_type = ty;
        vert.sdf_feather = feather;
        self.has_sdf = true;
        self.vertices.push(vert);
    }

    /// 添加带 UV 的顶点（自动应用当前 transform，索引查表）。
    pub fn push_vertex_uv(&mut self, x: f32, y: f32, u: f32, v: f32, color: crate::color::Color) {
        let idx = self.current_transform_index();
        self.vertices.push(Vertex::new_uv_xform(x, y, u, v, color, idx));
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
            sdf_feather: self.sdf_feather,
            uv: self.uv,
            polygon_edges: self.polygon_edges.clone(),
            transform_table: self.transform_table.clone(),
            transform_map: self.transform_map.clone(),
            has_sdf: self.has_sdf,
            cached_transform_index: self.cached_transform_index,
        }
    }

    /// 直接设置 bind group（高级用法，如离屏画布贴回窗口）。
    pub fn set_bind_group(&mut self, bg: wgpu::BindGroup) { self.texture = Some(bg); }

    /// 设置 UV 子区域，后续形状的纹理坐标在此范围内映射。
    pub fn set_uv(&mut self, u0: f32, v0: f32, u1: f32, v1: f32) {
        self.uv = UvRect { u0, v0, u1, v1 };
    }

    /// 恢复 UV 为全纹理范围 (0,0)-(1,1)。
    pub fn clear_uv(&mut self) {
        self.uv = UvRect::default();
    }

    /// 绑定纹理，后续形状自动使用。同一 batch 多次调用即切换纹理。
    pub fn set_texture(&mut self, texture: &crate::texture::Texture) {
        // 把当前纹理覆盖的索引范围记入 segment，然后切到新纹理
        if let Some(ref bg) = self.texture {
            self.add_texture_segment(bg.clone());
        }
        self.texture = Some(texture.bind_group.clone());
    }

    /// 记录纹理段：自上次段以来的所有新顶点归入此 bind group。
    pub(crate) fn add_texture_segment(&mut self, bg: wgpu::BindGroup) {
        let start = self.texture_segments.last().map_or(0, |s| s.ndx_start + s.ndx_count);
        let end = self.indices.len() as u32;
        if end > start {
            self.texture_segments.push(TextureSegment { ndx_start: start, ndx_count: end - start, bind_group: bg });
        }
    }

    // ---- 形状委托（去 draw_ 前缀） ----

    pub fn rectangle(&mut self, x: f32, y: f32, w: f32, h: f32, c: crate::color::Color) { crate::shapes::draw_rectangle(self, x, y, w, h, c); }
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: crate::color::Color) { crate::shapes::draw_circle(self, cx, cy, r, c); }
    pub fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, t: f32, c: crate::color::Color) { crate::shapes::draw_line(self, x1, y1, x2, y2, t, c); }
    pub fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, c: crate::color::Color) { crate::shapes::draw_ellipse(self, cx, cy, rx, ry, c); }
    pub fn rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: crate::color::Color) { crate::shapes::draw_rounded_rect(self, x, y, w, h, r, c); }
    pub fn triangle(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, c: crate::color::Color) { crate::shapes::draw_triangle(self, x1, y1, x2, y2, x3, y3, c); }
    pub fn polygon(&mut self, pts: &[(f32, f32)], c: crate::color::Color) { crate::shapes::draw_polygon(self, pts, c); }
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, sa: f32, ea: f32, c: crate::color::Color) { crate::shapes::draw_arc(self, cx, cy, r, sa, ea, c); }
    pub fn rect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, t: f32, c: crate::color::Color) { crate::shapes::draw_rect_outline(self, x, y, w, h, t, c); }
    pub fn circle_outline(&mut self, cx: f32, cy: f32, r: f32, t: f32, c: crate::color::Color, seg: u32) { crate::shapes::draw_circle_outline(self, cx, cy, r, t, c, seg); }
    pub fn ellipse_outline(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, t: f32, c: crate::color::Color, seg: u32) { crate::shapes::draw_ellipse_outline(self, cx, cy, rx, ry, t, c, seg); }
    pub fn rounded_rect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, t: f32, c: crate::color::Color, cs: u32) { crate::shapes::draw_rounded_rect_outline(self, x, y, w, h, r, t, c, cs); }
    pub fn line_chain(&mut self, pts: &[(f32, f32)], t: f32, c: crate::color::Color) { crate::shapes::draw_line_chain(self, pts, t, c); }
    pub fn triangle_outline(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, t: f32, c: crate::color::Color) { crate::shapes::draw_triangle_outline(self, x1, y1, x2, y2, x3, y3, t, c); }
    pub fn polygon_outline(&mut self, pts: &[(f32, f32)], t: f32, c: crate::color::Color) { crate::shapes::draw_polygon_outline(self, pts, t, c); }
    pub fn arc_outline(&mut self, cx: f32, cy: f32, r: f32, sa: f32, ea: f32, t: f32, c: crate::color::Color, seg: u32) { crate::shapes::draw_arc_outline(self, cx, cy, r, sa, ea, t, c, seg); }

    /// 添加文字，自动捕获当前 transform。
    pub fn text(&mut self, text: &str, options: TextOptions) {
        let idx = self.current_transform_index();
        self.texts.push_indexed(text, options, idx);
    }

    /// HUD 多段（Static / Dynamic / Digits），捕获当前 transform。
    pub fn text_parts(&mut self, parts: &[crate::text::TextPart<'_>], options: TextOptions) {
        let idx = self.current_transform_index();
        self.texts.push_parts_indexed(parts, options, idx);
    }

    /// HUD 自动切分（[`crate::text::split_hud`]），捕获当前 transform。
    pub fn text_hud(&mut self, text: &str, options: TextOptions) {
        let idx = self.current_transform_index();
        self.texts.push_hud_indexed(text, options, idx);
    }

    /// 绘制 [`crate::text::HudLine`]，捕获当前 transform。
    pub fn hud_line(&mut self, line: &crate::text::HudLine, options: TextOptions) {
        let idx = self.current_transform_index();
        line.draw_indexed(&mut self.texts, options, idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::colors::*;
    use crate::shapes::{draw_circle, draw_polygon, draw_rectangle};

    #[test]
    fn has_sdf_flag_set_on_sdf_shapes() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_rectangle(&mut b, 0.0, 0.0, 10.0, 10.0, RED);
        assert!(b.has_sdf);
        b.clear();
        assert!(!b.has_sdf);
        b.sdf_feather = None;
        draw_rectangle(&mut b, 0.0, 0.0, 10.0, 10.0, RED);
        assert!(!b.has_sdf);
    }

    #[test]
    fn transform_index_stable_across_same_transform() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(10.0, 20.0);
        draw_rectangle(&mut b, 0.0, 0.0, 5.0, 5.0, RED);
        draw_circle(&mut b, 0.0, 0.0, 3.0, BLUE);
        let idxs: Vec<u32> = b.vertices.iter().map(|v| v.transform_index).collect();
        assert!(idxs.iter().all(|&i| i == idxs[0]));
        assert_eq!(b.transform_table.len() / 12, 1);
    }

    #[test]
    fn transform_cache_invalidates_on_set_position() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(0.0, 0.0);
        draw_rectangle(&mut b, 0.0, 0.0, 5.0, 5.0, RED);
        b.set_position(100.0, 0.0);
        draw_rectangle(&mut b, 0.0, 0.0, 5.0, 5.0, BLUE);
        let i0 = b.vertices[0].transform_index;
        let i1 = b.vertices[4].transform_index;
        assert_ne!(i0, i1);
        assert_eq!(b.transform_table.len() / 12, 2);
    }

    #[test]
    fn multi_batch_poly_base_patch_values() {
        // 模拟 Renderer 多 batch poly 偏移：第二 batch 的 type6 start 应加上第一 batch 边数
        let mut b0 = DrawBatch::new();
        b0.sdf_feather = Some(1.0);
        let pts = [(0., 0.), (10., 0.), (5., 8.)];
        draw_polygon(&mut b0, &pts, RED);
        let edges0 = b0.polygon_edges.len() / 4;

        let mut b1 = DrawBatch::new();
        b1.sdf_feather = Some(1.0);
        draw_polygon(&mut b1, &pts, BLUE);
        let start_local = b1.vertices[0].sdf_params[0];
        assert_eq!(start_local, 0.0);

        let poly_base = edges0 as f32;
        let mut patched = b1.vertices.clone();
        for v in &mut patched {
            if v.sdf_type == 6 || v.sdf_type == 7 {
                v.sdf_params[0] += poly_base;
            }
        }
        assert_eq!(patched[0].sdf_params[0], poly_base);
    }

    #[test]
    fn transform_key_distinguishes_similar_matrices() {
        let k1 = transform_key([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        let k2 = transform_key([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 1.0]);
        let k3 = transform_key([2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]);
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        assert_ne!(k2, k3);
    }

    #[test]
    fn clear_preserves_vertex_capacity() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        for i in 0..32 {
            b.set_position(i as f32, 0.0);
            draw_rectangle(&mut b, 0.0, 0.0, 4.0, 4.0, RED);
        }
        let cap_v = b.vertices.capacity();
        let cap_i = b.indices.capacity();
        b.clear();
        assert!(b.vertices.capacity() >= cap_v);
        assert!(b.indices.capacity() >= cap_i);
        assert!(b.vertices.is_empty());
        assert!(!b.has_sdf);
    }

    #[test]
    fn translate_and_draw_share_cached_index() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(1.0, 2.0);
        let i0 = b.current_transform_index();
        let i1 = b.current_transform_index();
        assert_eq!(i0, i1);
        b.translate(3.0, 4.0);
        let i2 = b.current_transform_index();
        assert_ne!(i0, i2);
    }
}
