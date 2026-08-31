//! 渲染核心：批量绘制、渲染目标和渲染器。

use parking_lot::Mutex;
use std::sync::Arc;
use rustc_hash::FxHashMap;

use wgpu::util::DeviceExt;

pub use crate::gpu::Vertex;
pub use crate::math::{Pos, Rect, Transform, UvRect};
use crate::gpu::{GpuContext, GeoInstance, GeoVertex, ShapeInstance};
use crate::material::Material;
use crate::area::AreaStencilOp;

mod batch;
mod cull;
mod draw;
pub use batch::{DrawBatch, InheritFromParent};
pub(crate) use batch::BatchShapeCommand;
pub(crate) use cull::{prepare_culling, AabbMap, ViewMap};

/// CPU 真实数据分布（诊断用）。
///
/// - `mesh_vertices`：CPU 推入的 mesh 顶点数（仅 mesh 路径使用）
/// - `sdf_instances`：SDF instance 参数数（SDF 实例路径）
/// - `geo_instances`：几何模板实例参数数（几何模板实例路径，含重复引用）
/// - `geo_templates`：几何模板**条数**（去重后，`geo_templates.len()`）；
///   同参数形状共享同一模板，反映模板复用后的实际几何种类数
/// - `geo_template_vertices`：几何模板顶点总量（模板共享后的实际顶点量；
///   各 geo 实例引用其中一段，通常远小于各实例展开顶点数之和）
///
/// 注：GPU 端最终输出顶点数 ≠ 上述任一字段；
/// instance 路径 GPU 输出 = `sdf_instances * 4`（每个 instance 1 个 unit quad）；
/// geo instance 路径 GPU 输出 = `sum(geo_instances[i].template_vertex_count)`。
/// 合并值见 [`DrawBatch::shape_vertex_count`].
///
/// **draw call 数不在本结构**：真实 draw call 由渲染器统计（
/// [`crate::render::Renderer::last_draw_calls`]，经 [`crate::window::VireoWindow::last_draw_calls`]
/// 获取），而非 batch 侧 `shape_commands.len()` 的估算值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeStats {
    pub mesh_vertices: usize,
    pub sdf_instances: usize,
    pub geo_instances: usize,
    pub geo_templates: usize,
    pub geo_template_vertices: usize,
}

/// 形状与文字共享状态机的并集覆盖（去重）。
///
/// `uv` / `bind_group` / `color` / `transform` 在 `DrawBatch` 形状与文字间本就共享
///（`uv`→`TextTextureState`, `bind_group`→`TextTextureState.view`, `color`→`batch_color`,
/// `transform`→`transform_table`），故并集仅保留一份；独有位为
/// `sdf_feather`（形状）与 `text_clip`（文字）。
///
/// 构造按集合覆盖：`shape(ShapeOverride)` / `text(TextOverride)` 用对应覆盖的所有 `Some` 字段
/// 覆盖并集；`sdf_feather` / `text_clip` 只动独有位。
#[derive(Clone, Debug, Default)]
pub struct BatchOverride {
    pub color: Option<crate::color::Color>,
    pub sdf_feather: Option<Option<f32>>,
    pub uv: Option<UvRect>,
    pub transform: Option<Transform>,
    pub bind_group: Option<Option<wgpu::BindGroup>>,
    pub text_clip: Option<Option<crate::glyphon::TextBounds>>,
}

impl BatchOverride {
    pub fn new() -> Self {
        Self::default()
    }
    /// 用 `ShapeOverride` 的所有 `Some` 字段覆盖（`sdf_feather` 含独有位）。
    pub fn shape(mut self, s: crate::shapes::ShapeOverride) -> Self {
        if s.color.is_some() {
            self.color = s.color;
        }
        if s.sdf_feather.is_some() {
            self.sdf_feather = s.sdf_feather;
        }
        if s.uv.is_some() {
            self.uv = s.uv;
        }
        if s.transform.is_some() {
            self.transform = s.transform;
        }
        if s.bind_group.is_some() {
            self.bind_group = s.bind_group;
        }
        self
    }
    /// 用 `TextOverride` 的 `Some` 字段覆盖（`color/transform/uv/bind_group` 为共享位，`clip` 为独有位）。
    pub fn text(mut self, t: crate::text::TextOverride) -> Self {
        if t.color.is_some() {
            self.color = t.color;
        }
        if t.clip.is_some() {
            self.text_clip = t.clip;
        }
        if t.transform.is_some() {
            self.transform = t.transform;
        }
        if t.uv.is_some() {
            self.uv = t.uv;
        }
        if t.bind_group.is_some() {
            self.bind_group = t.bind_group;
        }
        self
    }
    /// 仅覆盖独有：文字裁切
    pub fn text_clip(mut self, clip: Option<crate::glyphon::TextBounds>) -> Self {
        self.text_clip = Some(clip);
        self
    }
    /// 仅覆盖独有：形状 SDF 柔边
    pub fn sdf_feather(mut self, f: Option<f32>) -> Self {
        self.sdf_feather = Some(f);
        self
    }
    pub fn color(mut self, c: crate::color::Color) -> Self {
        self.color = Some(c);
        self
    }
    pub fn transform(mut self, t: Transform) -> Self {
        self.transform = Some(t);
        self
    }
    pub fn uv(mut self, uv: UvRect) -> Self {
        self.uv = Some(uv);
        self
    }
    pub fn bind_group(mut self, bg: Option<wgpu::BindGroup>) -> Self {
        self.bind_group = Some(bg);
        self
    }
}

/// 一次 `Renderer::draw` 的扁平事件序列（模块级，扁平方法可引用）。
///
/// - `Batch`：常规 batch（自身 shapes + texts）。有效视图（累计祖先 view）在
///   `flatten_events` 时写入旁路 `view_map`（按 batch 指针索引），不放进事件。
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
    material: Option<Arc<Material>>,
}

#[derive(Clone)]
struct GeoInstanceSegment {
    geo_instance_start: u32,
    geo_instance_count: u32,
    template_vertex_start: u32,
    template_index_start: u32,
    index_count: u32,
    bind_group: wgpu::BindGroup,
    material: Option<Arc<Material>>,
}

/// 几何模板：batch 内 `geo_template_vertices` / `geo_template_indices` 的一段。
/// 模板数据与 color/transform 无关，可被多个 `GeoInstance` 共享。
#[derive(Clone, Copy, Debug)]
pub(crate) struct GeoTemplate {
    vertex_start: u32,
    index_start: u32,
    index_count: u32,
    vertex_count: u32,
}

enum OrderedShapeSegment {
    Mesh {
        ndx_start: u32,
        ndx_count: u32,
        bind_group: wgpu::BindGroup,
        geometry: bool,
        material: Option<Arc<Material>>,
    },
    Instances(InstanceSegment),
    GeoInstances(GeoInstanceSegment),
}

impl OrderedShapeSegment {
    /// 排序键：`(0 = mesh, 1 = instances, 2 = geo instances, geometry/bg)`。
    /// 同类且同 bind group 的段会被排到一起以便合并。
    #[inline]
    fn sort_key(&self) -> (u8, u8, u64) {
        match self {
            OrderedShapeSegment::Mesh { bind_group, geometry, .. } => {
                (0, *geometry as u8, bind_group_id(bind_group))
            }
            OrderedShapeSegment::Instances(s) => (1, 0, bind_group_id(&s.bind_group)),
            OrderedShapeSegment::GeoInstances(s) => (2, 0, bind_group_id(&s.bind_group)),
        }
    }

    /// 尝试把 `self`（前段）与 `other`（后段）合并为一段。
    /// 仅当 pipeline 状态相同（bind group / geometry / 模板）且范围连续时可合并，
    /// 否则返回 `None`。不连续时合并会误画两段之间的内容，必须拒绝。
    fn try_merge(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (
                OrderedShapeSegment::Mesh { ndx_start, ndx_count, bind_group, geometry, material },
                OrderedShapeSegment::Mesh {
                    ndx_start: n2,
                    ndx_count: n2_count,
                    bind_group: b2,
                    geometry: g2,
                    material: m2,
                },
            ) => {
                let merged = merge_decision(
                    geometry == g2 && bind_group == b2
                        && material.as_ref().map(Arc::as_ptr) == m2.as_ref().map(Arc::as_ptr),
                    *ndx_start,
                    *ndx_count,
                    *n2,
                    *n2_count,
                )?;
                Some(OrderedShapeSegment::Mesh {
                    ndx_start: merged.0,
                    ndx_count: merged.1,
                    bind_group: bind_group.clone(),
                    geometry: *geometry,
                    material: material.clone(),
                })
            }
            (OrderedShapeSegment::Instances(s), OrderedShapeSegment::Instances(s2)) => {
                let merged = merge_decision(
                    s.bind_group == s2.bind_group
                        && s.material.as_ref().map(Arc::as_ptr)
                            == s2.material.as_ref().map(Arc::as_ptr),
                    s.instance_start,
                    s.instance_count,
                    s2.instance_start,
                    s2.instance_count,
                )?;
                Some(OrderedShapeSegment::Instances(InstanceSegment {
                    instance_start: merged.0,
                    instance_count: merged.1,
                    bind_group: s.bind_group.clone(),
                    material: s.material.clone(),
                }))
            }
            (
                OrderedShapeSegment::GeoInstances(s),
                OrderedShapeSegment::GeoInstances(s2),
            ) => {
                let merged = merge_decision(
                    s.bind_group == s2.bind_group
                        && s.template_vertex_start == s2.template_vertex_start
                        && s.template_index_start == s2.template_index_start
                        && s.index_count == s2.index_count
                        && s.material.as_ref().map(Arc::as_ptr)
                            == s2.material.as_ref().map(Arc::as_ptr),
                    s.geo_instance_start,
                    s.geo_instance_count,
                    s2.geo_instance_start,
                    s2.geo_instance_count,
                )?;
                Some(OrderedShapeSegment::GeoInstances(GeoInstanceSegment {
                    geo_instance_start: merged.0,
                    geo_instance_count: merged.1,
                    template_vertex_start: s.template_vertex_start,
                    template_index_start: s.template_index_start,
                    index_count: s.index_count,
                    bind_group: s.bind_group.clone(),
                    material: s.material.clone(),
                }))
            }
            _ => None,
        }
    }
}

/// 合并决策（纯函数）：`same_state` = pipeline 状态一致（bind group / geometry）。
/// 仅当状态一致且范围连续（`start + count == next_start`）时返回合并后的 `(start, count)`。
fn merge_decision(
    same_state: bool,
    start: u32,
    count: u32,
    next_start: u32,
    next_count: u32,
) -> Option<(u32, u32)> {
    if same_state && start + count == next_start {
        Some((start, count + next_count))
    } else {
        None
    }
}

/// wgpu `BindGroup` 的稳定身份。`BindGroup` 实现 `Eq`/`Hash` 但无 `Ord`，
/// 排序键需要标量；用 `FxHasher` 折叠 hash 得到 u64 即可（同 bind group 恒同值）。
fn bind_group_id(bg: &wgpu::BindGroup) -> u64 {
    use std::hash::{BuildHasher, Hash, Hasher};
    let mut hasher = rustc_hash::FxBuildHasher::default().build_hasher();
    bg.hash(&mut hasher);
    hasher.finish()
}

struct ShapeInfo {
    base_vertex: i32,
    segments: Vec<ShapeSegment>,
    geometry: bool,
    instances: Vec<InstanceSegment>,
    geo_instances: Vec<GeoInstanceSegment>,
    ordered: Vec<OrderedShapeSegment>,
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
/// 窗口和离屏纹理都通过此类型编码绘制命令。
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
    vertex_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    index_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    instance_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    geo_instance_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    geo_template_vertex_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    geo_template_index_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    physical_width: u32,
    physical_height: u32,
    scale: f32,
    /// 文字 shader `screen_resolution` 覆盖（`layout_follow` 拖动中用）。
    /// `Some((w,h))` = 虚拟新物理尺寸（新逻辑 × dpi）：glyph 不重新光栅化
    /// （scale/dpi 不变 → 图集 cache key 稳定），仅 shader NDC 映射补偿 DXGI 拉伸。
    /// `None` = 用 `physical_width/height`（旧 surface 尺寸）。
    text_viewport_override: Mutex<Option<(u32, u32)>>,
    sample_count: u32,
    alpha_to_coverage: bool,
    ssaa: bool,
    msaa_tex: Mutex<Option<(wgpu::Texture, wgpu::TextureView)>>,
    ds_tex: Mutex<Option<(wgpu::Texture, wgpu::TextureView)>>,
    polygon_edge_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    transform_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    engine_storage_bind_group_cache: Mutex<Option<wgpu::BindGroup>>,
    /// 逻辑视口尺寸（逻辑像素，浮点用户坐标系）
    logical_width: f32,
    logical_height: f32,
    /// 帧间复用的 CPU 暂存，避免每帧大块分配
    scratch_vdata: Mutex<Vec<u8>>,
    scratch_idata: Mutex<Vec<u8>>,
    scratch_transforms: Mutex<Vec<f32>>,
    scratch_poly_edges: Mutex<Vec<f32>>,
    scratch_event_infos: Mutex<Vec<EventInfo>>,
    scratch_aabb_map: Mutex<AabbMap>,
    scratch_view_map: Mutex<ViewMap>,
    scratch_view_table: Mutex<Vec<f32>>,
    scratch_ref_stack: Mutex<Vec<u32>>,
    scratch_batch_transform_bases: Mutex<Vec<u32>>,
    scratch_batch_poly_base: Mutex<Vec<u32>>,
    scratch_batch_geo_vertex_base: Mutex<Vec<u32>>,
    scratch_batch_geo_index_base: Mutex<Vec<u32>>,
    scratch_last_dynamic_offsets: Mutex<Vec<u32>>,
    scratch_scissor_stack: Mutex<Vec<(u32, u32, u32, u32)>>,
    scratch_instances: Mutex<Vec<ShapeInstance>>,
    scratch_geo_instances: Mutex<Vec<GeoInstance>>,
    scratch_geo_vertices: Mutex<Vec<GeoVertex>>,
    scratch_geo_indices: Mutex<Vec<u32>>,
    scratch_geo_merge_per_inst_seg: Mutex<Vec<u32>>,
    scratch_geo_merge_order: Mutex<Vec<u32>>,
    scratch_geo_merge_sorted: Mutex<Option<Vec<u32>>>,
    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数（真实 draw call 数）。
    /// `preserve_order=false` 重排合并后此值下降（bench 场景 3 混合可 1000→2）。
    last_draw_calls: Mutex<u32>,
}

impl Renderer {
    /// 访问 GPU context（用于离屏像素回读等需要 device/queue 的场景）。
    pub fn gpu(&self) -> &std::sync::Arc<GpuContext> {
        &self.gpu
    }
    pub fn new(
        gpu: std::sync::Arc<GpuContext>,
        logical_width: f32,
        logical_height: f32,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
        aa: crate::window::AntiAliasing,
        dpi_scale: f32,
    ) -> Self {
        let proj = glam::camera::rh::proj::opengl::orthographic(0.0, logical_width, logical_height, 0.0, -1.0, 1.0);
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
            vertex_buf: Mutex::new(None),
            index_buf: Mutex::new(None),
            instance_buf: Mutex::new(None),
            geo_instance_buf: Mutex::new(None),
            geo_template_vertex_buf: Mutex::new(None),
            geo_template_index_buf: Mutex::new(None),
            physical_width,
            physical_height,
            scale,
            text_viewport_override: Mutex::new(None),
            sample_count: aa.sample_count(),
            alpha_to_coverage: aa.alpha_to_coverage(),
            ssaa: aa.is_ssaa(),
            msaa_tex: Mutex::new(None),
            ds_tex: Mutex::new(None),
            polygon_edge_buf: Mutex::new(None),
            transform_buf: Mutex::new(None),
            engine_storage_bind_group_cache: Mutex::new(None),
            scratch_vdata: Mutex::new(Vec::new()),
            scratch_idata: Mutex::new(Vec::new()),
            scratch_transforms: Mutex::new(Vec::new()),
            scratch_poly_edges: Mutex::new(Vec::new()),
            scratch_event_infos: Mutex::new(Vec::new()),
            scratch_aabb_map: Mutex::new(FxHashMap::default()),
            scratch_view_map: Mutex::new(FxHashMap::default()),
            scratch_view_table: Mutex::new(Vec::new()),
            scratch_ref_stack: Mutex::new(Vec::new()),
            scratch_batch_transform_bases: Mutex::new(Vec::new()),
            scratch_batch_poly_base: Mutex::new(Vec::new()),
            scratch_batch_geo_vertex_base: Mutex::new(Vec::new()),
            scratch_batch_geo_index_base: Mutex::new(Vec::new()),
            scratch_last_dynamic_offsets: Mutex::new(Vec::new()),
            scratch_scissor_stack: Mutex::new(Vec::new()),
            scratch_instances: Mutex::new(Vec::new()),
            scratch_geo_instances: Mutex::new(Vec::new()),
            scratch_geo_vertices: Mutex::new(Vec::new()),
            scratch_geo_indices: Mutex::new(Vec::new()),
            scratch_geo_merge_per_inst_seg: Mutex::new(Vec::new()),
            scratch_geo_merge_order: Mutex::new(Vec::new()),
            scratch_geo_merge_sorted: Mutex::new(None),
            last_draw_calls: Mutex::new(0),
            logical_width,
            logical_height,
        }
    }

    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数。
    /// 由 [`Self::draw`] 在每帧统计；未 draw 时为 0。
    pub fn last_draw_calls(&self) -> u32 {
        *self.last_draw_calls.lock()
    }

    /// 更新抗锯齿设置。
    pub fn update_aa(&mut self, aa: crate::window::AntiAliasing) {
        self.sample_count = aa.sample_count();
        self.alpha_to_coverage = aa.alpha_to_coverage();
        self.ssaa = aa.is_ssaa();
        *self.msaa_tex.lock() = None;
        *self.ds_tex.lock() = None;
    }

    /// 获取匹配当前 sample_count 的 pipeline

    /// 获取 multisampled 视图（必要时创建），无 MSAA 返回 None
    fn msaa_view(&self, format: wgpu::TextureFormat) -> Option<wgpu::TextureView> {
        if self.sample_count <= 1 { return None; }
        let mut mt = self.msaa_tex.lock();
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
        let mut dt = self.ds_tex.lock();
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

    /// 只更新相机投影 + 逻辑尺寸/scale/dpi 字段，**不**触碰 surface 相关资源
    /// （`physical_width/height`、msaa/ds 纹理保持旧 surface 尺寸）。
    ///
    /// `layout_follow`（`VireoWindow::set_layout_follow`，默认开）拖动中每帧调用：
    /// 窗口已变但 surface 未重配时，把 camera 切到新逻辑尺寸 → 几何/形状按新布局
    /// 实时重排。复合映射 `logical→NDC→旧surface→window` 对 x/y 分别使用新宽/高，
    /// 宽高比变化时仍可逐轴精确映射；残余误差来自尺寸采样时序、整数舍入和 DPI
    /// 转换。`scale` 保持 dpi 不变（glyph 光栅化 cache key 稳定），文字拉伸由
    /// `set_text_viewport_override` 在 shader 层补偿；`dpi_scale` 保持 OS 缩放
    /// （SDF feather 用）。
    pub(crate) fn update_layout(
        &mut self,
        logical_width: f32,
        logical_height: f32,
        scale: f32,
        dpi_scale: f32,
    ) {
        let proj = glam::camera::rh::proj::opengl::orthographic(0.0, logical_width, logical_height, 0.0, -1.0, 1.0);
        let camera_data: [[f32; 4]; 4] = proj.to_cols_array_2d();
        let mut camera_raw = [0u8; 80];
        camera_raw[..64].copy_from_slice(bytemuck::cast_slice(&camera_data));
        camera_raw[64..68].copy_from_slice(&dpi_scale.to_le_bytes());
        self.gpu.queue.write_buffer(&self.camera_buf, 0, &camera_raw);
        self.logical_width = logical_width;
        self.logical_height = logical_height;
        self.scale = scale;
    }

    /// 设置文字 shader `screen_resolution` 覆盖（`layout_follow` 拖动中）。
    /// `None` = 用物理 surface 尺寸；`Some((w,h))` = 虚拟新物理尺寸（新逻辑 × dpi），
    /// glyph 不重新光栅化（`scale` 保持 dpi 不变），纯 shader 层 NDC 补偿拉伸。
    pub(crate) fn set_text_viewport_override(&self, size: Option<(u32, u32)>) {
        *self.text_viewport_override.lock() = size;
    }

    /// 更新相机投影（窗口 resize 时调用）。
    /// `scale`：逻辑→物理（文字/scissor）；`dpi_scale`：OS 缩放（SDF feather，可与 scale 不同）。
    /// surface 已重配时调用（重建 msaa/ds 纹理以匹配新物理尺寸）。
    pub fn resize(
        &mut self,
        logical_width: f32,
        logical_height: f32,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
        dpi_scale: f32,
    ) {
        self.update_layout(logical_width, logical_height, scale, dpi_scale);
        self.physical_width = physical_width;
        self.physical_height = physical_height;
        *self.text_viewport_override.lock() = None;
        *self.msaa_tex.lock() = None;
        *self.ds_tex.lock() = None;
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
        let mut slot = self.vertex_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size { return; }
        let new_cap = if cur == 0 { size.next_power_of_two() } else { (cur * 2).max(size) };
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buf, new_cap));
    }

    fn ensure_instance_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.instance_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size { return; }
        let new_cap = if cur == 0 { size.next_power_of_two() } else { (cur * 2).max(size) };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo shape instance buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
    }

    fn ensure_geo_instance_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.geo_instance_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size { return; }
        let new_cap = if cur == 0 { size.next_power_of_two() } else { (cur * 2).max(size) };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo geo instance buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
    }

    fn ensure_geo_template_vertex_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.geo_template_vertex_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size { return; }
        let new_cap = if cur == 0 { size.next_power_of_two() } else { (cur * 2).max(size) };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo geo template vertex buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
    }

    fn ensure_geo_template_index_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.geo_template_index_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size { return; }
        let new_cap = if cur == 0 { size.next_power_of_two() } else { (cur * 2).max(size) };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo geo template index buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
    }

    fn ensure_index_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.index_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size { return; }
        let new_cap = if cur == 0 { size.next_power_of_two() } else { (cur * 2).max(size) };
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("index buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buf, new_cap));
    }

    fn ensure_polygon_edge_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.polygon_edge_buf.lock();
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
        *self.engine_storage_bind_group_cache.lock() = None;
    }

    fn ensure_transform_buffer(&self, size: u64) {
        if size == 0 { return; }
        let mut slot = self.transform_buf.lock();
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
        *self.engine_storage_bind_group_cache.lock() = None;
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::area::Area;
    use crate::color::colors::*;
    use crate::math::{left_mul_view_table, transform_key};
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
    fn with_override_applies_then_restores() {
        let mut b = DrawBatch::new();
        b.set_color(crate::color::Color::new(1.0, 0.0, 0.0, 1.0));
        let before = b.color();
        let mut seen: Option<crate::color::Color> = None;
        b.with_override(
            BatchOverride::default().color(crate::color::Color::new(0.0, 1.0, 0.0, 1.0)),
            |_b, c| {
                seen = Some(c);
            },
        );
        assert_eq!(b.color(), before);
        assert_eq!(
            seen,
            Some(crate::color::Color::new(0.0, 1.0, 0.0, 1.0))
        );
    }

    #[test]
    fn with_override_noop_passthrough() {
        let mut b = DrawBatch::new();
        b.set_color(crate::color::Color::new(1.0, 0.0, 0.0, 1.0));
        let before = b.color();
        let mut ran = false;
        b.with_override(
            BatchOverride::default(),
            |_b, c| {
                ran = true;
                assert_eq!(c, before);
            },
        );
        assert!(ran);
        assert_eq!(b.color(), before);
    }

    #[test]
    fn with_override_text_clip_and_shared_color() {
        let mut b = DrawBatch::new();
        let clip = crate::glyphon::TextBounds { left: 0, top: 0, right: 10, bottom: 10 };
        let green = crate::color::Color::new(0.0, 1.0, 0.0, 1.0);
        b.with_override(
            BatchOverride::default().color(green).text_clip(Some(clip)),
            |b, c| {
                assert_eq!(c, green);
                assert_eq!(b.color(), green);
                assert_eq!(b.text_clip, Some(clip));
                // 形状与文字共享 batch.color：闭包内形状与文字 fallback 同色
                b.text("hi", Pos::new(0.0, 0.0), TextDef::default(), TextOverride::default());
                // 文字未显式 color，prepare 将用 batch_color (=green) 兜底，此处 entry 仍为 None
                assert_eq!(b.texts.entries[0].override_().color, None);
            },
        );
        // 退出后恢复
        assert_eq!(b.text_clip, None);
        assert_eq!(b.color(), crate::color::Color::new(1.0, 1.0, 1.0, 1.0));
    }

    #[test]
    fn text_override_has_uv_and_bind_group_shared() {
        let uv = UvRect { u0: 0.1, v0: 0.2, u1: 0.8, v1: 0.9 };
        let ov = TextOverride::default().uv(uv).clear_texture().color(RED);
        assert_eq!(ov.uv, Some(uv));
        assert_eq!(ov.bind_group, Some(None));
        assert_eq!(ov.color, Some(RED));
        let bo = BatchOverride::default().text(ov);
        assert_eq!(bo.uv, Some(uv));
        assert_eq!(bo.bind_group, Some(None));
        assert_eq!(bo.color, Some(RED));
    }

    #[test]
    fn with_override_uv_and_text_texture_restores() {
        let mut b = DrawBatch::new();
        let uv0 = b.uv();
        let uv1 = UvRect { u0: 0.1, v0: 0.2, u1: 0.3, v1: 0.4 };
        b.with_override(BatchOverride::default().uv(uv1), |b, _| {
            assert_eq!(b.uv(), uv1);
            assert_eq!(b.texts.texture_state.uv, uv1);
        });
        assert_eq!(b.uv(), uv0);
        assert_eq!(b.texts.texture_state.uv, uv0);
    }

    #[test]
    fn transform_index_stable_across_same_transform() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(10.0, 20.0);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 5.0, 5.0, Some(RED));
        draw_circle(&mut b, Pos::new(0.0, 0.0), 3.0, Some(BLUE));
        let idxs: Vec<u32> = b.instances.iter().map(|v| v.transform_index).collect();
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
        let i0 = b.instances[0].transform_index;
        let i1 = b.instances[1].transform_index;
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
        assert_eq!(b.instances[0].transform_index, 1);
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
        let start_local = b1.instances[0].sdf_params[0];
        assert_eq!(start_local, 0.0);

        let poly_base = edges0 as f32;
        let mut patched = b1.instances.clone();
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
        assert!(!batch.geo_instances.is_empty());
        assert!(!batch.geo_template_vertices.is_empty());
        assert!(!batch.geo_template_indices.is_empty());
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
    fn repeated_instance_polygon_reuses_edges() {
        let points = [(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)];
        let mut batch = DrawBatch::new();
        batch.instance_polygon(&points, Some(WHITE));
        batch.instance_polygon(&points, Some(WHITE));

        assert_eq!(batch.polygon_edges.len(), points.len() * 4);
        assert_eq!(batch.instances[0].sdf_params[0], 0.0);
        assert_eq!(batch.instances[1].sdf_params[0], 0.0);
    }

    #[test]
    fn polygon_and_line_chain_edges_use_distinct_templates() {
        let points = [(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)];
        let mut batch = DrawBatch::new();
        batch.instance_polygon(&points, Some(WHITE));
        batch.instance_line_chain(&points, 2.0, Some(WHITE));

        assert_eq!(batch.instances[0].sdf_params[0], 0.0);
        assert_eq!(batch.instances[1].sdf_params[0], 3.0);
        assert_eq!(batch.polygon_edges.len(), (3 + 2) * 4);
    }

    #[test]
    fn repeated_instance_line_chain_reuses_edges() {
        let points = [(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)];
        let mut batch = DrawBatch::new();
        batch.instance_line_chain(&points, 2.0, Some(WHITE));
        batch.instance_line_chain(&points, 4.0, Some(RED));

        assert_eq!(batch.polygon_edges.len(), 2 * 4);
        assert_eq!(batch.instances[0].sdf_params[0], 0.0);
        assert_eq!(batch.instances[1].sdf_params[0], 0.0);
        assert_eq!(batch.instances[1].sdf_params[2], 2.0);
    }

    #[test]
    fn edge_template_recovers_after_public_edge_buffer_mutation() {
        let points = [(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)];
        let mut batch = DrawBatch::new();
        batch.instance_polygon(&points, Some(WHITE));
        let expected = batch.polygon_edges.clone();

        batch.polygon_edges.clear();
        batch.instance_polygon(&points, Some(WHITE));

        assert_eq!(batch.polygon_edges, expected);
        assert_eq!(batch.instances[1].sdf_params[0], 0.0);
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
    fn shape_stats_separates_mesh_vertices_and_instances() {
        let mut batch = DrawBatch::new();
        // geo-instance 路径（sdf_feather=None）
        batch.sdf_feather = None;
        draw_circle(&mut batch, Pos::new(5.0, 5.0), 4.0, Some(RED));
        let geo_stats = batch.shape_stats();
        assert_eq!(geo_stats.mesh_vertices, 0, "geo 路径不应推 mesh 顶点");
        assert_eq!(geo_stats.sdf_instances, 0, "geo 路径不应有 SDF instance");
        assert!(geo_stats.geo_instances > 0, "geo 路径应推送 geo instance");
        assert_eq!(geo_stats.geo_instances, batch.geo_instances.len());
        assert!(geo_stats.geo_templates > 0, "geo 路径应有模板");
        assert_eq!(geo_stats.geo_templates, batch.geo_templates.len());
        assert!(geo_stats.geo_template_vertices > 0, "geo 路径应有模板顶点");
        assert_eq!(
            geo_stats.geo_template_vertices,
            batch.geo_template_vertices.len()
        );

        // instance 路径
        let mut batch2 = DrawBatch::new();
        batch2.sdf_feather = Some(1.0);
        draw_rectangle(&mut batch2, Pos::ZERO, 8.0, 8.0, Some(RED));
        draw_circle(&mut batch2, Pos::new(20.0, 0.0), 4.0, Some(BLUE));
        let sdf_stats = batch2.shape_stats();
        assert_eq!(sdf_stats.mesh_vertices, 0, "instance 路径不应推 mesh 顶点");
        assert_eq!(sdf_stats.sdf_instances, 2);
        assert_eq!(sdf_stats.geo_instances, 0);
        assert_eq!(sdf_stats.geo_templates, 0);
        assert_eq!(sdf_stats.geo_template_vertices, 0);
        // shape_vertex_count 仍是 4-顶点等价
        assert_eq!(sdf_stats.sdf_instances * 4, batch2.shape_vertex_count());
    }

    #[test]
    fn set_uv_propagates_to_text_entries() {
        let mut batch = DrawBatch::new();
        batch.set_uv(0.25, 0.25, 0.75, 0.75);
        let expected = UvRect { u0: 0.25, v0: 0.25, u1: 0.75, v1: 0.75 };
        assert_eq!((batch.uv.u0, batch.uv.u1), (expected.u0, expected.u1));
        // set_uv 同步到 text 画笔；之后入队条目冻结该 uv
        batch.texts.push(
            "H",
            Pos::ZERO,
            Default::default(),
            Default::default(),
        );
        let entries = batch.texts.entries.clone();
        let frozen = entries[0].texture_state().uv;
        assert_eq!(frozen.u0, 0.25);
        assert_eq!(frozen.u1, 0.75);
        // getter 与 setter 往返一致
        let got = batch.uv();
        assert_eq!((got.u0, got.v0, got.u1, got.v1), (0.25, 0.25, 0.75, 0.75));
        batch.clear_uv();
        let reset = batch.uv();
        assert_eq!((reset.u0, reset.v0, reset.u1, reset.v1), (0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn inherit_uv_propagates_to_child_text_brush() {
        // 回归：apply_inherit_from 的 uv 分支必须走 set_uv（传播到 texts.texture_state.uv），
        // 否则 child 继承 uv 后文字画笔仍默认 UV，形状/文字 UV 不一致。
        let mut parent = DrawBatch::new();
        parent.set_uv(0.25, 0.25, 0.75, 0.75);

        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::ALL;
        parent.push_child(child);

        // 继承后入队文字，冻结的画笔 UV 应为父值
        let mut inherited = DrawBatch::new();
        inherited.inherit = InheritFromParent::ALL;
        let mut p2 = DrawBatch::new();
        p2.set_uv(0.25, 0.25, 0.75, 0.75);
        p2.push_child(inherited);
        p2.children[0].texts.push(
            "H",
            Pos::ZERO,
            Default::default(),
            Default::default(),
        );
        let expected = UvRect { u0: 0.25, v0: 0.25, u1: 0.75, v1: 0.75 };
        let frozen = p2.children[0].texts.entries[0].texture_state().uv;
        assert_eq!((frozen.u0, frozen.u1), (expected.u0, expected.u1),
            "uv 继承必须同步到子 batch 文字画笔");
        _ = parent;
    }

    #[test]
    fn automatic_instances_merge_contiguous_commands() {
        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        draw_circle(&mut batch, Pos::new(10.0, 0.0), 4.0, Some(BLUE));

        assert_eq!(batch.instances.len(), 2);
        assert_eq!(batch.shape_commands.len(), 1);
        assert!(matches!(
            batch.shape_commands[0],
            BatchShapeCommand::Instances { instance_start: 0, instance_count: 2, .. }
        ));
    }

    #[test]
    fn ordered_commands_preserve_instance_mesh_instance_order() {
        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        batch.sdf_feather = None;
        draw_rectangle(&mut batch, Pos::new(2.0, 2.0), 8.0, 8.0, Some(GREEN));
        batch.sdf_feather = Some(1.0);
        draw_circle(&mut batch, Pos::new(10.0, 0.0), 4.0, Some(BLUE));

        assert_eq!(batch.shape_commands.len(), 3);
        assert!(matches!(batch.shape_commands[0], BatchShapeCommand::Instances { .. }));
        assert!(matches!(batch.shape_commands[1], BatchShapeCommand::GeoInstances { .. }));
        assert!(matches!(batch.shape_commands[2], BatchShapeCommand::Instances { .. }));
    }

    #[test]
    fn merge_decision_requires_same_state_and_contiguity() {
        // 状态一致 + 连续 → 合并
        let merged = merge_decision(true, 0, 6, 6, 6).unwrap();
        assert_eq!(merged, (0, 12));
        // 状态一致但中间有间隙 → 不合并
        assert!(merge_decision(true, 0, 6, 10, 6).is_none());
        // 状态不一致 → 不合并（即使连续）
        assert!(merge_decision(false, 0, 6, 6, 6).is_none());
        // 跨种类（mesh vs instances）→ 不合并
        assert!(merge_decision(false, 0, 6, 6, 6).is_none());
        // 三段连续合并
        let m1 = merge_decision(true, 0, 6, 6, 6).unwrap();
        let m2 = merge_decision(true, m1.0, m1.1, 12, 6).unwrap();
        assert_eq!(m2, (0, 18));
    }

    #[test]
    fn preserve_order_flag_defaults_and_resets() {
        let batch = DrawBatch::new();
        assert!(batch.preserve_order, "默认应保序");
        let mut b2 = DrawBatch::new();
        b2.preserve_order = false;
        b2.clear();
        assert!(b2.preserve_order, "clear() 应重置为默认 true");
    }

    #[test]
    fn merge_geo_templates_flag_defaults_and_resets() {
        let batch = DrawBatch::new();
        assert!(!batch.merge_geo_templates, "默认不合并 geo 模板");
        let mut b2 = DrawBatch::new();
        b2.merge_geo_templates = true;
        b2.clear();
        assert!(!b2.merge_geo_templates, "clear() 应重置为默认 false");
        let mut b3 = DrawBatch::new();
        b3.merge_geo_templates = true;
        let cloned = b3.clone_batch();
        assert!(cloned.merge_geo_templates, "clone_batch 应携带该字段");
    }


    #[test]
    fn texture_generation_splits_instance_commands() {
        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        batch.advance_shape_texture_generation();
        draw_circle(&mut batch, Pos::new(10.0, 0.0), 4.0, Some(BLUE));

        assert_eq!(batch.shape_commands.len(), 2);
        assert!(matches!(batch.shape_commands[0], BatchShapeCommand::Instances { .. }));
        assert!(matches!(batch.shape_commands[1], BatchShapeCommand::Instances { .. }));
    }

    #[test]
    fn automatic_rectangle_bounds_include_feather() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = Some(2.0);
        draw_rectangle(&mut batch, Pos::ZERO, 10.0, 20.0, Some(WHITE));

        assert_eq!(batch.instances[0].bounds, [-2.0, -2.0, 12.0, 22.0]);
        assert_eq!(batch.instances[0].uv_bounds, [0.0, 0.0, 10.0, 20.0]);
    }

    #[test]
    fn geo_same_params_share_single_template() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = None;
        for i in 0..50u32 {
            batch.set_position(i as f32 * 2.0, 0.0);
            draw_circle(&mut batch, Pos::new(8.0, 8.0), 8.0, Some(RED));
        }
        for i in 0..50u32 {
            batch.set_position(i as f32 * 2.0, 40.0);
            draw_rounded_rect(&mut batch, Pos::ZERO, 20.0, 16.0, 4.0, Some(BLUE));
        }
        // 同模板圆合并为 1 个命令；圆角矩形独立模板 → 第 2 个命令
        assert_eq!(batch.shape_commands.len(), 2, "两个模板应各一个命令");
        assert!(matches!(batch.shape_commands[0], BatchShapeCommand::GeoInstances { geo_instance_count: 50, .. }));
        assert!(matches!(batch.shape_commands[1], BatchShapeCommand::GeoInstances { geo_instance_count: 50, .. }));
    }

    #[test]
    fn stale_commands_fall_back_after_public_indices_clear() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = None;
        batch.custom_material = Some(Arc::new(crate::material::Material::new_zero_resource(
            "fn material_main(in: crate_material_never) -> vec4<f32> { return vec4<f32>(1.0); }"
                .to_string(),
            None,
            rustc_hash::FxHashMap::default(),
        )));
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        assert!(batch.shape_commands_valid());

        // fragment-only custom material + sdf_feather=None 走 geo_instance path，
        // 数据在 geo_template_indices / geo_instances（不在 indices）。
        batch.geo_template_indices.clear();
        assert!(!batch.shape_commands_valid());
    }

    #[test]
    fn stale_commands_fall_back_after_geo_template_clear() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = None;
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        assert!(batch.shape_commands_valid());

        batch.geo_template_indices.clear();
        assert!(!batch.shape_commands_valid());
    }

    #[test]
    fn stale_commands_fall_back_after_geo_instance_clear() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = None;
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        assert!(batch.shape_commands_valid());

        batch.geo_instances.clear();
        assert!(!batch.shape_commands_valid());
    }

    #[test]
    fn clear_resets_and_rebuilds_ordered_commands() {
        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        batch.sdf_feather = None;
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(BLUE));
        assert_eq!(batch.shape_commands.len(), 2);

        batch.clear();
        assert!(batch.shape_commands.is_empty());
        draw_circle(&mut batch, Pos::ZERO, 4.0, Some(GREEN));
        assert_eq!(batch.shape_commands.len(), 1);
        assert!(batch.shape_commands_valid());
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
        assert_eq!(flat[0].instances.len(), 1); // parent rect
        assert_eq!(flat[1].instances.len(), 1); // child circle
        assert_eq!(flat[2].instances.len(), 1); // child rect
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
        let idx = child.instances[0].transform_index as usize;
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
                    let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.geo_instances.is_empty();
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
        let rvi = root.instances[0].transform_index;
        assert_eq!(rti, rvi, "root text/shape index");
        let (_, _, _, _, rtx, rty) = mat6(&root.transform_table, rti);
        assert!((rtx - 230.0).abs() < 1e-3, "root tx={rtx}");
        assert!((rty - 270.0).abs() < 1e-3, "root ty={rty}");

        // --- mid（继承后应为 root∘mid_local = (270, 270)）---
        let mid = &root.children[0];
        assert_eq!(mid.texts.entries.len(), 1);
        let mti = mid.texts.entries[0].transform_index();
        let mvi = mid.instances[0].transform_index;
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
        let lvi = leaf.instances[0].transform_index;
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
            b.instances[0].transform_index
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        root.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        parent.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());

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
                    let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.geo_instances.is_empty();
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
        b.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        assert!(events.is_empty());
    }

    #[test]
    fn bounds_keeps_onscreen_subtree() {
        let mut b = DrawBatch::new();
        b.bounds = Some(Some(Rect::new(100.0, 100.0, 50.0, 50.0)));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        parent.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // 无子：不发空 scissor Push/Pop
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
    }

    #[test]
    fn geo_instance_clip_emits_stencil_pop() {
        // 回归：flatten_events 的 has_geom 此前漏了 geo_instances。
        // sdf_feather=None 的几何走 geo_instance_shape → 只有 geo_instances
        //（vertices/instances 均空）；clips_children=true 时 draw 阶段会 Push
        // 但 flatten 不发 StencilPop → clip_depth 泄漏、后续 batch stencil ref 偏移。
        let mut g = DrawBatch::new();
        g.sdf_feather = None;
        g.clips_children = true;
        crate::shapes::draw_triangle(&mut g, 0.0, 0.0, 100.0, 0.0, 0.0, 50.0, Some(WHITE));
        assert!(!g.geo_instances.is_empty());
        assert!(g.vertices.is_empty() && g.instances.is_empty());
        let mut events: Vec<DrawEvent> = Vec::new();
        g.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        assert!(events.iter().any(|e| matches!(e, DrawEvent::StencilPop)));
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
            &Transform::IDENTITY,
            &mut FxHashMap::default(),
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
            &Transform::IDENTITY,
            &mut FxHashMap::default(),
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
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
            &Transform::IDENTITY,
            &mut FxHashMap::default(),
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

    #[test]
    fn view_field_defaults_to_identity() {
        let b = DrawBatch::new();
        assert_eq!(b.view, Transform::IDENTITY);
    }

    #[test]
    fn view_field_resets_on_clear() {
        let mut b = DrawBatch::new();
        b.view = Transform::translation(100.0, 200.0);
        b.clear();
        assert_eq!(b.view, Transform::IDENTITY);
    }

    #[test]
    fn view_field_carries_on_clone() {
        let mut b = DrawBatch::new();
        b.view = Transform::translation(10.0, 20.0);
        let c = b.clone_batch();
        assert_eq!(c.view, Transform::translation(10.0, 20.0));
    }

    #[test]
    fn flatten_records_effective_view_for_batch_and_children() {
        let mut parent = DrawBatch::new();
        parent.view = Transform::translation(100.0, 200.0);
        let mut child = DrawBatch::new();
        child.view = Transform::translation(5.0, 7.0);
        parent.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        let mut view_map = FxHashMap::default();
        parent.flatten_events(
            &mut events,
            0,
            None,
            &FxHashMap::default(),
            &Transform::IDENTITY,
            &mut view_map,
        );
        assert_eq!(events.len(), 2);
        let pkey = &(&parent as *const DrawBatch as *const () as usize);
        let child_ref = match &events[1] {
            DrawEvent::Batch(cb) => *cb,
            _ => unreachable!(),
        };
        let ckey = &(child_ref as *const DrawBatch as *const () as usize);
        // 父子 view 有效值：父 = 自身 view；子 = 父 view × 子 view（左乘）。
        let pe = view_map[pkey];
        assert_eq!(pe, Transform::translation(100.0, 200.0));
        let ce = view_map[ckey];
        assert_eq!(ce, Transform::translation(105.0, 207.0));
    }

    #[test]
    fn left_mul_view_table_applies_view_to_rows() {
        // 单位视图：整表原样
        let table = vec![
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 12.0, 34.0, 1.0, 0.0,
        ];
        let mut out = Vec::new();
        left_mul_view_table(&Transform::IDENTITY, &table, &mut out);
        assert_eq!(out, table);
        // 平移视图：tx/ty 列被左乘（单位线性部分）
        let view = Transform::translation(50.0, 60.0);
        left_mul_view_table(&view, &table, &mut out);
        assert_eq!(out.len(), 12);
        // 平移列 = (50+12, 60+34)（view 线性部分为单位阵）
        assert!((out[8] - 62.0).abs() < 1e-4);
        assert!((out[9] - 94.0).abs() < 1e-4);
    }
}
