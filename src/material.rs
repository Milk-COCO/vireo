use std::cell::{Cell, RefCell};
use std::sync::Arc;

use rustc_hash::FxHashMap;

/// group 3 纹理槽数量（tex0..tex3）。保留兼容旧 BGL layout 引用。
pub const MATERIAL_TEX_SLOTS: usize = 4;

/// 整体 uniform 上限（兼容旧的 1024B 常量）。
pub const MATERIAL_UNIFORM_SIZE: usize = 1024;

// ---------------------------------------------------------------------------
// WGSL 内建片段库（#include "vireo_*.wgsl"）
// ---------------------------------------------------------------------------

/// 可用片段名 → WGSL 源码。
pub fn wgsl_snippets() -> FxHashMap<&'static str, &'static str> {
    let mut m = FxHashMap::default();
    m.reserve(3);
    m.insert("vireo_color.wgsl", r#"
fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0/3.0, 1.0/3.0);
    let p = abs(fract(vec3<f32>(h) + k) * 6.0 - 3.0);
    return v * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}
fn hsl2rgb(h: f32, s: f32, l: f32) -> vec3<f32> {
    let rgb = hsv2rgb(h, s, l - 0.5 * s * (1.0 - abs(2.0 * l - 1.0)) / max(1.0 - abs(2.0 * l - 1.0), 0.001));
    return rgb;
}
"#);
    m.insert("vireo_noise.wgsl", r#"
fn hash21(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}
fn smooth_noise(p: vec2<f32>) -> f32 {
    let i = floor(p); let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash21(i + vec2<f32>(0.0,0.0)), hash21(i + vec2<f32>(1.0,0.0)), u.x),
               mix(hash21(i + vec2<f32>(0.0,1.0)), hash21(i + vec2<f32>(1.0,1.0)), u.x), u.y);
}
"#);
    m.insert("vireo_sdf_helper.wgsl", r#"
fn sdf_circle(p: vec2<f32>, r: f32) -> f32 { return length(p) - r; }
fn sdf_box(p: vec2<f32>, b: vec2<f32>) -> f32 {
    let d = abs(p) - b;
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}
fn sdf_rounded_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let d = abs(p) - b + vec2<f32>(r);
    return min(max(d.x, d.y), 0.0) + length(max(d, vec2<f32>(0.0))) - r;
}
"#);
    m
}

/// 展开用户 WGSL 中的 `// #include "vireo_*.wgsl"` 为内建片段源码。
/// 仅支持一行 `// #include "name"` 格式，不支持嵌套 include。
pub fn expand_includes(source: &str) -> Result<String, String> {
    let snippets = wgsl_snippets();
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("// #include ") {
            let name = rest.trim().trim_matches('"');
            if let Some(snippet) = snippets.get(name) {
                out.push_str(snippet);
                out.push('\n');
            } else {
                return Err(format!("unknown WGSL snippet: {name}"));
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// BindGroupPool
// ---------------------------------------------------------------------------

/// 跨材质 bind group 复用池。
/// key = (BGL 指针 as u64, 按 binding 排序的 slot 指纹列表)。
pub(crate) struct BindGroupPool {
    inner: RefCell<FxHashMap<(u64, Vec<u64>), wgpu::BindGroup>>,
}

impl BindGroupPool {
    pub(crate) fn new() -> Self {
        Self { inner: RefCell::new(FxHashMap::default()) }
    }

    pub(crate) fn resolve(
        &self,
        bgl: &wgpu::BindGroupLayout,
        slots: &FxHashMap<String, ResourceSlot>,
        build: impl FnOnce() -> wgpu::BindGroup,
    ) -> wgpu::BindGroup {
        let bgl_id = std::ptr::from_ref(bgl) as u64;
        let mut fps: Vec<(u32, u64)> = slots.values()
            .map(|s| (s.binding, slot_fingerprint(&s.kind)))
            .collect();
        fps.sort_by_key(|&(b, _)| b);
        let fps: Vec<u64> = fps.into_iter().map(|(_, f)| f).collect();
        let key = (bgl_id, fps);

        if let Some(bg) = self.inner.borrow().get(&key) {
            return bg.clone();
        }
        let bg = build();
        self.inner.borrow_mut().insert(key, bg.clone());
        bg
    }

    #[allow(dead_code)]
    pub fn clear(&self) {
        self.inner.borrow_mut().clear();
    }
}

fn slot_fingerprint(kind: &SlotKind) -> u64 {
    match kind {
        SlotKind::Uniform { buffer, .. } | SlotKind::Storage { buffer, .. } => {
            let p = std::ptr::from_ref(buffer) as u64;
            p.wrapping_mul(6364136223846793005)
        }
        SlotKind::Texture { view, sampler, .. } => {
            let vp = std::ptr::from_ref(view) as u64;
            let sp = std::ptr::from_ref(sampler) as u64;
            vp.wrapping_mul(6364136223846793005).wrapping_add(sp)
        }
    }
}

// ---------------------------------------------------------------------------
// Descriptor types (public)
// ---------------------------------------------------------------------------

/// 纹理采样类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexSample {
    Float,
    Unfilterable,
    Sint,
    Uint,
}

/// 纹理维度/类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexKind {
    D2(TexSample),
    Cube(TexSample),
    D2Array(TexSample),
    D3(TexSample),
    D2Depth,
    D2DepthArray,
    D2Multi(TexSample),
}

/// 采样器类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampKind {
    Filtering,
    NonFiltering,
    Comparison,
}

/// 单个资源描述符。
#[derive(Clone, Debug)]
pub struct MaterialResource<'a> {
    pub name: &'a str,
    pub kind: MaterialResourceKind<'a>,
}

/// 资源类型：纹理、Storage、Uniform。
///
/// - `type_name`：WGSL 结构体名（如 `"Pulse"`，对应 `var<storage> u_pulse: Pulse`），必填。
/// - `dynamic`：启用动态偏移（通过 `DrawBatch.dynamic_offsets` 逐 draw 切换 buffer 槽）。
/// - `size`/`min_size`：buffer 最小容量（wgpu 要求），单位字节。
#[derive(Clone, Debug)]
pub enum MaterialResourceKind<'a> {
    Texture { view: TexKind, sampler: SampKind },
    Storage { read_only: bool, size: u64, type_name: &'a str, dynamic: bool },
    Uniform { min_size: u64, type_name: &'a str, dynamic: bool },
}

/// 描述符切片。
#[derive(Clone, Copy)]
pub struct MaterialResources<'a>(pub &'a [MaterialResource<'a>]);

// ---------------------------------------------------------------------------
// Cache policy (public)
// ---------------------------------------------------------------------------

/// Bind group 缓存策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachePolicy {
    /// 每帧重建 bind group。
    AlwaysRebuild,
    /// 资源指纹变化时才重建（默认）。
    Dirty,
}

// ---------------------------------------------------------------------------
// Internal resource slot
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct ResourceSlot {
    pub(crate) binding: u32,
    pub(crate) kind: SlotKind,
}

#[derive(Debug)]
pub(crate) enum SlotKind {
    Texture {
        view: wgpu::TextureView,
        sampler: wgpu::Sampler,
        #[allow(dead_code)]
        tex_kind: TexKind,
    },
    Uniform {
        buffer: wgpu::Buffer,
        min_size: u64,
        #[allow(dead_code)]
        dynamic: bool,
        #[allow(dead_code)]
        min_bind: Option<wgpu::BufferSize>,
    },
    Storage {
        buffer: wgpu::Buffer,
        #[allow(dead_code)]
        read_only: bool,
        #[allow(dead_code)]
        dynamic: bool,
        min_bind: Option<wgpu::BufferSize>,
    },
}

// ---------------------------------------------------------------------------
// MaterialState (internal)
// ---------------------------------------------------------------------------

pub(crate) enum MaterialState {
    ZeroResource,
    A {
        bgl: wgpu::BindGroupLayout,
        slots: RefCell<FxHashMap<String, ResourceSlot>>,
        cache_policy: CachePolicy,
        bind_group: RefCell<wgpu::BindGroup>,
        #[allow(dead_code)]
        fingerprints: RefCell<FxHashMap<String, u64>>,
        dirty: Cell<bool>,
        device: wgpu::Device,
    },
    B {
        bgl: wgpu::BindGroupLayout,
        provider: RefCell<Option<Box<dyn FnMut(&wgpu::Device, &wgpu::Queue) -> wgpu::BindGroup + Send>>>,
        bind_group: RefCell<Option<wgpu::BindGroup>>,
    },
}

// ---------------------------------------------------------------------------
// Material
// ---------------------------------------------------------------------------

/// 运行时自定义 fragment shader 材质。
///
/// 通过 `App` 创建，**同一 Material 可同时用于形状、文字**——
/// 引擎按目标自动选对应顶点布局与 pipeline。
///
/// | 创建方法 | group 3 资源 |
/// |---------|-------------|
/// | [`App::material`] | 无 group 3 |
/// | [`App::material_with_resources`] | 描述符驱动：引擎自动建 BGL + 注入 WGSL + 名字 API |
/// | [`App::material_manual`] | 用户自管 BGL（见 `custom_material_manual` 示例） |
///
/// 前两者是 99% 场景的入口。`material_manual` 仅当需要自定义 BGL layout
///（第三方 shader / 已有 wgpu 代码）时用。
///
/// `material_with_resources` 用户**不要**在自己 WGSL 里写 `@group(3)` ——
/// 引擎按描述符自动注入。
///
/// # WGSL 入口
///
/// ```wgsl
/// fn material_main(in: MaterialInput) -> vec4<f32> {
///     let base = vireo_base_sample(in.base_uv);
///     return vec4<f32>(base.rgb, in.color.a);
/// }
/// ```
pub struct Material {
    pub(crate) state: MaterialState,
    pub(crate) source: String,
    pub(crate) shape_vertex_source: Option<String>,
    /// 统一 pipeline 缓存：key = (Target, sample_count, atc, ssaa, stencil_op?)
    pub(crate) pipelines: RefCell<FxHashMap<u64, Arc<wgpu::RenderPipeline>>>,
}

impl Material {
    pub(crate) fn new_zero_resource(
        source: String,
        shape_vertex_source: Option<String>,
        pipelines: FxHashMap<u64, Arc<wgpu::RenderPipeline>>,
    ) -> Self {
        Self {
            state: MaterialState::ZeroResource,
            source,
            shape_vertex_source,
            pipelines: RefCell::new(pipelines),
        }
    }

    pub(crate) fn new_a(
        bgl: wgpu::BindGroupLayout,
        slots: FxHashMap<String, ResourceSlot>,
        init_bind_group: wgpu::BindGroup,
        cache_policy: CachePolicy,
        source: String,
        shape_vertex_source: Option<String>,
        pipelines: FxHashMap<u64, Arc<wgpu::RenderPipeline>>,
        device: wgpu::Device,
    ) -> Self {
        let mut fingerprints = FxHashMap::default();
        fingerprints.reserve(slots.len());
        Self {
            state: MaterialState::A {
                bgl,
                slots: RefCell::new(slots),
                cache_policy,
                bind_group: RefCell::new(init_bind_group),
                fingerprints: RefCell::new(fingerprints),
                dirty: Cell::new(false),
                device,
            },
            source,
            shape_vertex_source,
            pipelines: RefCell::new(pipelines),
        }
    }

    pub(crate) fn new_b(
        bgl: wgpu::BindGroupLayout,
        init_bind_group: Option<wgpu::BindGroup>,
        source: String,
        shape_vertex_source: Option<String>,
        pipelines: FxHashMap<u64, Arc<wgpu::RenderPipeline>>,
    ) -> Self {
        Self {
            state: MaterialState::B {
                bgl,
                provider: RefCell::new(None),
                bind_group: RefCell::new(init_bind_group),
            },
            source,
            shape_vertex_source,
            pipelines: RefCell::new(pipelines),
        }
    }

    /// group 3 bind group layout（0 资源材质为 `None`）。
    pub(crate) fn bgl(&self) -> Option<&wgpu::BindGroupLayout> {
        match &self.state {
            MaterialState::ZeroResource => None,
            MaterialState::A { bgl, .. } => Some(bgl),
            MaterialState::B { bgl, .. } => Some(bgl),
        }
    }

    /// 是否为 0 资源材质（无 group 3）。
    #[allow(dead_code)]
    pub(crate) fn is_zero_resource(&self) -> bool {
        matches!(self.state, MaterialState::ZeroResource)
    }

    /// 确保 bind group 就绪（Dirty 检测/手动 provider），返回当前 bind group。
    /// 0 资源材质返回 `None`。
    pub(crate) fn ensure_bind_group(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pool: &BindGroupPool,
    ) -> Option<wgpu::BindGroup> {
        match &self.state {
            MaterialState::ZeroResource => None,
            MaterialState::A {
                bgl,
                slots,
                cache_policy,
                bind_group,
                dirty,
                ..
            } => {
                if *cache_policy == CachePolicy::AlwaysRebuild || dirty.get() {
                    let slots_ref = slots.borrow();
                    let new_bg = pool.resolve(bgl, &slots_ref, || {
                        build_bind_group_from_slots(device, bgl, &slots_ref)
                    });
                    *bind_group.borrow_mut() = new_bg;
                    dirty.set(false);
                }
                Some(bind_group.borrow().clone())
            }
            MaterialState::B {
                provider,
                bind_group,
                ..
            } => {
                if let Some(ref mut p) = *provider.borrow_mut() {
                    let new_bg = p(device, queue);
                    *bind_group.borrow_mut() = Some(new_bg);
                }
                bind_group.borrow().clone()
            }
        }
    }

    // -----------------------------------------------------------------------
    // Name-based set_* API (material_with_resources only)
    // -----------------------------------------------------------------------

    fn a_state(&self) -> (&RefCell<FxHashMap<String, ResourceSlot>>, &Cell<bool>) {
        match &self.state {
            MaterialState::A { slots, dirty, .. } => (slots, dirty),
            _ => panic!(
                "this material has no resource descriptors; \
                 create with material_with_resources() for name-based set_* API"
            ),
        }
    }

    /// 写入 uniform buffer（按名字查找）。
    pub fn set_uniform_bytes(&self, queue: &wgpu::Queue, name: &str, data: &[u8]) {
        match &self.state {
            MaterialState::A { slots, dirty, device, .. } => {
                let mut slots = slots.borrow_mut();
                let slot = slots.get_mut(name).expect("set_uniform_bytes: unknown resource name");
                match &mut slot.kind {
                    SlotKind::Uniform { buffer, min_size, dynamic: is_dynamic, .. } => {
                        if *is_dynamic && data.len() as u64 > buffer.size() {
                            *buffer = create_dynamic_uniform_buffer(device, data, wgpu::BufferUsages::UNIFORM);
                        } else if !*is_dynamic {
                            assert!(
                                data.len() as u64 <= *min_size,
                                "uniform '{}': data {} bytes exceeds min_size {}",
                                name, data.len(), min_size
                            );
                        }
                        queue.write_buffer(buffer, 0, data);
                    }
                    SlotKind::Storage { buffer, dynamic: is_dynamic, .. } => {
                        if *is_dynamic && data.len() as u64 > buffer.size() {
                            *buffer = create_dynamic_uniform_buffer(device, data, wgpu::BufferUsages::STORAGE);
                        } else if !*is_dynamic {
                            assert!(
                                data.len() as u64 <= buffer.size(),
                                "storage '{}': data {} bytes exceeds buffer size {}",
                                name, data.len(), buffer.size()
                            );
                        }
                        queue.write_buffer(buffer, 0, data);
                    }
                    _ => panic!("set_uniform_bytes: '{}' is not a Uniform or Storage resource", name),
                }
                dirty.set(true);
            }
            _ => panic!(
                "this material has no resource descriptors; \
                 create with material_with_resources() for name-based set_* API"
            ),
        }
    }

    /// 写入 bytemuck Pod 到 uniform 或 storage（按名字查找）。
    pub fn set_uniform<T: bytemuck::Pod>(&self, queue: &wgpu::Queue, name: &str, data: &T) {
        self.set_uniform_bytes(queue, name, bytemuck::cast_slice(std::slice::from_ref(data)));
    }

    /// 写入 bytemuck Pod 到 storage buffer。
    pub fn set_storage<T: bytemuck::Pod>(&self, queue: &wgpu::Queue, name: &str, data: &T) {
        self.set_uniform_bytes(queue, name, bytemuck::cast_slice(std::slice::from_ref(data)));
    }

    /// 设置纹理 + sampler（按名字查找）。
    pub fn set_texture(
        &self,
        _device: &wgpu::Device,
        name: &str,
        view: &wgpu::TextureView,
        samp: &wgpu::Sampler,
    ) {
        let (slots, dirty) = self.a_state();
        let mut slots_ref = slots.borrow_mut();
        let slot = slots_ref.get_mut(name).expect("set_texture: unknown resource name");
        match &mut slot.kind {
            SlotKind::Texture { view: tex_view, sampler: samp_ref, .. } => {
                *tex_view = view.clone();
                *samp_ref = samp.clone();
            }
            _ => panic!("set_texture: '{}' is not a Texture resource", name),
        }
        dirty.set(true);
    }

    /// 设置 sampler（按名字查找）。保留原纹理不变。
    pub fn set_sampler(&self, _device: &wgpu::Device, name: &str, samp: &wgpu::Sampler) {
        let (slots, dirty) = self.a_state();
        let mut slots_ref = slots.borrow_mut();
        let slot = slots_ref.get_mut(name).expect("set_sampler: unknown resource name");
        match &mut slot.kind {
            SlotKind::Texture { sampler: samp_ref, .. } => {
                *samp_ref = samp.clone();
            }
            _ => panic!("set_sampler: '{}' is not a Texture resource", name),
        }
        dirty.set(true);
    }

    // -----------------------------------------------------------------------
    // Advanced: bind group provider (material_manual only)
    // -----------------------------------------------------------------------

    /// 安装自定义 bind group provider（`material_manual` 创建者必须调用）。
    /// 每帧渲染前调用，返回当前帧的 bind group。
    pub fn set_bind_group_provider<F>(&self, f: F)
    where
        F: FnMut(&wgpu::Device, &wgpu::Queue) -> wgpu::BindGroup + Send + 'static,
    {
        match &self.state {
            MaterialState::B { provider, .. } => {
                *provider.borrow_mut() = Some(Box::new(f));
            }
            MaterialState::ZeroResource => panic!(
                "set_bind_group_provider: this material has no group 3; \
                 use material_manual() to create a material with custom bind groups"
            ),
            MaterialState::A { .. } => {
                panic!(
                    "set_bind_group_provider is for material_manual only; \
                     name-based materials use set_uniform / set_texture / set_storage"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public API: Build BGL from descriptors
// ---------------------------------------------------------------------------

/// 为动态 buffer 创建新 buffer（容量足够容纳 data），并写入初始数据。
fn create_dynamic_uniform_buffer(
    device: &wgpu::Device,
    data: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("material dynamic buffer (resized)"),
        size: data.len() as u64,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // 数据在别处写入（set_uniform_bytes 已持有 queue）。
    // 调用者负责在创建后 write_buffer。
    buf
}

/// 根据描述符构建 group 3 bind group layout。
/// 返回 `None` 表示 0 资源（空的 MaterialResources）。
/// binding 序号按描述符顺序从 0 开始；texture 占 2 个 binding（texture + sampler）。
pub fn build_bgl_from_resources(
    device: &wgpu::Device,
    resources: &[MaterialResource<'_>],
) -> Result<Option<wgpu::BindGroupLayout>, String> {
    if resources.is_empty() {
        return Ok(None);
    }

    let mut entries: Vec<wgpu::BindGroupLayoutEntry> = Vec::with_capacity(resources.len() * 2);
    let mut binding: u32 = 0;

    for res in resources {
        match &res.kind {
            MaterialResourceKind::Texture { view, sampler } => {
                validate_tex_samp_combo(*view, *sampler)
                    .map_err(|e| format!("resource '{}': {}", res.name, e))?;
                let tex_visibility = wgpu::ShaderStages::FRAGMENT;
                match view {
                    TexKind::D2(sample) | TexKind::D2Array(sample) | TexKind::D3(sample) => {
                        entries.push(wgpu::BindGroupLayoutEntry {
                            binding,
                            visibility: tex_visibility,
                            ty: wgpu::BindingType::Texture {
                                sample_type: tex_sample_to_wgpu(*sample),
                                view_dimension: tex_kind_to_dimension(*view),
                                multisampled: false,
                            },
                            count: None,
                        });
                        binding += 1;
                    }
                    TexKind::Cube(sample) => {
                        entries.push(wgpu::BindGroupLayoutEntry {
                            binding,
                            visibility: tex_visibility,
                            ty: wgpu::BindingType::Texture {
                                sample_type: tex_sample_to_wgpu(*sample),
                                view_dimension: wgpu::TextureViewDimension::Cube,
                                multisampled: false,
                            },
                            count: None,
                        });
                        binding += 1;
                    }
                    TexKind::D2Depth | TexKind::D2DepthArray => {
                        entries.push(wgpu::BindGroupLayoutEntry {
                            binding,
                            visibility: tex_visibility,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Depth,
                                view_dimension: match view {
                                    TexKind::D2Depth => wgpu::TextureViewDimension::D2,
                                    TexKind::D2DepthArray => wgpu::TextureViewDimension::D2Array,
                                    _ => unreachable!(),
                                },
                                multisampled: false,
                            },
                            count: None,
                        });
                        binding += 1;
                    }
                    TexKind::D2Multi(sample) => {
                        entries.push(wgpu::BindGroupLayoutEntry {
                            binding,
                            visibility: tex_visibility,
                            ty: wgpu::BindingType::Texture {
                                sample_type: tex_sample_to_wgpu(*sample),
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: true,
                            },
                            count: None,
                        });
                        binding += 1;
                    }
                }
                // sampler entry
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(samp_kind_to_wgpu(*sampler)),
                    count: None,
                });
                binding += 1;
            }
            MaterialResourceKind::Storage { read_only, size, dynamic, .. } => {
                let min_size = wgpu::BufferSize::new(*size as u64);
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage {
                            read_only: *read_only,
                        },
                        has_dynamic_offset: *dynamic,
                        min_binding_size: if *dynamic { min_size } else { None },
                    },
                    count: None,
                });
                binding += 1;
            }
            MaterialResourceKind::Uniform { min_size, dynamic, .. } => {
                let min_bind = wgpu::BufferSize::new(*min_size as u64);
                entries.push(wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: *dynamic,
                        min_binding_size: if *dynamic { min_bind } else { None },
                    },
                    count: None,
                });
                binding += 1;
            }
        }
    }

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("material resources bgl"),
        entries: &entries,
    });
    Ok(Some(bgl))
}

/// 运行时校验纹理/采样器组合合法性。
pub fn validate_tex_samp_combo(view: TexKind, sampler: SampKind) -> Result<(), String> {
    let sample = match view {
        TexKind::D2(s) | TexKind::Cube(s) | TexKind::D2Array(s) | TexKind::D3(s) | TexKind::D2Multi(s) => Some(s),
        TexKind::D2Depth | TexKind::D2DepthArray => None,
    };
    let is_depth = matches!(view, TexKind::D2Depth | TexKind::D2DepthArray);
    let is_msaa = matches!(view, TexKind::D2Multi(_));

    match (sampler, sample, is_depth, is_msaa) {
        (SampKind::Filtering, _, _, true) => {
            return Err("D2Multi + Filtering: MSAA textures require NonFiltering sampler".into());
        }
        (SampKind::Comparison, _, _, true) => {
            return Err("D2Multi + Comparison: MSAA textures require NonFiltering sampler".into());
        }
        (SampKind::Filtering, Some(TexSample::Sint), _, _)
        | (SampKind::Filtering, Some(TexSample::Uint), _, _) => {
            return Err("integer texture + Filtering: integer textures cannot use filtering sampler".into());
        }
        (SampKind::Comparison, Some(TexSample::Sint), _, _)
        | (SampKind::Comparison, Some(TexSample::Uint), _, _) => {
            return Err("integer texture + Comparison: integer textures cannot use comparison sampler".into());
        }
        (SampKind::Filtering, Some(TexSample::Unfilterable), _, _) => {
            return Err("unfilterable texture + Filtering: use NonFiltering sampler instead".into());
        }
        (SampKind::Comparison, Some(TexSample::Unfilterable), _, _) => {
            return Err("unfilterable texture + Comparison: use NonFiltering sampler instead".into());
        }
        (SampKind::Comparison, _, false, _) => {
            return Err("Comparison sampler on non-depth texture: only D2Depth/D2DepthArray support Comparison".into());
        }
        (SampKind::Filtering, _, true, _) if !is_msaa => {
            return Err("Filtering sampler on depth texture: use Comparison or NonFiltering sampler".into());
        }
        (SampKind::NonFiltering, _, _, false) | (SampKind::Filtering, _, _, false) | (SampKind::Comparison, _, true, false) => {},
        (SampKind::NonFiltering, _, _, true) => {},
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// WGSL injection (append at end)
// ---------------------------------------------------------------------------

/// 将 `@group(3) @binding(N)` 声明追加到用户 WGSL 末尾。
pub fn inject_wgsl_resources(source: &str, resources: &[MaterialResource<'_>]) -> String {
    if resources.is_empty() {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len() + resources.len() * 128);
    out.push_str(source);
    out.push('\n');

    let mut binding: u32 = 0;
    for res in resources {
        let name = res.name;
        match &res.kind {
            MaterialResourceKind::Texture { view, sampler: _ } => {
                let tex_type = tex_kind_to_wgsl(*view);
                out.push_str(&format!(
                    "@group(3) @binding({}) var {}: {};\n",
                    binding, name, tex_type
                ));
                binding += 1;
                let samp_type = samp_kind_to_wgsl_wgsl(*view, res.kind.sampler());
                out.push_str(&format!(
                    "@group(3) @binding({}) var {}_samp: {};\n",
                    binding, name, samp_type
                ));
                binding += 1;
            }
            MaterialResourceKind::Storage { read_only, type_name, .. } => {
                let access = if *read_only { "" } else { ", read_write" };
                out.push_str(&format!(
                    "@group(3) @binding({}) var<storage{}> {}: {};\n",
                    binding, access, name, type_name
                ));
                binding += 1;
            }
            MaterialResourceKind::Uniform { type_name, .. } => {
                out.push_str(&format!(
                    "@group(3) @binding({}) var<uniform> {}: {};\n",
                    binding, name, type_name
                ));
                binding += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// AutoDefaults: build initial slots + init bind group
// ---------------------------------------------------------------------------

/// 构建初始 slot 表 + 初始 bind group（`material_with_resources` 内部使用）。
pub(crate) fn build_auto_defaults(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    resources: &[MaterialResource<'_>],
    white_view: &wgpu::TextureView,
    filtering_sampler: &wgpu::Sampler,
    non_filtering_sampler: &wgpu::Sampler,
    comparison_sampler: &wgpu::Sampler,
) -> (FxHashMap<String, ResourceSlot>, wgpu::BindGroup) {
    let mut slots = FxHashMap::default();
    slots.reserve(resources.len());

    let mut binding: u32 = 0;

    for res in resources {
        let name = res.name.to_owned();
        match &res.kind {
            MaterialResourceKind::Texture { view, sampler } => {
                let tex_kind = *view;
                let samp_kind = *sampler;

                let has_default = matches!(tex_kind, TexKind::D2(TexSample::Float))
                    && matches!(samp_kind, SampKind::Filtering);

                let (tex_view, samp) = if has_default {
                    (white_view.clone(), filtering_sampler.clone())
                } else {
                    let samp = match samp_kind {
                        SampKind::Filtering => filtering_sampler.clone(),
                        SampKind::NonFiltering => non_filtering_sampler.clone(),
                        SampKind::Comparison => comparison_sampler.clone(),
                    };
                    (white_view.clone(), samp)
                };

                let tex_binding = binding;
                binding += 1;
                let _samp_binding = binding;
                binding += 1;

                slots.insert(
                    name,
                    ResourceSlot {
                        binding: tex_binding,
                        kind: SlotKind::Texture { view: tex_view, sampler: samp, tex_kind },
                    },
                );
            }
            MaterialResourceKind::Storage { read_only, size, dynamic, .. } => {
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("material storage '{}'", res.name)),
                    size: *size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                slots.insert(
                    name,
                    ResourceSlot {
                        binding,
                        kind: SlotKind::Storage {
                            buffer,
                            read_only: *read_only,
                            dynamic: *dynamic,
                            min_bind: if *dynamic { wgpu::BufferSize::new(*size) } else { None },
                        },
                    },
                );
                binding += 1;
            }
            MaterialResourceKind::Uniform { min_size, dynamic, .. } => {
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("material uniform '{}'", res.name)),
                    size: *min_size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                slots.insert(
                    name,
                    ResourceSlot {
                        binding,
                        kind: SlotKind::Uniform {
                            buffer,
                            min_size: *min_size,
                            dynamic: *dynamic,
                            min_bind: if *dynamic { wgpu::BufferSize::new(*min_size) } else { None },
                        },
                    },
                );
                binding += 1;
            }
        }
    }

    let bg_entries: Vec<wgpu::BindGroupEntry> = slots
        .iter()
        .flat_map(|(_name, slot)| {
            let mut entries: Vec<wgpu::BindGroupEntry> = Vec::with_capacity(2);
            match &slot.kind {
                SlotKind::Texture { view, sampler, .. } => {
                    entries.push(wgpu::BindGroupEntry {
                        binding: slot.binding,
                        resource: wgpu::BindingResource::TextureView(view),
                    });
                    entries.push(wgpu::BindGroupEntry {
                        binding: slot.binding + 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    });
                }
                SlotKind::Uniform { buffer, .. } => {
                    entries.push(wgpu::BindGroupEntry {
                        binding: slot.binding,
                        resource: buffer.as_entire_binding(),
                    });
                }
                SlotKind::Storage { buffer, .. } => {
                    entries.push(wgpu::BindGroupEntry {
                        binding: slot.binding,
                        resource: buffer.as_entire_binding(),
                    });
                }
            }
            entries
        })
        .collect();

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material auto-defaults bind group"),
        layout: bgl,
        entries: &bg_entries,
    });

    (slots, bind_group)
}

// 从 slots 重建 bind group
fn build_bind_group_from_slots(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    slots: &FxHashMap<String, ResourceSlot>,
) -> wgpu::BindGroup {
    let mut entries: Vec<(u32, wgpu::BindingResource)> = Vec::with_capacity(slots.len() * 2);
    for slot in slots.values() {
        match &slot.kind {
            SlotKind::Texture { view, sampler, .. } => {
                entries.push((slot.binding, wgpu::BindingResource::TextureView(view)));
                entries.push((slot.binding + 1, wgpu::BindingResource::Sampler(sampler)));
            }
            SlotKind::Uniform { buffer, min_bind, .. } => {
                if let Some(mb) = min_bind {
                    entries.push((slot.binding, wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer,
                        offset: 0,
                        size: Some(*mb),
                    })));
                } else {
                    entries.push((slot.binding, buffer.as_entire_binding()));
                }
            }
            SlotKind::Storage { buffer, min_bind, .. } => {
                if let Some(mb) = min_bind {
                    entries.push((slot.binding, wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer,
                        offset: 0,
                        size: Some(*mb),
                    })));
                } else {
                    entries.push((slot.binding, buffer.as_entire_binding()));
                }
            }
        }
    }
    entries.sort_by_key(|(b, _)| *b);
    let bind_entries: Vec<wgpu::BindGroupEntry> = entries
        .into_iter()
        .map(|(binding, resource)| wgpu::BindGroupEntry { binding, resource })
        .collect();

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material rebuilt bind group"),
        layout: bgl,
        entries: &bind_entries,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl MaterialResourceKind<'_> {
    fn sampler(&self) -> SampKind {
        match self {
            MaterialResourceKind::Texture { sampler, .. } => *sampler,
            _ => SampKind::Filtering,
        }
    }
}

fn tex_sample_to_wgpu(s: TexSample) -> wgpu::TextureSampleType {
    match s {
        TexSample::Float => wgpu::TextureSampleType::Float { filterable: true },
        TexSample::Unfilterable => wgpu::TextureSampleType::Float { filterable: false },
        TexSample::Sint => wgpu::TextureSampleType::Sint,
        TexSample::Uint => wgpu::TextureSampleType::Uint,
    }
}

fn tex_kind_to_dimension(k: TexKind) -> wgpu::TextureViewDimension {
    match k {
        TexKind::D2(_) | TexKind::D2Depth | TexKind::D2Multi(_) => wgpu::TextureViewDimension::D2,
        TexKind::Cube(_) => wgpu::TextureViewDimension::Cube,
        TexKind::D2Array(_) | TexKind::D2DepthArray => wgpu::TextureViewDimension::D2Array,
        TexKind::D3(_) => wgpu::TextureViewDimension::D3,
    }
}

fn samp_kind_to_wgpu(s: SampKind) -> wgpu::SamplerBindingType {
    match s {
        SampKind::Filtering => wgpu::SamplerBindingType::Filtering,
        SampKind::NonFiltering => wgpu::SamplerBindingType::NonFiltering,
        SampKind::Comparison => wgpu::SamplerBindingType::Comparison,
    }
}

fn tex_kind_to_wgsl(k: TexKind) -> &'static str {
    match k {
        TexKind::D2(TexSample::Float) | TexKind::D2(TexSample::Unfilterable) => "texture_2d<f32>",
        TexKind::D2(TexSample::Sint) => "texture_2d<i32>",
        TexKind::D2(TexSample::Uint) => "texture_2d<u32>",
        TexKind::Cube(TexSample::Float) | TexKind::Cube(TexSample::Unfilterable) => "texture_cube<f32>",
        TexKind::Cube(TexSample::Sint) => "texture_cube<i32>",
        TexKind::Cube(TexSample::Uint) => "texture_cube<u32>",
        TexKind::D2Array(TexSample::Float) | TexKind::D2Array(TexSample::Unfilterable) => "texture_2d_array<f32>",
        TexKind::D2Array(TexSample::Sint) => "texture_2d_array<i32>",
        TexKind::D2Array(TexSample::Uint) => "texture_2d_array<u32>",
        TexKind::D3(TexSample::Float) | TexKind::D3(TexSample::Unfilterable) => "texture_3d<f32>",
        TexKind::D3(TexSample::Sint) => "texture_3d<i32>",
        TexKind::D3(TexSample::Uint) => "texture_3d<u32>",
        TexKind::D2Depth => "texture_depth_2d",
        TexKind::D2DepthArray => "texture_depth_2d_array",
        TexKind::D2Multi(TexSample::Float) | TexKind::D2Multi(TexSample::Unfilterable) => "texture_multisampled_2d<f32>",
        TexKind::D2Multi(TexSample::Sint) => "texture_multisampled_2d<i32>",
        TexKind::D2Multi(TexSample::Uint) => "texture_multisampled_2d<u32>",
    }
}

fn samp_kind_to_wgsl_wgsl(view: TexKind, s: SampKind) -> &'static str {
    match (view, s) {
        (TexKind::D2Depth | TexKind::D2DepthArray, SampKind::Comparison) => "sampler_comparison",
        _ => "sampler",
    }
}
