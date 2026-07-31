//! 渲染核心：批量绘制、渲染目标和渲染器。

use std::cell::RefCell;
use std::sync::Arc;
use rustc_hash::FxHashMap;

use wgpu::util::DeviceExt;

pub use crate::gpu::Vertex;
use crate::gpu::{GpuContext, ShapeInstance};
use crate::gpu::MaterialTarget;
use crate::material::Material;
use crate::area::{effective_area, Area, AreaGeom, AreaStencilOp};

/// 轴对齐矩形，用于 culling 的包围盒/视口。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn left(&self) -> f32 { self.x }
    pub fn right(&self) -> f32 { self.x + self.w }
    pub fn top(&self) -> f32 { self.y }
    pub fn bottom(&self) -> f32 { self.y + self.h }

    pub fn contains(&self, p: [f32; 2]) -> bool {
        p[0] >= self.x && p[0] <= self.x + self.w
            && p[1] >= self.y && p[1] <= self.y + self.h
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        let l = self.x.max(other.x);
        let r = (self.x + self.w).min(other.x + other.w);
        if r < l { return false; }
        let t = self.y.max(other.y);
        let b = (self.y + self.h).min(other.y + other.h);
        b >= t
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let l = self.x.min(other.x);
        let r = (self.x + self.w).max(other.x + other.w);
        let t = self.y.min(other.y);
        let b = (self.y + self.h).max(other.y + other.h);
        Rect::new(l, t, r - l, b - t)
    }
}

/// 2D 坐标位置。语义上为"画在哪"（WHERE），与"画什么"（WHAT）、"如何画"（OVERRIDE）区分。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

impl Pos {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 一次 `Renderer::draw` 的扁平事件序列（模块级，扁平方法可引用）。
///
/// - `Batch`：常规 batch（自身 shapes + texts）。
/// - `StencilPop`：父 batch `clips_children` 收尾（op=3 模板，ref=父 Push 后层级）。
/// - `AreaOp`：Area 掩码单 op（来自 `Area::compile_cover` / `compile_erase` 的展平）。
///   `is_setup=true` 是 batch 子树前的 cover，渲染时累加 area_depth；
///   `is_setup=false` 是子树后的 erase，渲染后减回。
///   走 stencil 管线 op 3（Erase）或 op 4（Cover），无色。
/// - `ScissorPush(Rect)`：用 scissor 代替 stencil（`scissor` + `clips_children`）。
/// - `ScissorPop`：恢复前一级 scissor。
pub(crate) enum DrawEvent<'a> {
    Batch(&'a DrawBatch),
    StencilPop,
    AreaOp { op: AreaStencilOp, is_setup: bool },
    ScissorPush(Rect),
    ScissorPop,
}

struct ShapeSegment {
    ndx_start: u32,
    ndx_count: u32,
    bind_group: wgpu::BindGroup,
}

struct InstanceSegment {
    instance_start: u32,
    instance_count: u32,
    bind_group: wgpu::BindGroup,
}

struct ShapeInfo {
    base_vertex: i32,
    segments: Vec<ShapeSegment>,
    geometry: bool,
    instances: Vec<InstanceSegment>,
}

struct TextRenderSegment {
    vertex_start: u32,
    vertex_count: u32,
    bind_group: Option<wgpu::BindGroup>,
}

struct EventInfo {
    shape: Option<ShapeInfo>,
    text: Vec<TextRenderSegment>,
    stencil_op: u32,
    stencil_ref: u32,
    area_op: Option<u32>,
    scissor_push: Option<Rect>,
    scissor_pop: bool,
    custom_material: Option<Arc<Material>>,
    custom_text_pipeline: Option<Arc<wgpu::RenderPipeline>>,
    dynamic_offsets: Vec<u32>,
}

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

    /// 编码渲染命令到 `CommandBuffer`（**不** submit/present）。
    /// 便捷包装：等价于 `renderer.draw(self, clear_color, batches)`。
    pub fn draw(
        &self,
        renderer: &Renderer,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) -> wgpu::CommandBuffer {
        renderer.draw(self, clear_color, batches)
    }

}

/// 渲染器 —— 管理 vertex/index buffer 复用，执行单 pass 渲染。
///
/// 内部维护 GPU buffer，支持在多 batch 间以偏移量追加写入。
pub struct Renderer {
    pub(crate) gpu: std::sync::Arc<GpuContext>,
    camera_buf: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    vertex_buf: RefCell<Option<wgpu::Buffer>>,
    vertex_cap: RefCell<u64>,
    index_buf: RefCell<Option<wgpu::Buffer>>,
    index_cap: RefCell<u64>,
    instance_buf: RefCell<Option<wgpu::Buffer>>,
    instance_cap: RefCell<u64>,
    physical_width: u32,
    physical_height: u32,
    scale: f32,
    dpi_scale: f32,
    sample_count: u32,
    alpha_to_coverage: bool,
    ssaa: bool,
    msaa_tex: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
    ds_tex: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
    polygon_edge_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    transform_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    engine_storage_bind_group_cache: RefCell<Option<wgpu::BindGroup>>,
    /// 逻辑视口尺寸（逻辑像素）
    logical_width: u32,
    logical_height: u32,
    /// 帧间复用的 CPU 暂存，避免每帧大块分配
    scratch_vdata: RefCell<Vec<u8>>,
    scratch_idata: RefCell<Vec<u8>>,
    scratch_transforms: RefCell<Vec<f32>>,
    scratch_poly_edges: RefCell<Vec<f32>>,
    scratch_event_infos: RefCell<Vec<EventInfo>>,
    scratch_aabb_map: RefCell<FxHashMap<usize, Option<Rect>>>,
    scratch_ref_stack: RefCell<Vec<u32>>,
    scratch_batch_transform_bases: RefCell<Vec<u32>>,
    scratch_batch_poly_base: RefCell<Vec<u32>>,
    scratch_last_dynamic_offsets: RefCell<Vec<u32>>,
    scratch_scissor_stack: RefCell<Vec<(u32, u32, u32, u32)>>,
    scratch_instances: RefCell<Vec<ShapeInstance>>,
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
            instance_buf: RefCell::new(None),
            instance_cap: RefCell::new(0),
            physical_width,
            physical_height,
            scale,
            dpi_scale,
            sample_count: aa.sample_count(),
            alpha_to_coverage: aa.alpha_to_coverage(),
            ssaa: aa.is_ssaa(),
            msaa_tex: RefCell::new(None),
            ds_tex: RefCell::new(None),
            polygon_edge_buf: RefCell::new(None),
            transform_buf: RefCell::new(None),
            engine_storage_bind_group_cache: RefCell::new(None),
            scratch_vdata: RefCell::new(Vec::new()),
            scratch_idata: RefCell::new(Vec::new()),
            scratch_transforms: RefCell::new(Vec::new()),
            scratch_poly_edges: RefCell::new(Vec::new()),
            scratch_event_infos: RefCell::new(Vec::new()),
            scratch_aabb_map: RefCell::new(FxHashMap::default()),
            scratch_ref_stack: RefCell::new(Vec::new()),
            scratch_batch_transform_bases: RefCell::new(Vec::new()),
            scratch_batch_poly_base: RefCell::new(Vec::new()),
            scratch_last_dynamic_offsets: RefCell::new(Vec::new()),
            scratch_scissor_stack: RefCell::new(Vec::new()),
            scratch_instances: RefCell::new(Vec::new()),
            logical_width,
            logical_height,
        }
    }

    /// 更新抗锯齿设置。
    pub fn update_aa(&mut self, aa: crate::window::AntiAliasing) {
        self.sample_count = aa.sample_count();
        self.alpha_to_coverage = aa.alpha_to_coverage();
        self.ssaa = aa.is_ssaa();
        *self.msaa_tex.borrow_mut() = None;
        *self.ds_tex.borrow_mut() = None;
    }

    /// 获取匹配当前 sample_count 的 pipeline

    /// 获取 multisampled 视图（必要时创建），无 MSAA 返回 None
    fn msaa_view(&self, format: wgpu::TextureFormat) -> Option<wgpu::TextureView> {
        if self.sample_count <= 1 { return None; }
        let mut mt = self.msaa_tex.borrow_mut();
        if mt.is_none()
            || mt.as_ref().unwrap().0.width() != self.physical_width
            || mt.as_ref().unwrap().0.height() != self.physical_height
            || mt.as_ref().unwrap().0.sample_count() != self.sample_count
        {
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

    /// 获取 depth/stencil 视图（Depth24PlusStencil8，必要时创建）。sample_count 与 color 一致。
    fn ds_view(&self) -> wgpu::TextureView {
        let mut dt = self.ds_tex.borrow_mut();
        let ok = dt.as_ref()
            .map(|(t,_)| {
                t.width() == self.physical_width
                    && t.height() == self.physical_height
                    && t.sample_count() == self.sample_count
            })
            .unwrap_or(false);
        if !ok {
            let tex = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("depth_stencil"),
                size: wgpu::Extent3d { width: self.physical_width, height: self.physical_height, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: self.sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            *dt = Some((tex, view));
        }
        dt.as_ref().unwrap().1.clone()
    }

    /// 更新相机投影（窗口 resize 时调用）。
    /// `scale`：逻辑→物理（文字/scissor）；`dpi_scale`：OS 缩放（SDF feather，可与 scale 不同）。
    pub fn resize(
        &mut self,
        logical_width: u32,
        logical_height: u32,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
        dpi_scale: f32,
    ) {
        let proj = glam::camera::rh::proj::opengl::orthographic(0.0, logical_width as f32, logical_height as f32, 0.0, -1.0, 1.0);
        let camera_data: [[f32; 4]; 4] = proj.to_cols_array_2d();
        let mut camera_raw = [0u8; 80];
        camera_raw[..64].copy_from_slice(bytemuck::cast_slice(&camera_data));
        camera_raw[64..68].copy_from_slice(&dpi_scale.to_le_bytes());
        self.gpu.queue.write_buffer(&self.camera_buf, 0, &camera_raw);
        self.physical_width = physical_width;
        self.physical_height = physical_height;
        self.logical_width = logical_width;
        self.logical_height = logical_height;
        self.scale = scale;
        self.dpi_scale = dpi_scale;
        *self.msaa_tex.borrow_mut() = None;
        *self.ds_tex.borrow_mut() = None;
    }

    /// 编码渲染命令到 `CommandBuffer`，**不** submit/present。
    ///
    /// 调用方负责在 wgpu owner 线程（= 窗口 owner 线程）上：
    /// ```ignore
    /// queue.submit([cmd_buf]);
    /// queue.present(surface_texture);
    /// ```
    ///
    /// 返回的 `CommandBuffer` 持有对 `target.view` 的引用（`TextureView`），
    /// 在 `submit` 之前 `target.view` 必须保持有效（即 `SurfaceTexture` 未被销毁）。
    ///
    /// 拆分 submit/present 到 winit 线程解决了"模态循环期间冻屏"：present 必须在
    /// owner 线程，DWM 才接受。详见 VIREO_OPT_NOTES 第三十五轮。
    pub fn draw(
        &self,
        target: &RenderTarget,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) -> wgpu::CommandBuffer {
        // ---- 前序展开子树（含 Pop 事件 + Area 事件）----
        // 可见 = 祖先 stencil ∧ batch 自身有效 Area。
        // Area 编译为掩码 op（无色）：AreaSetup 在 batch 前盖、AreaCleanup 在子树后擦。
        // Area 存在时，batch 自身 content 在 base+1 测（Area∩base），子树按 clips_children 走。
        // clips_children + Area：Push at base+1（content level），子看 base+2；Pop 回 base+1。
        let viewport = Rect::new(0.0, 0.0, self.logical_width as f32, self.logical_height as f32);

        // Pass 1: bottom-up 计算子树 AABB（供 culling 用）
        {
            let mut aabb_map = self.scratch_aabb_map.borrow_mut();
            aabb_map.clear();
            for b in batches {
                compute_subtree_aabb(b, &mut aabb_map);
            }
        }

        // Pass 2: flatten with culling
        let mut events: Vec<DrawEvent> = Vec::new();
        let mut uses_stencil = false;
        {
            let aabb_map = self.scratch_aabb_map.borrow();
            for b in batches {
                let event_start = events.len();
                b.flatten_events(&mut events, 0, Some(viewport), &aabb_map);
                uses_stencil |= events[event_start..]
                    .iter()
                    .any(|ev| matches!(ev, DrawEvent::StencilPop | DrawEvent::AreaOp { .. }));
            }
        }

        let has_content = clear_color.is_some()
            || events.iter().any(|e| matches!(e, DrawEvent::Batch(b) if !b.vertices.is_empty() || !b.instances.is_empty() || !b.texts.entries.is_empty()));
        if !has_content {
            // 无内容：返回空 cmd_buf（不创建 render pass 即可）
            let empty_encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vireo empty encoder"),
            });
            return empty_encoder.finish();
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
        // 相机为逻辑像素正交；Pop 全屏四边形也用逻辑尺寸
        let lw = self.physical_width as f32 / self.scale.max(1e-6);
        let lh = self.physical_height as f32 / self.scale.max(1e-6);

        // ---- 在 pass 外写入所有 batch 的 vertex/index 数据 ----
        let mut event_infos = self.scratch_event_infos.borrow_mut();
        event_infos.clear();
        let vertex_count: u32 = 0;
        let ndx_accum: u32 = 0;

        // ---- 单次扫描：合并 transform/poly + 统计顶点数 ----
        let mut global_transforms = self.scratch_transforms.borrow_mut();
        global_transforms.clear();
        // 全局表槽 0 = 单位阵（与 batch `transform_table` 槽 0 约定一致）。
        // `draw_text` / glyphon 默认 transform_index=0 表示恒等；batch 表上传时
        // `transform_base` 会偏移局部 index，故全局槽 0 仍须单独预留，不能被首个 batch 占用。
        global_transforms.extend_from_slice(&IDENTITY_TRANSFORM_ROW);
        let mut polygon_edges_global = self.scratch_poly_edges.borrow_mut();
        polygon_edges_global.clear();

        // stencil 两路计数（不可混用）：
        // - `clip_depth`：仅 clips_children 的 Push 层数（不含 Area）
        // - `area_depth`：仍打开的 Area 框架数
        // content_level = clip_depth + area_depth_ancestors + has_own_area
        // Push@content_level 后 buffer 为 content_level+1；clip_depth+=1，
        // 子节点 content = (clip_depth) + area… 不会把 Area 算两次。
        fn compute_stencil_at_level(
            batch: &DrawBatch,
            content_level: u32,
            ref_stack: &mut Vec<u32>,
        ) -> (u32, u32) {
            let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty();
            let has_draw = has_geom || !batch.texts.entries.is_empty();
            if batch.clips_children && (has_geom || batch.scissor.is_some()) {
                // Push: Test content_level → Inc；ref_stack 存抬升后绝对值供 Pop
                let push_ref = content_level;
                ref_stack.push(push_ref + 1);
                (1u32, push_ref)
            } else {
                // `clips_children && !has_geom && scissor.is_none()` 是 no-op；
                // dev 模式立刻提示用户，release 保持原行为（静默跳过）
                debug_assert!(
                    !batch.clips_children || has_geom || batch.scissor.is_some(),
                    "clips_children=true 但 batch 无几何且无显式 scissor；裁切不会生效。请提供几何裁切形状或显式设置 batch.scissor"
                );
                if content_level > 0 {
                    if batch.inherit.clipped && has_draw {
                        (2u32, content_level) // Test
                    } else {
                        (0u32, 0)
                    }
                } else {
                    (0u32, 0)
                }
            }
        }

        let mut ref_stack = self.scratch_ref_stack.borrow_mut();
        ref_stack.clear();
        let mut clip_depth: u32 = 0;
        let mut area_depth: u32 = 0;
        // 连续 cleanup AreaOp 只 -1 一次（compile_erase 可多 op）
        let mut prev_area_cleanup = false;

        for event in &events {
            match event {
                DrawEvent::Batch(batch) => {
                    prev_area_cleanup = false;
                    let has_own_area = batch
                        .effective_area()
                        .as_ref()
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    let ancestors_area_depth = area_depth;
                    if has_own_area {
                        area_depth += 1;
                    }
                    let content_level =
                        clip_depth + ancestors_area_depth + (has_own_area as u32);
                    // 与 flatten_events 共用条件（见 DrawBatch::uses_scissor_path）
                    let use_scissor = batch.uses_scissor_path(has_own_area);
                    let (stencil_op, stencil_ref) = if use_scissor {
                        let has_draw =
                            !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.texts.entries.is_empty();
                        if content_level > 0 && batch.inherit.clipped && has_draw {
                            (2u32, content_level) // Test 祖先，不 Push
                        } else {
                            (0u32, 0u32)
                        }
                    } else {
                        compute_stencil_at_level(batch, content_level, &mut *ref_stack)
                    };
                    if stencil_op == 1 {
                        // 只增加 clip 层，不含 Area（Area 已在 content_level 里）
                        clip_depth += 1;
                    }
                    let custom_mat = batch.custom_material.clone();
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op,
                        stencil_ref,
                        area_op: None,
                        scissor_push: None,
                        scissor_pop: false,
                        custom_material: custom_mat,
                        custom_text_pipeline: None,
                        dynamic_offsets: batch.dynamic_offsets.clone(),
                    });
                }
                DrawEvent::StencilPop => {
                    prev_area_cleanup = false;
                    let popped = ref_stack.pop();
                    clip_depth = clip_depth.saturating_sub(1);
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op: 3,
                        stencil_ref: popped.unwrap_or(0),
                        area_op: None,
                        scissor_push: None,
                        scissor_pop: false,
                        custom_material: None,
                        custom_text_pipeline: None,
                        dynamic_offsets: Vec::new(),
                    });
                }
                DrawEvent::AreaOp { op, is_setup } => {
                    // Area 单 op：cover (op 4) 在 batch 前，erase (op 3) 在子树后。
                    // setup 在 Batch 事件里 +1；cleanup 连续多 op 只 -1 一次。
                    let pipe_op = op.stencil_pipeline_op(); // 3 or 4
                    let r = op.stencil_ref();
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op: pipe_op,
                        stencil_ref: r,
                        area_op: Some(pipe_op),
                        scissor_push: None,
                        scissor_pop: false,
                        custom_material: None,
                        custom_text_pipeline: None,
                        dynamic_offsets: Vec::new(),
                    });
                    if !is_setup {
                        if !prev_area_cleanup {
                            area_depth = area_depth.saturating_sub(1);
                        }
                        prev_area_cleanup = true;
                    } else {
                        prev_area_cleanup = false;
                    }
                }
                DrawEvent::ScissorPush(rect) => {
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op: 0,
                        stencil_ref: 0,
                        area_op: None,
                        scissor_push: Some(*rect),
                        scissor_pop: false,
                        custom_material: None,
                        custom_text_pipeline: None,
                        dynamic_offsets: Vec::new(),
                    });
                }
                DrawEvent::ScissorPop => {
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op: 0,
                        stencil_ref: 0,
                        area_op: None,
                        scissor_push: None,
                        scissor_pop: true,
                        custom_material: None,
                        custom_text_pipeline: None,
                        dynamic_offsets: Vec::new(),
                    });
                }
            }
        }

        // 收集 transform/poly 信息
        let mut batch_transform_bases = self.scratch_batch_transform_bases.borrow_mut();
        batch_transform_bases.clear();
        let mut batch_poly_base = self.scratch_batch_poly_base.borrow_mut();
        batch_poly_base.clear();
        let mut total_vcount: u32 = 0;
        let mut total_icount: u32 = 0;
        let mut poly_offset: u32 = 0;
        let mut pop_screen_verts: u32 = 0; // 全屏 Pop 顶点数
        let mut pop_screen_idx: u32 = 0;

        for (ei, event) in events.iter().enumerate() {
            if let DrawEvent::Batch(batch) = event {
                let _e = &mut event_infos[ei];
                batch_transform_bases.push((global_transforms.len() / 12) as u32);
                global_transforms.extend_from_slice(&batch.transform_table);
                batch_poly_base.push(poly_offset);
                poly_offset += batch.polygon_edges.len() as u32 / 4;
                polygon_edges_global.extend_from_slice(&batch.polygon_edges);
                total_vcount += batch.vertices.len() as u32;
                total_icount += batch.indices.len() as u32;
                if batch.custom_material.is_some() {
                    total_vcount += batch.instances.len() as u32 * 4;
                    total_icount += batch.instances.len() as u32 * 6;
                }
            } else if let DrawEvent::StencilPop = event {
                // Pop 事件：添加全屏四边形（2 三角，6 索引）
                pop_screen_verts += 4;
                pop_screen_idx += 6;
                batch_transform_bases.push(0);
                batch_poly_base.push(poly_offset);
            } else if let DrawEvent::ScissorPush(_) | DrawEvent::ScissorPop = event {
                // Scissor 事件不需要 transform/poly，但保留索引对齐
                batch_transform_bases.push(0);
                batch_poly_base.push(poly_offset);
            } else if let DrawEvent::AreaOp { op, .. } = event {
                // Area 事件：Full → 全屏 4v/6i；Geom → AreaGeom 自带 v/i。
                if let Some(geom) = op.geom() {
                    total_vcount += geom.vertices.len() as u32;
                    total_icount += geom.indices.len() as u32;
                    // 空表：顶点 index 走全局槽 0（单位阵），不追加、不 patch 偏移。
                    if geom.transform_table.is_empty() {
                        batch_transform_bases.push(0);
                    } else {
                        batch_transform_bases.push((global_transforms.len() / 12) as u32);
                        global_transforms.extend_from_slice(&geom.transform_table);
                    }
                    batch_poly_base.push(poly_offset);
                    poly_offset += geom.polygon_edges.len() as u32 / 4;
                    polygon_edges_global.extend_from_slice(&geom.polygon_edges);
                } else {
                    pop_screen_verts += 4;
                    pop_screen_idx += 6;
                    batch_transform_bases.push(0);
                    batch_poly_base.push(poly_offset);
                }
            }
        }

        let total_vbytes = (total_vcount + pop_screen_verts) as u64 * size_of::<Vertex>() as u64;
        let total_ibytes = (total_icount + pop_screen_idx) as u64 * 4;
        self.ensure_vertex_buffer(total_vbytes);
        self.ensure_index_buffer(total_ibytes);
        let mut combined_vdata = self.scratch_vdata.borrow_mut();
        let mut combined_idata = self.scratch_idata.borrow_mut();
        let mut combined_instances = self.scratch_instances.borrow_mut();
        combined_vdata.clear();
        combined_idata.clear();
        combined_instances.clear();
        let cap_v = total_vbytes as usize;
        let cap_i = total_ibytes as usize;
        if combined_vdata.capacity() < cap_v {
            combined_vdata.reserve(cap_v);
        }
        if combined_idata.capacity() < cap_i {
            combined_idata.reserve(cap_i);
        }

        // 合并数据 + 为 Pop 事件添加全屏顶点
        let mut v_offset = vertex_count;
        let mut idx_offset = ndx_accum;
        for (ei, event) in events.iter().enumerate() {
            match event {
                DrawEvent::Batch(batch) => {
                    let info_idx = ei;
                    let instance_start = combined_instances.len() as u32;
                    let use_instances = !batch.instances.is_empty() && batch.custom_material.is_none();
                    if use_instances {
                        let transform_base = batch_transform_bases[info_idx];
                        combined_instances.extend(batch.instances.iter().copied().map(|mut instance| {
                            instance.transform_index += transform_base;
                            if instance.sdf_type == 6 || instance.sdf_type == 7 {
                                instance.sdf_params[0] += batch_poly_base[info_idx] as f32;
                            }
                            instance
                        }));
                    }
                    let resolve_bg = |bg: Option<wgpu::BindGroup>| {
                        bg.unwrap_or_else(|| self.gpu.white_bind_group.as_ref().clone())
                    };
                    let instance_segments = if !use_instances {
                        Vec::new()
                    } else if batch.instance_texture_segments.is_empty() {
                        vec![InstanceSegment {
                            instance_start,
                            instance_count: batch.instances.len() as u32,
                            bind_group: resolve_bg(batch.bind_group.clone()),
                        }]
                    } else {
                        let mut segments: Vec<InstanceSegment> = batch.instance_texture_segments.iter().map(|segment| InstanceSegment {
                            instance_start: instance_start + segment.instance_start,
                            instance_count: segment.instance_count,
                            bind_group: resolve_bg(segment.bind_group.clone()),
                        }).collect();
                        let last_end = segments.last().map_or(instance_start, |s| s.instance_start + s.instance_count);
                        let total_end = instance_start + batch.instances.len() as u32;
                        if last_end < total_end {
                            segments.push(InstanceSegment {
                                instance_start: last_end,
                                instance_count: total_end - last_end,
                                bind_group: resolve_bg(batch.bind_group.clone()),
                            });
                        }
                        segments
                    };
                    let shape = if !batch.vertices.is_empty() || !batch.instances.is_empty() {
                        let transform_base = batch_transform_bases[info_idx];
                        let poly_base = batch_poly_base[info_idx] as f32;
                        let needs_patch = !batch.polygon_edges.is_empty() || transform_base > 0;
                        let v_start = combined_vdata.len();
                        combined_vdata.extend_from_slice(bytemuck::cast_slice(&batch.vertices));
                        if needs_patch {
                            let has_poly = !batch.polygon_edges.is_empty();
                            let verts: &mut [Vertex] = bytemuck::cast_slice_mut(&mut combined_vdata[v_start..]);
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
                        let mut mesh_index_count = batch.indices.len() as u32;
                        if !use_instances {
                            for (instance_index, instance) in batch.instances.iter().enumerate() {
                                let base = batch.vertices.len() as u32 + instance_index as u32 * 4;
                                let [x0, y0, x1, y1] = instance.bounds;
                                let [ux0, uy0, ux1, uy1] = instance.uv_bounds;
                                let [u0, v0, u1, v1] = instance.uv_rect;
                                let uv_at = |x: f32, y: f32| {
                                    (
                                        u0 + (x - ux0) / (ux1 - ux0) * (u1 - u0),
                                        v0 + (y - uy0) / (uy1 - uy0) * (v1 - v0),
                                    )
                                };
                                let (uv00, uv01) = uv_at(x0, y0);
                                let (uv10, uv11) = uv_at(x1, y0);
                                let (uv20, uv21) = uv_at(x1, y1);
                                let (uv30, uv31) = uv_at(x0, y1);
                                let color = crate::color::Color::new(
                                    instance.color[0], instance.color[1], instance.color[2], instance.color[3],
                                );
                                let mut verts = [
                                    Vertex::new_uv_xform(x0, y0, uv00, uv01, color, instance.transform_index + transform_base),
                                    Vertex::new_uv_xform(x1, y0, uv10, uv11, color, instance.transform_index + transform_base),
                                    Vertex::new_uv_xform(x1, y1, uv20, uv21, color, instance.transform_index + transform_base),
                                    Vertex::new_uv_xform(x0, y1, uv30, uv31, color, instance.transform_index + transform_base),
                                ];
                                for vertex in &mut verts {
                                    vertex.sdf_params = instance.sdf_params;
                                    if vertex.sdf_type == 6 || vertex.sdf_type == 7 {
                                        vertex.sdf_params[0] += poly_base;
                                    }
                                    vertex.sdf_extra = instance.sdf_extra;
                                    vertex.sdf_type = instance.sdf_type;
                                    vertex.sdf_feather = instance.sdf_feather;
                                }
                                combined_vdata.extend_from_slice(bytemuck::cast_slice(&verts));
                                combined_idata.extend_from_slice(bytemuck::cast_slice(&[
                                    base, base + 1, base + 2, base, base + 2, base + 3,
                                ]));
                                mesh_index_count += 6;
                            }
                        }

                        let segs: Vec<ShapeSegment> = if batch.texture_segments.is_empty() {
                            let bg = resolve_bg(batch.bind_group.clone());
                            vec![ShapeSegment { ndx_start: idx_offset, ndx_count: mesh_index_count, bind_group: bg }]
                        } else {
                            let mut v: Vec<ShapeSegment> = batch.texture_segments.iter().map(|s| ShapeSegment {
                                ndx_start: idx_offset + s.ndx_start,
                                ndx_count: s.ndx_count,
                                bind_group: resolve_bg(s.bind_group.clone()),
                            }).collect();
                            let last_end = v.last().map(|s| s.ndx_start + s.ndx_count).unwrap_or(idx_offset);
                            let total_end = idx_offset + mesh_index_count;
                            if last_end < total_end {
                                let bg = resolve_bg(batch.bind_group.clone());
                                v.push(ShapeSegment { ndx_start: last_end, ndx_count: total_end - last_end, bind_group: bg });
                            }
                            v
                        };
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: !batch.has_sdf && batch.sdf_feather.is_none(),
                            instances: instance_segments,
                        };
                        v_offset += batch.vertices.len() as u32;
                        idx_offset += mesh_index_count;
                        Some(info)
                    } else if !instance_segments.is_empty() {
                        Some(ShapeInfo {
                            base_vertex: 0,
                            segments: Vec::new(),
                            geometry: false,
                            instances: instance_segments,
                        })
                    } else {
                        None
                    };
                    event_infos[ei].shape = shape;
                }
                DrawEvent::StencilPop => {
                    // 全屏四边形（逻辑像素）；索引相对 base_vertex；单位矩阵
                    // 复用全局槽 0（恒为单位阵，见 `Renderer::draw` 初始化），避免深嵌套浪费 transform 槽
                    let id_idx = 0u32;
                    let verts = [
                        Vertex::new_uv_xform(0.0, 0.0, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        Vertex::new_uv_xform(lw, 0.0, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        Vertex::new_uv_xform(lw, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        Vertex::new_uv_xform(0.0, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                    ];
                    combined_vdata.extend_from_slice(bytemuck::cast_slice(&verts));
                    let indices = [0u32, 1, 2, 0, 2, 3];
                    combined_idata.extend_from_slice(bytemuck::cast_slice(&indices));

                    let bg = self.gpu.white_bind_group.as_ref().clone();
                    let segs = vec![ShapeSegment {
                        ndx_start: idx_offset,
                        ndx_count: 6,
                        bind_group: bg,
                    }];
                    let si = ShapeInfo {
                        base_vertex: v_offset as i32,
                        segments: segs,
                        geometry: true,
                        instances: Vec::new(),
                    };
                    event_infos[ei].shape = Some(si);
                    v_offset += 4;
                    idx_offset += 6;
                }
                DrawEvent::ScissorPush(_) | DrawEvent::ScissorPop => {}
                DrawEvent::AreaOp { op, .. } => {
                    // Area 掩码：Full → 全屏 4v/6i；Geom → AreaGeom 自带 v/i。
                    // 走 stencil 管线 op 3/4（无色），由 pass 内 `area_op` 决定管线 key。
                    let transform_base = batch_transform_bases[ei];
                    let poly_base = batch_poly_base[ei] as f32;
                    let si = if let Some(geom) = op.geom() {
                        let needs_patch = !geom.polygon_edges.is_empty() || transform_base > 0;
                        let v_start = combined_vdata.len();
                        combined_vdata.extend_from_slice(bytemuck::cast_slice(&geom.vertices));
                        if needs_patch {
                            let has_poly = !geom.polygon_edges.is_empty();
                            let verts: &mut [Vertex] = bytemuck::cast_slice_mut(&mut combined_vdata[v_start..]);
                            for v in verts.iter_mut() {
                                if transform_base > 0 {
                                    v.transform_index += transform_base;
                                }
                                if has_poly && (v.sdf_type == 6 || v.sdf_type == 7) {
                                    v.sdf_params[0] += poly_base;
                                }
                            }
                        }
                        combined_idata.extend_from_slice(bytemuck::cast_slice(&geom.indices));
                        let n = geom.indices.len() as u32;
                        let bg = self.gpu.white_bind_group.as_ref().clone();
                        let segs = vec![ShapeSegment {
                            ndx_start: idx_offset,
                            ndx_count: n,
                            bind_group: bg,
                        }];
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: !geom.has_sdf && geom.sdf_feather.is_none(),
                            instances: Vec::new(),
                        };
                        v_offset += geom.vertices.len() as u32;
                        idx_offset += n;
                        info
                    } else {
                        // Full：全屏四边形 + 单位矩阵；复用全局槽 0（恒为单位阵）
                        let id_idx = 0u32;
                        let verts = [
                            Vertex::new_uv_xform(0.0, 0.0, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                            Vertex::new_uv_xform(lw, 0.0, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                            Vertex::new_uv_xform(lw, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                            Vertex::new_uv_xform(0.0, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        ];
                        combined_vdata.extend_from_slice(bytemuck::cast_slice(&verts));
                        let indices = [0u32, 1, 2, 0, 2, 3];
                        combined_idata.extend_from_slice(bytemuck::cast_slice(&indices));
                        let bg = self.gpu.white_bind_group.as_ref().clone();
                        let segs = vec![ShapeSegment {
                            ndx_start: idx_offset,
                            ndx_count: 6,
                            bind_group: bg,
                        }];
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: true,
                            instances: Vec::new(),
                        };
                        v_offset += 4;
                        idx_offset += 6;
                        info
                    };
                    event_infos[ei].shape = Some(si);
                }
            }
        }

        // ---- 合并上传 ----
        if !combined_vdata.is_empty() {
            let vbuf = self.vertex_buf.borrow();
            self.gpu.queue.write_buffer(vbuf.as_ref().unwrap(), 0, &combined_vdata);
        }
        if !combined_idata.is_empty() {
            let ibuf = self.index_buf.borrow();
            self.gpu.queue.write_buffer(ibuf.as_ref().unwrap(), 0, &combined_idata);
        }
        if !combined_instances.is_empty() {
            let size = (combined_instances.len() * size_of::<ShapeInstance>()) as u64;
            self.ensure_instance_buffer(size);
            let instance_buf = self.instance_buf.borrow();
            self.gpu.queue.write_buffer(
                instance_buf.as_ref().unwrap(),
                0,
                bytemuck::cast_slice(&combined_instances),
            );
        }

        // ---- 上传多边形边数据 ----
        if !polygon_edges_global.is_empty() {
            let size = (polygon_edges_global.len() * 4) as u64;
            self.ensure_polygon_edge_buffer(size);
            {
                let buf = self.polygon_edge_buf.borrow();
                let buf_ref = buf.as_ref().unwrap();
                self.gpu.queue.write_buffer(&buf_ref.0, 0, bytemuck::cast_slice(&polygon_edges_global));
            }
        }

        // ---- 准备所有文本（DS 与本帧 attachment 一致）----
        {
            let mut tc = self.gpu.text_ctx.lock().unwrap();
            tc.ensure_sample_count(&self.gpu.device, self.sample_count);
            tc.ensure_text_ds(&self.gpu.device, uses_stencil);
        }
        let mut text_ctx = self.gpu.text_ctx.lock().unwrap();
        text_ctx.text_renderer.begin_frame();
        text_ctx.advance_frame();
        drop(text_ctx);
        for (ei, event) in events.iter().enumerate() {
            if let DrawEvent::Batch(batch) = event {
                if !batch.texts.entries.is_empty() {
                    let prepared = batch.texts.prepare_texts(
                        &self.gpu,
                        self.physical_width,
                        self.physical_height,
                        self.scale,
                        &batch.transform_table,
                        &mut global_transforms,
                        batch.text_clip,
                        batch.color,
                    );
                    let text_ctx = self.gpu.text_ctx.lock().unwrap();
                    event_infos[ei].text = prepared
                        .into_iter()
                        .map(|segment| TextRenderSegment {
                            vertex_start: segment.vertex_start,
                            vertex_count: segment.vertex_count,
                            bind_group: segment.texture_view.as_ref().map(|view| {
                                text_ctx
                                    .text_atlas
                                    .bind_group_for_base_texture(&self.gpu.device, view)
                            }),
                        })
                        .collect();
                    drop(text_ctx);
                    if let Some(material) = batch.custom_material.as_ref() {
                        let text_tests_stencil = uses_stencil
                            && (event_infos[ei].stencil_op == 1
                                || event_infos[ei].stencil_op == 2
                                || event_infos[ei].area_op.is_some());
                        event_infos[ei].custom_text_pipeline = Some(
                            self.gpu.ensure_material_pipeline(
                                material,
                                MaterialTarget::Text,
                                self.sample_count,
                                self.alpha_to_coverage,
                                false,
                                uses_stencil,
                                if text_tests_stencil { 2 } else { 0 },
                            ),
                        );
                    }
                }
            }
        }
        self.gpu
            .text_ctx
            .lock()
            .unwrap()
            .text_renderer
            .finish_frame(&self.gpu.device, &self.gpu.queue);

        // ---- 上传 transform 数据 ----
        if !global_transforms.is_empty() {
            let size = (global_transforms.len() * 4) as u64;
            self.ensure_transform_buffer(size);
            {
                let buf = self.transform_buf.borrow();
                let buf_ref = buf.as_ref().unwrap();
                self.gpu.queue.write_buffer(&buf_ref.0, 0, bytemuck::cast_slice(&global_transforms));
            }
        }
        let engine_storage_bind_group = {
            let mut cache = self.engine_storage_bind_group_cache.borrow_mut();
            if cache.is_none() {
                let transforms = self.transform_buf.borrow();
                let polygons = self.polygon_edge_buf.borrow();
                let transform_buf = transforms
                    .as_ref()
                    .map(|(buf, _)| buf)
                    .unwrap_or(&self.gpu.transform_dummy_buf);
                let polygon_buf = polygons
                    .as_ref()
                    .map(|(buf, _)| buf)
                    .unwrap_or(&self.gpu.polygon_dummy_buf);
                *cache = Some(self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("engine storage bind group"),
                    layout: &self.gpu.engine_storage_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: transform_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: polygon_buf.as_entire_binding() },
                    ],
                }));
            }
            cache.clone().unwrap()
        };

        // ---- 单 pass：仅 clips_children 帧挂 DS（热路径无 DS 开销）----
        let has_any_content = event_infos.iter().any(|e| e.shape.is_some() || !e.text.is_empty());
        // clear-only draw 也必须开启 pass，否则 LoadOp::Clear 不会执行。
        if has_any_content || clear_color.is_some() {
            let msaa_view = self.msaa_view(self.gpu.surface_format);
            let (color_view, resolve): (&wgpu::TextureView, Option<&wgpu::TextureView>) = match &msaa_view {
                Some(msaa) => (msaa, Some(target_view)),
                None => (target_view, None),
            };
            let dv;
            let ds_attachment = if uses_stencil {
                dv = self.ds_view();
                // depth 也 Clear：部分后端在 depth_ops=None 时对未定义 depth 行为异常，
                // 且 glyphon 写 depth=0，需可预测的 depth 缓冲。
                // 每次 draw 独立建立并清理 stencil；multi-draw 只复用颜色 attachment。
                Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Discard,
                    }),
                })
            } else {
                None
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vireo render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target: resolve,
                    ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: ds_attachment,
                ..Default::default()
            });

            let vbuf = self.vertex_buf.borrow();
            let ibuf = self.index_buf.borrow();
            let instance_buf = self.instance_buf.borrow();
            let mut text_ctx = self.gpu.text_ctx.lock().unwrap();
            let engine_bg = &engine_storage_bind_group;
            let mut shapes_bound = false;
            let mut last_geometry: Option<bool> = None;
            let mut last_stencil_op: u32 = u32::MAX;
            let mut last_custom_ptr: *const Material = std::ptr::null();
            let mut last_dynamic_offsets = self.scratch_last_dynamic_offsets.borrow_mut();
            last_dynamic_offsets.clear();
            let mut last_text_mode: Option<crate::text::TextStencilMode> = None;
            let mut scissor_stack = self.scratch_scissor_stack.borrow_mut();
            scissor_stack.clear();
            scissor_stack.push((0, 0, self.physical_width, self.physical_height));

            for info in event_infos.iter() {
                // ScissorPush: 计算物理像素 scissor rect，与当前 scissor 求交
                if let Some(scissor_rect) = info.scissor_push {
                    let sx = self.physical_width as f32 / self.logical_width.max(1) as f32;
                    let sy = self.physical_height as f32 / self.logical_height.max(1) as f32;
                    let fw = self.physical_width as f32;
                    let fh = self.physical_height as f32;
                    // 负坐标 / 越界：先 float 裁到视口再转 u32，避免 as u32 回绕
                    let x0 = (scissor_rect.x * sx).clamp(0.0, fw);
                    let y0 = (scissor_rect.y * sy).clamp(0.0, fh);
                    let x1 = ((scissor_rect.x + scissor_rect.w) * sx).clamp(0.0, fw);
                    let y1 = ((scissor_rect.y + scissor_rect.h) * sy).clamp(0.0, fh);
                    let px = x0.floor() as u32;
                    let py = y0.floor() as u32;
                    let pr = x1.ceil() as u32;
                    let pb = y1.ceil() as u32;
                    let (cx, cy, cw, ch) = *scissor_stack.last().unwrap_or(&(0, 0, self.physical_width, self.physical_height));
                    let ix = px.max(cx);
                    let iy = py.max(cy);
                    let ir = pr.min(cx + cw);
                    let ib = pb.min(cy + ch);
                    let (nx, ny, nw, nh) = if ir > ix && ib > iy {
                        (ix, iy, ir - ix, ib - iy)
                    } else {
                        (0u32, 0u32, 0u32, 0u32)
                    };
                    pass.set_scissor_rect(nx, ny, nw, nh);
                    scissor_stack.push((nx, ny, nw, nh));
                }
                if info.scissor_pop {
                    scissor_stack.pop();
                    let (cx, cy, cw, ch) = *scissor_stack.last().unwrap_or(&(0, 0, self.physical_width, self.physical_height));
                    pass.set_scissor_rect(cx, cy, cw, ch);
                }

                if let Some(ref shape) = info.shape {
                    // Area 事件：op 3/4 来自 area_op；普通 batch/StencilPop：op 0..3 来自 stencil_op。
                    let pipe_op = info.area_op.unwrap_or(info.stencil_op);
                    let custom_ptr: *const Material = info.custom_material
                        .as_ref()
                        .map_or(std::ptr::null(), |m| Arc::as_ptr(m));
                    let use_custom = info.custom_material.is_some();
                    let need_rebind = !shapes_bound
                        || custom_ptr != last_custom_ptr
                        || (!use_custom && last_geometry != Some(shape.geometry))
                        || (uses_stencil && pipe_op != last_stencil_op)
                        || info.dynamic_offsets != *last_dynamic_offsets;
                    if need_rebind {
                        let tmp_pipe: wgpu::RenderPipeline;
                        let custom_pipe: Arc<wgpu::RenderPipeline>;
                        let pipe: &wgpu::RenderPipeline = if use_custom {
                            let mat = info.custom_material.as_ref().unwrap();
                            if uses_stencil {
                                custom_pipe = self.gpu.ensure_material_pipeline(
                                    mat,
                                    MaterialTarget::Shape,
                                    self.sample_count,
                                    self.alpha_to_coverage,
                                    self.ssaa,
                                    true,
                                    pipe_op.min(4),
                                );
                            } else {
                                custom_pipe = self.gpu.ensure_material_pipeline(
                                    mat,
                                    MaterialTarget::Shape,
                                    self.sample_count,
                                    self.alpha_to_coverage,
                                    self.ssaa,
                                    false,
                                    0,
                                );
                            }
                            &custom_pipe
                        } else if uses_stencil {
                            tmp_pipe = self.gpu.ensure_stencil_pipeline(
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                shape.geometry,
                                pipe_op.min(4),
                            );
                            &tmp_pipe
                        } else {
                            tmp_pipe = self.gpu.ensure_pipeline(
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                shape.geometry,
                            );
                            &tmp_pipe
                        };
                        pass.set_pipeline(pipe);
                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                        pass.set_bind_group(2, engine_bg, &[]);
                        if use_custom {
                            let mat = info.custom_material.as_ref().unwrap();
                            if let Some(bg) = mat.ensure_bind_group(&self.gpu.device, &self.gpu.queue, &self.gpu.bind_group_pool) {
                                pass.set_bind_group(3, &bg, &info.dynamic_offsets);
                            }
                        }
                        if let Some(vb) = vbuf.as_ref() {
                            pass.set_vertex_buffer(0, vb.slice(..));
                        }
                        if let Some(ib) = ibuf.as_ref() {
                            pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                        }
                        shapes_bound = true;
                        last_custom_ptr = custom_ptr;
                        last_geometry = Some(shape.geometry);
                        last_stencil_op = pipe_op;
                        last_dynamic_offsets.clone_from(&info.dynamic_offsets);
                    }
                    if uses_stencil {
                        pass.set_stencil_reference(info.stencil_ref);
                    }
                    for seg in &shape.segments {
                        pass.set_bind_group(1, &seg.bind_group, &[]);
                        pass.draw_indexed(
                            seg.ndx_start..seg.ndx_start + seg.ndx_count,
                            shape.base_vertex,
                            0..1,
                        );
                    }
                    if !shape.instances.is_empty() {
                        let instance_pipeline = self.gpu.ensure_instance_pipeline(
                            self.sample_count,
                            self.alpha_to_coverage,
                            self.ssaa,
                            uses_stencil,
                            pipe_op,
                        );
                        pass.set_pipeline(&instance_pipeline);
                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                        pass.set_bind_group(2, engine_bg, &[]);
                        pass.set_vertex_buffer(0, self.gpu.instance_quad_vertex_buf.slice(..));
                        pass.set_vertex_buffer(1, instance_buf.as_ref().unwrap().slice(..));
                        pass.set_index_buffer(
                            self.gpu.instance_quad_index_buf.slice(..),
                            wgpu::IndexFormat::Uint32,
                        );
                        if uses_stencil {
                            pass.set_stencil_reference(info.stencil_ref);
                        }
                        for segment in &shape.instances {
                            pass.set_bind_group(1, &segment.bind_group, &[]);
                            pass.draw_indexed(
                                0..6,
                                0,
                                segment.instance_start
                                    ..segment.instance_start + segment.instance_count,
                            );
                        }
                        shapes_bound = false;
                        last_geometry = None;
                    }
                }

                if !info.text.is_empty() {
                    // 有 DS 时：Push/Test 用 Equal；op=0（UI/unclipped）用 Always，避免误裁
                    // Area 存在时：当前文本在 Area content level，测 (Test)。
                    let has_area_at_text = info.area_op.is_some()
                        || info.stencil_op == 1
                        || info.stencil_op == 2;
                    let text_mode = if !uses_stencil {
                        crate::text::TextStencilMode::None
                    } else if has_area_at_text {
                        crate::text::TextStencilMode::Test
                    } else {
                        crate::text::TextStencilMode::Pass
                    };
                    if last_text_mode != Some(text_mode) {
                        text_ctx.ensure_text_stencil_mode(&self.gpu.device, text_mode);
                        last_text_mode = Some(text_mode);
                    }
                    // Push 后 mask 已 Inc：父文字测 new_level = ref+1
                    let text_ref = if info.stencil_op == 1 {
                        info.stencil_ref + 1
                    } else {
                        info.stencil_ref
                    };
                    // 必须在 set_pipeline（render_range 内）之后再 set_stencil_reference，
                    // 否则部分后端会把 ref 重置为 0。
                    let material_bg = info.custom_material.as_ref()
                        .and_then(|m| m.ensure_bind_group(&self.gpu.device, &self.gpu.queue, &self.gpu.bind_group_pool));
                    for segment in &info.text {
                        let _ = text_ctx.text_renderer.render_range_with_material(
                            &text_ctx.text_atlas,
                            &text_ctx.viewport,
                            &mut pass,
                            engine_bg,
                            segment.vertex_start,
                            segment.vertex_count,
                            if uses_stencil { Some(text_ref) } else { None },
                            segment.bind_group.as_ref(),
                            info.custom_text_pipeline.as_deref(),
                            material_bg.as_ref(),
                            &info.dynamic_offsets,
                        );
                    }
                    shapes_bound = false;
                    last_geometry = None;
                }
            }
        }

        encoder.finish()
    }

    /// 强制 GPU 端 PSO 编译（DX12 懒编译需要）。在 `resumed()` 创建窗口后调用。
    /// 用 SDF + geo 管线各画一个 dummy 三角形，触发 PSO 编译；
    /// 同时预热文字管线（cosmic_text shape + swash 光栅化 + atlas 上传）。
    pub fn preheat(&self, target: &RenderTarget, clear_color: crate::color::Color) {
        // 1. 文字预热：cosmic_text shape + swash 光栅化 + atlas GPU 上传。
        // 首帧 ~33ms 的 text prepare 在这里完成。
        self.gpu.text_ctx.lock().unwrap().preheat(
            &self.gpu.device,
            &self.gpu.queue,
            self.physical_width,
            self.physical_height,
        );

        // 2. PSO 预热：SDF + geo 管线各画一个 dummy 三角形。
        // Geo 路径（sdf_feather: None）
        let mut geo_batch = DrawBatch::new();
        geo_batch.sdf_feather = None;
        geo_batch.vertices.push(Vertex::new(0.0, 0.0, clear_color));
        geo_batch.vertices.push(Vertex::new(1.0, 0.0, clear_color));
        geo_batch.vertices.push(Vertex::new(0.0, 1.0, clear_color));
        geo_batch.indices.push(0);
        geo_batch.indices.push(1);
        geo_batch.indices.push(2);

        // SDF 路径（sdf_feather: Some(0.0)）
        let mut sdf_batch = DrawBatch::new();
        sdf_batch.sdf_feather = Some(0.0);
        sdf_batch.vertices.push(Vertex::new(0.0, 0.0, clear_color));
        sdf_batch.vertices.push(Vertex::new(1.0, 0.0, clear_color));
        sdf_batch.vertices.push(Vertex::new(0.0, 1.0, clear_color));
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

    fn ensure_instance_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut cap = self.instance_cap.borrow_mut();
        if *cap >= size { return; }
        let new_cap = if *cap == 0 { size.next_power_of_two() } else { (*cap * 2).max(size) };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo shape instance buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *self.instance_buf.borrow_mut() = Some(buffer);
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
        *self.engine_storage_bind_group_cache.borrow_mut() = None;
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
        *self.engine_storage_bind_group_cache.borrow_mut() = None;
    }
}

use crate::text::{TextDef, TextEntryList};

/// 形状仿射变换：线性部分 + 平移 + 局部 pivot。
///
/// 线性 `[a b; c d]` 常为 `sx*cos, -sx*sin; sy*sin, sy*cos`（由 [`Transform::trs`] 构建）。
/// 顶点变换顺序：`T(x,y) * [a b; c d] * T(-pivot)`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// 线性部分：`[a b; c d]`
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    /// 世界坐标位置（局部坐标系原点）
    pub x: f32,
    pub y: f32,
    /// 局部空间旋转/缩放中心
    pub px: f32,
    pub py: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// 单位变换。
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        x: 0.0,
        y: 0.0,
        px: 0.0,
        py: 0.0,
    };

    /// 仅平移。
    pub fn translation(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            ..Self::IDENTITY
        }
    }

    /// 从平移 / pivot / 旋转（弧度，顺时针）/ 缩放构建。
    pub fn trs(x: f32, y: f32, px: f32, py: f32, rotation: f32, sx: f32, sy: f32) -> Self {
        let (c, s) = (rotation.cos(), rotation.sin());
        Self {
            a: sx * c,
            b: -sx * s,
            c: sy * s,
            d: sy * c,
            x,
            y,
            px,
            py,
        }
    }

    /// 原始 3×3 仿射 6 分量（列主序语义：`[a b tx; c d ty; 0 0 1]`），pivot 归零。
    pub fn matrix(a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) -> Self {
        Self {
            a,
            b,
            c,
            d,
            x: tx,
            y: ty,
            px: 0.0,
            py: 0.0,
        }
    }

    /// 返回 3x3 仿射变换矩阵的 3 个列（WGSL 列主序）。
    /// 变换顺序：T(x,y) * [a b; c d] * T(-pivot)，即 pivot → 线性 → 平移。
    pub(crate) fn to_cols(&self) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let tx = self.x - self.px * self.a - self.py * self.b;
        let ty = self.y - self.px * self.c - self.py * self.d;
        ([self.a, self.c, 0.0], [self.b, self.d, 0.0], [tx, ty, 1.0])
    }

    /// 组合：先应用 `child`，再应用 `self`（`M = self * child`，pivot 已烘焙进平移）。
    pub fn then(&self, child: &Transform) -> Transform {
        let (p0, p1, p2) = self.to_cols();
        let (c0, c1, c2) = child.to_cols();
        let (r0, r1, r2) = mul_affine_cols(p0, p1, p2, c0, c1, c2);
        Transform::matrix(r0[0], r1[0], r0[1], r1[1], r2[0], r2[1])
    }
}

/// `P * C` 的列向量（2D 仿射，第三行视为 0 0 1）。
fn mul_affine_cols(
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    c0: [f32; 3],
    c1: [f32; 3],
    c2: [f32; 3],
) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let mul = |c: [f32; 3]| -> [f32; 3] {
        [
            p0[0] * c[0] + p1[0] * c[1] + p2[0] * c[2],
            p0[1] * c[0] + p1[1] * c[1] + p2[1] * c[2],
            0.0,
        ]
    };
    let mut r0 = mul([c0[0], c0[1], 0.0]);
    let mut r1 = mul([c1[0], c1[1], 0.0]);
    let mut r2 = mul([c2[0], c2[1], 1.0]);
    r0[2] = 0.0;
    r1[2] = 0.0;
    r2[2] = 1.0;
    (r0, r1, r2)
}

/// 子 batch 从父继承哪些画笔 / 裁切行为。
///
/// 挂在子上：`child.inherit = …`，再 `parent.push_child(child)`。
/// 画笔类标志（`transform` / `color` / `sdf_feather` / `uv`）在 **`push_child` 时**写入子侧；
/// `clipped` 在 **`Renderer::draw` 时**决定是否测祖先 stencil。
///
/// # 字段
///
/// | 字段 | 默认（`NONE`） | 效果 |
/// |------|:--------------:|------|
/// | `transform` | false | 整棵子树 `transform_table` + 画笔 transform **左乘**父矩阵 |
/// | `color` | false | 子画笔色 = 父色（**已生成**顶点颜色不变） |
/// | `sdf_feather` | false | 子 `sdf_feather` = 父值 |
/// | `uv` | false | 子 `uv` = 父值 |
/// | `clipped` | **true** | 祖先有 mask 时测 stencil；`false` 可画出裁切区外 |
///
/// 不含「相对父包围盒左上角」的局部坐标。
///
/// # 与 `clips_children` 的分工
///
/// - 父 [`DrawBatch::clips_children`]` = true`：父几何 **写** stencil mask
/// - 子 `inherit.clipped`：是否 **测** 该 mask（默认 true）
///
/// 同一父下可混用 clipped / unclipped 多个子。
///
/// # 预设与链式开关
///
/// ```
/// use vireo::prelude::*;
///
/// // 预设
/// let _ = InheritFromParent::NONE;      // 不继承画笔，仍参与裁切
/// let _ = InheritFromParent::TRANSFORM; // 仅 transform
/// let _ = InheritFromParent::ALL;       // 画笔全开 + clipped
///
/// // 链式：开 / 关 成对
/// let a = InheritFromParent::NONE.color().sdf_feather();
/// let b = InheritFromParent::ALL.no_color().unclipped();
/// let c = InheritFromParent::TRANSFORM.unclipped();
/// assert!(a.color && a.sdf_feather && a.clipped);
/// assert!(!b.color && !b.clipped && b.transform);
/// assert!(c.transform && !c.clipped);
/// ```
///
/// # 基本用法
///
/// ```
/// use vireo::prelude::*;
///
/// let mut parent = DrawBatch::new();
/// parent.sdf_feather = Some(1.0);
/// parent.set_color(ORANGE);
/// parent.set_position(100.0, 80.0);
/// parent.set_deg(15.0);
/// parent.clips_children = true;
/// draw_rounded_rect(&mut parent, -40.0, -40.0, 80.0, 80.0, 12.0, Some(GRAY));
///
/// // 子：局部坐标；跟父转；测裁切
/// let mut child = DrawBatch::new();
/// child.inherit = InheritFromParent::TRANSFORM;
/// draw_rectangle(&mut child, -10.0, -10.0, 20.0, 20.0, Some(SKYBLUE));
/// parent.push_child(child);
///
/// // 另一子：不测 stencil，可越界
/// let mut overflow = DrawBatch::new();
/// overflow.inherit = InheritFromParent::TRANSFORM.unclipped();
/// draw_circle(&mut overflow, 50.0, 0.0, 12.0, Some(RED));
/// parent.push_child(overflow);
/// ```
///
/// # 画笔继承时机
///
/// `color` / `sdf_feather` / `uv` 在 `push_child` 时才写入子画笔。
/// 若要在**生成顶点之前**用父色/柔边，请在 `draw_*` 前自行赋值，或先写再画：
///
/// ```
/// use vireo::prelude::*;
///
/// let mut parent = DrawBatch::new();
/// parent.set_color(ORANGE);
/// parent.sdf_feather = Some(2.0);
///
/// let mut child = DrawBatch::new();
/// child.inherit = InheritFromParent::NONE.color().sdf_feather();
/// // 需要影响本批顶点时，在 push 前同步：
/// child.set_color(parent.color);
/// child.sdf_feather = parent.sdf_feather;
/// draw_circle(&mut child, 0.0, 0.0, 20.0, None);
/// parent.push_child(child); // 再写一次画笔无妨
/// ```
///
/// # 另见
///
/// 交互示例：`cargo run --example batch_inherit`、`batch_clip`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InheritFromParent {
    /// 左乘父变换到子树（含已画顶点的 `transform_table`）。
    pub transform: bool,
    /// 覆盖子画笔 `color`（已 bake 进顶点的颜色不变）。
    pub color: bool,
    /// 覆盖子 `sdf_feather`。
    pub sdf_feather: bool,
    /// 覆盖子 `uv`。
    pub uv: bool,
    /// 父（祖先）有裁切区时：`true` = 测 stencil；`false` = 不测（可越界）。默认 `true`。
    pub clipped: bool,
}

impl Default for InheritFromParent {
    fn default() -> Self {
        Self::NONE
    }
}

impl InheritFromParent {
    /// 不继承画笔；仍默认参与父裁切（`clipped = true`）。
    ///
    /// ```
    /// use vireo::prelude::InheritFromParent;
    /// assert!(!InheritFromParent::NONE.transform);
    /// assert!(InheritFromParent::NONE.clipped);
    /// ```
    pub const NONE: Self = Self {
        transform: false,
        color: false,
        sdf_feather: false,
        uv: false,
        clipped: true,
    };
    /// 画笔全继承 + 参与裁切。
    ///
    /// ```
    /// use vireo::prelude::InheritFromParent;
    /// let i = InheritFromParent::ALL;
    /// assert!(i.transform && i.color && i.sdf_feather && i.uv && i.clipped);
    /// ```
    pub const ALL: Self = Self {
        transform: true,
        color: true,
        sdf_feather: true,
        uv: true,
        clipped: true,
    };
    /// 仅继承 transform，参与裁切。
    ///
    /// ```
    /// use vireo::prelude::InheritFromParent;
    /// let i = InheritFromParent::TRANSFORM;
    /// assert!(i.transform && !i.color && i.clipped);
    /// ```
    pub const TRANSFORM: Self = Self {
        transform: true,
        color: false,
        sdf_feather: false,
        uv: false,
        clipped: true,
    };

    /// 开启继承父 transform。
    pub const fn transform(mut self) -> Self {
        self.transform = true;
        self
    }
    /// 关闭继承父 transform。
    pub const fn no_transform(mut self) -> Self {
        self.transform = false;
        self
    }
    /// 开启继承父画笔色。
    pub const fn color(mut self) -> Self {
        self.color = true;
        self
    }
    /// 关闭继承父画笔色。
    pub const fn no_color(mut self) -> Self {
        self.color = false;
        self
    }
    /// 开启继承父 `sdf_feather`。
    pub const fn sdf_feather(mut self) -> Self {
        self.sdf_feather = true;
        self
    }
    /// 关闭继承父 `sdf_feather`。
    pub const fn no_sdf_feather(mut self) -> Self {
        self.sdf_feather = false;
        self
    }
    /// 开启继承父 `uv`。
    pub const fn uv(mut self) -> Self {
        self.uv = true;
        self
    }
    /// 关闭继承父 `uv`。
    pub const fn no_uv(mut self) -> Self {
        self.uv = false;
        self
    }
    /// 参与父 stencil 裁切（默认）。
    ///
    /// ```
    /// use vireo::prelude::InheritFromParent;
    /// assert!(InheritFromParent::NONE.unclipped().clipped().clipped);
    /// ```
    pub const fn clipped(mut self) -> Self {
        self.clipped = true;
        self
    }
    /// 不测父 stencil，可画出裁切区外。
    ///
    /// ```
    /// use vireo::prelude::InheritFromParent;
    /// let i = InheritFromParent::ALL.unclipped();
    /// assert!(!i.clipped && i.transform);
    /// ```
    pub const fn unclipped(mut self) -> Self {
        self.clipped = false;
        self
    }

    /// 是否有需在 `push_child` 写入的画笔继承（不含 `clipped`，裁切在 draw 时生效）。
    ///
    /// ```
    /// use vireo::prelude::InheritFromParent;
    /// assert!(!InheritFromParent::NONE.any());
    /// assert!(InheritFromParent::NONE.color().any());
    /// assert!(!InheritFromParent::NONE.unclipped().any()); // 仅改 clipped
    /// ```
    #[inline]
    pub fn any(self) -> bool {
        self.transform || self.color || self.sdf_feather || self.uv
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

/// 单位矩阵一行（12 f32，mat3x3 列 vec4-padded），用作 `transform_table` 槽 0。
const IDENTITY_TRANSFORM_ROW: [f32; 12] = [
    1.0, 0.0, 0.0, 0.0, // col0
    0.0, 1.0, 0.0, 0.0, // col1
    0.0, 0.0, 1.0, 0.0, // col2
];

/// 清空并写入槽 0 = 单位矩阵；`transform_map` 同步登记 index 0。
///
/// **约定**：batch 内 `transform_index == 0` 恒表示单位变换（与 `Renderer` 全局表槽 0 一致）。
/// `draw_text` / glyphon 默认写 0，必须不能被第一个形状的平移占用。
fn seed_identity_transform_table(table: &mut Vec<f32>, map: &mut FxHashMap<u64, u32>) {
    table.clear();
    map.clear();
    table.extend_from_slice(&IDENTITY_TRANSFORM_ROW);
    let key = transform_key(
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
    );
    map.insert(key, 0);
}

/// Bottom-up 计算 batch 整棵子树的 AABB（含子顶点），存入 map 供 culling 使用。
fn compute_subtree_aabb(
    batch: &DrawBatch,
    map: &mut FxHashMap<usize, Option<Rect>>,
) -> Option<Rect> {
    let key = batch as *const DrawBatch as *const () as usize;
    let own = batch.compute_own_world_aabb();
    let mut combined = own;
    for child in &batch.children {
        let ca = compute_subtree_aabb(child, map);
        if let Some(c) = ca {
            combined = match combined {
                Some(a) => Some(a.union(&c)),
                None => Some(c),
            };
        }
    }
    map.insert(key, combined);
    combined
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
/// 根 batch 列表按顺序叠加；每个 batch 可含 `children`，绘制顺序为
/// **父 shapes → 父 texts → 子树（递归）**。
#[derive(Clone)]
struct TextureSegment {
    ndx_start: u32,
    ndx_count: u32,
    /// `None` = 白纹理路径（draw 时解析为 `gpu.white_bind_group`）
    bind_group: Option<wgpu::BindGroup>,
}

#[derive(Clone)]
struct InstanceTextureSegment {
    instance_start: u32,
    instance_count: u32,
    bind_group: Option<wgpu::BindGroup>,
}

#[derive(Clone)]
pub struct DrawBatch {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub texts: TextEntryList,
    pub(crate) bind_group: Option<wgpu::BindGroup>,
    pub(crate) text_texture_view: Option<wgpu::TextureView>,
    texture_segments: Vec<TextureSegment>,
    pub(crate) instances: Vec<ShapeInstance>,
    instance_texture_segments: Vec<InstanceTextureSegment>,
    pub(crate) transform: Option<Transform>,
    /// SDF 柔边宽度（逻辑像素，`None` = 几何光栅化模式，不走 SDF）。
    /// 默认值为 `Some(1.0)`；需要几何路径时显式设为 `None`。
    ///
    /// 注意：SDF 图形不受 MSAA 影响。
    pub sdf_feather: Option<f32>,
    /// 当前画笔颜色；`draw_*(…, None)` 使用此值。
    pub color: crate::color::Color,
    /// 纹理坐标子区域：后续 shape 顶点 UV，以及之后 text 入队冻结的
    /// [`crate::text::TextTextureState::uv`]，均在此范围内映射。
    pub uv: UvRect,
    /// 多边形的边数据：每条边 4 个 f32 (nx, ny, dot(vi,n), 0)
    /// 由 draw_polygon 填充，渲染时合并到 storage buffer。
    pub polygon_edges: Vec<f32>,
    /// 变换矩阵表（batch 内去重）。每个矩阵 12 f32（mat3x3，列 vec4-padded）。
    ///
    /// **槽 0 固定为单位矩阵**（`new`/`clear` 时写入，形状从 1 起占用）：
    /// - `transform_index == 0` = 恒等（与全局 `Renderer` 表槽 0、`draw_text` 默认 0 一致）
    /// - 切勿把第一个形状的平移写进槽 0，否则 `draw_text` 会二次平移（右下偏）
    /// - `push_child` 左乘父矩阵时会改写整表（含槽 0）；继承后槽 0 = 父变换，语义仍正确
    pub(crate) transform_table: Vec<f32>,
    /// hash → local index 映射（batch 内去重）。恒等矩阵始终映射到 0。
    transform_map: FxHashMap<u64, u32>,
    /// 是否含 SDF 顶点（避免 draw 时全表扫描）。
    pub(crate) has_sdf: bool,
    /// 当前 transform 的已注册 index 缓存；transform 变更时失效。
    pub(crate) cached_transform_index: Option<u32>,
    /// 子 batch（绘制顺序：本 batch 的 shapes → texts → 各 child 递归）。
    pub children: Vec<DrawBatch>,
    /// 若为 `true`，本 batch 的几何将作为子 batch 的裁切区（stencil 裁剪）。
    /// 默认 `false`（仅顺序层叠，不裁切）。
    pub clips_children: bool,
    /// 被 `push_child` 挂到父下时，从父写入本节点（及 transform 时整棵子树）的属性。
    pub inherit: InheritFromParent,
    /// 本 batch 可见区 include（`None` = Full）。与 `area_exclude` 合成有效 Area。
    /// 与 `clips_children` 正交：可见 = 祖先 stencil ∧ 有效 Area。
    pub area_include: Option<Area>,
    /// 本 batch 可见区 exclude（`None` = Empty）。
    pub area_exclude: Option<Area>,
    /// 本 batch 子树 AABB 模式：
    /// - `None`：不裁剪（始终绘制）
    /// - `Some(None)`：自动计算当前 batch 及其子树的世界坐标 AABB，会应用 transform
    /// - `Some(Some(rect))`：手动指定最终的世界坐标轴对齐 AABB；不会再应用 batch
    ///   或父节点 transform，transform 改变后需由调用者同步更新
    pub bounds: Option<Option<Rect>>,
    /// 当 `clips_children=true` 时，用此矩形 scissor 代替 stencil。
    /// 逻辑世界坐标，必须轴对齐。`None` = 走 stencil。
    pub scissor: Option<Rect>,
    /// 文本裁剪默认值。启用后，所有 `text()`/`text_stable()` 中字元超出此区域的部分
    /// 会被 CPU 裁切（glyphon per-glyph clip）。`None` = 不裁。
    /// 可通过 `TextOverride.clip` 单条覆盖。
    pub text_clip: Option<crate::glyphon::TextBounds>,
    /// 自定义材质。`Some` 时该 batch 的 shape/text 都走自定义材质 fragment shader。
    /// shape 仍用对应顶点管线；text 仍走 glyphon 顶点管线。
    /// `None` = 内置。
    /// 与 `clips_children` / Area stencil 兼容（自有 stencil pipeline 缓存）。
    pub custom_material: Option<Arc<Material>>,
    /// Dynamic uniform/storage offsets for group 3 binding（逐 draw 偏移，字节）。
    /// 长度必须等于 BGL 中 `has_dynamic_offset` 的 binding 数量。
    pub dynamic_offsets: Vec<u32>,
}

impl DrawBatch {
    pub fn new() -> Self {
        let mut transform_table = Vec::with_capacity(48);
        let mut transform_map = FxHashMap::default();
        seed_identity_transform_table(&mut transform_table, &mut transform_map);
        Self {
            vertices: Vec::with_capacity(64),
            indices: Vec::with_capacity(96),
            texts: TextEntryList::new(),
            bind_group: None,
            text_texture_view: None,
            texture_segments: Vec::with_capacity(2),
            instances: Vec::with_capacity(32),
            instance_texture_segments: Vec::with_capacity(2),
            transform: None,
            sdf_feather: Some(1.0),
            color: crate::color::colors::WHITE,
            uv: UvRect::default(),
            polygon_edges: Vec::with_capacity(16),
            transform_table,
            transform_map,
            has_sdf: false,
            cached_transform_index: None,
            children: Vec::new(),
            clips_children: false,
            inherit: InheritFromParent::NONE,
            area_include: None,
            area_exclude: None,
            bounds: Some(None),
            scissor: None,
            text_clip: None,
            custom_material: None,
            dynamic_offsets: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.texts.clear();
        self.bind_group = None;
        self.text_texture_view = None;
        self.texture_segments.clear();
        self.instances.clear();
        self.instance_texture_segments.clear();
        self.transform = None;
        self.sdf_feather = Some(1.0); // 与 new() 一致：SDF 路径
        self.color = crate::color::colors::WHITE;
        self.uv = UvRect::default();
        self.polygon_edges.clear();
        seed_identity_transform_table(&mut self.transform_table, &mut self.transform_map);
        self.has_sdf = false;
        self.cached_transform_index = None;
        self.children.clear();
        self.clips_children = false;
        self.inherit = InheritFromParent::NONE;
        self.area_include = None;
        self.area_exclude = None;
        self.bounds = Some(None);
        self.scissor = None;
        self.text_clip = None;
        self.custom_material = None;
        self.dynamic_offsets.clear();
    }
    pub fn to_area(&self) -> Area {
        if (self.vertices.is_empty() || self.indices.is_empty()) && self.instances.is_empty() {
            return Area::Empty;
        }
        let mut vertices = self.vertices.clone();
        let mut indices = self.indices.clone();
        for instance in &self.instances {
            let base = vertices.len() as u32;
            let [x0, y0, x1, y1] = instance.bounds;
            let [ux0, uy0, ux1, uy1] = instance.uv_bounds;
            let [u0, v0, u1, v1] = instance.uv_rect;
            for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
                let u = u0 + (x - ux0) / (ux1 - ux0) * (u1 - u0);
                let v = v0 + (y - uy0) / (uy1 - uy0) * (v1 - v0);
                let mut vertex = Vertex::new_uv_xform(
                    x,
                    y,
                    u,
                    v,
                    crate::color::Color::new(instance.color[0], instance.color[1], instance.color[2], instance.color[3]),
                    instance.transform_index,
                );
                vertex.sdf_params = instance.sdf_params;
                vertex.sdf_type = instance.sdf_type;
                vertex.sdf_feather = instance.sdf_feather;
                vertex.sdf_extra = instance.sdf_extra;
                vertices.push(vertex);
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        Area::geom(AreaGeom {
            vertices,
            indices,
            transform_table: self.transform_table.clone(),
            polygon_edges: self.polygon_edges.clone(),
            has_sdf: self.has_sdf,
            sdf_feather: self.sdf_feather,
        })
    }

    /// 有效可见区：`include.unwrap_or(Full) \ exclude.unwrap_or(Empty)`；皆 None 则 `None`。
    pub fn effective_area(&self) -> Option<Area> {
        effective_area(self.area_include.as_ref(), self.area_exclude.as_ref())
    }

    /// 从 `transform_table` 取 index 对应列（越界 → 单位阵）。
    #[inline]
    fn table_cols_at(table: &[f32], idx: u32) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let base = idx as usize * 12;
        if base + 12 > table.len() {
            return ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        }
        let t = &table[base..base + 12];
        ([t[0], t[1], 0.0], [t[4], t[5], 0.0], [t[8], t[9], 1.0])
    }

    /// 顶点局部坐标 × 表内矩阵 → 世界坐标。
    #[inline]
    fn world_xy(table: &[f32], idx: u32, lx: f32, ly: f32) -> (f32, f32) {
        let (c0, c1, c2) = Self::table_cols_at(table, idx);
        (
            c0[0] * lx + c1[0] * ly + c2[0],
            c0[1] * lx + c1[1] * ly + c2[1],
        )
    }

    /// 计算本 batch 自身在**世界（绝对逻辑）空间**的 AABB。
    /// 含形状顶点 + 文字近似框（pos / 字号）；不含子节点。
    /// 按 `transform_index` 查表，不用画笔 `current_matrix`。
    fn compute_own_world_aabb(&self) -> Option<Rect> {
        let mut w_min_x = f32::INFINITY;
        let mut w_max_x = f32::NEG_INFINITY;
        let mut w_min_y = f32::INFINITY;
        let mut w_max_y = f32::NEG_INFINITY;
        let mut any = false;
        let expand = |w_min_x: &mut f32, w_max_x: &mut f32, w_min_y: &mut f32, w_max_y: &mut f32, wx: f32, wy: f32| {
            if wx < *w_min_x { *w_min_x = wx; }
            if wx > *w_max_x { *w_max_x = wx; }
            if wy < *w_min_y { *w_min_y = wy; }
            if wy > *w_max_y { *w_max_y = wy; }
        };
        for v in &self.vertices {
            any = true;
            let (wx, wy) = Self::world_xy(
                &self.transform_table,
                v.transform_index,
                v.position[0],
                v.position[1],
            );
            expand(&mut w_min_x, &mut w_max_x, &mut w_min_y, &mut w_max_y, wx, wy);
        }
        for instance in &self.instances {
            any = true;
            let [x0, y0, x1, y1] = instance.bounds;
            for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
                let (wx, wy) = Self::world_xy(&self.transform_table, instance.transform_index, x, y);
                expand(&mut w_min_x, &mut w_max_x, &mut w_min_y, &mut w_max_y, wx, wy);
            }
        }
        // 文字：逻辑 pos + 近似行高/宽（未 shape 前保守估计，避免纯文字被误裁）
        for entry in &self.texts.entries {
            any = true;
            let p = entry.pos();
            let ti = entry.transform_index();
            let fs = entry.approx_font_size();
            // 行数估算：Normal 按 max_width 折行；Parts/Stable 永远单行
            let lines = entry.approx_line_count();
            // 宽：用 max_width（若设）或自然宽度（Parts/Stable 无 max_width 概念）
            let tw = entry.approx_width();
            let th = lines as f32 * fs * 1.25;
            // 叠加 TextOverride.transform（与 prepare_texts 中 phys_transform_index_with_override 一致）
            let (mc0, mc1, mc2) = match entry.override_().transform.as_ref() {
                Some(ov) => {
                    let base = ti as usize * 12;
                    let m = if base + 12 <= self.transform_table.len() {
                        let t = &self.transform_table[base..base + 12];
                        Transform::matrix(t[0], t[4], t[1], t[5], t[8], t[9])
                    } else {
                        Transform::IDENTITY
                    };
                    let composed = m.then(ov);
                    composed.to_cols()
                }
                None => Self::table_cols_at(&self.transform_table, ti),
            };
            for (lx, ly) in [(p.x, p.y), (p.x + tw, p.y), (p.x, p.y + th), (p.x + tw, p.y + th)] {
                let wx = mc0[0] * lx + mc1[0] * ly + mc2[0];
                let wy = mc0[1] * lx + mc1[1] * ly + mc2[1];
                expand(&mut w_min_x, &mut w_max_x, &mut w_min_y, &mut w_max_y, wx, wy);
            }
        }
        if !any || w_min_x > w_max_x {
            return None;
        }
        Some(Rect::new(w_min_x, w_min_y, w_max_x - w_min_x, w_max_y - w_min_y))
    }

    /// flatten / draw 共用：是否走 scissor 代替本层 stencil Push。
    /// - 显式 `scissor`：独立于 `clips_children`
    /// - auto-scissor（单矩形检测）：仍要求 `clips_children=true`
    fn uses_scissor_path(&self, has_area: bool) -> bool {
        if has_area {
            return false;
        }
        self.scissor.is_some() || (self.clips_children && self.auto_scissor().is_some())
    }

    /// 检测 batch 自身是否只含一个**几何**轴对齐矩形（4 顶点 + 无旋转）。
    /// SDF 四顶点 AABB 不走 scissor（圆/圆角等须 stencil）。
    /// 世界位置来自顶点 `transform_index`（非画笔）。
    fn auto_scissor(&self) -> Option<Rect> {
        // SDF 填充也是 4v/6i 外接框 → 禁止 auto-scissor，否则圆裁成方
        if self.has_sdf || self.sdf_feather.is_some() {
            return None;
        }
        if self.vertices.len() != 4 || self.indices.len() != 6 {
            return None;
        }
        if self.vertices.iter().any(|v| v.sdf_type != 0) {
            return None;
        }
        let ti = self.vertices[0].transform_index;
        if self.vertices.iter().any(|v| v.transform_index != ti) {
            return None;
        }
        let (c0, c1, _c2) = Self::table_cols_at(&self.transform_table, ti);
        // 表内线性部分无旋转/倾斜（b=c1[0], c=c0[1]）
        if c1[0].abs() > 1e-6 || c0[1].abs() > 1e-6 {
            return None;
        }
        let mut worlds: [(f32, f32); 4] = [(0.0, 0.0); 4];
        for (i, v) in self.vertices.iter().enumerate() {
            worlds[i] = Self::world_xy(
                &self.transform_table,
                ti,
                v.position[0],
                v.position[1],
            );
        }
        let w_min_x = worlds.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let w_max_x = worlds.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
        let w_min_y = worlds.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let w_max_y = worlds.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
        // 四角应对应轴对齐 AABB 的四个角
        let corners = [
            (w_min_x, w_min_y),
            (w_max_x, w_min_y),
            (w_max_x, w_max_y),
            (w_min_x, w_max_y),
        ];
        let mut found = [false; 4];
        for &(wx, wy) in &worlds {
            let matched = corners.iter().position(|&(cx, cy)| {
                (wx - cx).abs() < 1e-4 && (wy - cy).abs() < 1e-4
            });
            match matched {
                Some(i) => found[i] = true,
                None => return None,
            }
        }
        if !found.iter().all(|&x| x) {
            return None;
        }
        Some(Rect::new(w_min_x, w_min_y, w_max_x - w_min_x, w_max_y - w_min_y))
    }

    /// 追加子 batch。若 `child.inherit` 有标志，先把父属性写入子（transform 作用于整棵子树）。
    pub fn push_child(&mut self, mut child: DrawBatch) {
        if child.inherit.any() {
            child.apply_inherit_from(self);
        }
        self.children.push(child);
    }

    /// 指定继承标志后追加（覆盖 `child.inherit`）。
    pub fn push_child_with(&mut self, mut child: DrawBatch, inherit: InheritFromParent) {
        child.inherit = inherit;
        self.push_child(child);
    }

    /// 按 `self.inherit` 从 `parent` 写入本节点（及 transform 时递归子树）。
    fn apply_inherit_from(&mut self, parent: &DrawBatch) {
        let flags = self.inherit;
        if flags.color {
            self.color = parent.color;
        }
        if flags.sdf_feather {
            self.sdf_feather = parent.sdf_feather;
        }
        if flags.uv {
            self.uv = parent.uv;
        }
        if flags.transform {
            let p = parent.transform.unwrap_or(Transform::IDENTITY);
            self.left_mul_transform_tree(&p);
        }
    }

    /// 整棵子树 transform 左乘 `parent`（已画顶点的 `transform_table` + 画笔 transform）。
    /// 会改写整表含槽 0：继承后槽 0 从单位阵变为父变换（`draw_text` 默认 0 = 局部恒等 → 世界父变换）。
    fn left_mul_transform_tree(&mut self, parent: &Transform) {
        let (p0, p1, p2) = parent.to_cols();
        // 槽 0 在 new/clear 时已是单位阵；无需再因「空表」补恒等。
        let n = self.transform_table.len() / 12;
        for i in 0..n {
            let base = i * 12;
            let t = &self.transform_table[base..base + 12];
            let c0 = [t[0], t[1], 0.0];
            let c1 = [t[4], t[5], 0.0];
            let c2 = [t[8], t[9], 1.0];
            let (r0, r1, r2) = mul_affine_cols(p0, p1, p2, c0, c1, c2);
            self.transform_table[base] = r0[0];
            self.transform_table[base + 1] = r0[1];
            self.transform_table[base + 2] = 0.0;
            self.transform_table[base + 3] = 0.0;
            self.transform_table[base + 4] = r1[0];
            self.transform_table[base + 5] = r1[1];
            self.transform_table[base + 6] = 0.0;
            self.transform_table[base + 7] = 0.0;
            self.transform_table[base + 8] = r2[0];
            self.transform_table[base + 9] = r2[1];
            self.transform_table[base + 10] = 1.0;
            self.transform_table[base + 11] = 0.0;
        }
        self.rebuild_transform_map();
        let local = self.transform.unwrap_or(Transform::IDENTITY);
        self.transform = Some(parent.then(&local));
        self.cached_transform_index = None;
        for child in &mut self.children {
            child.left_mul_transform_tree(parent);
        }
    }

    fn rebuild_transform_map(&mut self) {
        self.transform_map.clear();
        let n = self.transform_table.len() / 12;
        for i in 0..n {
            let base = i * 12;
            let t = &self.transform_table[base..base + 12];
            let c0 = [t[0], t[1], 0.0];
            let c1 = [t[4], t[5], 0.0];
            let c2 = [t[8], t[9], 1.0];
            let key = transform_key(c0, c1, c2);
            self.transform_map.entry(key).or_insert(i as u32);
        }
    }

    /// 本节点或任意子孙是否含形状/文字。
    pub fn has_drawable_content(&self) -> bool {
        !self.vertices.is_empty()
            || !self.instances.is_empty()
            || !self.texts.entries.is_empty()
            || self.children.iter().any(Self::has_drawable_content)
    }

    /// 旧 API：返回 `Some(batch)` / `None`（Pop）。已由 [`Self::flatten_events`] 取代；
    /// 仅供 tests 中验证 Push/Pop 顺序使用。
    #[cfg(test)]
    pub(crate) fn flatten_with_pop<'a>(&'a self, out: &mut Vec<Option<&'a DrawBatch>>) {
        out.push(Some(self));
        let child_start = out.len();
        for child in &self.children {
            child.flatten_with_pop(out);
        }
        if self.clips_children && out.len() > child_start {
            out.push(None);
        }
    }

    /// 扩展版：额外为有 effective Area 的 batch 输出 AreaSetup（子树前）/ AreaCleanup（子树后）。
    /// 每个 AreaStencilOp 展平为独立 event，复用 shape 路径渲染。
    /// empty Area 不发 AreaSetup/AreaCleanup。
    ///
    /// `level` 是本 batch 的「祖先 stencil level」：
    ///   - cover 在 `level` 处写，area 内 level → level+1
    ///   - erase 在 `level+1` 处写，恢复 level
    ///   - 若自身有 Area，子树看到 level+1（content level）
    ///   - 若自身有 Area 且 clips_children，Push 在 level+1，子看 level+2
    ///
    /// `aabb_map` 是 pre-pass 计算的子树 AABB 表（`compute_subtree_aabb`）。
    ///   bounds 优先 > map 内子树 AABB > 自身顶点
    pub(crate) fn flatten_events<'a>(
        &'a self,
        out: &mut Vec<DrawEvent<'a>>,
        level: u32,
        viewport: Option<Rect>,
        aabb_map: &FxHashMap<usize, Option<Rect>>,
    ) {
        // Culling: 跳过屏外子树
        if let Some(vp) = viewport {
            let effective = match self.bounds {
                None => None,
                Some(None) => {
                    let key = self as *const DrawBatch as *const () as usize;
                    aabb_map.get(&key).copied().flatten()
                        .or_else(|| self.compute_own_world_aabb())
                }
                Some(Some(b)) => Some(b),
            };
            if let Some(b) = effective {
                if !vp.intersects(&b) {
                    return;
                }
            }
        }

        let area = self.effective_area();
        let has_area = matches!(&area, Some(a) if !a.is_empty());
        let use_scissor = self.uses_scissor_path(has_area);
        let effective_clip = if use_scissor {
            self.scissor.or_else(|| self.auto_scissor())
        } else {
            None
        };
        if let Some(a) = &area {
            if !a.is_empty() {
                let mut ops = Vec::new();
                a.compile_cover(level, &mut ops);
                for op in ops {
                    out.push(DrawEvent::AreaOp { op, is_setup: true });
                }
            }
        }
        out.push(DrawEvent::Batch(self));
        let has_geom = !self.vertices.is_empty() || !self.instances.is_empty();
        // 子树 stencil base（与 draw 的 content_level 抬升一致）：
        // Area cover → +1；clips_children 且走 stencil Push → 再 +1；scissor 不抬 stencil。
        let child_level = level
            + (has_area as u32)
            + if self.clips_children && !use_scissor && has_geom {
                1
            } else {
                0
            };
        // scissor 仅包住子节点；无子则不发 Push/Pop
        if use_scissor && !self.children.is_empty() {
            if let Some(r) = effective_clip {
                out.push(DrawEvent::ScissorPush(r));
            }
        }
        for child in &self.children {
            child.flatten_events(out, child_level, viewport, aabb_map);
        }
        if use_scissor && !self.children.is_empty() {
            out.push(DrawEvent::ScissorPop);
        } else if self.clips_children && has_geom && !use_scissor {
            // 与 draw Push 成对：即使子全被 cull 也要 Pop，避免 clip_depth 泄漏
            out.push(DrawEvent::StencilPop);
        }
        if has_area {
            if let Some(a) = area {
                if !a.is_empty() {
                    let mut ops = Vec::new();
                    // cover 把 level 抬到 level+1；erase 在 level+1 上 Dec 回 level。
                    a.compile_erase(level + 1, &mut ops);
                    for op in ops {
                        out.push(DrawEvent::AreaOp { op, is_setup: false });
                    }
                }
            }
        }
    }

    /// 旧版：前序 DFS 扁平面板（无 Pop 事件，树内不含 `clips_children` 时等价）。
    #[allow(dead_code)]
    pub(crate) fn walk_preorder<'a>(&'a self, out: &mut Vec<&'a DrawBatch>) {
        out.push(self);
        for child in &self.children {
            child.walk_preorder(out);
        }
    }

    /// 设置画笔颜色（后续 `draw_*(…, None)` 使用）。
    pub fn set_color(&mut self, color: crate::color::Color) {
        self.color = color;
    }

    #[inline]
    fn invalidate_transform_cache(&mut self) {
        self.cached_transform_index = None;
    }

    /// 在临时应用 [`crate::shapes::ShapeOverride`] 后执行 `f`，结束时恢复状态（不写回）。
    #[allow(dead_code)]
    pub(crate) fn with_override<R>(
        &mut self,
        opts: crate::shapes::ShapeOverride,
        f: impl FnOnce(&mut Self, crate::color::Color) -> R,
    ) -> R {
        let saved_color = self.color;
        let saved_feather = self.sdf_feather;
        let saved_uv = self.uv;
        let saved_transform = self.transform;
        let saved_xform_cache = self.cached_transform_index;
        let saved_bind_group = self.bind_group.clone();
        let tex_overridden = opts.bind_group.is_some();

        if let Some(c) = opts.color {
            self.color = c;
        }
        // Some(None)=几何, Some(Some(f))=SDF；外层 None=保持
        if let Some(feather) = opts.sdf_feather {
            self.sdf_feather = feather;
        }
        if let Some(uv) = opts.uv {
            self.uv = uv;
        }
        if let Some(t) = opts.transform {
            self.set_transform(t);
        }
        // Some(None)=白纹理, Some(Some(bg))=绑定；外层 None=保持
        if let Some(bg) = opts.bind_group {
            self.add_texture_segment(self.bind_group.clone());
            self.bind_group = bg;
        }

        let color = self.color;
        let result = f(self, color);

        if tex_overridden {
            // 含 clear（None）：必须封段，否则后续/本段顶点会落到 trailing 用恢复后的贴图
            self.add_texture_segment(self.bind_group.clone());
            self.bind_group = saved_bind_group;
        }
        self.color = saved_color;
        self.sdf_feather = saved_feather;
        self.uv = saved_uv;
        self.transform = saved_transform;
        self.cached_transform_index = saved_xform_cache;
        result
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
        // 旧尺度为 0 时无法用乘法按比例重建 → 退化为绝对 sx/sy=1。
        // 调用者本意"设旋转"，因此 scale 不重要（仅形状可见性，不影响旋转本身）。
        let (sx, sy) = if old_sx > 0.0 && old_sy > 0.0 {
            (old_sx, old_sy)
        } else {
            (1.0, 1.0)
        };
        let (c, s) = (rad.cos(), rad.sin());
        t.a = sx * c;
        t.b = -sx * s;
        t.c = sy * s;
        t.d = sy * c;
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
        // 旧尺度为 0：增量乘法永远保持 0；改为按当前角度绝对重建
        // 这样 `set_scale(0,0); set_scale(1,1)` 之类链式调用可恢复。
        if old_sx > 0.0 && old_sy > 0.0 {
            let kx = sx / old_sx;
            let ky = sy / old_sy;
            t.a *= kx;
            t.b *= kx;
            t.c *= ky;
            t.d *= ky;
        } else {
            // 从 (a,b,c,d) 推角度；零缩放时角度=0
            let angle = if old_sx > 0.0 {
                (-t.b).atan2(t.a)
            } else {
                0.0
            };
            let (c, s) = (angle.cos(), angle.sin());
            t.a = sx * c;
            t.b = -sx * s;
            t.c = sy * s;
            t.d = sy * c;
        }
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 设置完整变换（替换当前笔刷变换）。
    pub fn set_transform(&mut self, t: Transform) {
        self.transform = Some(t);
        self.invalidate_transform_cache();
    }

    /// 直接设置原始 3x3 仿射矩阵（6 个有效分量），pivot 归零。
    /// 矩阵列主序：`[a b tx; c d ty; 0 0 1]`。
    pub fn set_matrix(&mut self, a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) {
        self.set_transform(Transform::matrix(a, b, c, d, tx, ty));
    }

    /// 公转变换：绕轨道中心 `(cx, cy)` 的圆周上运动，同时绕自身 pivot `(px, py)` 自转。
    pub fn orbit_transform(
        &mut self, cx: f32, cy: f32, orbit_radius: f32, orbit_angle: f32,
        px: f32, py: f32, self_rotation: f32, sx: f32, sy: f32,
    ) {
        let x = cx + orbit_angle.cos() * orbit_radius;
        let y = cy + orbit_angle.sin() * orbit_radius;
        self.set_transform(Transform::trs(x, y, px, py, self_rotation, sx, sy));
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
    ///
    /// **index 0 保留给单位矩阵**（见 [`seed_identity_transform_table`]）。
    /// 恒等变换命中 map 直接返回 0；新矩阵从 1 起分配。
    pub(crate) fn register_transform(&mut self, c0: [f32; 3], c1: [f32; 3], c2: [f32; 3]) -> u32 {
        // 6 个有意义的 f32 构成 key：col0.xy, col1.xy, col2.xy
        // col0.z=0, col1.z=0, col2.z=1 恒不变
        let key = transform_key(c0, c1, c2);
        let next_idx = (self.transform_table.len() / 12) as u32;
        let idx = *self.transform_map.entry(key).or_insert_with(|| {
            // mat3x3 在 storage buffer 中每列 vec4-padded（16 字节对齐）
            self.transform_table.extend_from_slice(&[
                c0[0], c0[1], 0.0, 0.0, // col0 (a, c, 0, _pad)
                c1[0], c1[1], 0.0, 0.0, // col1 (b, d, 0, _pad)
                c2[0], c2[1], 1.0, 0.0, // col2 (tx, ty, 1, _pad)
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
            bind_group: self.bind_group.clone(),
            text_texture_view: self.text_texture_view.clone(),
            texture_segments: self.texture_segments.clone(),
            instances: self.instances.clone(),
            instance_texture_segments: self.instance_texture_segments.clone(),
            texts: TextEntryList::new_from_entries(&self.texts),
            transform: self.transform,
            sdf_feather: self.sdf_feather,
            color: self.color,
            uv: self.uv,
            polygon_edges: self.polygon_edges.clone(),
            transform_table: self.transform_table.clone(),
            transform_map: self.transform_map.clone(),
            has_sdf: self.has_sdf,
            cached_transform_index: self.cached_transform_index,
            children: self.children.iter().map(|c| c.clone_batch()).collect(),
            clips_children: self.clips_children,
            inherit: self.inherit,
            area_include: self.area_include.clone(),
            area_exclude: self.area_exclude.clone(),
            bounds: self.bounds,
            scissor: self.scissor,
            text_clip: self.text_clip,
            custom_material: self.custom_material.clone(),
            dynamic_offsets: self.dynamic_offsets.clone(),
        }
    }

    /// 直接设置 shape 用 bind group（高级用法）。
    /// `None` 与 [`set_texture`]`(None)` 相同：后续形状走白纹理。
    ///
    /// **文字**：无法从裸 bind group 取出 view，会清空文字画笔贴图
    ///（之后 `text`/`push*` 走白 base）；需要文字贴图时请用 [`set_texture`]。
    pub fn set_bind_group(&mut self, bg: Option<wgpu::BindGroup>) {
        self.add_texture_segment(self.bind_group.clone());
        self.add_instance_texture_segment(self.bind_group.clone());
        self.bind_group = bg;
        self.text_texture_view = None;
        self.texts.set_texture_state(None);
    }

    /// 设置 UV 子区域：后续 **shape** 顶点 UV 与之后 **text** 入队时冻结的
    /// [`crate::text::TextTextureState::uv`] 均用此范围。
    pub fn set_uv(&mut self, u0: f32, v0: f32, u1: f32, v1: f32) {
        self.uv = UvRect { u0, v0, u1, v1 };
        self.texts.set_uv_state(self.uv);
    }

    /// 恢复 UV 为全纹理 (0,0)-(1,1)（shape 与之后 text 入队画笔同步）。
    pub fn clear_uv(&mut self) {
        self.uv = UvRect::default();
        self.texts.set_uv_state(self.uv);
    }

    /// 绑定 batch 基础贴图（`Some`）或白贴图路径（`None`）。
    ///
    /// - **Shape**：同一 batch 多次切换会写入 texture segments（已画顶点归上一段）。
    /// - **Text**：同步更新文字画笔；**仅影响之后** `text` / `push*` 的条目
    ///   （入队时冻结到 [`crate::text::TextEntry::texture_state`]；按 generation 分段渲染）。
    ///
    /// 内部 shape 侧只存 `BindGroup`；文字侧另存 `TextureView` 供 glyphon base 绑定。
    pub fn set_texture(&mut self, texture: Option<&crate::texture::Texture>) {
        self.add_texture_segment(self.bind_group.clone());
        self.add_instance_texture_segment(self.bind_group.clone());
        self.bind_group = texture.map(|t| t.bind_group.clone());
        self.text_texture_view = texture.map(|t| t.view.clone());
        self.texts.set_texture_state(self.text_texture_view.clone());
    }

    /// 记录纹理段：自上次段以来的新索引归入此 bind group（`None` = 白纹理路径）。
    pub(crate) fn add_texture_segment(&mut self, bg: Option<wgpu::BindGroup>) {
        let start = self.texture_segments.last().map_or(0, |s| s.ndx_start + s.ndx_count);
        let end = self.indices.len() as u32;
        if end > start {
            self.texture_segments.push(TextureSegment {
                ndx_start: start,
                ndx_count: end - start,
                bind_group: bg,
            });
        }
    }

    fn add_instance_texture_segment(&mut self, bg: Option<wgpu::BindGroup>) {
        let start = self
            .instance_texture_segments
            .last()
            .map_or(0, |s| s.instance_start + s.instance_count);
        let end = self.instances.len() as u32;
        if end > start {
            self.instance_texture_segments.push(InstanceTextureSegment {
                instance_start: start,
                instance_count: end - start,
                bind_group: bg,
            });
        }
    }

    fn push_sdf_instance(
        &mut self,
        pos: Pos,
        bounds: [f32; 4],
        uv_bounds: [f32; 4],
        sdf_params: [f32; 4],
        sdf_type: u32,
        sdf_extra: [f32; 2],
        color: Option<crate::color::Color>,
    ) -> bool {
        let Some(feather) = self.sdf_feather else {
            return false;
        };
        // Custom vertex shaders consume the public Vertex ABI, not the instance ABI.
        if self.custom_material.is_some() {
            return false;
        }
        let color = color.unwrap_or(self.color);
        if color.a == 0.0 || bounds[0] == bounds[2] || bounds[1] == bounds[3] {
            return true;
        }
        let shape_transform = Transform::translation(pos.x, pos.y);
        let composed = match self.transform {
            Some(current) => current.then(&shape_transform),
            None => shape_transform,
        };
        let saved_cache = self.cached_transform_index;
        let (c0, c1, c2) = composed.to_cols();
        let transform_index = self.register_transform(c0, c1, c2);
        self.cached_transform_index = saved_cache;
        self.instances.push(ShapeInstance {
            bounds,
            uv_bounds,
            uv_rect: [self.uv.u0, self.uv.v0, self.uv.u1, self.uv.v1],
            color: [color.r, color.g, color.b, color.a],
            sdf_params,
            sdf_extra,
            sdf_type,
            sdf_feather: feather,
            transform_index,
            _padding: 0,
        });
        self.note_sdf();
        true
    }

    /// Add an instanced SDF rectangle.
    ///
    /// Instance shapes are rendered after ordinary shapes and before text in
    /// this batch. Use separate child batches when exact interleaving matters.
    /// Geometry mode and custom materials fall back to the ordinary path.
    pub fn instance_rectangle(
        &mut self,
        pos: Pos,
        w: f32,
        h: f32,
        color: Option<crate::color::Color>,
    ) {
        if !self.push_sdf_instance(pos, [0.0, 0.0, w, h], [0.0, 0.0, w, h], [w * 0.5, h * 0.5, w * 0.5, h * 0.5], 2, [0.0, 0.0], color) {
            self.rectangle(pos, w, h, color);
        }
    }

    /// Add an instanced SDF circle. See [`Self::instance_rectangle`] for ordering.
    pub fn instance_circle(
        &mut self,
        pos: Pos,
        r: f32,
        color: Option<crate::color::Color>,
    ) {
        if !self.push_sdf_instance(pos, [-r, -r, r, r], [-r, -r, r, r], [0.0, 0.0, r, r], 1, [0.0, 0.0], color) {
            self.circle(pos, r, color);
        }
    }

    /// Add an instanced SDF ellipse. See [`Self::instance_rectangle`] for ordering.
    pub fn instance_ellipse(
        &mut self,
        pos: Pos,
        rx: f32,
        ry: f32,
        color: Option<crate::color::Color>,
    ) {
        if !self.push_sdf_instance(pos, [-rx, -ry, rx, ry], [-rx, -ry, rx, ry], [0.0, 0.0, rx, ry], 1, [0.0, 0.0], color) {
            self.ellipse(pos, rx, ry, color);
        }
    }

    /// Add an instanced SDF rounded rectangle.
    pub fn instance_rounded_rect(
        &mut self,
        pos: Pos,
        w: f32,
        h: f32,
        radius: f32,
        color: Option<crate::color::Color>,
    ) {
        let feather = self.sdf_feather.unwrap_or(0.0);
        let r = radius.min(w * 0.5).min(h * 0.5);
        if !self.push_sdf_instance(
            pos,
            [-feather, -feather, w + feather, h + feather],
            [0.0, 0.0, w, h],
            [w * 0.5, h * 0.5, w * 0.5, h * 0.5],
            2,
            [r, 0.0],
            color,
        ) {
            self.rounded_rect(pos, w, h, radius, color);
        }
    }

    /// Add an instanced SDF line in the batch's local coordinate space.
    pub fn instance_line(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        thickness: f32,
        color: Option<crate::color::Color>,
    ) {
        let half = thickness * 0.5;
        let feather = self.sdf_feather.unwrap_or(0.0);
        let pad = half + feather;
        if !self.push_sdf_instance(
            Pos::ZERO,
            [x1.min(x2) - pad, y1.min(y2) - pad, x1.max(x2) + pad, y1.max(y2) + pad],
            [x1.min(x2) - half, y1.min(y2) - half, x1.max(x2) + half, y1.max(y2) + half],
            [x1, y1, x2, y2],
            3,
            [half, 0.0],
            color,
        ) {
            self.line(x1, y1, x2, y2, thickness, color);
        }
    }

    /// Add an instanced SDF triangle in the batch's local coordinate space.
    pub fn instance_triangle(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        x3: f32,
        y3: f32,
        color: Option<crate::color::Color>,
    ) {
        let feather = self.sdf_feather.unwrap_or(0.0);
        if !self.push_sdf_instance(
            Pos::ZERO,
            [x1.min(x2).min(x3) - feather, y1.min(y2).min(y3) - feather, x1.max(x2).max(x3) + feather, y1.max(y2).max(y3) + feather],
            [x1.min(x2).min(x3), y1.min(y2).min(y3), x1.max(x2).max(x3), y1.max(y2).max(y3)],
            [x1, y1, x2, y2],
            4,
            [x3, y3],
            color,
        ) {
            self.triangle(x1, y1, x2, y2, x3, y3, color);
        }
    }

    /// Add an instanced SDF arc.
    pub fn instance_arc(
        &mut self,
        pos: Pos,
        r: f32,
        start_angle: f32,
        end_angle: f32,
        color: Option<crate::color::Color>,
    ) {
        let feather = self.sdf_feather.unwrap_or(0.0);
        let extent = r + feather;
        if !self.push_sdf_instance(
            pos,
            [-extent, -extent, extent, extent],
            [-r, -r, r, r],
            [0.0, 0.0, r, 0.0],
            5,
            [start_angle, end_angle],
            color,
        ) {
            self.arc(pos, r, start_angle, end_angle, color);
        }
    }

    /// Add an instanced convex SDF polygon. Points must be counter-clockwise.
    pub fn instance_polygon(
        &mut self,
        points: &[(f32, f32)],
        color: Option<crate::color::Color>,
    ) {
        if self.sdf_feather.is_none() || self.custom_material.is_some() || points.len() < 3 {
            self.polygon(points, color);
            return;
        }
        if color.unwrap_or(self.color).a == 0.0 {
            return;
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut edges = Vec::with_capacity(points.len() * 4);
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            min_x = min_x.min(a.0);
            min_y = min_y.min(a.1);
            max_x = max_x.max(a.0);
            max_y = max_y.max(a.1);
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            let len = (dx * dx + dy * dy).sqrt();
            if len < 0.001 { continue; }
            let nx = -dy / len;
            let ny = dx / len;
            edges.extend_from_slice(&[nx, ny, nx * a.0 + ny * a.1, 0.0]);
        }
        let count = (edges.len() / 4) as u32;
        if count < 3 {
            return;
        }
        let start = (self.polygon_edges.len() / 4) as f32;
        self.polygon_edges.extend_from_slice(&edges);
        let feather = self.sdf_feather.unwrap();
        if !self.push_sdf_instance(
            Pos::ZERO,
            [min_x - feather, min_y - feather, max_x + feather, max_y + feather],
            [min_x, min_y, max_x, max_y],
            [start, count as f32, 0.0, 0.0],
            6,
            [0.0, 0.0],
            color,
        ) {
            self.polygon_edges.truncate(start as usize * 4);
            self.polygon(points, color);
        }
    }

    /// Add an instanced SDF line chain. Consecutive duplicate points are ignored.
    pub fn instance_line_chain(
        &mut self,
        points: &[(f32, f32)],
        thickness: f32,
        color: Option<crate::color::Color>,
    ) {
        if self.sdf_feather.is_none() || self.custom_material.is_some() || points.len() < 2 || thickness == 0.0 {
            self.line_chain(points, thickness, color);
            return;
        }
        if color.unwrap_or(self.color).a == 0.0 {
            return;
        }
        let closed = points.len() > 2
            && (points[0].0 - points[points.len() - 1].0).abs() < 0.001
            && (points[0].1 - points[points.len() - 1].1).abs() < 0.001;
        let vertex_count = if closed { points.len() - 1 } else { points.len() };
        let segment_count = if closed { vertex_count } else { vertex_count - 1 };
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut edges = Vec::with_capacity(segment_count * 4);
        for i in 0..vertex_count {
            min_x = min_x.min(points[i].0);
            min_y = min_y.min(points[i].1);
            max_x = max_x.max(points[i].0);
            max_y = max_y.max(points[i].1);
        }
        for i in 0..segment_count {
            let a = points[i];
            let b = points[if i + 1 < vertex_count { i + 1 } else { 0 }];
            if (b.0 - a.0).abs() + (b.1 - a.1).abs() < 0.001 { continue; }
            edges.extend_from_slice(&[a.0, a.1, b.0, b.1]);
        }
        let count = (edges.len() / 4) as u32;
        if count == 0 { return; }
        let start = (self.polygon_edges.len() / 4) as f32;
        self.polygon_edges.extend_from_slice(&edges);
        let half = thickness * 0.5;
        let feather = self.sdf_feather.unwrap();
        if !self.push_sdf_instance(
            Pos::ZERO,
            [min_x - half - feather, min_y - half - feather, max_x + half + feather, max_y + half + feather],
            [min_x - half, min_y - half, max_x + half, max_y + half],
            [start, count as f32, half, 0.0],
            7,
            [0.0, 0.0],
            color,
        ) {
            self.polygon_edges.truncate(start as usize * 4);
            self.line_chain(points, thickness, color);
        }
    }

    /// Add an instanced rectangle outline.
    pub fn instance_rect_outline(
        &mut self,
        pos: Pos,
        w: f32,
        h: f32,
        thickness: f32,
        color: Option<crate::color::Color>,
    ) {
        let half = thickness * 0.5;
        let points = [
            (half, half),
            (w - half, half),
            (w - half, h - half),
            (half, h - half),
            (half, half),
        ];
        let saved = self.transform;
        self.transform = Some(match saved {
            Some(transform) => transform.then(&Transform::translation(pos.x, pos.y)),
            None => Transform::translation(pos.x, pos.y),
        });
        self.invalidate_transform_cache();
        self.instance_line_chain(&points, thickness, color);
        self.transform = saved;
        self.invalidate_transform_cache();
    }

    /// Add an instanced circle outline.
    pub fn instance_circle_outline(
        &mut self,
        pos: Pos,
        r: f32,
        thickness: f32,
        color: Option<crate::color::Color>,
        segments: u32,
    ) {
        let n = segments.max(8) as usize;
        let mut points = Vec::with_capacity(n + 1);
        for i in 0..n {
            let angle = std::f32::consts::TAU * i as f32 / n as f32;
            points.push((r * angle.cos(), r * angle.sin()));
        }
        points.push(points[0]);
        let saved = self.transform;
        self.transform = Some(match saved {
            Some(transform) => transform.then(&Transform::translation(pos.x, pos.y)),
            None => Transform::translation(pos.x, pos.y),
        });
        self.invalidate_transform_cache();
        self.instance_line_chain(&points, thickness, color);
        self.transform = saved;
        self.invalidate_transform_cache();
    }

    /// Add an instanced ellipse outline.
    pub fn instance_ellipse_outline(
        &mut self,
        pos: Pos,
        rx: f32,
        ry: f32,
        thickness: f32,
        color: Option<crate::color::Color>,
        segments: u32,
    ) {
        let n = segments.max(16) as usize;
        let mut points = Vec::with_capacity(n + 1);
        for i in 0..n {
            let angle = std::f32::consts::TAU * i as f32 / n as f32;
            points.push((rx * angle.cos(), ry * angle.sin()));
        }
        points.push(points[0]);
        let saved = self.transform;
        self.transform = Some(match saved {
            Some(transform) => transform.then(&Transform::translation(pos.x, pos.y)),
            None => Transform::translation(pos.x, pos.y),
        });
        self.invalidate_transform_cache();
        self.instance_line_chain(&points, thickness, color);
        self.transform = saved;
        self.invalidate_transform_cache();
    }

    /// Add an instanced triangle outline.
    pub fn instance_triangle_outline(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        x3: f32,
        y3: f32,
        thickness: f32,
        color: Option<crate::color::Color>,
    ) {
        self.instance_line_chain(&[(x1, y1), (x2, y2), (x3, y3), (x1, y1)], thickness, color);
    }

    /// Add an instanced polygon outline.
    pub fn instance_polygon_outline(
        &mut self,
        points: &[(f32, f32)],
        thickness: f32,
        color: Option<crate::color::Color>,
    ) {
        if points.len() < 3 {
            return;
        }
        let mut closed = Vec::with_capacity(points.len() + 1);
        closed.extend_from_slice(points);
        closed.push(points[0]);
        self.instance_line_chain(&closed, thickness, color);
    }

    /// Add an instanced rounded rectangle outline.
    pub fn instance_rounded_rect_outline(
        &mut self,
        pos: Pos,
        w: f32,
        h: f32,
        radius: f32,
        thickness: f32,
        color: Option<crate::color::Color>,
        corner_segments: u32,
    ) {
        let r = radius.min(w * 0.5).min(h * 0.5);
        if r <= 0.0 {
            self.instance_rect_outline(pos, w, h, thickness, color);
            return;
        }
        let half = thickness * 0.5;
        let inner_radius = (r - half).max(0.0);
        let segments = corner_segments.max(2);
        let mut points = Vec::with_capacity((segments as usize + 1) * 4 + 1);
        for (cx, cy, start, end) in [
            (r, r, std::f32::consts::PI, std::f32::consts::PI * 1.5),
            (w - r, r, std::f32::consts::PI * 1.5, std::f32::consts::TAU),
            (w - r, h - r, 0.0, std::f32::consts::FRAC_PI_2),
            (r, h - r, std::f32::consts::FRAC_PI_2, std::f32::consts::PI),
        ] {
            if inner_radius > 0.0 {
                for i in 0..=segments {
                    let angle = start + (end - start) * i as f32 / segments as f32;
                    points.push((cx + inner_radius * angle.cos(), cy + inner_radius * angle.sin()));
                }
            } else {
                points.push((cx, cy));
            }
        }
        points.push(points[0]);
        let saved = self.transform;
        self.transform = Some(match saved {
            Some(transform) => transform.then(&Transform::translation(pos.x, pos.y)),
            None => Transform::translation(pos.x, pos.y),
        });
        self.invalidate_transform_cache();
        self.instance_line_chain(&points, thickness, color);
        self.transform = saved;
        self.invalidate_transform_cache();
    }

    /// Add an instanced arc outline.
    pub fn instance_arc_outline(
        &mut self,
        pos: Pos,
        r: f32,
        start_angle: f32,
        end_angle: f32,
        thickness: f32,
        color: Option<crate::color::Color>,
        segments: u32,
    ) {
        let n = segments.max(2);
        let mut points = Vec::with_capacity(n as usize + 3);
        points.push((0.0, 0.0));
        for i in 0..=n {
            let angle = start_angle + (end_angle - start_angle) * i as f32 / n as f32;
            points.push((r * angle.cos(), r * angle.sin()));
        }
        points.push((0.0, 0.0));
        let saved = self.transform;
        self.transform = Some(match saved {
            Some(transform) => transform.then(&Transform::translation(pos.x, pos.y)),
            None => Transform::translation(pos.x, pos.y),
        });
        self.invalidate_transform_cache();
        self.instance_line_chain(&points, thickness, color);
        self.transform = saved;
        self.invalidate_transform_cache();
    }

    /// Draw any [`crate::shapes::Shape`] through the instanced SDF path.
    ///
    /// This mirrors [`crate::shapes::draw_shape`]. In geometry mode or with a
    /// custom material, individual calls fall back to the ordinary mesh path.
    pub fn instance_shape(
        &mut self,
        shape: &crate::shapes::Shape<'_>,
        opts: crate::shapes::ShapeOverride,
    ) {
        let saved_color = self.color;
        let saved_feather = self.sdf_feather;
        let saved_uv = self.uv;
        let saved_transform = self.transform;
        let saved_cache = self.cached_transform_index;
        let saved_bg = self.bind_group.clone();
        if let Some(color) = opts.color { self.color = color; }
        if let Some(feather) = opts.sdf_feather { self.sdf_feather = feather; }
        if let Some(uv) = opts.uv { self.uv = uv; }

        let base = shape.position().map_or(Transform::IDENTITY, |p| Transform::translation(p.x, p.y));
        if shape.position().is_some() || opts.transform.is_some() {
            let current = self.transform.take();
            self.transform = Some(match (current, opts.transform) {
                (Some(existing), Some(transform)) => existing.then(&base).then(&transform),
                (Some(existing), None) => existing.then(&base),
                (None, Some(transform)) => base.then(&transform),
                (None, None) => base,
            });
            self.invalidate_transform_cache();
        }
        let texture_overridden = opts.bind_group.is_some();
        if let Some(bind_group) = opts.bind_group {
            self.add_texture_segment(self.bind_group.clone());
            self.add_instance_texture_segment(self.bind_group.clone());
            self.bind_group = bind_group;
        }

        let color = Some(self.color);
        match shape {
            crate::shapes::Shape::Rect { w, h, .. } => self.instance_rectangle(Pos::ZERO, *w, *h, color),
            crate::shapes::Shape::RoundedRect { w, h, radius, .. } => self.instance_rounded_rect(Pos::ZERO, *w, *h, *radius, color),
            crate::shapes::Shape::Circle { r, .. } => self.instance_circle(Pos::ZERO, *r, color),
            crate::shapes::Shape::Ellipse { rx, ry, .. } => self.instance_ellipse(Pos::ZERO, *rx, *ry, color),
            crate::shapes::Shape::Line { x1, y1, x2, y2, thickness } => self.instance_line(*x1, *y1, *x2, *y2, *thickness, color),
            crate::shapes::Shape::LineChain { points, thickness } => self.instance_line_chain(points, *thickness, color),
            crate::shapes::Shape::Triangle { x1, y1, x2, y2, x3, y3 } => self.instance_triangle(*x1, *y1, *x2, *y2, *x3, *y3, color),
            crate::shapes::Shape::Polygon { points } => self.instance_polygon(points, color),
            crate::shapes::Shape::Arc { r, start, end, .. } => self.instance_arc(Pos::ZERO, *r, *start, *end, color),
            crate::shapes::Shape::RectOutline { w, h, thickness, .. } => self.instance_rect_outline(Pos::ZERO, *w, *h, *thickness, color),
            crate::shapes::Shape::CircleOutline { r, thickness, segments, .. } => self.instance_circle_outline(Pos::ZERO, *r, *thickness, color, *segments),
            crate::shapes::Shape::EllipseOutline { rx, ry, thickness, segments, .. } => self.instance_ellipse_outline(Pos::ZERO, *rx, *ry, *thickness, color, *segments),
            crate::shapes::Shape::RoundedRectOutline { w, h, radius, thickness, corner_segments, .. } => self.instance_rounded_rect_outline(Pos::ZERO, *w, *h, *radius, *thickness, color, *corner_segments),
            crate::shapes::Shape::TriangleOutline { x1, y1, x2, y2, x3, y3, thickness } => self.instance_triangle_outline(*x1, *y1, *x2, *y2, *x3, *y3, *thickness, color),
            crate::shapes::Shape::PolygonOutline { points, thickness } => self.instance_polygon_outline(points, *thickness, color),
            crate::shapes::Shape::ArcOutline { r, start, end, thickness, segments, .. } => self.instance_arc_outline(Pos::ZERO, *r, *start, *end, *thickness, color, *segments),
        }

        if texture_overridden {
            self.add_texture_segment(self.bind_group.clone());
            self.add_instance_texture_segment(self.bind_group.clone());
        }
        self.bind_group = saved_bg;
        self.transform = saved_transform;
        self.cached_transform_index = saved_cache;
        self.uv = saved_uv;
        self.sdf_feather = saved_feather;
        self.color = saved_color;
    }

    // ---- 形状委托（去 draw_ 前缀） ----

    pub fn rectangle(&mut self, pos: Pos, w: f32, h: f32, c: Option<crate::color::Color>) { crate::shapes::draw_rectangle(self, pos, w, h, c); }
    pub fn circle(&mut self, pos: Pos, r: f32, c: Option<crate::color::Color>) { crate::shapes::draw_circle(self, pos, r, c); }
    pub fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, t: f32, c: Option<crate::color::Color>) { crate::shapes::draw_line(self, x1, y1, x2, y2, t, c); }
    pub fn ellipse(&mut self, pos: Pos, rx: f32, ry: f32, c: Option<crate::color::Color>) { crate::shapes::draw_ellipse(self, pos, rx, ry, c); }
    pub fn rounded_rect(&mut self, pos: Pos, w: f32, h: f32, r: f32, c: Option<crate::color::Color>) { crate::shapes::draw_rounded_rect(self, pos, w, h, r, c); }
    pub fn triangle(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, c: Option<crate::color::Color>) { crate::shapes::draw_triangle(self, x1, y1, x2, y2, x3, y3, c); }
    pub fn polygon(&mut self, pts: &[(f32, f32)], c: Option<crate::color::Color>) { crate::shapes::draw_polygon(self, pts, c); }
    pub fn arc(&mut self, pos: Pos, r: f32, sa: f32, ea: f32, c: Option<crate::color::Color>) { crate::shapes::draw_arc(self, pos, r, sa, ea, c); }
    pub fn rect_outline(&mut self, pos: Pos, w: f32, h: f32, t: f32, c: Option<crate::color::Color>) { crate::shapes::draw_rect_outline(self, pos, w, h, t, c); }
    pub fn circle_outline(&mut self, pos: Pos, r: f32, t: f32, c: Option<crate::color::Color>, seg: u32) { crate::shapes::draw_circle_outline(self, pos, r, t, c, seg); }
    pub fn ellipse_outline(&mut self, pos: Pos, rx: f32, ry: f32, t: f32, c: Option<crate::color::Color>, seg: u32) { crate::shapes::draw_ellipse_outline(self, pos, rx, ry, t, c, seg); }
    pub fn rounded_rect_outline(&mut self, pos: Pos, w: f32, h: f32, r: f32, t: f32, c: Option<crate::color::Color>, cs: u32) { crate::shapes::draw_rounded_rect_outline(self, pos, w, h, r, t, c, cs); }
    pub fn line_chain(&mut self, pts: &[(f32, f32)], t: f32, c: Option<crate::color::Color>) { crate::shapes::draw_line_chain(self, pts, t, c); }
    pub fn triangle_outline(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, t: f32, c: Option<crate::color::Color>) { crate::shapes::draw_triangle_outline(self, x1, y1, x2, y2, x3, y3, t, c); }
    pub fn polygon_outline(&mut self, pts: &[(f32, f32)], t: f32, c: Option<crate::color::Color>) { crate::shapes::draw_polygon_outline(self, pts, t, c); }
    pub fn arc_outline(&mut self, pos: Pos, r: f32, sa: f32, ea: f32, t: f32, c: Option<crate::color::Color>, seg: u32) { crate::shapes::draw_arc_outline(self, pos, r, sa, ea, t, c, seg); }
    pub fn shape(&mut self, shape: &crate::shapes::Shape<'_>, opts: crate::shapes::ShapeOverride) {
        crate::shapes::draw_shape(self, shape, opts);
    }

    /// 添加文字，自动捕获当前 transform。
    pub fn text(&mut self, text: &str, pos: Pos, def: TextDef, ov: crate::text::TextOverride) {
        let idx = self.current_transform_index();
        self.texts.push_indexed(text, pos, def, ov, idx);
    }

    /// 使用 [`StableText`] 直接绘制（位置 pos + 覆盖 ov；字号等已在创建时定型）。
    pub fn text_stable(
        &mut self,
        stable: &crate::text::StableText,
        pos: Pos,
        ov: crate::text::TextOverride,
    ) {
        let idx = self.current_transform_index();
        self.texts.push_stable_indexed(stable, pos, ov, idx);
    }

    /// HUD 多段（Normal / Dynamic / Glyphs / Stable），捕获当前 transform。
    pub fn text_parts(
        &mut self,
        parts: &[crate::text::TextPart],
        pos: Pos,
        def: TextDef,
        ov: crate::text::TextOverride,
    ) {
        let idx = self.current_transform_index();
        self.texts.push_parts_indexed(parts, pos, def, ov, idx);
    }

    /// HUD 自动切分（[`crate::text::split_hud`]），捕获当前 transform。
    pub fn text_hud(&mut self, text: &str, pos: Pos, def: TextDef, ov: crate::text::TextOverride) {
        let idx = self.current_transform_index();
        self.texts.push_hud_indexed(text, pos, def, ov, idx);
    }

    /// 绘制 [`crate::text::HudLine`]，捕获当前 transform。
    pub fn hud_line(
        &mut self,
        line: &crate::text::HudLine,
        pos: Pos,
        def: TextDef,
        ov: crate::text::TextOverride,
    ) {
        let idx = self.current_transform_index();
        line.draw_indexed(&mut self.texts, pos, def, ov, idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::colors::*;
    use crate::shapes::{draw_circle, draw_polygon, draw_rectangle, draw_rounded_rect};
    use crate::text::{TextDef, TextOverride};

    #[test]
    fn has_sdf_flag_set_on_sdf_shapes() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        assert!(b.has_sdf);
        b.clear();
        assert!(!b.has_sdf);
        b.sdf_feather = None;
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        assert!(!b.has_sdf);
    }

    #[test]
    fn transform_index_stable_across_same_transform() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(10.0, 20.0);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 5.0, 5.0, Some(RED));
        draw_circle(&mut b, Pos::new(0.0, 0.0), 3.0, Some(BLUE));
        let idxs: Vec<u32> = b.vertices.iter().map(|v| v.transform_index).collect();
        assert!(idxs.iter().all(|&i| i == idxs[0]));
        // 槽 0 = 单位阵 + 1 个平移
        assert_eq!(b.transform_table.len() / 12, 2);
    }

    #[test]
    fn transform_cache_invalidates_on_set_position() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        // 不同 Pos 应产生不同 transform entry
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 5.0, 5.0, Some(RED));
        draw_rectangle(&mut b, Pos::new(100.0, 0.0), 5.0, 5.0, Some(BLUE));
        let i0 = b.vertices[0].transform_index;
        let i1 = b.vertices[4].transform_index;
        assert_ne!(i0, i1);
        // 槽 0 = 单位阵 + 2 个不同平移（Pos(0,0) 复用槽 0）
        assert_eq!(b.transform_table.len() / 12, 2);
        assert_eq!(i0, 0);
        assert_eq!(i1, 1);
    }

    #[test]
    fn transform_slot_zero_is_identity_after_shape() {
        let mut b = DrawBatch::new();
        draw_rectangle(&mut b, Pos::new(100.0, 200.0), 50.0, 40.0, Some(WHITE));
        assert!(b.transform_table.len() >= 12);
        let t0 = &b.transform_table[0..12];
        assert_eq!(t0[0], 1.0);
        assert_eq!(t0[5], 1.0);
        assert_eq!(t0[8], 0.0);
        assert_eq!(t0[9], 0.0);
        assert_eq!(b.vertices[0].transform_index, 1);
        let t1 = &b.transform_table[12..24];
        assert!((t1[8] - 100.0).abs() < 1e-4);
        assert!((t1[9] - 200.0).abs() < 1e-4);
        // draw_text 默认 index 0 → 恒等，不会吃到矩形的平移
        crate::text::draw_text(
            &mut b.texts,
            "hi",
            Pos::new(106.0, 204.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(WHITE),
        );
        assert_eq!(b.texts.entries[0].transform_index(), 0);
    }

    #[test]
    fn multi_batch_poly_base_patch_values() {
        // 模拟 Renderer 多 batch poly 偏移：第二 batch 的 type6 start 应加上第一 batch 边数
        let mut b0 = DrawBatch::new();
        b0.sdf_feather = Some(1.0);
        let pts = [(0., 0.), (10., 0.), (5., 8.)];
        draw_polygon(&mut b0, &pts, Some(RED));
        let edges0 = b0.polygon_edges.len() / 4;

        let mut b1 = DrawBatch::new();
        b1.sdf_feather = Some(1.0);
        draw_polygon(&mut b1, &pts, Some(BLUE));
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
            draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(RED));
        }
        let cap_v = b.vertices.capacity();
        let cap_i = b.indices.capacity();
        b.clear();
        assert!(b.vertices.capacity() >= cap_v);
        assert!(b.indices.capacity() >= cap_i);
        assert!(b.vertices.is_empty());
        assert!(!b.has_sdf);
        assert_eq!(b.sdf_feather, Some(1.0)); // 与 new() 一致
    }

    #[test]
    fn sdf_instances_store_one_record_per_shape() {
        let mut batch = DrawBatch::new();
        for i in 0..1000 {
            batch.instance_rectangle(Pos::new(i as f32, 0.0), 8.0, 4.0, Some(RED));
        }
        assert_eq!(batch.instances.len(), 1000);
        assert!(batch.vertices.is_empty());
        assert!(batch.indices.is_empty());
        assert!(batch.has_sdf);
    }

    #[test]
    fn instance_shapes_capture_transform_and_position() {
        let mut batch = DrawBatch::new();
        batch.set_position(10.0, 20.0);
        batch.instance_circle(Pos::new(5.0, 6.0), 3.0, Some(WHITE));
        let instance = batch.instances[0];
        assert_ne!(instance.transform_index, 0);
        let (_, _, translation) = DrawBatch::table_cols_at(
            &batch.transform_table,
            instance.transform_index,
        );
        assert!((translation[0] - 15.0).abs() < 1e-5);
        assert!((translation[1] - 26.0).abs() < 1e-5);
    }

    #[test]
    fn instance_geometry_mode_falls_back_to_vertices() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = None;
        batch.instance_circle(Pos::ZERO, 5.0, Some(WHITE));
        assert!(batch.instances.is_empty());
        assert!(!batch.vertices.is_empty());
        assert!(!batch.indices.is_empty());
    }

    #[test]
    fn clear_preserves_instance_capacity() {
        let mut batch = DrawBatch::new();
        for i in 0..64 {
            batch.instance_ellipse(Pos::new(i as f32, 0.0), 2.0, 3.0, Some(BLUE));
        }
        let capacity = batch.instances.capacity();
        batch.clear();
        assert!(batch.instances.is_empty());
        assert!(batch.instances.capacity() >= capacity);
    }

    #[test]
    fn to_area_expands_instances_to_legacy_quads() {
        let mut batch = DrawBatch::new();
        batch.instance_rectangle(Pos::new(3.0, 4.0), 10.0, 20.0, Some(GREEN));
        match batch.to_area() {
            Area::Geom(geom) => {
                assert_eq!(geom.vertices.len(), 4);
                assert_eq!(geom.indices.len(), 6);
                assert_eq!(geom.vertices[0].sdf_type, 2);
            }
            _ => panic!("expected Area::Geom"),
        }
    }

    #[test]
    fn extended_sdf_instances_do_not_expand_vertices() {
        let mut batch = DrawBatch::new();
        batch.instance_rounded_rect(Pos::new(1.0, 2.0), 20.0, 10.0, 3.0, Some(WHITE));
        batch.instance_line(0.0, 0.0, 10.0, 5.0, 2.0, Some(WHITE));
        batch.instance_triangle(0.0, 0.0, 10.0, 0.0, 5.0, 8.0, Some(WHITE));
        batch.instance_arc(Pos::new(20.0, 20.0), 8.0, 0.0, std::f32::consts::PI, Some(WHITE));
        batch.instance_polygon(&[(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)], Some(WHITE));
        batch.instance_line_chain(&[(0.0, 0.0), (4.0, 2.0), (8.0, 0.0)], 2.0, Some(WHITE));
        assert_eq!(batch.instances.len(), 6);
        assert!(batch.vertices.is_empty());
        assert_eq!(batch.instances[0].sdf_type, 2);
        assert_eq!(batch.instances[1].sdf_type, 3);
        assert_eq!(batch.instances[2].sdf_type, 4);
        assert_eq!(batch.instances[3].sdf_type, 5);
        assert_eq!(batch.instances[4].sdf_type, 6);
        assert_eq!(batch.instances[5].sdf_type, 7);
        assert!(!batch.polygon_edges.is_empty());
    }

    #[test]
    fn instance_shape_covers_positioned_and_outline_variants() {
        let mut batch = DrawBatch::new();
        batch.instance_shape(
            &crate::shapes::Shape::RoundedRect {
                pos: Pos::new(20.0, 30.0),
                w: 16.0,
                h: 8.0,
                radius: 2.0,
            },
            crate::shapes::ShapeOverride::default(),
        );
        batch.instance_shape(
            &crate::shapes::Shape::PolygonOutline {
                points: &[(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)],
                thickness: 1.0,
            },
            crate::shapes::ShapeOverride::default(),
        );
        assert_eq!(batch.instances.len(), 2);
        assert!(batch.vertices.is_empty());
        let (_, _, translation) = DrawBatch::table_cols_at(
            &batch.transform_table,
            batch.instances[0].transform_index,
        );
        assert!((translation[0] - 20.0).abs() < 1e-5);
        assert!((translation[1] - 30.0).abs() < 1e-5);
    }

    #[test]
    fn walk_preorder_parent_before_children() {
        let mut parent = DrawBatch::new();
        draw_rectangle(&mut parent, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        let mut c0 = DrawBatch::new();
        draw_circle(&mut c0, Pos::new(0.0, 0.0), 3.0, Some(GREEN));
        let mut c1 = DrawBatch::new();
        draw_rectangle(&mut c1, Pos::new(1.0, 1.0), 2.0, 2.0, Some(BLUE));
        parent.push_child(c0);
        parent.push_child(c1);
        let mut flat = Vec::new();
        parent.walk_preorder(&mut flat);
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].vertices.len(), 4); // parent rect
        assert_eq!(flat[1].vertices.len() > 0, true); // child circle
        assert_eq!(flat[2].vertices.len(), 4); // child rect
        assert!(parent.has_drawable_content());
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

    #[test]
    fn inherit_transform_left_muls_child_table() {
        let mut parent = DrawBatch::new();
        parent.set_position(100.0, 50.0);
        let mut child = DrawBatch::new();
        child.sdf_feather = Some(1.0);
        child.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        let idx = child.vertices[0].transform_index as usize;
        let base = idx * 12;
        // 继承前局部表为恒等
        assert_eq!(child.transform_table[base], 1.0);
        assert_eq!(child.transform_table[base + 8], 0.0);
        parent.push_child(child);
        let c = &parent.children[0];
        let t = &c.transform_table[base..base + 12];
        assert!((t[8] - 100.0).abs() < 1e-4, "tx={}", t[8]);
        assert!((t[9] - 50.0).abs() < 1e-4, "ty={}", t[9]);
    }

    #[test]
    fn inherit_color_and_feather_on_push() {
        let mut parent = DrawBatch::new();
        parent.color = GREEN;
        parent.sdf_feather = Some(2.5);
        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::NONE.color().sdf_feather();
        assert_eq!(child.color, WHITE);
        parent.push_child(child);
        assert_eq!(parent.children[0].color, GREEN);
        assert_eq!(parent.children[0].sdf_feather, Some(2.5));
    }

    #[test]
    fn transform_then_composes() {
        let p = Transform::translation(10.0, 20.0);
        let c = Transform::translation(3.0, 4.0);
        let m = p.then(&c);
        let (_, _, t) = m.to_cols();
        assert!((t[0] - 13.0).abs() < 1e-5);
        assert!((t[1] - 24.0).abs() < 1e-5);
    }

    #[test]
    fn inherit_default_is_clipped() {
        assert!(InheritFromParent::NONE.clipped);
        assert!(InheritFromParent::default().clipped);
        assert!(!InheritFromParent::NONE.unclipped().clipped);
        assert!(InheritFromParent::TRANSFORM.unclipped().transform);
        assert!(!InheritFromParent::TRANSFORM.unclipped().clipped);
    }

    #[test]
    fn inherit_builder_on_off_pairs() {
        let a = InheritFromParent::ALL
            .no_transform()
            .no_color()
            .no_sdf_feather()
            .no_uv()
            .unclipped();
        assert!(!a.transform && !a.color && !a.sdf_feather && !a.uv && !a.clipped);
        let b = InheritFromParent::NONE
            .transform()
            .color()
            .sdf_feather()
            .uv()
            .clipped();
        assert!(b.transform && b.color && b.sdf_feather && b.uv && b.clipped);
    }

    /// 三层 clips 的 flatten 顺序：root → mid → leaf → Pop → Pop
    #[test]
    fn nested_clips_flatten_emits_two_pops() {
        let mut root = DrawBatch::new();
        root.clips_children = true;
        draw_rectangle(&mut root, Pos::new(-10.0, -10.0), 20.0, 20.0, Some(RED));

        let mut mid = DrawBatch::new();
        mid.clips_children = true;
        mid.inherit = InheritFromParent::TRANSFORM;
        draw_circle(&mut mid, Pos::new(0.0, 0.0), 8.0, Some(GREEN));

        let mut leaf = DrawBatch::new();
        leaf.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut leaf, Pos::new(-2.0, -2.0), 4.0, 4.0, Some(BLUE));

        mid.push_child(leaf);
        root.push_child(mid);

        let mut flat: Vec<Option<&DrawBatch>> = Vec::new();
        root.flatten_with_pop(&mut flat);
        // root, mid, leaf, pop(mid), pop(root)
        assert_eq!(flat.len(), 5);
        assert!(flat[0].is_some());
        assert!(flat[1].is_some());
        assert!(flat[2].is_some());
        assert!(flat[3].is_none());
        assert!(flat[4].is_none());
        assert!(flat[0].unwrap().clips_children);
        assert!(flat[1].unwrap().clips_children);
        assert!(!flat[2].unwrap().clips_children);
    }

    /// 嵌套 push 时 ref 语义：root Push(0)→mid Push(1)→leaf Test(2)
    #[test]
    fn nested_clips_stencil_ref_sequence() {
        // 模拟 draw() 内 compute_stencil 的 ref 栈
        let mut root = DrawBatch::new();
        root.clips_children = true;
        draw_rectangle(&mut root, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        let mut mid = DrawBatch::new();
        mid.clips_children = true;
        mid.inherit = InheritFromParent::TRANSFORM;
        draw_circle(&mut mid, Pos::new(0.0, 0.0), 5.0, Some(GREEN));
        let mut leaf = DrawBatch::new();
        leaf.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut leaf, Pos::new(0.0, 0.0), 2.0, 2.0, Some(BLUE));
        mid.push_child(leaf);
        root.push_child(mid);

        let mut flat: Vec<Option<&DrawBatch>> = Vec::new();
        root.flatten_with_pop(&mut flat);

        let mut ref_stack: Vec<u32> = Vec::new();
        let mut active: Option<u32> = None;
        let mut ops: Vec<(u32, u32)> = Vec::new(); // (op, ref)
        for item in &flat {
            match item {
                Some(batch) => {
                    let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty();
                    let has_draw = has_geom || !batch.texts.entries.is_empty();
                    let (op, r) = if batch.clips_children && has_geom {
                        let push_ref = active.unwrap_or(0);
                        let new_lv = push_ref + 1;
                        ref_stack.push(new_lv);
                        active = Some(new_lv);
                        (1u32, push_ref)
                    } else if let Some(a) = active {
                        if batch.inherit.clipped && has_draw {
                            (2u32, a)
                        } else {
                            (0u32, 0)
                        }
                    } else {
                        (0u32, 0)
                    };
                    ops.push((op, r));
                }
                None => {
                    let popped = ref_stack.pop().unwrap_or(0);
                    active = if popped > 1 { Some(popped - 1) } else { None };
                    ops.push((3u32, popped));
                }
            }
        }
        assert_eq!(ops, vec![
            (1, 0), // root Push @0 → level 1
            (1, 1), // mid Push @1 → level 2
            (2, 2), // leaf Test @2
            (3, 2), // pop mid
            (3, 1), // pop root
        ]);
    }

    /// 读 transform 表第 `idx` 个 mat 的 (a,c,b,d,tx,ty)
    fn mat6(table: &[f32], idx: u32) -> (f32, f32, f32, f32, f32, f32) {
        let b = idx as usize * 12;
        assert!(b + 12 <= table.len(), "idx={idx} table_mats={}", table.len() / 12);
        (
            table[b],
            table[b + 1],
            table[b + 4],
            table[b + 5],
            table[b + 8],
            table[b + 9],
        )
    }

    /// 嵌套 Inherit TRANSFORM 后：leaf/mid 的形状与文字共用索引，且表内平移已含祖先
    #[test]
    fn nested_text_and_shape_share_composed_transform() {
        let mut root = DrawBatch::new();
        root.sdf_feather = Some(1.0);
        root.set_position(230.0, 270.0);
        root.clips_children = true;
        // 形状 Pos 为局部偏移；与 batch 平移组合后共享 transform entry
        draw_rounded_rect(&mut root, Pos::new(0.0, 0.0), 300.0, 260.0, 28.0, Some(RED));

        let mut mid = DrawBatch::new();
        mid.sdf_feather = Some(1.0);
        mid.set_position(40.0, 0.0);
        mid.clips_children = true;
        mid.inherit = InheritFromParent::TRANSFORM;
        // mid 形状用局部原点，与 batch 平移一致 → 共享 transform entry
        draw_circle(&mut mid, Pos::new(0.0, 0.0), 90.0, Some(GREEN));

        let mut leaf = DrawBatch::new();
        leaf.sdf_feather = Some(1.0);
        leaf.inherit = InheritFromParent::TRANSFORM;
        // leaf 无独立平移，形状用局部原点与 batch 一致
        draw_circle(&mut leaf, Pos::new(0.0, 0.0), 18.0, Some(WHITE));
        leaf.text(
            "LEAF",
            Pos::new(-40.0, -14.0),
            TextDef::default().font_size(28.0),
            TextOverride::from_color(BLACK),
        );

        mid.push_child(leaf);
        mid.text(
            "MID",
            Pos::new(-36.0, -34.0),
            TextDef::default().font_size(26.0),
            TextOverride::from_color(YELLOW),
        );
        root.push_child(mid);
        root.text(
            "ROOT",
            Pos::new(-50.0, -58.0),
            TextDef::default().font_size(26.0),
            TextOverride::from_color(SKYBLUE),
        );

        // --- root ---
        assert_eq!(root.texts.entries.len(), 1);
        let rti = root.texts.entries[0].transform_index();
        let rvi = root.vertices[0].transform_index;
        assert_eq!(rti, rvi, "root text/shape index");
        let (_, _, _, _, rtx, rty) = mat6(&root.transform_table, rti);
        assert!((rtx - 230.0).abs() < 1e-3, "root tx={rtx}");
        assert!((rty - 270.0).abs() < 1e-3, "root ty={rty}");

        // --- mid（继承后应为 root∘mid_local = (270, 270)）---
        let mid = &root.children[0];
        assert_eq!(mid.texts.entries.len(), 1);
        let mti = mid.texts.entries[0].transform_index();
        let mvi = mid.vertices[0].transform_index;
        assert_eq!(mti, mvi, "mid text/shape index");
        let (_, _, _, _, mtx, mty) = mat6(&mid.transform_table, mti);
        assert!(
            (mtx - 270.0).abs() < 1e-3 && (mty - 270.0).abs() < 1e-3,
            "mid composed tx,ty=({mtx},{mty}) want (270,270)"
        );

        // --- leaf（继承后应与 mid 同世界原点 (270,270)）---
        let leaf = &root.children[0].children[0];
        assert_eq!(leaf.texts.entries.len(), 1);
        let lti = leaf.texts.entries[0].transform_index();
        let lvi = leaf.vertices[0].transform_index;
        assert_eq!(lti, lvi, "leaf text/shape index");
        let (_, _, _, _, ltx, lty) = mat6(&leaf.transform_table, lti);
        assert!(
            (ltx - 270.0).abs() < 1e-3 && (lty - 270.0).abs() < 1e-3,
            "leaf composed tx,ty=({ltx},{lty}) want (270,270)"
        );

        // 文字局部坐标：LEAF 在 leaf 原点附近，变换后世界 ≈ (230,256) 仍在 mid 圆内
        let wx = ltx + leaf.texts.entries[0].pos().x;
        let wy = lty + leaf.texts.entries[0].pos().y;
        let dx = wx - 270.0;
        let dy = wy - 270.0;
        let dist = (dx * dx + dy * dy).sqrt();
        assert!(
            dist < 90.0,
            "LEAF text world ({wx},{wy}) dist_from_mid_center={dist} should be inside r=90"
        );
    }

    /// 仅文字、无形状的子：inherit 后 transform_table 仍应被左乘
    #[test]
    fn inherit_transform_text_only_child_gets_table_entry() {
        let mut parent = DrawBatch::new();
        parent.set_position(100.0, 50.0);
        draw_rectangle(&mut parent, Pos::new(-10.0, -10.0), 20.0, 20.0, Some(RED));

        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::TRANSFORM;
        child.text(
            "hi",
            Pos::new(0.0, 0.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(WHITE),
        );
        // text() 会注册当前 transform（恒等）到 table
        assert!(!child.transform_table.is_empty());
        let ti_before = child.texts.entries[0].transform_index();
        let (_, _, _, _, tx0, ty0) = mat6(&child.transform_table, ti_before);
        assert!(tx0.abs() < 1e-5 && ty0.abs() < 1e-5);

        parent.push_child(child);
        let c = &parent.children[0];
        let ti = c.texts.entries[0].transform_index();
        let (_, _, _, _, tx, ty) = mat6(&c.transform_table, ti);
        assert!((tx - 100.0).abs() < 1e-3, "tx={tx}");
        assert!((ty - 50.0).abs() < 1e-3, "ty={ty}");
    }

    /// 文字在 draw 形状之前 push：索引仍应与之后形状一致（同画笔）
    #[test]
    fn text_before_shape_shares_transform_index() {
        let mut b = DrawBatch::new();
        b.set_position(12.0, 34.0);
        b.text(
            "A",
            Pos::new(0.0, 0.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(WHITE),
        );
        // 形状 Pos 为局部原点，与 batch 平移组合 → 共享 transform entry
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(RED));
        assert_eq!(
            b.texts.entries[0].transform_index(),
            b.vertices[0].transform_index
        );
        let (_, _, _, _, tx, ty) = mat6(&b.transform_table, b.texts.entries[0].transform_index());
        assert!((tx - 12.0).abs() < 1e-4 && (ty - 34.0).abs() < 1e-4);
    }

    /// 绘制顺序：父 shapes+texts 先于子；父文字会被不透明子盖住
    #[test]
    fn draw_order_parent_text_before_children() {
        let mut root = DrawBatch::new();
        root.clips_children = true;
        draw_rectangle(&mut root, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        root.text("R", Pos::new(0.0, 0.0), TextDef::default().font_size(12.0), TextOverride::from_color(WHITE));

        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 10.0, 10.0, Some(BLUE));
        child.text("C", Pos::new(0.0, 0.0), TextDef::default().font_size(12.0), TextOverride::from_color(WHITE));
        root.push_child(child);

        let mut flat: Vec<Option<&DrawBatch>> = Vec::new();
        root.flatten_with_pop(&mut flat);
        // root(有字) → child(有字) → Pop
        assert_eq!(flat.len(), 3);
        assert!(!flat[0].unwrap().texts.entries.is_empty());
        assert!(!flat[1].unwrap().texts.entries.is_empty());
        assert!(flat[2].is_none());
        // 子在父之后 → 同区域会盖住父文字（文档化行为，非 bug）
        assert!(flat[0].unwrap().clips_children);
    }

    /// 单 batch 含 area_include → flatten 输出 AreaOp(setup) + Batch + AreaOp(cleanup)
    #[test]
    fn area_flatten_include_emits_cover_and_erase() {
        let mut b = DrawBatch::new();
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        // 用一个简单矩形作 include
        let mut include_batch = DrawBatch::new();
        draw_rectangle(&mut include_batch, Pos::new(0.0, 0.0), 100.0, 100.0, Some(WHITE));
        b.area_include = Some(include_batch.to_area());

        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        // 1 cover op + 1 Batch + 1 erase op = 3 events
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], DrawEvent::AreaOp { is_setup: true, .. }));
        assert!(matches!(events[1], DrawEvent::Batch(_)));
        assert!(matches!(events[2], DrawEvent::AreaOp { is_setup: false, .. }));
    }

    /// 嵌套 clips_children + Area：AreaOp 套住子树，clips Push/Pop 仍在子树内部
    #[test]
    fn area_flatten_with_clips_children() {
        let mut root = DrawBatch::new();
        root.clips_children = true;
        draw_rectangle(&mut root, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        // root 加 area_include
        let mut incl = DrawBatch::new();
        draw_rectangle(&mut incl, Pos::new(0.0, 0.0), 100.0, 100.0, Some(RED));
        root.area_include = Some(incl.to_area());

        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 4.0, 4.0, Some(GREEN));
        root.push_child(child);

        let mut events: Vec<DrawEvent> = Vec::new();
        root.flatten_events(&mut events, 0, None, &FxHashMap::default());
        // 1 setup + Batch + child.Batch + StencilPop + 1 cleanup = 5
        assert_eq!(events.len(), 5);
        assert!(matches!(events[0], DrawEvent::AreaOp { is_setup: true, .. }));
        assert!(matches!(events[1], DrawEvent::Batch(_)));
        assert!(matches!(events[2], DrawEvent::Batch(_)));
        assert!(matches!(events[3], DrawEvent::StencilPop));
        assert!(matches!(events[4], DrawEvent::AreaOp { is_setup: false, .. }));
    }

    /// effective Area = Empty → 不发 AreaOp
    #[test]
    fn area_flatten_empty_skips_ops() {
        let mut b = DrawBatch::new();
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        // empty 几何 → Area::Empty
        b.area_include = Some(Area::Empty);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        // 仅 Batch（无 AreaOp）
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DrawEvent::Batch(_)));
    }

    /// Area + clips_children：子 Test ref = cover 后 buffer（2），不是双重计数 3
    #[test]
    fn area_plus_clips_child_stencil_ref_not_double_counted() {
        let mut parent = DrawBatch::new();
        parent.clips_children = true;
        draw_rectangle(&mut parent, Pos::new(0.0, 0.0), 100.0, 100.0, Some(RED));
        let mut incl = DrawBatch::new();
        draw_circle(&mut incl, Pos::new(50.0, 50.0), 40.0, Some(WHITE));
        parent.area_include = Some(incl.to_area());
        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::NONE; // clipped 默认 true
        draw_rectangle(&mut child, Pos::new(10.0, 10.0), 20.0, 20.0, Some(GREEN));
        parent.push_child(child);

        let mut events: Vec<DrawEvent> = Vec::new();
        parent.flatten_events(&mut events, 0, None, &FxHashMap::default());

        // 模拟 draw 路径：clip_depth 与 area_depth 分离
        let mut clip_depth = 0u32;
        let mut area_depth = 0u32;
        let mut prev_cleanup = false;
        let mut child_test_ref: Option<u32> = None;
        let mut parent_push_ref: Option<u32> = None;

        for ev in &events {
            match ev {
                DrawEvent::Batch(batch) => {
                    prev_cleanup = false;
                    let has_own = batch
                        .effective_area()
                        .as_ref()
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    let anc = area_depth;
                    if has_own {
                        area_depth += 1;
                    }
                    let content = clip_depth + anc + (has_own as u32);
                    let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty();
                    if batch.clips_children && has_geom {
                        parent_push_ref = Some(content);
                        clip_depth += 1;
                    } else if content > 0 && batch.inherit.clipped && has_geom {
                        child_test_ref = Some(content);
                    }
                }
                DrawEvent::StencilPop => {
                    prev_cleanup = false;
                    clip_depth = clip_depth.saturating_sub(1);
                }
                DrawEvent::AreaOp { is_setup, .. } => {
                    if !*is_setup {
                        if !prev_cleanup {
                            area_depth = area_depth.saturating_sub(1);
                        }
                        prev_cleanup = true;
                    } else {
                        prev_cleanup = false;
                    }
                }
                _ => {}
            }
        }
        // cover@0 → buffer 1；parent content=1 Push@1 → buffer 2；child Test@2
        assert_eq!(parent_push_ref, Some(1), "parent Push ref");
        assert_eq!(child_test_ref, Some(2), "child Test must be 2 not 3");
    }

    // ---- culling tests ----

    #[test]
    fn bounds_culls_offscreen_subtree() {
        let mut b = DrawBatch::new();
        b.bounds = Some(Some(Rect::new(9999.0, 9999.0, 10.0, 10.0)));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default());
        assert!(events.is_empty());
    }

    #[test]
    fn bounds_keeps_onscreen_subtree() {
        let mut b = DrawBatch::new();
        b.bounds = Some(Some(Rect::new(100.0, 100.0, 50.0, 50.0)));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default());
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DrawEvent::Batch(_)));
    }

    #[test]
    fn auto_aabb_culls_offscreen_vertices() {
        let mut b = DrawBatch::new();
        // 无 bounds → 自动从顶点算 AABB
        b.set_position(9999.0, 9999.0);
        draw_rectangle(&mut b, Pos::ZERO, 4.0, 4.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default());
        assert!(events.is_empty());
    }

    #[test]
    fn empty_container_with_offscreen_children_recurse() {
        // 空容器无 bounds → 不能剪，自身体现为 event（无顶点）
        // 子屏外 → 子被剪
        let mut parent = DrawBatch::new(); // 无顶点
        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::TRANSFORM;
        child.set_position(9999.0, 9999.0);
        draw_rectangle(&mut child, Pos::ZERO, 4.0, 4.0, Some(WHITE));
        parent.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        parent.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default());
        // parent 空容器 → 自身 event（无顶点），子剪掉
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], DrawEvent::Batch(b) if b.vertices.is_empty()));
    }

    #[test]
    fn scissor_emits_scissor_events() {
        let mut b = DrawBatch::new();
        b.clips_children = true;
        b.scissor = Some(Rect::new(10.0, 10.0, 200.0, 150.0));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
        assert!(matches!(&events[1], DrawEvent::ScissorPush(r) if *r == Rect::new(10.0, 10.0, 200.0, 150.0)));
        assert!(matches!(&events[2], DrawEvent::Batch(_)));
        assert!(matches!(&events[3], DrawEvent::ScissorPop));
    }

    #[test]
    fn scissor_without_clips_children_still_emits_scissor() {
        let mut b = DrawBatch::new();
        b.scissor = Some(Rect::new(0.0, 0.0, 100.0, 100.0));
        b.clips_children = false;
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        // scissor 现在不依赖 clips_children，仍会为子节点发 ScissorPush/Pop
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
        assert!(matches!(&events[1], DrawEvent::ScissorPush(_)));
        assert!(matches!(&events[2], DrawEvent::Batch(_)));
        assert!(matches!(&events[3], DrawEvent::ScissorPop));
    }

    #[test]
    fn scissor_does_not_set_uses_stencil() {
        let mut b = DrawBatch::new();
        b.clips_children = true;
        b.scissor = Some(Rect::new(0.0, 0.0, 100.0, 100.0));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        let uses_stencil = events.iter().any(|e| matches!(e, DrawEvent::StencilPop | DrawEvent::AreaOp { .. }));
        assert!(!uses_stencil);
    }

    #[test]
    fn auto_scissor_detects_single_rect() {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        b.clips_children = true;
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 100.0, 50.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        // 自动检测为矩形 → ScissorPush/Pop 代替 StencilPop
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[1], DrawEvent::ScissorPush(r) if (r.w - 100.0).abs() < 1e-4));
        assert!(matches!(&events[3], DrawEvent::ScissorPop));
        assert!(events.iter().all(|e| !matches!(e, DrawEvent::StencilPop)));
    }

    #[test]
    fn auto_scissor_ignores_nonrect() {
        let mut b = DrawBatch::new();
        b.clips_children = true;
        // 三角形（3 顶点），不是矩形
        crate::shapes::draw_triangle(&mut b, 0.0, 0.0, 100.0, 0.0, 0.0, 50.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        // 非矩形 → 走 stencil
        assert!(events.iter().any(|e| matches!(e, DrawEvent::StencilPop)));
    }

    #[test]
    fn auto_scissor_no_children_skips_scissor_events() {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        b.clips_children = true;
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 100.0, 50.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        // 无子：不发空 scissor Push/Pop
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
    }

    #[test]
    fn auto_scissor_requires_clips_children() {
        let mut b = DrawBatch::new();
        b.clips_children = false;
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 100.0, 50.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
        assert!(matches!(&events[1], DrawEvent::Batch(_)));
    }

    #[test]
    fn stencil_pop_when_children_all_culled() {
        // 非矩形父 → stencil 路径；子全 cull 仍要 Pop（与 Push 成对）
        let mut parent = DrawBatch::new();
        parent.clips_children = true;
        crate::shapes::draw_triangle(
            &mut parent,
            0.0, 0.0, 100.0, 0.0, 0.0, 50.0,
            Some(RED),
        );
        let mut child = DrawBatch::new();
        child.bounds = Some(Some(Rect::new(9999.0, 9999.0, 4.0, 4.0)));
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        parent.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        parent.flatten_events(
            &mut events,
            0,
            Some(Rect::new(0.0, 0.0, 800.0, 600.0)),
            &FxHashMap::default(),
        );
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
        assert!(
            events.iter().any(|e| matches!(e, DrawEvent::StencilPop)),
            "culled children must still emit StencilPop"
        );
    }

    #[test]
    fn auto_aabb_culls_offscreen_pos() {
        // Pos 进表、画笔 restore 后：AABB 仍应按世界位置裁
        let mut b = DrawBatch::new();
        draw_rectangle(&mut b, Pos::new(9999.0, 9999.0), 4.0, 4.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(
            &mut events,
            0,
            Some(Rect::new(0.0, 0.0, 800.0, 600.0)),
            &FxHashMap::default(),
        );
        assert!(events.is_empty());
    }

    #[test]
    fn auto_scissor_uses_pos_world_rect() {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        b.clips_children = true;
        draw_rectangle(&mut b, Pos::new(50.0, 60.0), 100.0, 50.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        assert_eq!(events.len(), 4);
        match &events[1] {
            DrawEvent::ScissorPush(r) => {
                assert!((r.x - 50.0).abs() < 1e-3, "x={}", r.x);
                assert!((r.y - 60.0).abs() < 1e-3, "y={}", r.y);
                assert!((r.w - 100.0).abs() < 1e-3);
                assert!((r.h - 50.0).abs() < 1e-3);
            }
            _ => panic!("expected ScissorPush"),
        }
    }

    #[test]
    fn auto_scissor_skips_sdf_circle() {
        let mut b = DrawBatch::new();
        b.clips_children = true;
        b.sdf_feather = Some(1.0);
        draw_circle(&mut b, Pos::new(100.0, 100.0), 40.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default());
        // SDF 圆不得 auto-scissor → StencilPop
        assert!(events.iter().any(|e| matches!(e, DrawEvent::StencilPop)));
        assert!(events.iter().all(|e| !matches!(e, DrawEvent::ScissorPush(_))));
    }

    #[test]
    fn text_only_batch_not_culled_when_onscreen() {
        let mut b = DrawBatch::new();
        b.text(
            "hi",
            Pos::new(10.0, 10.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(WHITE),
        );
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(
            &mut events,
            0,
            Some(Rect::new(0.0, 0.0, 800.0, 600.0)),
            &FxHashMap::default(),
        );
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn transform_then_rotation_matches_table_compose() {
        // 验证 then 与列主序表布局一致（文字 override 依赖此）
        let m = Transform::translation(10.0, 20.0);
        let o = Transform::trs(0.0, 0.0, 0.0, 0.0, std::f32::consts::FRAC_PI_2, 1.0, 1.0);
        let c = m.then(&o);
        let (c0, c1, c2) = c.to_cols();
        // 90° 顺时针：a=0,b=-1,c=1,d=0；平移 (10,20)
        assert!(c0[0].abs() < 1e-5);
        assert!((c1[0] + 1.0).abs() < 1e-5);
        assert!((c0[1] - 1.0).abs() < 1e-5);
        assert!(c1[1].abs() < 1e-5);
        assert!((c2[0] - 10.0).abs() < 1e-3);
        assert!((c2[1] - 20.0).abs() < 1e-3);
    }

    #[test]
    fn custom_material_new_and_clear_are_none() {
        let mut b = DrawBatch::new();
        assert!(b.custom_material.is_none());
        b.clear();
        assert!(b.custom_material.is_none());
    }

    #[test]
    fn draw_batch_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DrawBatch>();
    }

    #[test]
    fn draw_batch_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DrawBatch>();
    }
}
