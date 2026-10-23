//! 渲染核心：批量绘制、渲染目标和渲染器。

use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::Arc;

use wgpu::util::DeviceExt;

use crate::area::AreaStencilOp;
use crate::gpu::{ClockIndex, GeoInstance, GeoVertex, GpuContext, ParticleInstance};
pub use crate::gpu::{InstanceData, Vertex};
use crate::material::Material;
pub use crate::math::{Pos, Rect, Transform, UvRect};

mod batch;
mod cull;
mod draw;
pub(crate) use batch::BatchShapeCommand;
pub use batch::{DrawBatch, InheritFromParent};
pub(crate) use cull::{AabbMap, ViewMap, prepare_culling};
pub use draw::PreparedFrame;

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
/// geo instance 路径 GPU 输出 = `sum(geo_instances[i].template_vertex_count)`；
/// 粒子路径 GPU 输出 = `particles * 4`（每个粒子 1 个 unit quad）。
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
    pub particles: usize,
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
    /// 设置颜色覆盖（形状与文字共享位）。
    pub fn color(mut self, c: crate::color::Color) -> Self {
        self.color = Some(c);
        self
    }
    /// 设置变换覆盖（形状与文字共享位）。
    pub fn transform(mut self, t: Transform) -> Self {
        self.transform = Some(t);
        self
    }
    /// 设置 UV 矩形覆盖（形状与文字共享位）。
    pub fn uv(mut self, uv: UvRect) -> Self {
        self.uv = Some(uv);
        self
    }
    /// 设置 bind group 覆盖（形状与文字共享位）。
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
/// `DrawBatch::clip_path` 的诊断结果：本 batch 实际走的裁剪路径。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClipPath {
    /// 无裁剪。
    None,
    /// Area stencil：`area_include` / `area_exclude` 生效。自身与子树都受 Area 约束；
    /// 若同时 `clips_children`，子树还叠加几何 mask。
    AreaStencil,
    /// 矩形 scissor。
    Scissor {
        /// 逻辑世界坐标窗口。
        rect: Rect,
        /// `true` = `clips_children` 从单矩形几何自动探测；`false` = 显式 `scissor`。
        automatic: bool,
        /// 自身内容是否也被裁（`clip_self` 且自身有可绘制内容）。
        clips_self: bool,
    },
    /// `clips_children` + 非矩形几何：stencil mask 只作用于子 batch。
    Stencil,
}

pub(crate) enum DrawEvent<'a> {
    Batch(&'a DrawBatch),
    StencilPop,
    AreaOp { op: AreaStencilOp, is_setup: bool },
    ScissorPush(Rect),
    ScissorPop,
}

pub(crate) struct ShapeSegment {
    ndx_start: u32,
    ndx_count: u32,
    bind_group: wgpu::BindGroup,
    blend: wgpu::BlendState,
    blend_constant: wgpu::Color,
}

pub(crate) struct InstanceSegment {
    instance_start: u32,
    instance_count: u32,
    bind_group: wgpu::BindGroup,
    material: Option<Arc<Material>>,
    blend: wgpu::BlendState,
    blend_constant: wgpu::Color,
}

#[derive(Clone, Copy)]
pub(crate) struct GeoTemplate {
    vertex_start: u32,
    index_start: u32,
    index_count: u32,
    vertex_count: u32,
}

/// `sdf_type` 是否为「多边形 / 折线」（`6` / `7`）。
///
/// 这两类的 `sdf_params[0]` 是**边索引**，记录期只存 batch 局部值；引擎在把 batch
/// 合并进全局 buffer 时必须补上该 batch 的全局基址。判定集中在这里，避免
/// `sdf_type == 6 || sdf_type == 7` 散落在多处（曾经 6 处重复，改一处极易漏）。
#[inline]
pub(crate) fn sdf_uses_polygon_edges(sdf_type: u32) -> bool {
    sdf_type == 6 || sdf_type == 7
}

/// 给实例补 batch 的边索引基址（只对多边形/折线生效）。
#[inline]
pub(crate) fn patch_instance_poly_base(instance: &mut InstanceData, base: f32) {
    if sdf_uses_polygon_edges(instance.sdf_type) {
        instance.sdf_params[0] += base;
    }
}

/// 给 mesh 顶点补 batch 的边索引基址（只对多边形/折线生效）。
#[inline]
pub(crate) fn patch_vertex_poly_base(vertex: &mut Vertex, base: f32) {
    if sdf_uses_polygon_edges(vertex.sdf_type) {
        vertex.sdf_params[0] += base;
    }
}

/// 粒子段：`combined_particles` 的一段，共享 unit quad（与 `InstanceSegment` 同机理，
/// 缓冲不同）。`material` 逐段捕获（画笔状态机，不同材质不合并）。
pub(crate) struct ParticleSegment {
    particle_start: u32,
    particle_count: u32,
    bind_group: wgpu::BindGroup,
    material: Option<Arc<Material>>,
    blend: wgpu::BlendState,
    blend_constant: wgpu::Color,
}

#[derive(Clone)]
pub(crate) struct GeoInstanceSegment {
    geo_instance_start: u32,
    geo_instance_count: u32,
    template_vertex_start: u32,
    template_index_start: u32,
    index_count: u32,
    bind_group: wgpu::BindGroup,
    material: Option<Arc<Material>>,
    blend: wgpu::BlendState,
    blend_constant: wgpu::Color,
}

pub(crate) enum OrderedShapeSegment {
    Mesh {
        ndx_start: u32,
        ndx_count: u32,
        bind_group: wgpu::BindGroup,
        geometry: bool,
        material: Option<Arc<Material>>,
        blend: wgpu::BlendState,
        blend_constant: wgpu::Color,
    },
    Instances(InstanceSegment),
    GeoInstances(GeoInstanceSegment),
    Particles(ParticleSegment),
}

impl OrderedShapeSegment {
    /// 排序键：`(0 = mesh, 1 = instances, 2 = geo instances, 3 = particles,
    /// geometry, bind_group, state)`。
    ///
    /// 同类 + 同 bind group + 同状态（material / blend / blend constant / geo 模板）
    /// 的段会被排到一起，减少管线与绑定切换。**真正合并仍要求底层范围连续**
    /// （见 [`Self::try_merge`]）——本键只改善排序，不改变可合并性。
    #[inline]
    fn sort_key(&self) -> (u8, u8, u64, u64) {
        match self {
            OrderedShapeSegment::Mesh {
                bind_group,
                geometry,
                material,
                blend,
                blend_constant,
                ..
            } => (
                0,
                *geometry as u8,
                bind_group_id(bind_group),
                segment_state_hash(
                    material.as_ref().map(Arc::as_ptr),
                    blend,
                    *blend_constant,
                    0,
                ),
            ),
            OrderedShapeSegment::Instances(s) => (
                1,
                0,
                bind_group_id(&s.bind_group),
                segment_state_hash(
                    s.material.as_ref().map(Arc::as_ptr),
                    &s.blend,
                    s.blend_constant,
                    0,
                ),
            ),
            OrderedShapeSegment::GeoInstances(s) => (
                2,
                0,
                bind_group_id(&s.bind_group),
                segment_state_hash(
                    s.material.as_ref().map(Arc::as_ptr),
                    &s.blend,
                    s.blend_constant,
                    geo_template_key(
                        s.template_vertex_start,
                        s.template_index_start,
                        s.index_count,
                    ),
                ),
            ),
            OrderedShapeSegment::Particles(s) => (
                3,
                0,
                bind_group_id(&s.bind_group),
                segment_state_hash(
                    s.material.as_ref().map(Arc::as_ptr),
                    &s.blend,
                    s.blend_constant,
                    0,
                ),
            ),
        }
    }

    /// 尝试把 `self`（前段）与 `other`（后段）合并为一段。
    /// 仅当 pipeline 状态相同（bind group / geometry / 模板）且范围连续时可合并，
    /// 否则返回 `None`。不连续时合并会误画两段之间的内容，必须拒绝。
    fn try_merge(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (
                OrderedShapeSegment::Mesh {
                    ndx_start,
                    ndx_count,
                    bind_group,
                    geometry,
                    material,
                    blend,
                    blend_constant,
                },
                OrderedShapeSegment::Mesh {
                    ndx_start: n2,
                    ndx_count: n2_count,
                    bind_group: b2,
                    geometry: g2,
                    material: m2,
                    blend: b2_blend,
                    blend_constant: b2_constant,
                },
            ) => {
                let merged = merge_decision(
                    geometry == g2
                        && bind_group == b2
                        && material.as_ref().map(Arc::as_ptr) == m2.as_ref().map(Arc::as_ptr)
                        && blend == b2_blend
                        && blend_constant == b2_constant,
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
                    blend: *blend,
                    blend_constant: *blend_constant,
                })
            }
            (OrderedShapeSegment::Instances(s), OrderedShapeSegment::Instances(s2)) => {
                let merged = merge_decision(
                    s.bind_group == s2.bind_group
                        && s.material.as_ref().map(Arc::as_ptr)
                            == s2.material.as_ref().map(Arc::as_ptr)
                        && s.blend == s2.blend
                        && s.blend_constant == s2.blend_constant,
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
                    blend: s.blend,
                    blend_constant: s.blend_constant,
                }))
            }
            (OrderedShapeSegment::Particles(s), OrderedShapeSegment::Particles(s2)) => {
                let merged = merge_decision(
                    s.bind_group == s2.bind_group
                        && s.material.as_ref().map(Arc::as_ptr)
                            == s2.material.as_ref().map(Arc::as_ptr)
                        && s.blend == s2.blend
                        && s.blend_constant == s2.blend_constant,
                    s.particle_start,
                    s.particle_count,
                    s2.particle_start,
                    s2.particle_count,
                )?;
                Some(OrderedShapeSegment::Particles(ParticleSegment {
                    particle_start: merged.0,
                    particle_count: merged.1,
                    bind_group: s.bind_group.clone(),
                    material: s.material.clone(),
                    blend: s.blend,
                    blend_constant: s.blend_constant,
                }))
            }
            (OrderedShapeSegment::GeoInstances(s), OrderedShapeSegment::GeoInstances(s2)) => {
                let merged = merge_decision(
                    s.bind_group == s2.bind_group
                        && s.template_vertex_start == s2.template_vertex_start
                        && s.template_index_start == s2.template_index_start
                        && s.index_count == s2.index_count
                        && s.material.as_ref().map(Arc::as_ptr)
                            == s2.material.as_ref().map(Arc::as_ptr)
                        && s.blend == s2.blend
                        && s.blend_constant == s2.blend_constant,
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
                    blend: s.blend,
                    blend_constant: s.blend_constant,
                }))
            }
            _ => None,
        }
    }
}

/// 合并决策（纯函数）：`same_state` = pipeline 状态一致（bind group / geometry / blend）。
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
    use std::hash::BuildHasher;

    rustc_hash::FxBuildHasher.hash_one(bg)
}

/// 段的状态指纹：material 指针 / blend / blend constant / 额外键（geo 模板）。
///
/// 只用于 `preserve_order=false` 的排序分组；哈希碰撞只会少合并一次（`try_merge`
/// 仍做逐字段相等比较），不影响正确性。
fn segment_state_hash(
    material: Option<*const Material>,
    blend: &wgpu::BlendState,
    blend_constant: wgpu::Color,
    extra: u64,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut h = std::hash::DefaultHasher::new();
    material.hash(&mut h);
    blend.hash(&mut h);
    blend_constant.r.to_bits().hash(&mut h);
    blend_constant.g.to_bits().hash(&mut h);
    blend_constant.b.to_bits().hash(&mut h);
    blend_constant.a.to_bits().hash(&mut h);
    extra.hash(&mut h);
    h.finish()
}

/// geo 模板身份（顶点起点 / 索引起点 / 索引数）。
fn geo_template_key(vertex_start: u32, index_start: u32, index_count: u32) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut h = std::hash::DefaultHasher::new();
    (vertex_start, index_start, index_count).hash(&mut h);
    h.finish()
}

struct ShapeInfo {
    base_vertex: i32,
    segments: Vec<ShapeSegment>,
    geometry: bool,
    instances: Vec<InstanceSegment>,
    geo_instances: Vec<GeoInstanceSegment>,
    particles: Vec<ParticleSegment>,
    /// legacy 路径的统一 blend（段构建时即 batch 级一致；ordered 路径用段自带值，此处忽略）。
    blend: wgpu::BlendState,
    /// 本批粒子的时钟（`batch.particle_clock`，batch-local）。
    /// 粒子段 group 0 绑此钟的 camera 组；非粒子绘制不用。
    particle_clock: ClockIndex,
    ordered: Vec<OrderedShapeSegment>,
}

struct TextRenderSegment {
    vertex_start: u32,
    vertex_count: u32,
    bind_group: Option<wgpu::BindGroup>,
    blend: wgpu::BlendState,
    blend_constant: wgpu::Color,
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
    dynamic_offsets: Vec<u32>,
}

/// 单批次编码到已有 `RenderPass` 所需的渲染器资源。
///
/// 由 `Renderer::draw` 的合并阶段提供：合并后的顶点/索引/实例/几何 buffer、
/// 相机与引擎存储 bind group、采样参数。`encode_into` 只负责把本批次的形状
/// 命令发给 GPU，不负责合并数据或创建 pass，因此同一批 `ctx` 可复用到多个
/// pass（深度预通道＋着色通道、场景／后处理／UI 同一 Pass 内分段），用户
/// 自管 Pass 结构，无需额外全屏采样合成回主屏。
pub(crate) struct BatchEncodeCtx<'a> {
    pub renderer: &'a Renderer,
    pub gpu: &'a GpuContext,
    pub camera_bind_group: &'a wgpu::BindGroup,
    pub engine_bg: &'a wgpu::BindGroup,
    /// 下列 buffer 只在对应内容存在时才创建；`encode_into` 仅在对应分支取用。
    pub vertex_buf: Option<&'a wgpu::Buffer>,
    pub index_buf: Option<&'a wgpu::Buffer>,
    pub particle_buf: Option<&'a wgpu::Buffer>,
    pub geo_template_vbuf: Option<&'a wgpu::Buffer>,
    pub geo_template_ibuf: Option<&'a wgpu::Buffer>,
    pub geo_instance_buf: Option<&'a wgpu::Buffer>,
    pub sample_count: u32,
    pub alpha_to_coverage: bool,
    pub ssaa: bool,
}

/// 单批次在本次合并后的绘制状态。
///
/// 段内的 `ndx_start`/`instance_start` 等已偏移到合并 buffer 的全局位置；
/// `encode_into` 直接按段发 `draw_indexed`，不再做数据合并。
pub(crate) struct BatchEncodeInfo<'a> {
    pub base_vertex: i32,
    pub geometry: bool,
    pub blend: wgpu::BlendState,
    /// MRT：每个颜色附件的混合状态，由 RenderTarget 提供；batch.blend 会覆盖第 0 个。
    pub target_blends: &'a [Option<wgpu::BlendState>],
    pub custom_material: &'a Option<Arc<Material>>,
    pub segments: &'a [ShapeSegment],
    pub instances: &'a [InstanceSegment],
    pub geo_instances: &'a [GeoInstanceSegment],
    pub particles: &'a [ParticleSegment],
    pub ordered: &'a [OrderedShapeSegment],
    pub particle_clock: ClockIndex,
    pub stencil_ref: u32,
    pub dynamic_offsets: &'a [u32],
    pub pipe_op: u32,
    pub uses_stencil: bool,
    pub has_ds: bool,
}

/// 自带的 depth-stencil 附件（用户自己的 `TextureView` + 读写操作）。
///
/// 约束：
/// - 纹理格式必须为 `Depth24PlusStencil8`（vireo 全部内置管线固定该格式）；
/// - `sample_count` 必须与本次绘制的 sample count 一致（`AntiAliasing::Msaa { samples }`
///   时用多重采样纹理；`None`/`Ssaa` 时用 1）——否则 wgpu 校验失败。
///
/// `depth_ops` / `stencil_ops` 为 `None` 表示该 aspect 只读（不 load/clear/store）；
/// 两者都为 `None` 时本次绘制视为**没有** depth-stencil（告警一次）。
pub struct DepthStencilAttachment {
    pub view: wgpu::TextureView,
    pub depth_ops: Option<wgpu::Operations<f32>>,
    pub stencil_ops: Option<wgpu::Operations<u32>>,
}

/// 引擎管理的 DS 附件操作（view 由渲染器提供）。
///
/// 语义（与 [`DepthStencilTarget::External`] 同）：`None` = 该 aspect 只读；
/// 两者都 `None` = 本次绘制视为没有 depth-stencil（告警一次）。
/// 绘制命令带 stencil 裁切时，缺失的 aspect 自动补为
/// depth `Clear(1.0)+Discard`、stencil `Clear(0)+Discard`。
#[derive(Clone, Debug, Default)]
pub struct DepthStencilOps {
    pub depth: Option<wgpu::Operations<f32>>,
    pub stencil: Option<wgpu::Operations<u32>>,
}

/// 本帧的 depth-stencil 附件来源。
///
/// - `None`（`RenderTarget::depth_stencil` 为 `None`）= 与历史行为一致：
///   仅 stencil 裁切帧挂载内部 DS；
/// - `Engine`：挂载渲染器内部 DS 纹理，只给 load/store 操作；
/// - `External`：挂载用户自带的 view（可做深度预通道等跨帧/跨 draw 的深度传递）。
pub enum DepthStencilTarget {
    External(DepthStencilAttachment),
    Engine(DepthStencilOps),
}

/// 一次绘制的采样契约（MSAA / SSAA / alpha-to-coverage）。
///
/// 它是**目标**的属性，不是 pass 的属性：附件有多少采样点、片元着色器是否按采样点
/// 求值、是否启用 alpha-to-coverage——这三件事共同决定管线形状与附件组成，所以随
/// [`RenderTarget`] 一起传入，渲染器不再自己保存一份 AA 状态。
///
/// | 取值 | 含义 |
/// |---|---|
/// | `samples == 1` | 无多重采样（`per_sample` / `alpha_to_coverage` 必须为 false） |
/// | `samples > 1, per_sample == false` | MSAA：每像素着色，硬件 resolve |
/// | `samples > 1, per_sample == true` | SSAA：`@interpolate(linear, sample)`，每采样点着色 |
///
/// 引擎自建 MSAA（`samples > 1` 且附件本身是单采样且没有 `resolve_target`）时，
/// 用的就是这个 `samples`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetAa {
    /// 本次 pass 的采样点数（1 = 无多重采样）。
    pub samples: u32,
    /// 片元着色器是否按采样点求值（SSAA 与 MSAA 的唯一区别）。
    pub per_sample: bool,
    /// 是否启用 alpha-to-coverage（与 MSAA/SSAA 正交）。
    pub alpha_to_coverage: bool,
}

impl Default for TargetAa {
    fn default() -> Self {
        Self::none()
    }
}

impl TargetAa {
    /// 单采样、无极性特性。
    pub const fn none() -> Self {
        Self {
            samples: 1,
            per_sample: false,
            alpha_to_coverage: false,
        }
    }

    /// MSAA：每像素着色 + 硬件 resolve。
    pub const fn msaa(samples: u32) -> Self {
        Self {
            samples,
            per_sample: false,
            alpha_to_coverage: false,
        }
    }

    /// SSAA：每采样点着色（`@interpolate(linear, sample)`）。
    pub const fn ssaa(samples: u32) -> Self {
        Self {
            samples,
            per_sample: true,
            alpha_to_coverage: false,
        }
    }

    /// 打开/关闭 alpha-to-coverage（builder）。
    pub const fn with_alpha_to_coverage(mut self, enabled: bool) -> Self {
        self.alpha_to_coverage = enabled;
        self
    }

    /// 从附件的实际采样点数推导（单采样 → [`TargetAa::none`]）。
    pub fn from_view(view: &wgpu::TextureView) -> Self {
        match view.texture().sample_count() {
            n if n > 1 => Self::msaa(n),
            _ => Self::none(),
        }
    }

    /// `samples == 1` 时不允许 `per_sample` / `alpha_to_coverage`。
    pub(crate) fn is_consistent(&self) -> bool {
        self.samples > 0 && (self.samples > 1 || (!self.per_sample && !self.alpha_to_coverage))
    }
}

impl From<crate::window::AntiAliasing> for TargetAa {
    /// 画布级抗锯齿配置 → 一次绘制的采样契约。
    ///
    /// `samples == 1` 时丢掉 `per_sample` / `alpha_to_coverage`（单采样下无意义，
    /// 且 wgpu 会拒绝）。
    fn from(aa: crate::window::AntiAliasing) -> Self {
        use crate::window::AntiAliasing;
        let aa = match aa {
            AntiAliasing::None => TargetAa::none(),
            AntiAliasing::Msaa {
                samples,
                alpha_to_coverage,
            } => TargetAa::msaa(samples).with_alpha_to_coverage(alpha_to_coverage),
            AntiAliasing::Ssaa {
                samples,
                alpha_to_coverage,
            } => TargetAa::ssaa(samples).with_alpha_to_coverage(alpha_to_coverage),
        };
        if aa.samples <= 1 {
            TargetAa::none()
        } else {
            aa
        }
    }
}

/// 单个颜色附件描述（MRT 支持多个）。
pub struct ColorTargetDesc {
    pub view: wgpu::TextureView,
    pub resolve_target: Option<wgpu::TextureView>,
    /// 该目标的混合状态；`None` 表示不混合（全写）。
    pub blend: Option<wgpu::BlendState>,
}

/// 渲染目标，封装用于 render pass 的一个或多个 `TextureView`（MRT）。
///
/// 窗口和离屏纹理都通过此类型编码绘制命令。`color_attachments` 非空时以
/// 第 0 个为主；`draw()` 的 `clear_colors` 长度须与之相等。
pub struct RenderTarget {
    pub color_attachments: Vec<ColorTargetDesc>,
    /// 本帧 depth-stencil 附件（`None` = 引擎默认行为，见 [`DepthStencilTarget`]）。
    pub depth_stencil: Option<DepthStencilTarget>,
    /// 本次绘制的采样契约（采样点数 / SSAA / alpha-to-coverage）。
    ///
    /// 构造时从附件推导（单采样 → [`TargetAa::none`]）；引擎自建 MSAA 时由
    /// 画布（窗口/离屏）显式覆盖，例如 `target.set_aa(TargetAa::msaa(4))`。
    pub aa: TargetAa,
}

impl RenderTarget {
    /// 从已有的 TextureView 创建（单目标，兼容旧调用）。
    pub fn from_texture_view(view: wgpu::TextureView) -> Self {
        let aa = TargetAa::from_view(&view);
        Self {
            color_attachments: vec![ColorTargetDesc {
                view,
                resolve_target: None,
                blend: None,
            }],
            depth_stencil: None,
            aa,
        }
    }

    /// 从多个颜色附件创建（MRT）。
    pub fn from_color_attachments(attachments: Vec<ColorTargetDesc>) -> Self {
        debug_assert!(!attachments.is_empty(), "color_attachments 不能为空");
        let aa = attachments
            .first()
            .map(|a| TargetAa::from_view(&a.view))
            .unwrap_or_default();
        Self {
            color_attachments: attachments,
            depth_stencil: None,
            aa,
        }
    }

    /// 设置本帧 depth-stencil 附件（builder，默认 `None`）。
    pub fn with_depth_stencil(mut self, depth_stencil: Option<DepthStencilTarget>) -> Self {
        self.depth_stencil = depth_stencil;
        self
    }

    /// 覆盖本次绘制的采样契约（builder）。引擎自建 MSAA 时用。
    pub fn with_aa(mut self, aa: TargetAa) -> Self {
        debug_assert!(
            aa.is_consistent(),
            "TargetAa: samples == 1 时不能开 per_sample/atc"
        );
        self.aa = aa;
        self
    }

    /// 覆盖本次绘制的采样契约。
    pub fn set_aa(&mut self, aa: TargetAa) {
        debug_assert!(
            aa.is_consistent(),
            "TargetAa: samples == 1 时不能开 per_sample/atc"
        );
        self.aa = aa;
    }

    /// 设置本帧 depth-stencil 附件（默认 `None`）。
    pub fn set_depth_stencil(&mut self, depth_stencil: Option<DepthStencilTarget>) {
        self.depth_stencil = depth_stencil;
    }

    /// 编码渲染命令到 `CommandBuffer`（**不** submit/present）。
    /// `clear_colors` 长度须与 `color_attachments` 长度相等；`None` 表示该目标
    /// 使用 `LoadOp::Load`（保留旧内容），`Some(c)` 表示 `LoadOp::Clear(c)`。
    pub fn draw(
        &self,
        renderer: &Renderer,
        clear_colors: &[Option<crate::color::Color>],
        batches: &[&DrawBatch],
    ) -> wgpu::CommandBuffer {
        renderer.draw(self, clear_colors, batches)
    }
}

/// 渲染器 —— 管理 vertex/index buffer 复用，执行单 pass 渲染。
///
/// 内部维护 GPU buffer，支持在多 batch 间以偏移量追加写入。
pub struct Renderer {
    pub(crate) gpu: std::sync::Arc<GpuContext>,
    camera_buf: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    /// camera uniform CPU 镜像（80B：投影 64＋dpi 4＋padding/time 12）。
    /// per-clock 组写 buffer 时从镜像拷投影/dpi，只 patch 时间字节，避免重算。
    camera_mirror: [u8; 80],
    /// per-clock camera 组：`(index, generation)` →（80B buffer，bind group）。
    /// 同 group-0 BGL（零管线影响），按需创建；野 handle 回落 0 号组。
    clock_groups: Mutex<FxHashMap<(u32, u32), (wgpu::Buffer, wgpu::BindGroup)>>,
    vertex_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    index_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    geo_instance_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
    particle_buf: Mutex<Option<(wgpu::Buffer, u64)>>,
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
    /// 引擎自建的 MSAA 附件，按颜色附件下标缓存（MRT 时每个附件各一份）。
    msaa_texs: Mutex<Vec<Option<(wgpu::Texture, wgpu::TextureView)>>>,
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

    scratch_scissor_stack: Mutex<Vec<(u32, u32, u32, u32)>>,
    scratch_instances: Mutex<Vec<InstanceData>>,
    scratch_particles: Mutex<Vec<ParticleInstance>>,
    scratch_geo_instances: Mutex<Vec<GeoInstance>>,
    scratch_geo_vertices: Mutex<Vec<GeoVertex>>,
    scratch_geo_indices: Mutex<Vec<u32>>,
    scratch_geo_merge_per_inst_seg: Mutex<Vec<u32>>,
    scratch_geo_merge_order: Mutex<Vec<u32>>,
    scratch_geo_merge_sorted: Mutex<Option<Vec<u32>>>,
    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数（真实 draw call 数）。
    /// `preserve_order=false` 重排合并后此值下降（bench 场景 3 混合可 1000→2）。
    last_draw_calls: Mutex<u32>,
    /// 上一帧参与绘制的 batch（扁平化顺序）实际使用的裁剪路径（诊断）。
    last_clip_paths: Mutex<Vec<ClipPath>>,
    /// `prepare_frame` 重入保护（嵌套 prep 会破坏 scratch 状态）。
    frame_active: std::sync::atomic::AtomicBool,
}

impl Renderer {
    /// 访问 GPU context（用于离屏像素回读等需要 device/queue 的场景）。
    pub fn gpu(&self) -> &std::sync::Arc<GpuContext> {
        &self.gpu
    }

    /// 构造渲染器。逻辑/物理尺寸、scale、dpi_scale 用于 camera 投影与文字缩放。
    pub fn new(
        gpu: std::sync::Arc<GpuContext>,
        logical_width: f32,
        logical_height: f32,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
        dpi_scale: f32,
    ) -> Self {
        let proj = glam::camera::rh::proj::opengl::orthographic(
            0.0,
            logical_width,
            logical_height,
            0.0,
            -1.0,
            1.0,
        );
        let camera_data: [[f32; 4]; 4] = proj.to_cols_array_2d();
        let mut camera_raw = [0u8; 80];
        camera_raw[..64].copy_from_slice(bytemuck::cast_slice(&camera_data));
        camera_raw[64..68].copy_from_slice(&dpi_scale.to_le_bytes());
        let camera_buf = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
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
            camera_mirror: camera_raw,
            clock_groups: Mutex::new(FxHashMap::default()),
            vertex_buf: Mutex::new(None),
            index_buf: Mutex::new(None),
            geo_instance_buf: Mutex::new(None),
            particle_buf: Mutex::new(None),
            geo_template_vertex_buf: Mutex::new(None),
            geo_template_index_buf: Mutex::new(None),
            physical_width,
            physical_height,
            scale,
            text_viewport_override: Mutex::new(None),
            msaa_texs: Mutex::new(Vec::new()),
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
            scratch_scissor_stack: Mutex::new(Vec::new()),
            scratch_instances: Mutex::new(Vec::new()),
            scratch_particles: Mutex::new(Vec::new()),
            scratch_geo_instances: Mutex::new(Vec::new()),
            scratch_geo_vertices: Mutex::new(Vec::new()),
            scratch_geo_indices: Mutex::new(Vec::new()),
            scratch_geo_merge_per_inst_seg: Mutex::new(Vec::new()),
            scratch_geo_merge_order: Mutex::new(Vec::new()),
            scratch_geo_merge_sorted: Mutex::new(None),
            last_draw_calls: Mutex::new(0),
            last_clip_paths: Mutex::new(Vec::new()),
            frame_active: std::sync::atomic::AtomicBool::new(false),
            logical_width,
            logical_height,
        }
    }

    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数。
    /// 由 [`Self::draw`] 在每帧统计；未 draw 时为 0。
    pub fn last_draw_calls(&self) -> u32 {
        *self.last_draw_calls.lock()
    }

    /// 上一帧参与绘制的 batch（含子 batch，按扁平化顺序）实际使用的裁剪路径。
    ///
    /// 只统计通过剔除的 batch；未 draw / 空帧时为空。诊断用 —— 例如确认矩形裁剪
    /// 实际走了 scissor 还是自动探测，或 Area 是否生效。
    pub fn last_clip_paths(&self) -> Vec<ClipPath> {
        self.last_clip_paths.lock().clone()
    }

    /// 清空 MSAA/DS 纹理缓存（格式漂移、尺寸或采样点数变化后下一帧重建）。
    pub(crate) fn clear_msaa_ds(&self) {
        self.msaa_texs.lock().clear();
        *self.ds_tex.lock() = None;
    }

    /// 获取第 `index` 个颜色附件的引擎 MSAA 视图（必要时创建）。
    ///
    /// `samples <= 1` 返回 `None`，调用方直接用原附件。
    fn msaa_view_at(
        &self,
        index: usize,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> Option<wgpu::TextureView> {
        if samples <= 1 {
            return None;
        }
        let mut views = self.msaa_texs.lock();
        if views.len() <= index {
            views.resize_with(index + 1, || None);
        }
        let slot = &mut views[index];
        let stale = slot.as_ref().is_none_or(|(t, _)| {
            t.width() != self.physical_width
                || t.height() != self.physical_height
                || t.sample_count() != samples
                || t.format() != format
        });
        if stale {
            let tex = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("msaa"),
                size: wgpu::Extent3d {
                    width: self.physical_width,
                    height: self.physical_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            *slot = Some((tex, view));
        }
        slot.as_ref().map(|(_, v)| v.clone())
    }

    /// 获取 depth/stencil 视图（Depth24PlusStencil8，必要时创建）。
    /// 采样点数与颜色附件一致。
    fn ds_view(&self, samples: u32) -> wgpu::TextureView {
        let mut dt = self.ds_tex.lock();
        let ok = dt.as_ref().is_some_and(|(t, _)| {
            t.width() == self.physical_width
                && t.height() == self.physical_height
                && t.sample_count() == samples
        });
        if !ok {
            let tex = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("depth_stencil"),
                size: wgpu::Extent3d {
                    width: self.physical_width,
                    height: self.physical_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
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
        let proj = glam::camera::rh::proj::opengl::orthographic(
            0.0,
            logical_width,
            logical_height,
            0.0,
            -1.0,
            1.0,
        );
        let camera_data: [[f32; 4]; 4] = proj.to_cols_array_2d();
        let mut camera_raw = [0u8; 80];
        camera_raw[..64].copy_from_slice(bytemuck::cast_slice(&camera_data));
        camera_raw[64..68].copy_from_slice(&dpi_scale.to_le_bytes());
        self.gpu
            .queue
            .write_buffer(&self.camera_buf, 0, &camera_raw);
        self.camera_mirror = camera_raw;
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
        self.msaa_texs.lock().clear();
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

        self.draw(target, &[Some(clear_color)], &[&geo_batch, &sdf_batch]);
    }

    fn ensure_vertex_buffer(&self, size: u64) {
        if size == 0 {
            return;
        }
        let mut slot = self.vertex_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two()
        } else {
            (cur * 2).max(size)
        };
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertex buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buf, new_cap));
    }

    /// 确保 instance storage buffer 足够大（vertex-pulling，VS 按 instance_index 读取）。
    fn ensure_instance_storage_buffer(&self, size: u64) {
        if size == 0 {
            return;
        }
        let mut slot = self.gpu.instance_storage_buf.lock().unwrap();
        let cur = (*slot).as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two()
        } else {
            (cur * 2).max(size)
        };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo instance storage buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
        // buffer 已重建：engine storage bind group 缓存绑的是旧 buffer，必须失效。
        *self.engine_storage_bind_group_cache.lock() = None;
    }

    fn ensure_geo_instance_buffer(&self, size: u64) {
        if size == 0 {
            return;
        }
        let mut slot = self.geo_instance_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two()
        } else {
            (cur * 2).max(size)
        };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo geo instance buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
    }

    fn ensure_particle_buffer(&self, size: u64) {
        if size == 0 {
            return;
        }
        let mut slot = self.particle_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two()
        } else {
            (cur * 2).max(size)
        };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo particle instance buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
    }

    /// 取时钟的 camera 组（按需创建＋写入当前时间），供粒子段绑 group 0。
    ///
    /// - 同 group-0 BGL 的独立组实例：其他绘制（SDF/geo/text）不受影响，零管线变动。
    /// - 内容每用必写（`camera_mirror` 投影/dpi ＋注册表时间），故槽位复用/野 handle
    ///   天然正确；野 handle 由 `resolve_clock` 回落 0 号（warn 一条，见 `clock_time`）。
    /// - TEMP 桥删除后，共享 `camera_buf` 的时间字节不再有人读（SDF/geo/FS 均无 time 字段）。
    fn clock_camera_group(&self, id: ClockIndex) -> wgpu::BindGroup {
        let eff = self.gpu.resolve_clock(id);
        let key = eff.key();
        let t = self.gpu.clock_time(eff);
        let mut map = self.clock_groups.lock();
        let entry = map.entry(key).or_insert_with(|| {
            let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vireo particle clock camera buffer"),
                size: 80,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bg = self
                .gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("vireo particle clock camera bind group"),
                    layout: &self.gpu.camera_bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    }],
                });
            (buf, bg)
        });
        let mut raw = self.camera_mirror;
        raw[68..72].copy_from_slice(&t.to_le_bytes());
        self.gpu.queue.write_buffer(&entry.0, 0, &raw);
        entry.1.clone()
    }

    fn ensure_geo_template_vertex_buffer(&self, size: u64) {
        if size == 0 {
            return;
        }
        let mut slot = self.geo_template_vertex_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two()
        } else {
            (cur * 2).max(size)
        };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo geo template vertex buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
    }

    fn ensure_geo_template_index_buffer(&self, size: u64) {
        if size == 0 {
            return;
        }
        let mut slot = self.geo_template_index_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two()
        } else {
            (cur * 2).max(size)
        };
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vireo geo template index buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buffer, new_cap));
    }

    fn ensure_index_buffer(&self, size: u64) {
        if size == 0 {
            return;
        }
        let mut slot = self.index_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two()
        } else {
            (cur * 2).max(size)
        };
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("index buffer"),
            size: new_cap,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some((buf, new_cap));
    }

    fn ensure_polygon_edge_buffer(&self, size: u64) {
        if size == 0 {
            return;
        }
        let mut slot = self.polygon_edge_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two().max(64)
        } else {
            (cur * 2).max(size)
        };
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
        if size == 0 {
            return;
        }
        let mut slot = self.transform_buf.lock();
        let cur = slot.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if cur >= size {
            return;
        }
        let new_cap = if cur == 0 {
            size.next_power_of_two().max(48)
        } else {
            (cur * 2).max(size)
        };
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
mod tests;
