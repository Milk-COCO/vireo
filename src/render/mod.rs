//! 渲染核心：批量绘制、渲染目标和渲染器。

use std::cell::RefCell;
use std::sync::Arc;
use rustc_hash::FxHashMap;

use wgpu::util::DeviceExt;

pub use crate::gpu::Vertex;
pub use crate::math::{Pos, Rect, Transform, UvRect};
use crate::math::{
    affine_rect_bounds, left_mul_view_table, mul_affine_cols, seed_identity_transform_table,
    transform_key, IDENTITY_TRANSFORM_ROW,
};
use crate::gpu::{GpuContext, GeoInstance, GeoVertex, ShapeInstance};
use crate::gpu::MaterialTarget;
use crate::material::Material;
use crate::area::{effective_area, Area, AreaGeom, AreaStencilOp};

mod batch;
pub use batch::{DrawBatch, InheritFromParent};
pub(crate) use batch::{
    BatchShapeCommand, EdgeTemplate, EdgeTemplateKind, InstanceTextureSegment, TextureSegment,
    compute_subtree_aabb,
};

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
}

#[derive(Clone)]
struct GeoInstanceSegment {
    geo_instance_start: u32,
    geo_instance_count: u32,
    template_vertex_start: u32,
    template_index_start: u32,
    index_count: u32,
    bind_group: wgpu::BindGroup,
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
                OrderedShapeSegment::Mesh { ndx_start, ndx_count, bind_group, geometry },
                OrderedShapeSegment::Mesh {
                    ndx_start: n2,
                    ndx_count: n2_count,
                    bind_group: b2,
                    geometry: g2,
                },
            ) => {
                let merged = merge_decision(
                    geometry == g2 && bind_group == b2,
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
                })
            }
            (OrderedShapeSegment::Instances(s), OrderedShapeSegment::Instances(s2)) => {
                let merged = merge_decision(
                    s.bind_group == s2.bind_group,
                    s.instance_start,
                    s.instance_count,
                    s2.instance_start,
                    s2.instance_count,
                )?;
                Some(OrderedShapeSegment::Instances(InstanceSegment {
                    instance_start: merged.0,
                    instance_count: merged.1,
                    bind_group: s.bind_group.clone(),
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
                        && s.index_count == s2.index_count,
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
    vertex_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    index_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    instance_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    geo_instance_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    geo_template_vertex_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    geo_template_index_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    physical_width: u32,
    physical_height: u32,
    scale: f32,
    /// 文字 shader `screen_resolution` 覆盖（`layout_follow` 拖动中用）。
    /// `Some((w,h))` = 虚拟新物理尺寸（新逻辑 × dpi）：glyph 不重新光栅化
    /// （scale/dpi 不变 → 图集 cache key 稳定），仅 shader NDC 映射补偿 DXGI 拉伸。
    /// `None` = 用 `physical_width/height`（旧 surface 尺寸）。
    text_viewport_override: std::cell::Cell<Option<(u32, u32)>>,
    sample_count: u32,
    alpha_to_coverage: bool,
    ssaa: bool,
    msaa_tex: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
    ds_tex: RefCell<Option<(wgpu::Texture, wgpu::TextureView)>>,
    polygon_edge_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    transform_buf: RefCell<Option<(wgpu::Buffer, u64)>>,
    engine_storage_bind_group_cache: RefCell<Option<wgpu::BindGroup>>,
    /// 逻辑视口尺寸（逻辑像素，浮点用户坐标系）
    logical_width: f32,
    logical_height: f32,
    /// 帧间复用的 CPU 暂存，避免每帧大块分配
    scratch_vdata: RefCell<Vec<u8>>,
    scratch_idata: RefCell<Vec<u8>>,
    scratch_transforms: RefCell<Vec<f32>>,
    scratch_poly_edges: RefCell<Vec<f32>>,
    scratch_event_infos: RefCell<Vec<EventInfo>>,
    scratch_aabb_map: RefCell<FxHashMap<usize, Option<Rect>>>,
    scratch_view_map: RefCell<FxHashMap<usize, Transform>>,
    scratch_view_table: RefCell<Vec<f32>>,
    scratch_ref_stack: RefCell<Vec<u32>>,
    scratch_batch_transform_bases: RefCell<Vec<u32>>,
    scratch_batch_poly_base: RefCell<Vec<u32>>,
    scratch_batch_geo_vertex_base: RefCell<Vec<u32>>,
    scratch_batch_geo_index_base: RefCell<Vec<u32>>,
    scratch_last_dynamic_offsets: RefCell<Vec<u32>>,
    scratch_scissor_stack: RefCell<Vec<(u32, u32, u32, u32)>>,
    scratch_instances: RefCell<Vec<ShapeInstance>>,
    scratch_geo_instances: RefCell<Vec<GeoInstance>>,
    scratch_geo_vertices: RefCell<Vec<GeoVertex>>,
    scratch_geo_indices: RefCell<Vec<u32>>,
    scratch_geo_merge_per_inst_seg: RefCell<Vec<u32>>,
    scratch_geo_merge_order: RefCell<Vec<u32>>,
    scratch_geo_merge_sorted: RefCell<Option<Vec<u32>>>,
    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数（真实 draw call 数）。
    /// `preserve_order=false` 重排合并后此值下降（bench 场景 3 混合可 1000→2）。
    last_draw_calls: std::cell::Cell<u32>,
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
            vertex_buf: RefCell::new(None),
            index_buf: RefCell::new(None),
            instance_buf: RefCell::new(None),
            geo_instance_buf: RefCell::new(None),
            geo_template_vertex_buf: RefCell::new(None),
            geo_template_index_buf: RefCell::new(None),
            physical_width,
            physical_height,
            scale,
            text_viewport_override: std::cell::Cell::new(None),
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
            scratch_view_map: RefCell::new(FxHashMap::default()),
            scratch_view_table: RefCell::new(Vec::new()),
            scratch_ref_stack: RefCell::new(Vec::new()),
            scratch_batch_transform_bases: RefCell::new(Vec::new()),
            scratch_batch_poly_base: RefCell::new(Vec::new()),
            scratch_batch_geo_vertex_base: RefCell::new(Vec::new()),
            scratch_batch_geo_index_base: RefCell::new(Vec::new()),
            scratch_last_dynamic_offsets: RefCell::new(Vec::new()),
            scratch_scissor_stack: RefCell::new(Vec::new()),
            scratch_instances: RefCell::new(Vec::new()),
            scratch_geo_instances: RefCell::new(Vec::new()),
            scratch_geo_vertices: RefCell::new(Vec::new()),
            scratch_geo_indices: RefCell::new(Vec::new()),
            scratch_geo_merge_per_inst_seg: RefCell::new(Vec::new()),
            scratch_geo_merge_order: RefCell::new(Vec::new()),
            scratch_geo_merge_sorted: RefCell::new(None),
            last_draw_calls: std::cell::Cell::new(0),
            logical_width,
            logical_height,
        }
    }

    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数。
    /// 由 [`Self::draw`] 在每帧统计；未 draw 时为 0。
    pub fn last_draw_calls(&self) -> u32 {
        self.last_draw_calls.get()
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
        self.text_viewport_override.set(size);
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
        self.text_viewport_override.set(None);
        *self.msaa_tex.borrow_mut() = None;
        *self.ds_tex.borrow_mut() = None;
    }

    /// 编码渲染命令到 `CommandBuffer`，**不** submit/present。
    ///
    /// 调用方负责在持有目标 surface/texture 帧循环的线程上：
    /// ```ignore
    /// queue.submit([cmd_buf]);
    /// queue.present(surface_texture);
    /// ```
    ///
    /// 返回的 `CommandBuffer` 持有对 `target.view` 的引用（`TextureView`），
    /// 在 `submit` 之前 `target.view` 必须保持有效（即 `SurfaceTexture` 未被销毁）。
    ///
    /// 当前窗口路径由渲染线程在 `VireoWindow::draw` 内完成 acquire、调用本方法、
    /// submit 和 present；winit owner 线程不参与逐帧 surface 提交。离屏调用方则自行 submit。
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
        let viewport = Rect::new(0.0, 0.0, self.logical_width, self.logical_height);

        // Pass 1: bottom-up 计算子树 AABB（供 culling 用）
        {
            let mut aabb_map = self.scratch_aabb_map.borrow_mut();
            aabb_map.clear();
            for b in batches {
                compute_subtree_aabb(b, &mut aabb_map, &Transform::IDENTITY);
            }
        }

        // Pass 2: flatten with culling
        let mut events: Vec<DrawEvent> = Vec::new();
        let mut uses_stencil = false;
        {
            let aabb_map = self.scratch_aabb_map.borrow();
            let mut view_map = self.scratch_view_map.borrow_mut();
            view_map.clear();
            for b in batches {
                let event_start = events.len();
                b.flatten_events(
                    &mut events,
                    0,
                    Some(viewport),
                    &aabb_map,
                    &Transform::IDENTITY,
                    &mut view_map,
                );
                uses_stencil |= events[event_start..]
                    .iter()
                    .any(|ev| matches!(ev, DrawEvent::StencilPop | DrawEvent::AreaOp { .. }));
            }
        }

        let has_content = clear_color.is_some()
            || events.iter().any(|e| matches!(e, DrawEvent::Batch(b) if !b.vertices.is_empty() || !b.instances.is_empty() || !b.geo_instances.is_empty() || !b.texts.entries.is_empty()));
        if !has_content {
            // 无内容：返回空 cmd_buf（不创建 render pass 即可）
            self.last_draw_calls.set(0);
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
        let lw = self.logical_width;
        let lh = self.logical_height;

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
            let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.geo_instances.is_empty();
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
                            !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.geo_instances.is_empty() || !batch.texts.entries.is_empty();
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

        let mut combined_geo_vertices = self.scratch_geo_vertices.borrow_mut();
        let mut combined_geo_indices = self.scratch_geo_indices.borrow_mut();
        combined_geo_vertices.clear();
        combined_geo_indices.clear();
        let mut batch_geo_vertex_base = self.scratch_batch_geo_vertex_base.borrow_mut();
        let mut batch_geo_index_base = self.scratch_batch_geo_index_base.borrow_mut();
        batch_geo_vertex_base.clear();
        batch_geo_index_base.clear();
        {
            let view_map = self.scratch_view_map.borrow();
            let mut view_table = self.scratch_view_table.borrow_mut();

            for (ei, event) in events.iter().enumerate() {
                if let DrawEvent::Batch(batch) = event {
                    let _e = &mut event_infos[ei];
                    batch_transform_bases.push((global_transforms.len() / 12) as u32);
                    // 左乘有效视图：几何与文字共用同一张表（见 `flatten_events` view_map）。
                let eff = view_map
                    .get(&(*batch as *const DrawBatch as *const () as usize))
                    .copied()
                    .unwrap_or(Transform::IDENTITY);
                    left_mul_view_table(&eff, &batch.transform_table, &mut view_table);
                    global_transforms.extend_from_slice(&view_table);
                    batch_poly_base.push(poly_offset);
                    poly_offset += batch.polygon_edges.len() as u32 / 4;
                    polygon_edges_global.extend_from_slice(&batch.polygon_edges);
                    total_vcount += batch.vertices.len() as u32;
                    total_icount += batch.indices.len() as u32;
                    if batch.custom_material.is_some() {
                        total_vcount += batch.instances.len() as u32 * 4;
                        total_icount += batch.instances.len() as u32 * 6;
                    }
                    batch_geo_vertex_base.push(combined_geo_vertices.len() as u32);
                    batch_geo_index_base.push(combined_geo_indices.len() as u32);
                    combined_geo_vertices.extend_from_slice(&batch.geo_template_vertices);
                    combined_geo_indices.extend_from_slice(&batch.geo_template_indices);
                } else if let DrawEvent::StencilPop = event {
                    // Pop 事件：添加全屏四边形（2 三角，6 索引）
                    pop_screen_verts += 4;
                    pop_screen_idx += 6;
                    batch_transform_bases.push(0);
                    batch_poly_base.push(poly_offset);
                    batch_geo_vertex_base.push(0);
                    batch_geo_index_base.push(0);
                } else if let DrawEvent::ScissorPush(_) | DrawEvent::ScissorPop = event {
                    // Scissor 事件不需要 transform/poly，但保留索引对齐
                    batch_transform_bases.push(0);
                    batch_poly_base.push(poly_offset);
                    batch_geo_vertex_base.push(0);
                    batch_geo_index_base.push(0);
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
                    batch_geo_vertex_base.push(0);
                    batch_geo_index_base.push(0);
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
        let mut combined_geo_instances = self.scratch_geo_instances.borrow_mut();
        combined_vdata.clear();
        combined_idata.clear();
        combined_instances.clear();
        combined_geo_instances.clear();
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
        // merge_geo 排序后每实例的 texture segment 索引（None = 本轮未启用重排）
        let mut geo_merge_sorted_seg = self.scratch_geo_merge_sorted.borrow_mut();
        for (ei, event) in events.iter().enumerate() {
            match event {
                DrawEvent::Batch(batch) => {
                    let info_idx = ei;
                    let instance_start = combined_instances.len() as u32;
                    // fragment-only custom material（无 custom VS）可走 SDF instance path；
                    // 带 custom VS 的 Material 必须落回 mesh（VS 与 instance 字段契约不一致）。
                    let fragment_only = batch
                        .custom_material
                        .as_ref()
                        .map(|m| !m.has_custom_vertex_shader())
                        .unwrap_or(true);
                    let use_instances = !batch.instances.is_empty() && fragment_only;
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
                    let geo_instance_start = combined_geo_instances.len() as u32;
                    let use_geo = !batch.geo_instances.is_empty() && fragment_only;
                    // merge_geo_templates：把同模板实例重排到连续范围以便合并 draw call。
                    // 按（texture segment, 模板）分组重排，多纹理 batch 也可合并——
                    // 每个 texture segment 内部按模板聚拢，段间仍保持各自 bind group。
                    let merge_geo = use_geo && batch.merge_geo_templates;
                    if use_geo {
                        let transform_base = batch_transform_bases[info_idx];
                        let gv_base = batch_geo_vertex_base[info_idx];
                        let gi_base = batch_geo_index_base[info_idx];
                        if merge_geo {
                            // 每实例原始下标 → texture segment 索引（超出段尾 → segments.len()，走 batch.bind_group）
                            let seg_count = batch.geo_instance_texture_segments.len() as u32;
                            let mut per_inst_seg = self.scratch_geo_merge_per_inst_seg.borrow_mut();
                            per_inst_seg.clear();
                            per_inst_seg.resize(batch.geo_instances.len(), seg_count);
                            for (si, seg) in batch.geo_instance_texture_segments.iter().enumerate() {
                                for k in seg.instance_start..seg.instance_start + seg.instance_count {
                                    per_inst_seg[k as usize] = si as u32;
                                }
                            }
                            let mut order = self.scratch_geo_merge_order.borrow_mut();
                            order.clear();
                            order.extend(0..batch.geo_instances.len() as u32);
                            order.sort_by_key(|&i| {
                                let g = &batch.geo_instances[i as usize];
                                (
                                    per_inst_seg[i as usize],
                                    g.template_vertex_start,
                                    g.template_index_start,
                                    g.index_count,
                                )
                            });
                            let sorted_seg = geo_merge_sorted_seg.get_or_insert_with(Vec::new);
                            sorted_seg.clear();
                            combined_geo_instances.extend(order.iter().copied().map(|i| {
                                sorted_seg.push(per_inst_seg[i as usize]);
                                let mut instance = batch.geo_instances[i as usize];
                                instance.template_vertex_start += gv_base;
                                instance.template_index_start += gi_base;
                                instance.transform_index += transform_base;
                                instance
                            }));
                        } else {
                            combined_geo_instances.extend(batch.geo_instances.iter().copied().map(|mut instance| {
                                instance.template_vertex_start += gv_base;
                                instance.template_index_start += gi_base;
                                instance.transform_index += transform_base;
                                instance
                            }));
                        }
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
                    let geo_segments = if !use_geo {
                        Vec::new()
                    } else if merge_geo {
                        // 实例已按（texture segment, 模板）重排：扫描排序后的连续范围，
                        // 每个 (segment, 模板) 组合一段，段用对应 segment 的 bind group。
                        let total = batch.geo_instances.len() as u32;
                        let mk_seg = |start: u32, count: u32, bg: wgpu::BindGroup| -> GeoInstanceSegment {
                            let tpl = combined_geo_instances[start as usize];
                            GeoInstanceSegment {
                                geo_instance_start: start,
                                geo_instance_count: count,
                                template_vertex_start: tpl.template_vertex_start,
                                template_index_start: tpl.template_index_start,
                                index_count: tpl.index_count,
                                bind_group: bg,
                            }
                        };
                        let seg_count = batch.geo_instance_texture_segments.len() as u32;
                        let sorted_seg = geo_merge_sorted_seg.as_deref().unwrap_or(&[]);
                        let resolve_seg_bg = |si: u32| -> wgpu::BindGroup {
                            if si < seg_count {
                                resolve_bg(batch.geo_instance_texture_segments[si as usize].bind_group.clone())
                            } else {
                                resolve_bg(batch.bind_group.clone())
                            }
                        };
                        let mut segments: Vec<GeoInstanceSegment> = Vec::new();
                        let mut i = 0u32;
                        while i < total {
                            let tpl_start = geo_instance_start + i;
                            let seg_i = sorted_seg.get(i as usize).copied().unwrap_or(seg_count);
                            let key = (
                                combined_geo_instances[tpl_start as usize].template_vertex_start,
                                combined_geo_instances[tpl_start as usize].template_index_start,
                                combined_geo_instances[tpl_start as usize].index_count,
                            );
                            let mut j = i + 1;
                            while j < total {
                                let seg_j = sorted_seg.get(j as usize).copied().unwrap_or(seg_count);
                                let g = &combined_geo_instances[(geo_instance_start + j) as usize];
                                if seg_j != seg_i
                                    || (g.template_vertex_start, g.template_index_start, g.index_count) != key
                                {
                                    break;
                                }
                                j += 1;
                            }
                            segments.push(mk_seg(tpl_start, j - i, resolve_seg_bg(seg_i)));
                            i = j;
                        }
                        segments
                    } else {
                        let mk_seg = |start: u32, count: u32, bg: wgpu::BindGroup| -> GeoInstanceSegment {
                            let tpl = combined_geo_instances[start as usize];
                            GeoInstanceSegment {
                                geo_instance_start: start,
                                geo_instance_count: count,
                                template_vertex_start: tpl.template_vertex_start,
                                template_index_start: tpl.template_index_start,
                                index_count: tpl.index_count,
                                bind_group: bg,
                            }
                        };
                        if batch.geo_instance_texture_segments.is_empty() {
                            vec![mk_seg(geo_instance_start, batch.geo_instances.len() as u32, resolve_bg(batch.bind_group.clone()))]
                        } else {
                            let mut segments: Vec<GeoInstanceSegment> = batch.geo_instance_texture_segments.iter().map(|segment| {
                                mk_seg(geo_instance_start + segment.instance_start, segment.instance_count, resolve_bg(segment.bind_group.clone()))
                            }).collect();
                            let last_end = segments.last().map_or(geo_instance_start, |s| s.geo_instance_start + s.geo_instance_count);
                            let total_end = geo_instance_start + batch.geo_instances.len() as u32;
                            if last_end < total_end {
                                segments.push(mk_seg(last_end, total_end - last_end, resolve_bg(batch.bind_group.clone())));
                            }
                            segments
                        }
                    };
                    let shape = if !batch.vertices.is_empty()
                        || !batch.instances.is_empty()
                        || !batch.geo_instances.is_empty()
                    {
                        let transform_base = batch_transform_bases[info_idx];
                        let poly_base = batch_poly_base[info_idx] as f32;
                        let needs_patch = !batch.polygon_edges.is_empty() || transform_base > 0;
                        if needs_patch {
                            let has_poly = !batch.polygon_edges.is_empty();
                            for mut v in batch.vertices.iter().copied() {
                                if transform_base > 0 {
                                    v.transform_index += transform_base;
                                }
                                if has_poly && (v.sdf_type == 6 || v.sdf_type == 7) {
                                    v.sdf_params[0] += poly_base;
                                }
                                combined_vdata.extend_from_slice(bytemuck::bytes_of(&v));
                            }
                        } else {
                            combined_vdata.extend_from_slice(bytemuck::cast_slice(&batch.vertices));
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
                                    if instance.sdf_type == 6 || instance.sdf_type == 7 {
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
                        // merge_geo 时 ordered 路径需要 geo_segments 的克隆（原值移入 ShapeInfo）
                        let geo_segments_for_ordered = merge_geo.then(|| geo_segments.clone());
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: !batch.has_sdf && batch.sdf_feather.is_none(),
                            instances: instance_segments,
                            geo_instances: geo_segments,
                            ordered: if batch.shape_commands.is_empty() || !batch.shape_commands_valid() {
                                Vec::new()
                            } else {
                                let mut ordered = Vec::with_capacity(batch.shape_commands.len() + 1);
                                // merge_geo：shape_commands 的 GeoInstances 用原始局部偏移，
                                // 重排后失效 → 改用 `geo_segments` 的分组段（已在合并 buffer 上按模板分组）。
                                let geo_merged = merge_geo;
                                let mut geo_pushed = false;
                                for command in &batch.shape_commands {
                                    match command {
                                        BatchShapeCommand::Mesh { ndx_start, ndx_count, bind_group, geometry, .. } => {
                                            ordered.push(OrderedShapeSegment::Mesh {
                                                ndx_start: idx_offset + *ndx_start,
                                                ndx_count: *ndx_count,
                                                bind_group: resolve_bg(bind_group.clone()),
                                                geometry: *geometry,
                                            });
                                        }
                                        BatchShapeCommand::Instances { instance_start: local_start, instance_count, bind_group, .. } => {
                                            if use_instances {
                                                ordered.push(OrderedShapeSegment::Instances(InstanceSegment {
                                                    instance_start: instance_start + *local_start,
                                                    instance_count: *instance_count,
                                                    bind_group: resolve_bg(bind_group.clone()),
                                                }));
                                            } else {
                                                ordered.push(OrderedShapeSegment::Mesh {
                                                    ndx_start: idx_offset + batch.indices.len() as u32 + *local_start * 6,
                                                    ndx_count: *instance_count * 6,
                                                    bind_group: resolve_bg(bind_group.clone()),
                                                    geometry: false,
                                                });
                                            }
                                        }
                                        BatchShapeCommand::GeoInstances { geo_instance_start: local_start, geo_instance_count, bind_group, .. } => {
                                            if use_geo {
                                                if geo_merged {
                                                    if !geo_pushed {
                                                        ordered.extend(geo_segments_for_ordered.as_deref().unwrap_or(&[]).iter().map(|s| {
                                                            OrderedShapeSegment::GeoInstances(s.clone())
                                                        }));
                                                        geo_pushed = true;
                                                    }
                                                } else {
                                                    let tpl = combined_geo_instances[(geo_instance_start + *local_start) as usize];
                                                    ordered.push(OrderedShapeSegment::GeoInstances(GeoInstanceSegment {
                                                        geo_instance_start: geo_instance_start + *local_start,
                                                        geo_instance_count: *geo_instance_count,
                                                        template_vertex_start: tpl.template_vertex_start,
                                                        template_index_start: tpl.template_index_start,
                                                        index_count: tpl.index_count,
                                                        bind_group: resolve_bg(bind_group.clone()),
                                                    }));
                                                }
                                            }
                                        }
                                    }
                                }
                                if batch.shape_mesh_end < batch.indices.len() as u32 {
                                    ordered.push(OrderedShapeSegment::Mesh {
                                        ndx_start: idx_offset + batch.shape_mesh_end,
                                        ndx_count: batch.indices.len() as u32 - batch.shape_mesh_end,
                                        bind_group: resolve_bg(batch.bind_group.clone()),
                                        geometry: !batch.has_sdf && batch.sdf_feather.is_none(),
                                    });
                                }
                                if !batch.preserve_order {
                                    // 允许重排：按（种类, geometry, bind group）稳定排序，
                                    // 再把 pipeline 状态相同且范围连续的相邻段合并，
                                    // 减少 pipeline 切换与 draw call。
                                    ordered.sort_by_key(OrderedShapeSegment::sort_key);
                                    let mut merged: Vec<OrderedShapeSegment> =
                                        Vec::with_capacity(ordered.len());
                                    for segment in ordered {
                                        if let Some(last) = merged.last() {
                                            if let Some(combined) = last.try_merge(&segment) {
                                                *merged.last_mut().unwrap() = combined;
                                                continue;
                                            }
                                        }
                                        merged.push(segment);
                                    }
                                    ordered = merged;
                                }
                                ordered
                            },
                        };
                        v_offset += batch.vertices.len() as u32
                            + if use_instances { 0 } else { batch.instances.len() as u32 * 4 };
                        idx_offset += mesh_index_count;
                        Some(info)
                    } else if !instance_segments.is_empty() || !geo_segments.is_empty() {
                        Some(ShapeInfo {
                            base_vertex: 0,
                            segments: Vec::new(),
                            geometry: false,
                            instances: instance_segments,
                            geo_instances: geo_segments,
                            ordered: Vec::new(),
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
                        geo_instances: Vec::new(),
                        ordered: Vec::new(),
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
                        if needs_patch {
                            let has_poly = !geom.polygon_edges.is_empty();
                            for mut v in geom.vertices.iter().copied() {
                                if transform_base > 0 {
                                    v.transform_index += transform_base;
                                }
                                if has_poly && (v.sdf_type == 6 || v.sdf_type == 7) {
                                    v.sdf_params[0] += poly_base;
                                }
                                combined_vdata.extend_from_slice(bytemuck::bytes_of(&v));
                            }
                        } else {
                            combined_vdata.extend_from_slice(bytemuck::cast_slice(&geom.vertices));
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
                            geo_instances: Vec::new(),
                            ordered: Vec::new(),
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
                            geo_instances: Vec::new(),
                            ordered: Vec::new(),
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
            self.gpu.queue.write_buffer(&vbuf.as_ref().unwrap().0, 0, &combined_vdata);
        }
        if !combined_idata.is_empty() {
            let ibuf = self.index_buf.borrow();
            self.gpu.queue.write_buffer(&ibuf.as_ref().unwrap().0, 0, &combined_idata);
        }
        if !combined_instances.is_empty() {
            let size = (combined_instances.len() * size_of::<ShapeInstance>()) as u64;
            self.ensure_instance_buffer(size);
            let instance_buf = self.instance_buf.borrow();
            self.gpu.queue.write_buffer(
                &instance_buf.as_ref().unwrap().0,
                0,
                bytemuck::cast_slice(&combined_instances),
            );
        }

        // ---- 上传几何模板顶点/索引 ----
        if !combined_geo_vertices.is_empty() {
            let size = (combined_geo_vertices.len() * size_of::<GeoVertex>()) as u64;
            self.ensure_geo_template_vertex_buffer(size);
            let buf = self.geo_template_vertex_buf.borrow();
            self.gpu.queue.write_buffer(&buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&combined_geo_vertices));
        }
        if !combined_geo_indices.is_empty() {
            let size = (combined_geo_indices.len() * 4) as u64;
            self.ensure_geo_template_index_buffer(size);
            let buf = self.geo_template_index_buf.borrow();
            self.gpu.queue.write_buffer(&buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&combined_geo_indices));
        }

        // ---- 上传几何实例 ----
        if !combined_geo_instances.is_empty() {
            let size = (combined_geo_instances.len() * size_of::<GeoInstance>()) as u64;
            self.ensure_geo_instance_buffer(size);
            let buf = self.geo_instance_buf.borrow();
            self.gpu.queue.write_buffer(&buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&combined_geo_instances));
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
                    // layout_follow 时用虚拟新物理尺寸（screen_resolution uniform 补偿 DXGI 拉伸）
                    let (tw, th) = self.text_viewport_override.get()
                        .unwrap_or((self.physical_width, self.physical_height));
                    // 文字与几何共用同一张表：左乘有效视图，保证 view 同时作用于文字。
                    let mut view_table = self.scratch_view_table.borrow_mut();
                    let eff = self
                        .scratch_view_map
                        .borrow()
                        .get(&(*batch as *const DrawBatch as *const () as usize))
                        .copied()
                        .unwrap_or(Transform::IDENTITY);
                    left_mul_view_table(&eff, &batch.transform_table, &mut view_table);
                    let prepared = batch.texts.prepare_texts(
                        &self.gpu,
                        tw,
                        th,
                        self.scale,
                        &view_table,
                        &mut global_transforms,
                        batch.text_clip,
                        batch.color,
                    );
                    drop(view_table);
                    let text_ctx = self.gpu.text_ctx.lock().unwrap();
                    event_infos[ei].text = prepared
                        .into_iter()
                        .map(|segment| {
                            let bind_group = if let Some(bg) = segment.bind_group.clone() {
                                Some(bg)
                            } else {
                                segment.texture_view.as_ref().map(|view| {
                                    text_ctx
                                        .text_atlas
                                        .bind_group_for_base_texture(&self.gpu.device, view)
                                })
                            };
                            TextRenderSegment {
                                vertex_start: segment.vertex_start,
                                vertex_count: segment.vertex_count,
                                bind_group,
                            }
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
                                crate::gpu::ShapeVertexLayout::Mesh,
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
        let mut shape_draw_calls: u32 = 0;
        if has_any_content || clear_color.is_some() {
            let msaa_view = self.msaa_view(self.gpu.surface_format());
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
            let geo_template_vbuf = self.geo_template_vertex_buf.borrow();
            let geo_template_ibuf = self.geo_template_index_buf.borrow();
            let geo_instance_buf = self.geo_instance_buf.borrow();
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
                    let sx = self.physical_width as f32 / self.logical_width.max(1.0);
                    let sy = self.physical_height as f32 / self.logical_height.max(1.0);
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

                // 整批材质 bind group：有 group 3（非 ZeroResource）的材质若绑定失败
                // （纹理槽未 set_texture 等），整批跳过——custom pipeline 引用 group 3，
                // 不绑会触发 wgpu validation error。ZeroResource 无 group 3，None 合法。
                let custom_bg: Option<wgpu::BindGroup> = match info.custom_material.as_ref() {
                    Some(m) if m.bgl().is_some() => m.ensure_bind_group(
                        &self.gpu.device,
                        &self.gpu.queue,
                        &self.gpu.bind_group_pool,
                    ),
                    _ => None,
                };
                if info.custom_material.is_some()
                    && info.custom_material.as_ref().map_or(false, |m| m.bgl().is_some())
                    && custom_bg.is_none()
                {
                    continue;
                }

                if let Some(ref shape) = info.shape {
                    // Area 事件：op 3/4 来自 area_op；普通 batch/StencilPop：op 0..3 来自 stencil_op。
                    let pipe_op = info.area_op.unwrap_or(info.stencil_op);
                    let custom_ptr: *const Material = info.custom_material
                        .as_ref()
                        .map_or(std::ptr::null(), |m| Arc::as_ptr(m));
                    let use_custom = info.custom_material.is_some();
                    let has_custom_vs = info
                        .custom_material
                        .as_ref()
                        .map(|m| m.has_custom_vertex_shader())
                        .unwrap_or(false);
                    // instance 段仅在 fragment-only material 时可走对应 layout pipeline；
                    // custom VS 必须 mesh。
                    let use_custom_instance = use_custom && !has_custom_vs;
                    if !shape.ordered.is_empty() {
                        for segment in &shape.ordered {
                            match segment {
                                OrderedShapeSegment::Mesh {
                                    ndx_start,
                                    ndx_count,
                                    bind_group,
                                    geometry,
                                } => {
                                    let need_rebind = !shapes_bound
                                        || custom_ptr != last_custom_ptr
                                        || (!use_custom && last_geometry != Some(*geometry))
                                        || (uses_stencil && pipe_op != last_stencil_op)
                                        || info.dynamic_offsets != *last_dynamic_offsets;
                                    if need_rebind {
                                        let tmp_pipe: wgpu::RenderPipeline;
                                        let custom_pipe: Arc<wgpu::RenderPipeline>;
                                        let pipe: &wgpu::RenderPipeline = if use_custom {
                                            let mat = info.custom_material.as_ref().unwrap();
                                            custom_pipe = self.gpu.ensure_material_pipeline(
                                                mat,
                                                MaterialTarget::Shape,
                                                self.sample_count,
                                                self.alpha_to_coverage,
                                                self.ssaa,
                                                uses_stencil,
                                                if uses_stencil { pipe_op.min(4) } else { 0 },
                                                crate::gpu::ShapeVertexLayout::Mesh,
                                            );
                                            &custom_pipe
                                        } else if uses_stencil {
                                            tmp_pipe = self.gpu.ensure_stencil_pipeline(
                                                self.sample_count,
                                                self.alpha_to_coverage,
                                                self.ssaa,
                                                *geometry,
                                                pipe_op.min(4),
                                            );
                                            &tmp_pipe
                                        } else {
                                            tmp_pipe = self.gpu.ensure_pipeline(
                                                self.sample_count,
                                                self.alpha_to_coverage,
                                                self.ssaa,
                                                *geometry,
                                            );
                                            &tmp_pipe
                                        };
                                        pass.set_pipeline(pipe);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        if use_custom {
                                            if let Some(bg) = custom_bg.as_ref() {
                                                pass.set_bind_group(3, bg, &info.dynamic_offsets);
                                            }
                                        }
                                        pass.set_vertex_buffer(0, vbuf.as_ref().unwrap().0.slice(..));
                                        pass.set_index_buffer(
ibuf.as_ref().unwrap().0.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        shapes_bound = true;
                                        last_custom_ptr = custom_ptr;
                                        last_geometry = Some(*geometry);
                                        last_stencil_op = pipe_op;
                                        last_dynamic_offsets.clone_from(&info.dynamic_offsets);
                                    }
                                    if uses_stencil {
                                        pass.set_stencil_reference(info.stencil_ref);
                                    }
                                    pass.set_bind_group(1, bind_group, &[]);
                                    pass.draw_indexed(
                                        *ndx_start..*ndx_start + *ndx_count,
                                        shape.base_vertex,
                                        0..1,
                                    );
                                    shape_draw_calls += 1;
                                }
                                OrderedShapeSegment::Instances(segment) => {
                                    if use_custom_instance {
                                        let mat = info.custom_material.as_ref().unwrap();
                                        let custom_pipe = self.gpu.ensure_material_pipeline(
                                            mat,
                                            MaterialTarget::Shape,
                                            self.sample_count,
                                            self.alpha_to_coverage,
                                            self.ssaa,
                                            uses_stencil,
                                            if uses_stencil { pipe_op.min(4) } else { 0 },
                                            crate::gpu::ShapeVertexLayout::SdfInstance,
                                        );
                                        pass.set_pipeline(&custom_pipe);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(1, &segment.bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        if let Some(bg) = custom_bg.as_ref() {
                                            pass.set_bind_group(3, bg, &info.dynamic_offsets);
                                        }
                                        pass.set_vertex_buffer(
                                            0,
                                            self.gpu.instance_quad_vertex_buf.slice(..),
                                        );
                                        pass.set_vertex_buffer(
                                            1,
                                            instance_buf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_index_buffer(
                                            self.gpu.instance_quad_index_buf.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        if uses_stencil {
                                            pass.set_stencil_reference(info.stencil_ref);
                                        }
                                        pass.draw_indexed(
                                            0..6,
                                            0,
                                            segment.instance_start
                                                ..segment.instance_start + segment.instance_count,
                                        );
                                        shape_draw_calls += 1;
                                    } else {
                                        let instance_pipeline = self.gpu.ensure_instance_pipeline(
                                            self.sample_count,
                                            self.alpha_to_coverage,
                                            self.ssaa,
                                            uses_stencil,
                                            pipe_op,
                                        );
                                        pass.set_pipeline(&instance_pipeline);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(1, &segment.bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        pass.set_vertex_buffer(
                                            0,
                                            self.gpu.instance_quad_vertex_buf.slice(..),
                                        );
                                        pass.set_vertex_buffer(
                                            1,
                                            instance_buf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_index_buffer(
                                            self.gpu.instance_quad_index_buf.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        if uses_stencil {
                                            pass.set_stencil_reference(info.stencil_ref);
                                        }
                                        pass.draw_indexed(
                                            0..6,
                                            0,
                                            segment.instance_start
                                                ..segment.instance_start + segment.instance_count,
                                        );
                                        shape_draw_calls += 1;
                                    }
                                    shapes_bound = false;
                                    last_geometry = None;
                                }
                                OrderedShapeSegment::GeoInstances(segment) => {
                                    if use_custom_instance {
                                        let mat = info.custom_material.as_ref().unwrap();
                                        let custom_pipe = self.gpu.ensure_material_pipeline(
                                            mat,
                                            MaterialTarget::Shape,
                                            self.sample_count,
                                            self.alpha_to_coverage,
                                            self.ssaa,
                                            uses_stencil,
                                            if uses_stencil { pipe_op.min(4) } else { 0 },
                                            crate::gpu::ShapeVertexLayout::GeoInstance,
                                        );
                                        pass.set_pipeline(&custom_pipe);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(1, &segment.bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        if let Some(bg) = custom_bg.as_ref() {
                                            pass.set_bind_group(3, bg, &info.dynamic_offsets);
                                        }
                                        pass.set_vertex_buffer(
                                            0,
                                            geo_template_vbuf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_vertex_buffer(
                                            1,
                                            geo_instance_buf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_index_buffer(
                                            geo_template_ibuf.as_ref().unwrap().0.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        if uses_stencil {
                                            pass.set_stencil_reference(info.stencil_ref);
                                        }
                                        pass.draw_indexed(
                                            segment.template_index_start
                                                ..segment.template_index_start + segment.index_count,
                                            segment.template_vertex_start as i32,
                                            segment.geo_instance_start
                                                ..segment.geo_instance_start + segment.geo_instance_count,
                                        );
                                        shape_draw_calls += 1;
                                    } else {
                                        let geo_pipeline = self.gpu.ensure_geo_instance_pipeline(
                                            self.sample_count,
                                            self.alpha_to_coverage,
                                            self.ssaa,
                                            uses_stencil,
                                            pipe_op,
                                        );
                                        pass.set_pipeline(&geo_pipeline);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(1, &segment.bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        pass.set_vertex_buffer(
                                            0,
                                            geo_template_vbuf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_vertex_buffer(
                                            1,
                                            geo_instance_buf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_index_buffer(
                                            geo_template_ibuf.as_ref().unwrap().0.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        if uses_stencil {
                                            pass.set_stencil_reference(info.stencil_ref);
                                        }
                                        pass.draw_indexed(
                                            segment.template_index_start
                                                ..segment.template_index_start + segment.index_count,
                                            segment.template_vertex_start as i32,
                                            segment.geo_instance_start
                                                ..segment.geo_instance_start + segment.geo_instance_count,
                                        );
                                        shape_draw_calls += 1;
                                    }
                                    shapes_bound = false;
                                    last_geometry = None;
                                }
                            }
                        }
                    } else {
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
                                    crate::gpu::ShapeVertexLayout::Mesh,
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
                                    crate::gpu::ShapeVertexLayout::Mesh,
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
                            if let Some(bg) = custom_bg.as_ref() {
                                pass.set_bind_group(3, bg, &info.dynamic_offsets);
                            }
                        }
                        if let Some(vb) = vbuf.as_ref() {
                            pass.set_vertex_buffer(0, vb.0.slice(..));
                        }
                        if let Some(ib) = ibuf.as_ref() {
                            pass.set_index_buffer(ib.0.slice(..), wgpu::IndexFormat::Uint32);
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
                        shape_draw_calls += 1;
                    }
                    if !shape.instances.is_empty() {
                        if use_custom_instance {
                            let mat = info.custom_material.as_ref().unwrap();
                            let custom_pipe = self.gpu.ensure_material_pipeline(
                                mat,
                                MaterialTarget::Shape,
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                uses_stencil,
                                if uses_stencil { pipe_op.min(4) } else { 0 },
                                crate::gpu::ShapeVertexLayout::SdfInstance,
                            );
                            pass.set_pipeline(&custom_pipe);
                            pass.set_bind_group(0, &self.camera_bind_group, &[]);
                            pass.set_bind_group(2, engine_bg, &[]);
                            if let Some(bg) = custom_bg.as_ref() {
                                pass.set_bind_group(3, bg, &info.dynamic_offsets);
                            }
                            pass.set_vertex_buffer(0, self.gpu.instance_quad_vertex_buf.slice(..));
                            pass.set_vertex_buffer(1, instance_buf.as_ref().unwrap().0.slice(..));
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
                                shape_draw_calls += 1;
                            }
                        } else {
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
                            pass.set_vertex_buffer(1, instance_buf.as_ref().unwrap().0.slice(..));
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
                                shape_draw_calls += 1;
                            }
                        }
                        shapes_bound = false;
                        last_geometry = None;
                    }
                    if !shape.geo_instances.is_empty() {
                        if use_custom_instance {
                            let mat = info.custom_material.as_ref().unwrap();
                            let custom_pipe = self.gpu.ensure_material_pipeline(
                                mat,
                                MaterialTarget::Shape,
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                uses_stencil,
                                if uses_stencil { pipe_op.min(4) } else { 0 },
                                crate::gpu::ShapeVertexLayout::GeoInstance,
                            );
                            pass.set_pipeline(&custom_pipe);
                            pass.set_bind_group(0, &self.camera_bind_group, &[]);
                            pass.set_bind_group(2, engine_bg, &[]);
                            if let Some(bg) = custom_bg.as_ref() {
                                pass.set_bind_group(3, bg, &info.dynamic_offsets);
                            }
                            pass.set_vertex_buffer(0, geo_template_vbuf.as_ref().unwrap().0.slice(..));
                            pass.set_vertex_buffer(1, geo_instance_buf.as_ref().unwrap().0.slice(..));
                            pass.set_index_buffer(
                                geo_template_ibuf.as_ref().unwrap().0.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            if uses_stencil {
                                pass.set_stencil_reference(info.stencil_ref);
                            }
                            for segment in &shape.geo_instances {
                                pass.set_bind_group(1, &segment.bind_group, &[]);
                                pass.draw_indexed(
                                    segment.template_index_start
                                        ..segment.template_index_start + segment.index_count,
                                    segment.template_vertex_start as i32,
                                    segment.geo_instance_start
                                        ..segment.geo_instance_start + segment.geo_instance_count,
                                );
                                shape_draw_calls += 1;
                            }
                        } else {
                            let geo_pipeline = self.gpu.ensure_geo_instance_pipeline(
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                uses_stencil,
                                pipe_op,
                            );
                            pass.set_pipeline(&geo_pipeline);
                            pass.set_bind_group(0, &self.camera_bind_group, &[]);
                            pass.set_bind_group(2, engine_bg, &[]);
                            pass.set_vertex_buffer(0, geo_template_vbuf.as_ref().unwrap().0.slice(..));
                            pass.set_vertex_buffer(1, geo_instance_buf.as_ref().unwrap().0.slice(..));
                            pass.set_index_buffer(
                                geo_template_ibuf.as_ref().unwrap().0.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            if uses_stencil {
                                pass.set_stencil_reference(info.stencil_ref);
                            }
                            for segment in &shape.geo_instances {
                                pass.set_bind_group(1, &segment.bind_group, &[]);
                                pass.draw_indexed(
                                    segment.template_index_start
                                        ..segment.template_index_start + segment.index_count,
                                    segment.template_vertex_start as i32,
                                    segment.geo_instance_start
                                        ..segment.geo_instance_start + segment.geo_instance_count,
                                );
                                shape_draw_calls += 1;
                            }
                        }
                        shapes_bound = false;
                        last_geometry = None;
                    }
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
                    // 复用循环顶部已计算的整批材质 bind group（ZeroResource 无 group 3 → None）。
                    let material_bg = custom_bg.as_ref();
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
                            material_bg,
                            &info.dynamic_offsets,
                        );
                    }
                    shapes_bound = false;
                    last_geometry = None;
                }
            }
        }

        self.last_draw_calls.set(shape_draw_calls);
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
        let mut slot = self.vertex_buf.borrow_mut();
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
        let mut slot = self.instance_buf.borrow_mut();
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
        let mut slot = self.geo_instance_buf.borrow_mut();
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
        let mut slot = self.geo_template_vertex_buf.borrow_mut();
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
        let mut slot = self.geo_template_index_buf.borrow_mut();
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
        let mut slot = self.index_buf.borrow_mut();
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


#[cfg(test)]
mod tests {
    use super::*;
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
