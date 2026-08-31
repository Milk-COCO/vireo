use std::sync::Arc;
use rustc_hash::FxHashMap;

use crate::area::{effective_area, Area, AreaGeom};
use crate::gpu::{GeoInstance, GeoVertex, ShapeInstance, Vertex};
use crate::math::{
    affine_rect_bounds, mul_affine_cols, seed_identity_transform_table, transform_key, Pos,
    Rect, Transform, UvRect,
};
use crate::material::Material;
use crate::text::{TextDef, TextEntryList};

use super::{BatchOverride, DrawEvent, GeoTemplate, ShapeStats};

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

#[derive(Clone)]
pub(crate) struct TextureSegment {
    pub(crate) ndx_start: u32,
    pub(crate) ndx_count: u32,
    /// `None` = 白纹理路径（draw 时解析为 `gpu.white_bind_group`）
    pub(crate) bind_group: Option<wgpu::BindGroup>,
}

#[derive(Clone)]
pub(crate) struct InstanceTextureSegment {
    pub(crate) instance_start: u32,
    pub(crate) instance_count: u32,
    pub(crate) bind_group: Option<wgpu::BindGroup>,
}

#[derive(Clone)]
pub(crate) enum BatchShapeCommand {
    Mesh {
        ndx_start: u32,
        ndx_count: u32,
        bind_group: Option<wgpu::BindGroup>,
        texture_generation: u32,
        geometry: bool,
        material: Option<Arc<Material>>,
    },
    Instances {
        instance_start: u32,
        instance_count: u32,
        bind_group: Option<wgpu::BindGroup>,
        texture_generation: u32,
        material: Option<Arc<Material>>,
    },
    GeoInstances {
        geo_instance_start: u32,
        geo_instance_count: u32,
        template_vertex_start: u32,
        template_index_start: u32,
        index_count: u32,
        bind_group: Option<wgpu::BindGroup>,
        texture_generation: u32,
        material: Option<Arc<Material>>,
    },
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum EdgeTemplateKind {
    Polygon,
    LineChain,
}

#[derive(Clone)]
pub(crate) struct EdgeTemplate {
    pub(crate) kind: EdgeTemplateKind,
    pub(crate) point_bits: Box<[u32]>,
    pub(crate) edges: Box<[f32]>,
    pub(crate) start: u32,
}

#[derive(Clone)]
pub struct DrawBatch {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub texts: TextEntryList,
    pub(crate) bind_group: Option<wgpu::BindGroup>,
    pub(crate) text_texture_view: Option<wgpu::TextureView>,
    pub(crate) texture_segments: Vec<TextureSegment>,
    pub(crate) instances: Vec<ShapeInstance>,
    pub(crate) instance_texture_segments: Vec<InstanceTextureSegment>,
    pub(crate) geo_instances: Vec<GeoInstance>,
    pub(crate) geo_instance_texture_segments: Vec<InstanceTextureSegment>,
    pub(crate) geo_templates: Vec<GeoTemplate>,
    pub(crate) geo_template_vertices: Vec<GeoVertex>,
    pub(crate) geo_template_indices: Vec<u32>,
    pub(crate) geo_template_map: FxHashMap<u64, u32>,
    pub(crate) shape_commands: Vec<BatchShapeCommand>,
    pub(crate) shape_texture_generation: u32,
    pub(crate) shape_mesh_end: u32,
    pub(crate) transform: Option<Transform>,
    /// 整批视图变换（**属性，非画笔状态**）。单位阵 = 无变换。
    ///
    /// 对本 batch 全部形状/文字/子树统一**左乘**（渲染期应用到 `transform_table`，
    /// 不落盘进顶点/实例）；子树继承（`flatten_events` 递归累计祖先 view）。
    ///
    /// 与 [`Self::transform`]（状态机，record 时逐形状烘焙）正交：`view` 是属性，
    /// flatten/draw 时整批一次消费，零 record 副作用。`Transform::IDENTITY` 是自然缺省
    /// （非 `Option`：`None` 与 `Some(IDENTITY)` 等价）。
    pub view: Transform,
    /// SDF 柔边宽度（逻辑像素，`None` = 几何光栅化模式，不走 SDF）。
    /// 默认值为 `Some(1.0)`；需要几何路径时显式设为 `None`。
    ///
    /// 公开 API 走 [`Self::sdf_feather`] / [`Self::set_sdf_feather`] /
    /// [`Self::clear_sdf_feather`]；字段私有，shape 内部可直接读写。
    ///
    /// 注意：SDF 图形不受 MSAA 影响。
    pub(crate) sdf_feather: Option<f32>,
    /// 当前画笔颜色；`draw_*(…, None)` 使用此值。
    /// 公开 API 走 [`Self::color`] / [`Self::set_color`]。
    pub(crate) color: crate::color::Color,
    /// 纹理坐标子区域：后续 shape 顶点 UV，以及之后 text 入队冻结的
    /// [`crate::text::TextTextureState::uv`]，均在此范围内映射。
    /// 公开 API 走 [`Self::uv`] / [`Self::set_uv`] / [`Self::clear_uv`]。
    /// 字段私有赋值不会传播到 `texts.texture_state`（潜在 bug：必须用 `set_uv`）。
    pub(crate) uv: UvRect,
    /// 多边形的边数据：每条边 4 个 f32 (nx, ny, dot(vi,n), 0)
    /// 由 draw_polygon 填充，渲染时合并到 storage buffer。
    pub polygon_edges: Vec<f32>,
    pub(crate) edge_templates: FxHashMap<u64, Vec<EdgeTemplate>>,
    /// 变换矩阵表（batch 内去重）。每个矩阵 12 f32（mat3x3，列 vec4-padded）。
    ///
    /// **槽 0 固定为单位矩阵**（`new`/`clear` 时写入，形状从 1 起占用）：
    /// - `transform_index == 0` = 恒等（与全局 `Renderer` 表槽 0、`draw_text` 默认 0 一致）
    /// - 切勿把第一个形状的平移写进槽 0，否则 `draw_text` 会二次平移（右下偏）
    /// - `push_child` 左乘父矩阵时会改写整表（含槽 0）；继承后槽 0 = 父变换，语义仍正确
    pub(crate) transform_table: Vec<f32>,
    /// hash → local index 映射（batch 内去重）。恒等矩阵始终映射到 0。
    pub(crate) transform_map: FxHashMap<u64, u32>,
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
    /// 自定义材质（**画笔状态机**，同 `color`/`transform`/`sdf_feather`）。
    ///
    /// - `Some(mat)` = 后续 shape 走该材质的 fragment shader；
    ///   shape 仍用对应顶点管线（SDF/geo/custom VS → mesh）；text 仍走 glyphon 顶点管线。
    /// - `None` = 内置管线（默认）。
    /// - 与 `clips_children` / Area stencil 兼容（自有 stencil pipeline 缓存）。
    ///
    /// 每个 shape 在 record 时捕获当前材质，render 时按 shape 自带的材质选 pipeline。
    /// 批内可自由切换（`Some(A) → None → Some(B)`），不同材质的 shape 不会合并 draw call。
    ///
    /// 公开 API 走 [`Self::custom_material`] / [`Self::set_custom_material`] /
    /// [`Self::clear_custom_material`]；字段私有，shape 内部可直接读。
    pub(crate) custom_material: Option<Arc<Material>>,
    /// Dynamic uniform/storage offsets for group 3 binding（逐 draw 偏移，字节）。
    /// 长度必须等于 BGL 中 `has_dynamic_offset` 的 binding 数量。
    pub dynamic_offsets: Vec<u32>,
    /// 是否保留 shape 调用顺序。
    /// - `true`（默认）：按 `draw_*` 调用顺序绘制（保持 z 序）。
    /// - `false`：允许 renderer 按（mesh/instance 种类、geometry 模式、bind group）
    ///   重排并合并本 batch 的绘制命令，减少 pipeline 切换 / draw call。
    ///   代价：混合 SDF/几何或不同贴图时绘制顺序不保证，可能改变视觉层叠。
    pub preserve_order: bool,
    /// 是否把本 batch 的 geo 实例按模板分组（`sdf_feather=None` 的几何路径）。
    /// - `true`：draw 阶段把同模板的 geo 实例重排到连续范围并合并，整个 batch
    ///   同模板实例只占 1 个 draw call（draw call 数 = 模板种类数）。
    ///   代价：geo 实例间的相对顺序（z 序）不再保留，相同模板的实例一起绘制。
    /// - `false`（默认）：保持 `draw_*` 调用顺序，仅相邻且范围连续的实例合并。
    ///
    /// 与 [`Self::preserve_order`] 独立：此属性只作用于 geo 实例路径；
    /// `preserve_order` 控制的是重排后的段是否按 sort_key 再排序合并。
    pub merge_geo_templates: bool,
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
            geo_instances: Vec::with_capacity(32),
            geo_instance_texture_segments: Vec::with_capacity(2),
            geo_templates: Vec::with_capacity(8),
            geo_template_vertices: Vec::with_capacity(64),
            geo_template_indices: Vec::with_capacity(96),
            geo_template_map: FxHashMap::default(),
            shape_commands: Vec::with_capacity(8),
            shape_texture_generation: 0,
            shape_mesh_end: 0,
            transform: None,
            view: Transform::IDENTITY,
            sdf_feather: Some(1.0),
            color: crate::color::colors::WHITE,
            uv: UvRect::default(),
            polygon_edges: Vec::with_capacity(16),
            edge_templates: FxHashMap::default(),
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
            preserve_order: true,
            merge_geo_templates: false,
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
        self.geo_instances.clear();
        self.geo_instance_texture_segments.clear();
        self.geo_templates.clear();
        self.geo_template_vertices.clear();
        self.geo_template_indices.clear();
        self.geo_template_map.clear();
        self.shape_commands.clear();
        self.shape_texture_generation = 0;
        self.shape_mesh_end = 0;
        self.transform = None;
        self.view = Transform::IDENTITY;
        self.sdf_feather = Some(1.0); // 与 new() 一致：SDF 路径
        self.color = crate::color::colors::WHITE;
        self.uv = UvRect::default();
        self.polygon_edges.clear();
        self.edge_templates.clear();
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
        self.preserve_order = true;
        self.merge_geo_templates = false;
    }
    pub fn to_area(&self) -> Area {
        if (self.vertices.is_empty() || self.indices.is_empty())
            && self.instances.is_empty()
            && self.geo_instances.is_empty()
        {
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
        for instance in &self.geo_instances {
            if let Some(template) = self
                .geo_templates
                .iter()
                .find(|t| t.vertex_start == instance.template_vertex_start)
            {
                let base = vertices.len() as u32;
                let tvs = template.vertex_start as usize;
                let color = crate::color::Color::new(
                    instance.color[0], instance.color[1], instance.color[2], instance.color[3],
                );
                for gv in &self.geo_template_vertices[tvs..tvs + template.vertex_count as usize] {
                    vertices.push(Vertex::new_uv_xform(
                        gv.position[0],
                        gv.position[1],
                        gv.uv[0],
                        gv.uv[1],
                        color,
                        instance.transform_index,
                    ));
                }
                let tis = template.index_start as usize;
                for &idx in &self.geo_template_indices[tis..tis + template.index_count as usize] {
                    indices.push(base + (idx - template.vertex_start));
                }
            }
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
    pub(crate) fn table_cols_at(table: &[f32], idx: u32) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let base = idx as usize * 12;
        if base + 12 > table.len() {
            return ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        }
        let t = &table[base..base + 12];
        ([t[0], t[1], 0.0], [t[4], t[5], 0.0], [t[8], t[9], 1.0])
    }

    /// 顶点局部坐标 × 表内矩阵 → 世界坐标。
    #[inline]
    pub(crate) fn world_xy(table: &[f32], idx: u32, lx: f32, ly: f32) -> (f32, f32) {
        let (c0, c1, c2) = Self::table_cols_at(table, idx);
        (
            c0[0] * lx + c1[0] * ly + c2[0],
            c0[1] * lx + c1[1] * ly + c2[1],
        )
    }

    /// 计算本 batch 自身在**世界（绝对逻辑）空间**的 AABB。
    /// 含形状顶点 + 文字近似框（pos / 字号）；不含子节点。
    /// 按 `transform_index` 查表，不用画笔 `current_matrix`。
    pub(crate) fn compute_own_world_aabb(&self) -> Option<Rect> {
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
        for instance in &self.geo_instances {
            if let Some(template) = self
                .geo_templates
                .iter()
                .find(|t| t.vertex_start == instance.template_vertex_start)
            {
                any = true;
                let tvs = template.vertex_start as usize;
                for gv in &self.geo_template_vertices[tvs..tvs + template.vertex_count as usize] {
                    let (wx, wy) = Self::world_xy(&self.transform_table, instance.transform_index, gv.position[0], gv.position[1]);
                    expand(&mut w_min_x, &mut w_max_x, &mut w_min_y, &mut w_max_y, wx, wy);
                }
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
    pub(crate) fn uses_scissor_path(&self, has_area: bool) -> bool {
        if has_area {
            return false;
        }
        self.scissor.is_some() || (self.clips_children && self.auto_scissor().is_some())
    }

    /// 检测 batch 自身是否只含一个**几何**轴对齐矩形（4 顶点 + 无旋转）。
    /// SDF 四顶点 AABB 不走 scissor（圆/圆角等须 stencil）。
    /// 世界位置来自顶点 `transform_index`（非画笔）。
    /// 同时支持 geo-instance 模板路径（`sdf_feather=None` 时 4v/6i 矩形模板）。
    pub(crate) fn auto_scissor(&self) -> Option<Rect> {
        // SDF 填充也是 4v/6i 外接框 → 禁止 auto-scissor，否则圆裁成方
        if self.has_sdf || self.sdf_feather.is_some() {
            return None;
        }
        if !self.geo_instances.is_empty() {
            return self.auto_scissor_geo();
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

    /// geo-instance 模板路径的 auto-scissor：单个实例 + 4v/6i 矩形模板 + 无旋转。
    pub(crate) fn auto_scissor_geo(&self) -> Option<Rect> {
        if self.geo_instances.len() != 1 {
            return None;
        }
        let instance = &self.geo_instances[0];
        let template = self
            .geo_templates
            .iter()
            .find(|t| t.vertex_start == instance.template_vertex_start)?;
        if template.vertex_count != 4 || template.index_count != 6 {
            return None;
        }
        let tvs = template.vertex_start as usize;
        let ti = instance.transform_index;
        let (c0, c1, _c2) = Self::table_cols_at(&self.transform_table, ti);
        if c1[0].abs() > 1e-6 || c0[1].abs() > 1e-6 {
            return None;
        }
        let mut worlds: [(f32, f32); 4] = [(0.0, 0.0); 4];
        for (i, gv) in self.geo_template_vertices[tvs..tvs + 4].iter().enumerate() {
            worlds[i] = Self::world_xy(&self.transform_table, ti, gv.position[0], gv.position[1]);
        }
        let w_min_x = worlds.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let w_max_x = worlds.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
        let w_min_y = worlds.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let w_max_y = worlds.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
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
            // 必须走 set_uv（传播到 texts.texture_state.uv 并 bump generation），
            // 与 set_uv/clear_uv 语义一致；否则子 batch 文字画笔仍默认 UV。
            self.set_uv(parent.uv.u0, parent.uv.v0, parent.uv.u1, parent.uv.v1);
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
            || !self.geo_instances.is_empty()
            || !self.texts.entries.is_empty()
            || self.children.iter().any(Self::has_drawable_content)
    }

    /// 当前 batch 的等效 shape 顶点数（GPU 端最终输出），用于诊断和统计。
    ///
    /// = `mesh_vertices + instances * 4 + Σ geo 模板顶点数`
    ///
    /// - **mesh path**：每个 shape 推送 4 顶点 unit quad（CPU 端）。
    /// - **instance path**：CPU 端只推送 1 个 `ShapeInstance`（约 80 字节），
    ///   GPU 渲染时用 1 个共享 quad × N instances = `N * 4` 顶点。
    /// - **geo-instance path**：CPU 端只推送 1 个 `GeoInstance` + 共享模板顶点，
    ///   每个实例渲染模板的全部顶点；模板顶点数按实例重复计数。
    ///
    /// 不包含子 batch 与文字。如果想看 CPU 端实际推送量，调用
    /// [`Self::shape_stats`]。
    pub fn shape_vertex_count(&self) -> usize {
        let mut count = self.vertices.len() + self.instances.len() * 4;
        for instance in &self.geo_instances {
            if let Some(template) = self
                .geo_templates
                .iter()
                .find(|t| t.vertex_start == instance.template_vertex_start)
            {
                count += template.vertex_count as usize;
            }
        }
        count
    }

    /// 两段式诊断：CPU mesh 顶点数 / instance 参数数 / geo-instance 参数数。
    /// draw call 数见渲染器 [`Renderer::last_draw_calls`]。
    pub fn shape_stats(&self) -> ShapeStats {
        ShapeStats {
            mesh_vertices: self.vertices.len(),
            sdf_instances: self.instances.len(),
            geo_instances: self.geo_instances.len(),
            geo_templates: self.geo_templates.len(),
            geo_template_vertices: self.geo_template_vertices.len(),
        }
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
    ///
    /// `view` 是祖先累计视图（`Transform::IDENTITY` 起始），子树左乘继承；
    /// `view_map` 按 batch 指针记录本 batch **有效视图**（祖先 × 自身），
    /// 供 draw 阶段对 `transform_table` 左乘（几何与文字共用）。
    pub(crate) fn flatten_events<'a>(
        &'a self,
        out: &mut Vec<DrawEvent<'a>>,
        level: u32,
        viewport: Option<Rect>,
        aabb_map: &FxHashMap<usize, Option<Rect>>,
        view: &Transform,
        view_map: &mut FxHashMap<usize, Transform>,
    ) {
        // 有效视图 = 祖先累计 view × 自身 view（左乘，子树继承）。
        let eff_view = view.then(&self.view);
        view_map.insert(self as *const DrawBatch as *const () as usize, eff_view);

        // Culling: 跳过屏外子树。
        // map 命中项已由 `compute_subtree_aabb` 仿射到视图空间，不再左乘视图；
        // 手动 bounds 与自身顶点 fallback 是世界空间，需按 eff_view 仿射。
        if let Some(vp) = viewport {
            let (v0, v1, v2) = eff_view.to_cols();
            let effective = match self.bounds {
                None => None,
                Some(None) => {
                    let key = self as *const DrawBatch as *const () as usize;
                    match aabb_map.get(&key).copied().flatten() {
                        Some(b) => Some(b),
                        None => self.compute_own_world_aabb().map(|b| {
                            affine_rect_bounds(&b, v0, v1, v2)
                        }),
                    }
                }
                Some(Some(b)) => Some(affine_rect_bounds(&b, v0, v1, v2)),
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
        // 与 draw 阶段 `compute_stencil_at_level` 的 has_geom 一致：geo-instance
        // 路径（sdf_feather=None 的默认几何）也算有几何，否则 clips_children 的
        // geo-only batch 只 Push 不 Pop，clip_depth 泄漏。
        let has_geom = !self.vertices.is_empty()
            || !self.instances.is_empty()
            || !self.geo_instances.is_empty();
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
            child.flatten_events(out, child_level, viewport, aabb_map, &eff_view, view_map);
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

    /// 当前画笔颜色。
    #[inline]
    pub fn color(&self) -> crate::color::Color {
        self.color
    }

    /// 当前 SDF 柔边宽度。
    #[inline]
    pub fn sdf_feather(&self) -> Option<f32> {
        self.sdf_feather
    }

    /// 设置 SDF 柔边宽度：`Some(f)` = SDF 路径（f 为柔边像素），`None` = 几何路径。
    /// 内部纯赋值，shape 端无副作用。
    #[inline]
    pub fn set_sdf_feather(&mut self, value: Option<f32>) {
        self.sdf_feather = value;
    }

    /// 清除 SDF 柔边（设为 `None`），走几何路径。
    #[inline]
    pub fn clear_sdf_feather(&mut self) {
        self.sdf_feather = None;
    }

    /// 当前自定义材质。`None` = 内置管线。
    #[inline]
    pub fn custom_material(&self) -> Option<&Arc<Material>> {
        self.custom_material.as_ref()
    }

    /// 设置自定义材质：`Some(mat)` = 后续 shape 走该材质；`None` = 内置。
    /// 每个 shape 在 draw 时捕获当前材质，批内可自由切换。
    #[inline]
    pub fn set_custom_material(&mut self, material: Option<Arc<Material>>) {
        self.custom_material = material;
    }

    /// 清除自定义材质（设为 `None`），后续 shape 走内置管线。
    #[inline]
    pub fn clear_custom_material(&mut self) {
        self.custom_material = None;
    }

    /// 当前 UV 子区域。
    #[inline]
    pub fn uv(&self) -> UvRect {
        self.uv
    }

    #[inline]
    fn invalidate_transform_cache(&mut self) {
        self.cached_transform_index = None;
    }

    /// 在临时应用共享覆盖后执行 `f`，结束时自动恢复 batch 画笔状态（不写回）。
    ///
    /// `BatchOverride` 为 `ShapeOverride` 去重并集 + `text_clip`：
    /// `uv`/`bind_group`/`color`/`transform` 在形状与文字间共享（同一 `DrawBatch`
    /// 状态机），`sdf_feather` 仅形状，`text_clip` 仅文字。
    /// 构造按集合覆盖：`BatchOverride::default().shape(s).text(t)` 用对应 `Some` 字段
    /// 覆盖并集，`sdf_feather`/`text_clip` 只动独有位。
    pub fn with_override<R>(
        &mut self,
        ov: BatchOverride,
        f: impl FnOnce(&mut Self, crate::color::Color) -> R,
    ) -> R {
        if ov.color.is_none()
            && ov.sdf_feather.is_none()
            && ov.uv.is_none()
            && ov.transform.is_none()
            && ov.bind_group.is_none()
            && ov.text_clip.is_none()
        {
            return f(self, self.color);
        }
        let saved_color = self.color;
        let saved_feather = self.sdf_feather;
        let saved_uv = self.uv;
        let saved_transform = self.transform;
        let saved_xform_cache = self.cached_transform_index;
        let saved_bind_group = self.bind_group.clone();
        let tex_overridden = ov.bind_group.is_some();
        let uv_overridden = ov.uv.is_some();
        let saved_text_clip = self.text_clip;
        let saved_text_state = self.texts.texture_state.clone();

        if let Some(c) = ov.color {
            self.color = c;
        }
        // Some(None)=几何, Some(Some(f))=SDF；外层 None=保持
        if let Some(feather) = ov.sdf_feather {
            self.sdf_feather = feather;
        }
        if let Some(uv) = ov.uv {
            self.uv = uv;
            self.texts.set_uv_state(uv);
        }
        if let Some(t) = ov.transform {
            self.set_transform(t);
        }
        // Some(None)=白纹理, Some(Some(bg))=绑定；外层 None=保持
        if let Some(bg) = ov.bind_group {
            self.add_texture_segment(self.bind_group.clone());
            self.add_instance_texture_segment(self.bind_group.clone());
            self.add_geo_instance_texture_segment(self.bind_group.clone());
            self.advance_shape_texture_generation();
            self.bind_group = bg.clone();
            // 同步文字 batch 贴图：共享 uv/bind_group
            self.texts.set_bind_group_state(bg.clone());
            self.text_texture_view = None;
        }
        if let Some(clip) = ov.text_clip {
            self.text_clip = clip;
        }

        let color = self.color;
        let result = f(self, color);

        if tex_overridden {
            // 含 clear（None）：必须封段，否则后续/本段顶点会落到 trailing 用恢复后的贴图
            self.add_texture_segment(self.bind_group.clone());
            self.add_instance_texture_segment(self.bind_group.clone());
            self.add_geo_instance_texture_segment(self.bind_group.clone());
            self.advance_shape_texture_generation();
            self.bind_group = saved_bind_group;
            self.texts.texture_state = saved_text_state.clone();
            // 恢复后需 bump generation 以分离后续文字段
            self.texts.texture_state.generation = self.texts.texture_state.generation.wrapping_add(1);
        } else if uv_overridden {
            self.texts.texture_state = saved_text_state.clone();
            self.texts.texture_state.generation = self.texts.texture_state.generation.wrapping_add(1);
        }
        self.color = saved_color;
        self.sdf_feather = saved_feather;
        self.uv = saved_uv;
        self.transform = saved_transform;
        self.cached_transform_index = saved_xform_cache;
        self.text_clip = saved_text_clip;
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

    pub(crate) fn record_mesh_command(&mut self, start: u32, geometry: bool) {
        let end = self.indices.len() as u32;
        if end <= start {
            return;
        }
        if start > self.shape_mesh_end {
            let gap_geometry = self.mesh_range_is_geometry(self.shape_mesh_end, start);
            self.shape_commands.push(BatchShapeCommand::Mesh {
                ndx_start: self.shape_mesh_end,
                ndx_count: start - self.shape_mesh_end,
                bind_group: self.bind_group.clone(),
                texture_generation: self.shape_texture_generation,
                geometry: gap_geometry,
                material: self.custom_material.clone(),
            });
        }
        if let Some(BatchShapeCommand::Mesh {
            ndx_start,
            ndx_count,
            texture_generation,
            geometry: last_geometry,
            material: last_material,
            ..
        }) = self.shape_commands.last_mut()
        {
            if *texture_generation == self.shape_texture_generation
                && *last_geometry == geometry
                && *ndx_start + *ndx_count == start
                && last_material.as_ref().map(Arc::as_ptr)
                    == self.custom_material.as_ref().map(Arc::as_ptr)
            {
                *ndx_count += end - start;
                self.shape_mesh_end = end;
                return;
            }
        }
        self.shape_commands.push(BatchShapeCommand::Mesh {
            ndx_start: start,
            ndx_count: end - start,
            bind_group: self.bind_group.clone(),
            texture_generation: self.shape_texture_generation,
            geometry,
            material: self.custom_material.clone(),
        });
        self.shape_mesh_end = end;
    }

    pub(crate) fn record_pending_mesh_command(&mut self) {
        let start = self.shape_mesh_end;
        let end = self.indices.len() as u32;
        if start >= end {
            return;
        }
        let geometry = self.mesh_range_is_geometry(start, end);
        self.record_mesh_command(start, geometry);
    }

    fn mesh_range_is_geometry(&self, start: u32, end: u32) -> bool {
        self.indices[start as usize..end as usize]
            .iter()
            .all(|&index| self.vertices.get(index as usize).is_some_and(|v| v.sdf_type == 0))
    }

    pub(crate) fn shape_commands_valid(&self) -> bool {
        if self.shape_mesh_end > self.indices.len() as u32 {
            return false;
        }
        self.shape_commands.iter().all(|command| match command {
            BatchShapeCommand::Mesh { ndx_start, ndx_count, .. } => {
                let end = ndx_start.saturating_add(*ndx_count);
                end <= self.indices.len() as u32
                    && self.indices[*ndx_start as usize..end as usize]
                        .iter()
                        .all(|&index| index < self.vertices.len() as u32)
            }
            BatchShapeCommand::Instances { instance_start, instance_count, .. } => {
                if instance_start.saturating_add(*instance_count) > self.instances.len() as u32 {
                    return false;
                }
                let edge_count = self.polygon_edges.len() / 4;
                self.instances[*instance_start as usize..(*instance_start + *instance_count) as usize]
                    .iter()
                    .all(|inst| {
                        if inst.sdf_type == 6 || inst.sdf_type == 7 {
                            let s = inst.sdf_params[0] as usize;
                            let c = inst.sdf_params[1] as usize;
                            s.saturating_add(c) <= edge_count
                        } else {
                            true
                        }
                    })
            }
            BatchShapeCommand::GeoInstances { geo_instance_start, geo_instance_count, .. } => {
                if geo_instance_start.saturating_add(*geo_instance_count) > self.geo_instances.len() as u32 {
                    return false;
                }
                // 校验引用的模板索引仍在 geo_template_indices 内
                self.geo_instances[*geo_instance_start as usize
                    ..(geo_instance_start + geo_instance_count) as usize]
                    .iter()
                    .all(|g| {
                        let s = g.template_index_start as usize;
                        s + g.index_count as usize <= self.geo_template_indices.len()
                    })
            }
        })
    }

    fn record_instance_command(&mut self, start: u32) {
        self.record_pending_mesh_command();
        let end = self.instances.len() as u32;
        if end <= start {
            return;
        }
        if let Some(BatchShapeCommand::Instances {
            instance_start,
            instance_count,
            texture_generation,
            material: last_material,
            ..
        }) = self.shape_commands.last_mut()
        {
            if *texture_generation == self.shape_texture_generation
                && *instance_start + *instance_count == start
                && last_material.as_ref().map(Arc::as_ptr)
                    == self.custom_material.as_ref().map(Arc::as_ptr)
            {
                *instance_count += end - start;
                return;
            }
        }
        self.shape_commands.push(BatchShapeCommand::Instances {
            instance_start: start,
            instance_count: end - start,
            bind_group: self.bind_group.clone(),
            texture_generation: self.shape_texture_generation,
            material: self.custom_material.clone(),
        });
    }

    /// 几何模板去重：key 命中返回已存模板，否则追加并登记。
    /// 返回模板（顶点/索引在 batch 模板表内的偏移 + 数量）。
    pub(crate) fn ensure_geo_template(
        &mut self,
        key: u64,
        vertices: Vec<GeoVertex>,
        indices: Vec<u32>,
    ) -> GeoTemplate {
        if let Some(&index) = self.geo_template_map.get(&key) {
            return self.geo_templates[index as usize];
        }
        let vertex_start = self.geo_template_vertices.len() as u32;
        let index_start = self.geo_template_indices.len() as u32;
        let index_count = indices.len() as u32;
        let vertex_count = vertices.len() as u32;
        self.geo_template_vertices.extend_from_slice(&vertices);
        self.geo_template_indices.extend_from_slice(&indices);
        let template = GeoTemplate { vertex_start, index_start, index_count, vertex_count };
        let slot = self.geo_templates.len() as u32;
        self.geo_templates.push(template);
        self.geo_template_map.insert(key, slot);
        template
    }

    /// 追加几何实例：引用模板 + 每实例 color/transform。
    pub(crate) fn push_geo_instance(&mut self, template: GeoTemplate, color: crate::color::Color, transform_index: u32) {
        let start = self.geo_instances.len() as u32;
        self.geo_instances.push(GeoInstance {
            template_vertex_start: template.vertex_start,
            template_index_start: template.index_start,
            index_count: template.index_count,
            color: [color.r, color.g, color.b, color.a],
            transform_index,
        });
        self.record_geo_instance_command(start);
    }

    /// 几何模板生成 + 实例化（`draw_shape` 几何模式热路径）。
    ///
    /// `key` 已含形状参数 + uv 位（由 shapes 层计算）。命中缓存：跳过整个
    /// 网格生成，仅推一个实例（CPU 节省的核心）。未命中：暂时接管
    /// `vertices`/`indices`，运行原 mesh 发射器捕获几何，再剥掉 color/transform
    /// 转成模板登记。
    ///
    /// 复用原 mesh 发射器意味着模板与 mesh 路径几何**逐位一致**，无分叉。
    pub(crate) fn geo_emit_template(
        &mut self,
        key: u64,
        emit: impl FnOnce(&mut Self),
        color: crate::color::Color,
    ) {
        if let Some(&slot) = self.geo_template_map.get(&key) {
            let template = self.geo_templates[slot as usize];
            let transform_index = self.current_transform_index();
            self.push_geo_instance(template, color, transform_index);
            return;
        }
        let saved_vertices = std::mem::take(&mut self.vertices);
        let saved_indices = std::mem::take(&mut self.indices);
        emit(self);
        let captured_vertices = std::mem::replace(&mut self.vertices, saved_vertices);
        let captured_indices = std::mem::replace(&mut self.indices, saved_indices);
        if captured_vertices.is_empty() || captured_indices.is_empty() {
            return;
        }
        // 发射器内部已调用 `current_transform_index()` 并缓存；此处复用。
        let transform_index = self.current_transform_index();
        let mut geo_vertices = Vec::with_capacity(captured_vertices.len());
        for v in captured_vertices {
            geo_vertices.push(GeoVertex { position: v.position, uv: v.uv });
        }
        let template = self.ensure_geo_template(key, geo_vertices, captured_indices);
        self.push_geo_instance(template, color, transform_index);
    }

    fn record_geo_instance_command(&mut self, start: u32) {
        self.record_pending_mesh_command();
        let end = self.geo_instances.len() as u32;
        if end <= start {
            return;
        }
        if let Some(BatchShapeCommand::GeoInstances {
            geo_instance_start,
            geo_instance_count,
            template_vertex_start,
            template_index_start,
            index_count,
            texture_generation,
            material: last_material,
            ..
        }) = self.shape_commands.last_mut()
        {
            let cur = self.geo_instances[start as usize];
            if *texture_generation == self.shape_texture_generation
                && *geo_instance_start + *geo_instance_count == start
                && *template_vertex_start == cur.template_vertex_start
                && *template_index_start == cur.template_index_start
                && *index_count == cur.index_count
                && last_material.as_ref().map(Arc::as_ptr)
                    == self.custom_material.as_ref().map(Arc::as_ptr)
            {
                *geo_instance_count += end - start;
                return;
            }
        }
        let cur = self.geo_instances[start as usize];
        self.shape_commands.push(BatchShapeCommand::GeoInstances {
            geo_instance_start: start,
            geo_instance_count: end - start,
            template_vertex_start: cur.template_vertex_start,
            template_index_start: cur.template_index_start,
            index_count: cur.index_count,
            bind_group: self.bind_group.clone(),
            texture_generation: self.shape_texture_generation,
            material: self.custom_material.clone(),
        });
    }

    pub(crate) fn advance_shape_texture_generation(&mut self) {
        self.shape_texture_generation = self.shape_texture_generation.wrapping_add(1);
    }

    fn edge_template_hash(kind: EdgeTemplateKind, points: &[(f32, f32)]) -> u64 {
        let mut hash: u64 = match kind {
            EdgeTemplateKind::Polygon => 0x9e37_79b9_7f4a_7c15,
            EdgeTemplateKind::LineChain => 0xc2b2_ae3d_27d4_eb4f,
        };
        for &(x, y) in points {
            for bits in [x.to_bits(), y.to_bits()] {
                hash ^= bits as u64;
                hash = hash.wrapping_mul(0x100_0000_01b3);
                hash ^= hash >> 32;
            }
        }
        hash ^ points.len() as u64
    }

    fn edge_template_matches(template: &EdgeTemplate, kind: EdgeTemplateKind, points: &[(f32, f32)]) -> bool {
        template.kind == kind
            && template.point_bits.len() == points.len() * 2
            && template
                .point_bits
                .chunks_exact(2)
                .zip(points)
                .all(|(bits, &(x, y))| bits[0] == x.to_bits() && bits[1] == y.to_bits())
    }

    fn reuse_edge_template(
        &mut self,
        hash: u64,
        kind: EdgeTemplateKind,
        points: &[(f32, f32)],
    ) -> Option<(u32, u32)> {
        let (template_index, start, count, valid) = {
            let templates = self.edge_templates.get(&hash)?;
            let (index, template) = templates
                .iter()
                .enumerate()
                .find(|(_, template)| Self::edge_template_matches(template, kind, points))?;
            let count = (template.edges.len() / 4) as u32;
            let range = template.start as usize * 4..(template.start + count) as usize * 4;
            (
                index,
                template.start,
                count,
                self.polygon_edges.get(range) == Some(template.edges.as_ref()),
            )
        };
        if valid {
            return Some((start, count));
        }

        let edges = self.edge_templates[&hash][template_index].edges.clone();
        let start = (self.polygon_edges.len() / 4) as u32;
        self.polygon_edges.extend_from_slice(&edges);
        self.edge_templates.get_mut(&hash).unwrap()[template_index].start = start;
        Some((start, count))
    }

    fn insert_edge_template(
        &mut self,
        hash: u64,
        kind: EdgeTemplateKind,
        points: &[(f32, f32)],
        edges: Vec<f32>,
    ) -> (u32, u32) {
        let start = (self.polygon_edges.len() / 4) as u32;
        let count = (edges.len() / 4) as u32;
        self.polygon_edges.extend_from_slice(&edges);
        let point_bits = points
            .iter()
            .flat_map(|&(x, y)| [x.to_bits(), y.to_bits()])
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.edge_templates.entry(hash).or_default().push(EdgeTemplate {
            kind,
            point_bits,
            edges: edges.into_boxed_slice(),
            start,
        });
        (start, count)
    }

    fn intern_polygon_edges(&mut self, points: &[(f32, f32)]) -> Option<(u32, u32)> {
        let hash = Self::edge_template_hash(EdgeTemplateKind::Polygon, points);
        if let Some(range) = self.reuse_edge_template(hash, EdgeTemplateKind::Polygon, points) {
            return Some(range);
        }
        let mut edges = Vec::with_capacity(points.len() * 4);
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            let len = (dx * dx + dy * dy).sqrt();
            if len < 0.001 {
                continue;
            }
            let nx = -dy / len;
            let ny = dx / len;
            edges.extend_from_slice(&[nx, ny, nx * a.0 + ny * a.1, 0.0]);
        }
        if edges.len() < 12 {
            return None;
        }
        Some(self.insert_edge_template(hash, EdgeTemplateKind::Polygon, points, edges))
    }

    fn intern_line_chain_edges(&mut self, points: &[(f32, f32)]) -> Option<(u32, u32)> {
        let hash = Self::edge_template_hash(EdgeTemplateKind::LineChain, points);
        if let Some(range) = self.reuse_edge_template(hash, EdgeTemplateKind::LineChain, points) {
            return Some(range);
        }
        let closed = points.len() > 2
            && (points[0].0 - points[points.len() - 1].0).abs() < 0.001
            && (points[0].1 - points[points.len() - 1].1).abs() < 0.001;
        let vertex_count = if closed { points.len() - 1 } else { points.len() };
        let segment_count = if closed { vertex_count } else { vertex_count.saturating_sub(1) };
        let mut edges = Vec::with_capacity(segment_count * 4);
        for i in 0..segment_count {
            let a = points[i];
            let b = points[if i + 1 < vertex_count { i + 1 } else { 0 }];
            if (b.0 - a.0).abs() + (b.1 - a.1).abs() < 0.001 {
                continue;
            }
            edges.extend_from_slice(&[a.0, a.1, b.0, b.1]);
        }
        if edges.is_empty() {
            return None;
        }
        Some(self.insert_edge_template(hash, EdgeTemplateKind::LineChain, points, edges))
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
            geo_instances: self.geo_instances.clone(),
            geo_instance_texture_segments: self.geo_instance_texture_segments.clone(),
            geo_templates: self.geo_templates.clone(),
            geo_template_vertices: self.geo_template_vertices.clone(),
            geo_template_indices: self.geo_template_indices.clone(),
            geo_template_map: self.geo_template_map.clone(),
            shape_commands: self.shape_commands.clone(),
            shape_texture_generation: self.shape_texture_generation,
            shape_mesh_end: self.shape_mesh_end,
            texts: TextEntryList::new_from_entries(&self.texts),
            transform: self.transform,
            view: self.view,
            sdf_feather: self.sdf_feather,
            color: self.color,
            uv: self.uv,
            polygon_edges: self.polygon_edges.clone(),
            edge_templates: self.edge_templates.clone(),
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
            preserve_order: self.preserve_order,
            merge_geo_templates: self.merge_geo_templates,
        }
    }

    /// 直接设置 shape 用 bind group（高级用法）。
    /// `None` 与 [`set_texture`]`(None)` 相同：后续形状走白纹理。
    ///
    /// **文字**：无法从裸 bind group 取出 view，会清空文字画笔贴图
    ///（之后 `text`/`push*` 走白 base）；需要文字贴图时请用 [`set_texture`]。
    pub fn set_bind_group(&mut self, bg: Option<wgpu::BindGroup>) {
        self.record_pending_mesh_command();
        self.add_texture_segment(self.bind_group.clone());
        self.add_instance_texture_segment(self.bind_group.clone());
        self.add_geo_instance_texture_segment(self.bind_group.clone());
        self.advance_shape_texture_generation();
        self.bind_group = bg.clone();
        self.text_texture_view = None;
        self.texts.set_bind_group_state(bg);
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
        self.record_pending_mesh_command();
        self.add_texture_segment(self.bind_group.clone());
        self.add_instance_texture_segment(self.bind_group.clone());
        self.add_geo_instance_texture_segment(self.bind_group.clone());
        self.advance_shape_texture_generation();
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

    fn add_geo_instance_texture_segment(&mut self, bg: Option<wgpu::BindGroup>) {
        let start = self
            .geo_instance_texture_segments
            .last()
            .map_or(0, |s| s.instance_start + s.instance_count);
        let end = self.geo_instances.len() as u32;
        if end > start {
            self.geo_instance_texture_segments.push(InstanceTextureSegment {
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
        // fragment-only material（无 custom VS）可走 instance path。
        if self
            .custom_material
            .as_ref()
            .map(|m| m.has_custom_vertex_shader())
            .unwrap_or(false)
        {
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
        let instance_start = self.instances.len() as u32;
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
        self.record_instance_command(instance_start);
        self.note_sdf();
        true
    }

    /// Add an instanced SDF rectangle.
    ///
    /// Geometry mode and custom materials fall back to the ordinary path.
    /// Mixed instance and mesh calls retain their original order.
    pub fn instance_rectangle(
        &mut self,
        pos: Pos,
        w: f32,
        h: f32,
        color: Option<crate::color::Color>,
    ) {
        let feather = self.sdf_feather.unwrap_or(0.0);
        if !self.push_sdf_instance(pos, [-feather, -feather, w + feather, h + feather], [0.0, 0.0, w, h], [w * 0.5, h * 0.5, w * 0.5, h * 0.5], 2, [0.0, 0.0], color) {
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
        if r == 0.0 {
            return;
        }
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
        if rx == 0.0 || ry == 0.0 {
            return;
        }
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
        if w == 0.0 || h == 0.0 {
            return;
        }
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
        if thickness == 0.0 {
            return;
        }
        if (x2 - x1).abs() + (y2 - y1).abs() < 0.001 {
            let saved = self.transform;
            self.transform = Some(match saved {
                Some(transform) => transform.then(&Transform::translation(x1, y1)),
                None => Transform::translation(x1, y1),
            });
            self.invalidate_transform_cache();
            self.instance_circle(Pos::ZERO, thickness * 0.5, color);
            self.transform = saved;
            self.invalidate_transform_cache();
            return;
        }
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
        let abx = x2 - x1;
        let aby = y2 - y1;
        let bcx = x3 - x2;
        let bcy = y3 - y2;
        let cax = x1 - x3;
        let cay = y1 - y3;
        if abx * abx + aby * aby < 0.000001
            || bcx * bcx + bcy * bcy < 0.000001
            || cax * cax + cay * cay < 0.000001
            || (abx * bcy - aby * bcx).abs() < 0.0001
        {
            return;
        }
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
        if r == 0.0 || (end_angle - start_angle).abs() < 0.001 {
            return;
        }
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
        if self.sdf_feather.is_none()
            || self
                .custom_material
                .as_ref()
                .map(|m| m.has_custom_vertex_shader())
                .unwrap_or(false)
            || points.len() < 3
        {
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
        for &a in points {
            min_x = min_x.min(a.0);
            min_y = min_y.min(a.1);
            max_x = max_x.max(a.0);
            max_y = max_y.max(a.1);
        }
        let Some((start, count)) = self.intern_polygon_edges(points) else {
            return;
        };
        let feather = self.sdf_feather.unwrap();
        if !self.push_sdf_instance(
            Pos::ZERO,
            [min_x - feather, min_y - feather, max_x + feather, max_y + feather],
            [min_x, min_y, max_x, max_y],
            [start as f32, count as f32, 0.0, 0.0],
            6,
            [0.0, 0.0],
            color,
        ) {
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
        if self.sdf_feather.is_none()
            || self
                .custom_material
                .as_ref()
                .map(|m| m.has_custom_vertex_shader())
                .unwrap_or(false)
            || points.len() < 2
            || thickness == 0.0
        {
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
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for i in 0..vertex_count {
            min_x = min_x.min(points[i].0);
            min_y = min_y.min(points[i].1);
            max_x = max_x.max(points[i].0);
            max_y = max_y.max(points[i].1);
        }
        let Some((start, count)) = self.intern_line_chain_edges(points) else { return; };
        let half = thickness * 0.5;
        let feather = self.sdf_feather.unwrap();
        if !self.push_sdf_instance(
            Pos::ZERO,
            [min_x - half - feather, min_y - half - feather, max_x + half + feather, max_y + half + feather],
            [min_x - half, min_y - half, max_x + half, max_y + half],
            [start as f32, count as f32, half, 0.0],
            7,
            [0.0, 0.0],
            color,
        ) {
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
        self.record_pending_mesh_command();
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
            self.advance_shape_texture_generation();
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
            self.advance_shape_texture_generation();
        }
        self.bind_group = saved_bg;
        self.transform = saved_transform;
        self.cached_transform_index = saved_cache;
        self.uv = saved_uv;
        self.sdf_feather = saved_feather;
        self.color = saved_color;
    }

    /// 几何模板实例化路径（`draw_shape` 几何模式热路径）。
    ///
    /// 与 [`Self::instance_shape`]（SDF 实例路径）平行的几何版本：位置进 transform，
    /// 几何本体（模板）只生成一次并复用，实例只携带 color/transform。
    /// 纹理/材质处理语义与 instance_shape 一致。
    pub(crate) fn geo_instance_shape(
        &mut self,
        shape: &crate::shapes::Shape<'_>,
        opts: crate::shapes::ShapeOverride,
    ) {
        self.record_pending_mesh_command();
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
            self.add_geo_instance_texture_segment(self.bind_group.clone());
            self.advance_shape_texture_generation();
            self.bind_group = bind_group;
        }

        let color = self.color;
        match shape {
            crate::shapes::Shape::Rect { w, h, .. } => crate::shapes::geo_emit_rectangle(self, *w, *h, color),
            crate::shapes::Shape::RoundedRect { w, h, radius, .. } => crate::shapes::geo_emit_rounded_rect(self, *w, *h, *radius, color),
            crate::shapes::Shape::Circle { r, .. } => crate::shapes::geo_emit_circle(self, *r, color),
            crate::shapes::Shape::Ellipse { rx, ry, .. } => crate::shapes::geo_emit_ellipse(self, *rx, *ry, color),
            crate::shapes::Shape::Line { x1, y1, x2, y2, thickness } => crate::shapes::geo_emit_line(self, *x1, *y1, *x2, *y2, *thickness, color),
            crate::shapes::Shape::LineChain { points, thickness } => crate::shapes::geo_emit_line_chain(self, points, *thickness, color),
            crate::shapes::Shape::Triangle { x1, y1, x2, y2, x3, y3 } => crate::shapes::geo_emit_triangle(self, *x1, *y1, *x2, *y2, *x3, *y3, color),
            crate::shapes::Shape::Polygon { points } => crate::shapes::geo_emit_polygon(self, points, color),
            crate::shapes::Shape::Arc { r, start, end, .. } => crate::shapes::geo_emit_arc(self, *r, *start, *end, color),
            crate::shapes::Shape::RectOutline { w, h, thickness, .. } => crate::shapes::geo_emit_rect_outline(self, *w, *h, *thickness, color),
            crate::shapes::Shape::CircleOutline { r, thickness, segments, .. } => crate::shapes::geo_emit_circle_outline(self, *r, *thickness, color, *segments),
            crate::shapes::Shape::EllipseOutline { rx, ry, thickness, segments, .. } => crate::shapes::geo_emit_ellipse_outline(self, *rx, *ry, *thickness, color, *segments),
            crate::shapes::Shape::RoundedRectOutline { w, h, radius, thickness, corner_segments, .. } => crate::shapes::geo_emit_rounded_rect_outline(self, *w, *h, *radius, *thickness, color, *corner_segments),
            crate::shapes::Shape::TriangleOutline { x1, y1, x2, y2, x3, y3, thickness } => crate::shapes::geo_emit_triangle_outline(self, *x1, *y1, *x2, *y2, *x3, *y3, *thickness, color),
            crate::shapes::Shape::PolygonOutline { points, thickness } => crate::shapes::geo_emit_polygon_outline(self, points, *thickness, color),
            crate::shapes::Shape::ArcOutline { r, start, end, thickness, segments, .. } => crate::shapes::geo_emit_arc_outline(self, *r, *start, *end, *thickness, color, *segments),
        }

        if texture_overridden {
            self.add_texture_segment(self.bind_group.clone());
            self.add_geo_instance_texture_segment(self.bind_group.clone());
            self.advance_shape_texture_generation();
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