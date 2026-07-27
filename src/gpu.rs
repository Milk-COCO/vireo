//! GPU 上下文和顶点定义。初始化时创建，多窗口共享。

use std::cell::RefCell;
use std::sync::Arc;

use rustc_hash::FxHashMap;
use wgpu::util::DeviceExt;

use crate::material::Material;
use crate::glyphon::ColorMode;
use crate::text::TextContext;

const SHAPE_VERTEX_OUTPUT_WGSL: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) sdf_params: vec4<f32>,
    @location(3) @interpolate(linear) local_pos: vec2<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
    @location(6) sdf_extra: vec2<f32>,
};
"#;

fn default_shape_vertex_wgsl(ssaa: bool) -> String {
    let source = include_str!("shader.wgsl");
    if ssaa {
        source.to_owned()
    } else {
        source.replace("@interpolate(linear, sample)", "@interpolate(linear)")
    }
}

/// 返回 (final_source, user_source_line_offset) 其中 offset 是用户代码起始行号（1-indexed）。
fn material_fragment_source(source: &str, target: MaterialTarget, ssaa: bool) -> (String, u32) {
    let line_count = |s: &str| s.split('\n').count() as u32;
    match target {
        MaterialTarget::Shape => {
            let vertex_out = if ssaa {
                SHAPE_VERTEX_OUTPUT_WGSL.replace(
                    "@interpolate(linear) local_pos",
                    "@interpolate(linear, sample) local_pos",
                )
            } else {
                SHAPE_VERTEX_OUTPUT_WGSL.to_owned()
            };
            let offset = line_count(&vertex_out) + line_count(MATERIAL_INPUT_WGSL) + line_count(SHAPE_FRAGMENT_SUPPORT_WGSL);
            (
                format!(
                    "{}\n{}\n{}\n{}\n{}",
                    vertex_out, MATERIAL_INPUT_WGSL, SHAPE_FRAGMENT_SUPPORT_WGSL,
                    source,
                    r#"
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(vireo_base_texture, vireo_base_sampler, in.uv) * in.color;
    let material_in = MaterialInput(
        in.uv, in.uv, vec4<f32>(base.rgb, 1.0), in.local_pos, in.sdf_params, in.sdf_extra,
        in.sdf_type, in.sdf_feather, 0u, 0u,
    );
    var out_color = material_main(material_in);
    out_color.a *= base.a;
    return vireo_apply_sdf(in, out_color);
}
"#
                ),
                offset,
            )
        }
        MaterialTarget::Text => {
            let text_support = r#"
@group(0) @binding(0) var vireo_color_atlas: texture_2d<f32>;
@group(0) @binding(1) var vireo_mask_atlas: texture_2d<f32>;
@group(0) @binding(2) var vireo_atlas_sampler: sampler;
@group(0) @binding(3) var vireo_base_texture: texture_2d<f32>;
@group(0) @binding(4) var vireo_base_sampler: sampler;

fn vireo_base_sample(uv: vec2<f32>) -> vec4<f32> {
    return textureSample(vireo_base_texture, vireo_base_sampler, uv);
}

fn vireo_base_color(in: MaterialInput) -> vec4<f32> {
    return in.color;
}

fn vireo_has_base_sample() -> bool { return true; }
fn vireo_has_local_pos() -> bool { return false; }
fn vireo_has_sdf_data() -> bool { return false; }
"#;
            let offset = line_count(TEXT_VERTEX_OUTPUT_WGSL)
                + line_count(MATERIAL_INPUT_WGSL)
                + line_count(text_support);
            (
                format!(
                    "{}\n{}\n{}{}",
                    TEXT_VERTEX_OUTPUT_WGSL,
                    MATERIAL_INPUT_WGSL,
                    text_support,
                    format!(
                        "{}\n{}",
                        source,
                        r#"
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var base: vec4<f32>;
    if in.content_type == 0u {
        base = textureSampleLevel(vireo_color_atlas, vireo_atlas_sampler, in.uv, 0.0);
    } else {
        let mask = textureSampleLevel(vireo_mask_atlas, vireo_atlas_sampler, in.uv, 0.0).x;
        base = vec4<f32>(in.color.rgb, in.color.a * mask);
    }
    let material_in = MaterialInput(
        in.uv, in.base_uv, vec4<f32>(base.rgb, 1.0), vec2<f32>(0.0), vec4<f32>(0.0), vec2<f32>(0.0),
        0u, 0.0, in.content_type, 1u,
    );
    var out_color = material_main(material_in);
    out_color.a *= base.a;
    return out_color;
}
"#
                    )
                ),
                offset,
            )
        }
    }
}

/// 解析 naga 错误字符串，将行号偏移回用户原始代码。
/// `user_start` = 用户代码在最终 WGSL 里起始行（1-indexed）。
/// `user_len` = 用户代码行数。
fn offset_naga_error(msg: &str, user_start: u32, user_len: u32) -> String {
    let mut out = String::with_capacity(msg.len());
    for line in msg.lines() {
        // 匹配行如 "  42 │ var x: ..."
        if let Some(rest) = line.trim_start().strip_suffix('│') {
            let num_part = rest.trim();
            if let Ok(n) = num_part.parse::<u32>() {
                let adjusted = if n < user_start {
                    n // engine boilerplate
                } else if n < user_start + user_len {
                    n - user_start + 1 // user code: 1-indexed
                } else {
                    n - user_start - user_len + 1 // injected/wrapper code
                };
                out.push_str(&format!("{:>4} │", adjusted));
            } else {
                out.push_str(line);
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    // Also adjust ── lines like "  ┌─ shader.wgsl:42:18"
    if let Some(pos) = out.find("shader.wgsl:") {
        let rest = out[pos + 12..].to_owned();
        if let Some(col_pos) = rest.find(':') {
            let num_str = &rest[..col_pos];
            if let Ok(n) = num_str.parse::<u32>() {
                let adjusted = if n < user_start {
                    n
                } else if n < user_start + user_len {
                    n - user_start + 1
                } else {
                    n - user_start - user_len + 1
                };
                let before = &out[..pos + 12];
                let after = &rest[col_pos..];
                out = format!("{}{}{}", before, adjusted, after);
            }
        }
    }
    out
}

/// Material target discriminators (injected into WGSL as constants).
pub const VIREO_TARGET_SHAPE: u32 = 0;
pub const VIREO_TARGET_TEXT: u32 = 1;

const MATERIAL_INPUT_WGSL: &str = r#"
// MaterialInput contract for shape and text (not a frozen ABI yet).
//
// Fields (always present; some are target-specific):
// - uv: content-native UV
//     shape = current primitive texture UV
//     text  = glyph atlas UV
// - base_uv: batch base-texture UV from DrawBatch::set_texture / set_uv
//     shape = primitive UV mapped into batch UV rect (each draw_* remaps independently)
//     text  = per-glyph-quad UV mapped into batch UV rect (REPEATS per glyph; intentional)
//     NOT a continuous whole-line/text-area UV. Continuous text mapping needs a future
//     field (e.g. text_uv / screen_uv) — do NOT repurpose base_uv.
// - color: default base color after engine sampling * vertex/text color (rgb only for material_main;
//          alpha is reapplied by the engine wrapper after material_main)
// - local_pos: shape-only local position; text fills (0,0)
// - sdf_params / sdf_extra / sdf_type / sdf_feather: shape-only SDF data; text fills zeros
// - content_type: text-only (0=color atlas glyph, 1=mask glyph); shape fills 0
// - target_type: VIREO_TARGET_SHAPE (0) or VIREO_TARGET_TEXT (1)
//
// Prefer helpers over internal resource names:
// - vireo_base_sample(uv)
// - vireo_base_color(in)
// - vireo_has_base_sample() / vireo_has_local_pos() / vireo_has_sdf_data()
const VIREO_TARGET_SHAPE: u32 = 0u;
const VIREO_TARGET_TEXT: u32 = 1u;

struct MaterialInput {
    uv: vec2<f32>,
    base_uv: vec2<f32>,
    color: vec4<f32>,
    local_pos: vec2<f32>,
    sdf_params: vec4<f32>,
    sdf_extra: vec2<f32>,
    sdf_type: u32,
    sdf_feather: f32,
    content_type: u32,
    target_type: u32,
};
"#;

const SHAPE_FRAGMENT_SUPPORT_WGSL: &str = r#"
struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var vireo_base_texture: texture_2d<f32>;
@group(1) @binding(1) var vireo_base_sampler: sampler;
@group(2) @binding(1) var<storage> polygon_edges: array<vec4<f32>>;

fn vireo_base_sample(uv: vec2<f32>) -> vec4<f32> {
    return textureSample(vireo_base_texture, vireo_base_sampler, uv);
}

fn vireo_base_color(in: MaterialInput) -> vec4<f32> {
    return in.color;
}

fn vireo_has_base_sample() -> bool { return true; }
fn vireo_has_local_pos() -> bool { return true; }
fn vireo_has_sdf_data() -> bool { return true; }

fn vireo_apply_sdf(in: VertexOutput, base_color: vec4<f32>) -> vec4<f32> {
    var out_color = base_color;
    if in.sdf_type == 0u { return out_color; }
    let feather = in.sdf_feather / camera.dpi_scale;
    var d: f32;
    switch in.sdf_type {
        case 1u: {
            d = length((in.local_pos - in.sdf_params.xy) / vec2(in.sdf_params.z, in.sdf_params.w));
            if d >= 1.0 { discard; }
            if feather > 0.0 {
                let k = feather / max(in.sdf_params.z, in.sdf_params.w);
                out_color.a *= 1.0 - smoothstep(1.0 - k, 1.0, d);
            }
        }
        case 2u: {
            let hw = in.sdf_params.z; let hh = in.sdf_params.w; let r = in.sdf_extra.x;
            d = length(max(abs(in.local_pos - in.sdf_params.xy) - vec2(hw - r, hh - r), vec2(0.0))) - r;
            if feather > 0.0 {
                if d >= feather { discard; }
                out_color.a *= 1.0 - smoothstep(0.0, feather, d);
            } else if d > 0.0 { discard; }
        }
        case 3u: {
            let a = in.sdf_params.xy; let b = in.sdf_params.zw;
            let ab = b - a;
            let ab_len2 = max(dot(ab, ab), 1e-8);
            let t = clamp(dot(in.local_pos - a, ab) / ab_len2, 0.0, 1.0);
            d = length(in.local_pos - (a + t * ab)) - in.sdf_extra.x;
            if feather > 0.0 {
                if d >= feather { discard; }
                out_color.a *= 1.0 - smoothstep(0.0, feather, d);
            } else if d > 0.0 { discard; }
        }
        case 4u: {
            let a = in.sdf_params.xy; let b = in.sdf_params.zw; let c = in.sdf_extra;
            let ab = b - a; let bc = c - b; let ca = a - c;
            let n_ab = select(vec2(0.0, 1.0), normalize(vec2(-ab.y, ab.x)), length(ab) > 1e-6);
            let n_bc = select(vec2(0.0, 1.0), normalize(vec2(-bc.y, bc.x)), length(bc) > 1e-6);
            let n_ca = select(vec2(0.0, 1.0), normalize(vec2(-ca.y, ca.x)), length(ca) > 1e-6);
            let d_ab = dot(in.local_pos - a, n_ab); let d_bc = dot(in.local_pos - b, n_bc); let d_ca = dot(in.local_pos - c, n_ca);
            let inside = d_ab > -0.0001 && d_bc > -0.0001 && d_ca > -0.0001;
            d = select(max(-d_ab, max(-d_bc, -d_ca)), -min(d_ab, min(d_bc, d_ca)), inside);
            if feather > 0.0 {
                if d >= feather { discard; }
                out_color.a *= 1.0 - smoothstep(0.0, feather, d);
            } else if d > 0.0 { discard; }
        }
        case 6u: {
            let start = u32(in.sdf_params.x); let count = u32(in.sdf_params.y);
            var d_max = -1e10;
            var d_min = 1e10;
            var inside = true;
            for (var i = start; i < start + count; i++) {
                let e = polygon_edges[i];
                let sd = dot(in.local_pos, e.xy) - e.z;
                if sd < -0.0001 { inside = false; }
                d_max = max(d_max, -sd);
                d_min = min(d_min, sd);
            }
            d = select(d_max, -d_min, inside);
            if feather > 0.0 {
                if d >= feather { discard; }
                out_color.a *= 1.0 - smoothstep(0.0, feather, d);
            } else if d > 0.0 { discard; }
        }
        case 7u: {
            let start = u32(in.sdf_params.x); let count = u32(in.sdf_params.y);
            let h = in.sdf_params.z;
            d = 1e10;
            for (var i = start; i < start + count; i++) {
                let seg = polygon_edges[i];
                let a = seg.xy; let b = seg.zw;
                let ab = b - a;
                let ab_len2 = max(dot(ab, ab), 1e-8);
                let t = clamp(dot(in.local_pos - a, ab) / ab_len2, 0.0, 1.0);
                d = min(d, length(in.local_pos - (a + t * ab)));
            }
            d -= h;
            if feather > 0.0 {
                if d >= feather { discard; }
                out_color.a *= 1.0 - smoothstep(0.0, feather, d);
            } else if d > 0.0 { discard; }
        }
        default: {
            let center = in.sdf_params.xy; let r = in.sdf_params.z;
            let to_p = in.local_pos - center;
            let d_circle = length(to_p) - r;
            let sa = in.sdf_extra.x; let ea = in.sdf_extra.y;
            let raw_span = ea - sa;
            let ccw_span = select(raw_span, raw_span + 6.283185307, raw_span < 0.0);
            let n_start = vec2(sin(sa), -cos(sa));
            let n_end = vec2(-sin(ea), cos(ea));
            let d_start = dot(to_p, n_start);
            let d_end = dot(to_p, n_end);
            let cn_start = vec2(sin(ea), -cos(ea));
            let cn_end = vec2(-sin(sa), cos(sa));
            let d_edge = select(
                -max(dot(to_p, cn_start), dot(to_p, cn_end)),
                max(d_start, d_end),
                ccw_span <= 3.14159265,
            );
            d = max(d_circle, d_edge);
            if feather > 0.0 {
                if d >= feather { discard; }
                out_color.a *= 1.0 - smoothstep(0.0, feather, d);
            } else if d > 0.0 { discard; }
        }
    }
    return out_color;
}
"#;

const TEXT_VERTEX_OUTPUT_WGSL: &str = r#"
struct VertexOutput {
    @invariant @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) content_type: u32,
    @location(3) base_uv: vec2<f32>,
};
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaterialTarget {
    Shape = 0,
    Text = 1,
}

fn material_pipeline_key(
    target: MaterialTarget,
    sample_count: u32,
    alpha_to_coverage: bool,
    ssaa: bool,
    stencil_mode: bool,
    stencil_op: u32,
) -> u64 {
    let ssaa = ssaa && target == MaterialTarget::Shape;
    target as u64
        | ((sample_count as u64) << 4)
        | ((alpha_to_coverage as u64) << 12)
        | ((stencil_mode as u64) << 13)
        | ((ssaa as u64) << 14)
        | ((stencil_op.min(4) as u64) << 16)
}

/// 共享 GPU 资源 —— 多个窗口/离屏纹理共用同一套 device/queue/pipeline
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub render_pipeline: wgpu::RenderPipeline,
    pub camera_bind_group_layout: wgpu::BindGroupLayout,
    pub texture_bind_group_layout: wgpu::BindGroupLayout,
    pub engine_storage_bind_group_layout: wgpu::BindGroupLayout,
    pub custom_material_bgl: wgpu::BindGroupLayout,
    pub default_sampler: wgpu::Sampler,
    pub(crate) non_filtering_sampler: wgpu::Sampler,
    pub(crate) comparison_sampler: wgpu::Sampler,
    pub white_texture: wgpu::Texture,
    pub white_texture_view: wgpu::TextureView,
    pub white_bind_group: Arc<wgpu::BindGroup>,
    pub engine_storage_dummy_bind_group: wgpu::BindGroup,
    pub(crate) polygon_dummy_buf: wgpu::Buffer,
    pub(crate) transform_dummy_buf: wgpu::Buffer,
    pub surface_format: wgpu::TextureFormat,
    pub text_ctx: RefCell<TextContext>,
    /// 跨材质 bind group 复用池。
    pub(crate) bind_group_pool: crate::material::BindGroupPool,
    /// wgpu adapter（pub(crate) 用于 surface 能力查询，例如选择 alpha 模式）
    pub(crate) adapter: wgpu::Adapter,
    /// device 对 surface_format 支持的 MSAA sample_count 列表（升序，如 [1, 2, 4]）。
    /// 在 GpuContext::new 末尾由 device.get_texture_format_features 查询得到。
    supported_sample_counts: Vec<u32>,
    pipelines: RefCell<FxHashMap<u32, wgpu::RenderPipeline>>,
    shader: wgpu::ShaderModule,      // MSAA：per-pixel 着色
    shader_ssaa: wgpu::ShaderModule, // SSAA：per-sample 着色
    shader_geo: wgpu::ShaderModule,  // 几何光栅化：无 SDF 分支
}

impl GpuContext {
    /// 创建 GPU 上下文（不依赖 surface）。
    /// format 默认为 Rgba8UnormSrgb，首窗口创建时通过 ensure_pipeline_format 调整。
    pub fn new(instance: &wgpu::Instance) -> Self {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .unwrap();

        // 无此 feature 时，pipeline 校验只认 WebGPU 保底 sample count（通常 [1, 4]），
        // 即便 adapter 列表含 8 也会在 create_render_pipeline 时 Validation panic。
        // 开启后才能真正使用 adapter 报告的 2x/8x 等。
        let mut required_features = wgpu::Features::empty();
        if adapter
            .features()
            .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
        {
            required_features |= wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        }

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("vireo device"),
                required_features,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                experimental_features: wgpu::ExperimentalFeatures::default(),
                trace: wgpu::Trace::default(),
            }))
            .unwrap();

        Self::build_resources(
            device,
            queue,
            adapter,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            ColorMode::Accurate,
        )
    }

    fn build_resources(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter: wgpu::Adapter,
        surface_format: wgpu::TextureFormat,
        color_mode: ColorMode,
    ) -> Self {
        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("camera bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("texture bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let engine_storage_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("engine storage bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let default_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("default sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let non_filtering_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("non-filtering sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let comparison_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("comparison sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });

        let white_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("white texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let white_texture_view = white_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let white_bind_group = Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("white bind group"),
            layout: &texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&white_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&default_sampler),
                },
            ],
        }));

        // Dummy polygon storage buffer（无多边形时仍满足 pipeline layout）
        let polygon_dummy_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("polygon dummy buffer"),
            size: 16, // 1 个 vec4
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // Custom material bind group layout (group 3)：
        //   0 storage (VS|FS) | 1 tex0 | 2 samp0 | 3 tex1 | 4 samp1 | 5 tex2 | 6 samp2 | 7 tex3 | 8 samp3
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let samp_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let custom_material_bgl =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("custom material bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    tex_entry(1),
                    samp_entry(2),
                    tex_entry(3),
                    samp_entry(4),
                    tex_entry(5),
                    samp_entry(6),
                    tex_entry(7),
                    samp_entry(8),
                ],
            });

        // Dummy transform storage buffer（单位矩阵 mat3x3，48 字节）
        let identity: [f32; 12] = [
            1.0, 0.0, 0.0, 0.0, // col0: (a, c, 0, pad)
            0.0, 1.0, 0.0, 0.0, // col1: (b, d, 0, pad)
            0.0, 0.0, 1.0, 0.0, // col2: (tx, ty, 1, pad)
        ];
        let transform_dummy_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("transform dummy buffer"),
            contents: bytemuck::cast_slice(&identity),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let engine_storage_dummy_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("engine storage dummy bind group"),
            layout: &engine_storage_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: transform_dummy_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: polygon_dummy_buf.as_entire_binding(),
                },
            ],
        });

        let shader_src = include_str!("shader.wgsl");
        // SSAA：保留 `@interpolate(linear, sample)` — 每个采样点独立执行片段着色器
        let shader_ssaa = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vireo shader (SSAA)"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });
        // MSAA：去掉 `, sample` — 每像素执行一次片段着色器
        let msaa_src: String = shader_src.replace(
            "@interpolate(linear, sample)",
            "@interpolate(linear)",
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vireo shader (MSAA)"),
            source: wgpu::ShaderSource::Wgsl(msaa_src.into()),
        });

        // 几何光栅化 shader：无 SDF 分支，无 per-sample 插值
        let shader_geo_src = include_str!("shader_geo.wgsl");
        let shader_geo = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vireo shader (geometry)"),
            source: wgpu::ShaderSource::Wgsl(shader_geo_src.into()),
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vireo pipeline"),
            layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vireo pipeline layout"),
                bind_group_layouts: &[
                    Some(&camera_bind_group_layout),
                    Some(&texture_bind_group_layout),
                    Some(&engine_storage_bind_group_layout),
                ],
                immediate_size: 0,
            })),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(Vertex::desc())],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let text_ctx = RefCell::new(TextContext::new(
            &device,
            &queue,
            surface_format,
            color_mode,
            &engine_storage_bind_group_layout,
            &white_texture_view,
            &default_sampler,
        ));

        let mut pipelines = FxHashMap::default();
        pipelines.insert(1, render_pipeline.clone());

        // 查询 adapter 对 surface_format 的 sample_count；若未开
        // TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES，pipeline 只接受 WebGPU 保底 [1, 4]。
        let mut supported_sample_counts = adapter
            .get_texture_format_features(surface_format)
            .flags
            .supported_sample_counts();
        if !device
            .features()
            .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
        {
            supported_sample_counts.retain(|c| *c == 1 || *c == 4);
            if supported_sample_counts.is_empty() {
                supported_sample_counts = vec![1];
            }
        }

        Self {
            device,
            queue,
            render_pipeline,
            camera_bind_group_layout,
            texture_bind_group_layout,
            engine_storage_bind_group_layout,
            custom_material_bgl,
            default_sampler,
            non_filtering_sampler,
            comparison_sampler,
            white_texture,
            white_texture_view,
            white_bind_group,
            engine_storage_dummy_bind_group,
            polygon_dummy_buf,
            transform_dummy_buf,
            surface_format,
            text_ctx,
            bind_group_pool: crate::material::BindGroupPool::new(),
            adapter,
            supported_sample_counts,
            pipelines: RefCell::new(pipelines),
            shader,
            shader_ssaa,
            shader_geo,
        }
    }

    /// 当前设备对 surface_format 支持的 MSAA sample_count 列表（升序）。
    /// 如 `[1, 2, 4, 8]`。仅包含 **create_render_pipeline 实际可用** 的值
    ///（已考虑 `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`）。
    /// AA 请求会 snap 到此列表中 ≤ 请求值的最大项。
    pub fn supported_sample_counts(&self) -> &[u32] {
        &self.supported_sample_counts
    }

    /// 当前设备支持的 MSAA 最大 sample_count。
    pub fn max_sample_count(&self) -> u32 {
        *self.supported_sample_counts.last().unwrap_or(&1)
    }

    /// 将请求的 sample_count 收束到 `supported_sample_counts` 中
    /// ≤ requested 的最大支持值（至少 1）。
    pub fn clamp_sample_count(&self, requested: u32) -> u32 {
        let req = requested.max(1);
        self.supported_sample_counts
            .iter()
            .copied()
            .filter(|&c| c <= req)
            .max()
            .unwrap_or(1)
    }

    /// 无 DS attachment 的热路径管线（无 `clips_children` 时使用）。
    /// `geometry`: true 时使用无 SDF 分支的几何着色器，忽略 ssaa 参数。
    pub fn ensure_pipeline(&self, sample_count: u32, alpha_to_coverage: bool, ssaa: bool, geometry: bool) -> wgpu::RenderPipeline {
        // bit19 = use_stencil=0 → 与 stencil 管线缓存键不冲突
        let key = sample_count
            | ((alpha_to_coverage as u32) << 16)
            | ((ssaa as u32) << 17)
            | ((geometry as u32) << 18);
        let mut pipes = self.pipelines.borrow_mut();
        if let Some(p) = pipes.get(&key) {
            return p.clone();
        }
        let module = if geometry {
            &self.shader_geo
        } else if ssaa {
            &self.shader_ssaa
        } else {
            &self.shader
        };
        let pipeline_layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vireo pipeline layout"),
            bind_group_layouts: &[
                Some(&self.camera_bind_group_layout),
                Some(&self.texture_bind_group_layout),
                Some(&self.engine_storage_bind_group_layout),
            ],
            immediate_size: 0,
        });
        let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vireo pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs_main"),
                buffers: &[Some(Vertex::desc())],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: sample_count,
                alpha_to_coverage_enabled: alpha_to_coverage,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });
        pipes.insert(key, p.clone());
        p
    }

    /// 带 Depth24PlusStencil8 的管线（`clips_children` / Area 帧使用）。
    /// `stencil_op`:
    /// 0=Always+Keep 透传(色), 1=Equal+Inc Push(色), 2=Equal+Keep Test(色),
    /// 3=Equal+Dec Pop/Erase(无色), 4=Equal+Inc Cover(无色，Area)
    pub fn ensure_stencil_pipeline(
        &self,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        geometry: bool,
        stencil_op: u32,
    ) -> wgpu::RenderPipeline {
        let op = stencil_op.min(4);
        // bit19 = use_stencil=1；bits20-22 = stencil_op
        let key = sample_count
            | ((alpha_to_coverage as u32) << 16)
            | ((ssaa as u32) << 17)
            | ((geometry as u32) << 18)
            | (1u32 << 19)
            | (op << 20);
        let mut pipes = self.pipelines.borrow_mut();
        if let Some(p) = pipes.get(&key) {
            return p.clone();
        }
        let module = if geometry {
            &self.shader_geo
        } else if ssaa {
            &self.shader_ssaa
        } else {
            &self.shader
        };
        let pipeline_layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vireo pipeline layout"),
            bind_group_layouts: &[
                Some(&self.camera_bind_group_layout),
                Some(&self.texture_bind_group_layout),
                Some(&self.engine_storage_bind_group_layout),
            ],
            immediate_size: 0,
        });
        // op3/4 不写颜色，但仍走 fs_main，以便 SDF discard 裁出正确轮廓
        //（fs_stencil_only 无 SDF，圆/圆角会落成 AABB）。
        let no_color = op == 3 || op == 4;
        let frag_entry = "fs_main";
        let color_target = if no_color {
            Some(wgpu::ColorTargetState {
                format: self.surface_format,
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })
        } else {
            Some(wgpu::ColorTargetState {
                format: self.surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })
        };

        let (face, read_mask, write_mask) = match op {
            0 => (wgpu::StencilFaceState::IGNORE, 0u32, 0u32),
            1 | 4 => (
                wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementClamp,
                },
                0xff,
                0xff,
            ),
            2 => (
                wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::Keep,
                },
                0xff,
                0xff,
            ),
            _ => (
                wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::DecrementClamp,
                },
                0xff,
                0xff,
            ),
        };
        let depth_stencil = Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth24PlusStencil8,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState {
                front: face,
                back: face,
                read_mask,
                write_mask,
            },
            bias: wgpu::DepthBiasState::default(),
        });

        let p = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vireo stencil pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs_main"),
                buffers: &[Some(Vertex::desc())],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(frag_entry),
                targets: &[color_target],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil,
            multisample: wgpu::MultisampleState {
                count: sample_count,
                alpha_to_coverage_enabled: alpha_to_coverage,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });
        pipes.insert(key, p.clone());
        p
    }

    /// 测量文本尺寸（逻辑像素）。参数与 draw_text 一致。
    pub fn measure_text(&self, text: &str, options: &crate::text::TextDef) -> (f32, f32) {
        use crate::glyphon::{Attrs, Buffer, Metrics, Shaping};

        let mut text_ctx = self.text_ctx.borrow_mut();
        let line_height = options.font_size * 1.2;
        let metrics = Metrics::new(options.font_size, line_height);
        let mut buffer = Buffer::new(&mut text_ctx.font_system, metrics);
        buffer.set_size(options.max_width, None);

        let attrs = options.attrs.as_ref()
            .map(|a| a.as_attrs())
            .unwrap_or_else(Attrs::new);

        buffer.set_text(text, &attrs, Shaping::Advanced, Some(options.align.into()));
        buffer.shape_until_scroll(&mut text_ctx.font_system, false);

        let num_lines = buffer.lines.len() as f32;
        let max_w = (0..buffer.lines.len()).fold(0.0f32, |max, i| {
            let line_w = buffer
                .line_layout(&mut text_ctx.font_system, i)
                .map(|layout| layout.iter().map(|run| run.w).sum())
                .unwrap_or(0.0);
            max.max(line_w)
        });

        (max_w, line_height * num_lines)
    }

    /// 从文件加载字体（TTF/OTF），使该字体可用于 TextOptions::with_family
    pub fn load_font_file(&self, path: impl AsRef<std::path::Path>) -> Result<(), String> {
        let data = std::fs::read(path.as_ref()).map_err(|e| format!("failed to read font file: {}", e))?;
        self.load_font(&data);
        Ok(())
    }

    /// 加载自定义字体（TTF/OTF 字节数据），使该字体可用于 TextOptions::with_family
    pub fn load_font(&self, data: &[u8]) {
        self.text_ctx.borrow_mut().font_system.db_mut().load_font_data(data.to_vec());
    }

    /// 设置文字 shape 缓存 TTL（真实时间，与 FPS 无关）。
    /// - `Some(d)`：超过 d 未使用则过期
    /// - `None`：永不按时间自动回收
    pub fn set_shape_cache_ttl(&self, ttl: Option<std::time::Duration>) {
        self.text_ctx.borrow_mut().set_shape_cache_ttl(ttl);
    }

    /// 当前 shape 缓存 TTL（`None` = 不自动按时间回收）。
    pub fn shape_cache_ttl(&self) -> Option<std::time::Duration> {
        self.text_ctx.borrow().shape_cache_ttl()
    }

    /// 设置 shape 缓存最大条数。
    /// - `Some(n)`：最多 n 条不同文案键，满则 LRU
    /// - `None`：不限制条数
    pub fn set_shape_cache_max_entries(&self, max: Option<usize>) {
        self.text_ctx.borrow_mut().set_shape_cache_max_entries(max);
    }

    /// 当前 shape 缓存最大条数（`None` = 不限制）。
    pub fn shape_cache_max_entries(&self) -> Option<usize> {
        self.text_ctx.borrow().shape_cache_max_entries()
    }

    /// 立即清空文字 shape 缓存。
    pub fn clear_shape_cache(&self) {
        self.text_ctx.borrow_mut().clear_shape_cache();
    }

    /// 当前 shape 缓存条目数。
    pub fn shape_cache_len(&self) -> usize {
        self.text_ctx.borrow().shape_cache_len()
    }

    /// 缓存中由 [`StableText`] 活跃持有的条目数。
    /// 这些槽不会被 TTL/LRU/`clear_shape_cache` 回收。
    /// O(n) 扫描（n = shape_slots.len()）。
    pub fn shape_cache_held_count(&self) -> usize {
        self.text_ctx.borrow_mut().shape_cache_held_count()
    }

    /// shape 缓存命中统计。
    pub fn shape_cache_stats(&self) -> crate::text::ShapeCacheStats {
        self.text_ctx.borrow().shape_cache_stats()
    }

    /// 重置 shape 缓存命中统计。
    pub fn reset_shape_cache_stats(&self) {
        self.text_ctx.borrow_mut().reset_shape_cache_stats();
    }

    /// 从文本创建 [`StableText`]（预 shape，跨帧复用）。
    /// 只要返回的 `StableText` 存活，对应的 Buffer 不会被释放。
    ///
    /// **首帧性能提示**：首次 `make_stable_text` 会触发 `harfrust` shape 成本
    /// （典型 ~5–30ms / 字符串）。建议在加载/初始化阶段预创建常用 handle，
    /// 或先调 [`GpuContext::preheat_text`] 触发字体/atlas lazy init。
    pub fn make_stable_text(&self, text: &str, options: &crate::text::TextDef) -> crate::text::StableText {
        self.text_ctx.borrow_mut().make_stable(text, options)
    }

    /// 预热文字管线：用单字符 "A" 跑一次 prepare，触发首帧字体/atlas lazy 初始化。
    /// 推荐在 `App` 启动后立即调用，避免首帧文字绘制卡顿。
    /// 调前需 `Renderer` 存在并已 `resize` 至少一次（让 `viewport` 知道物理尺寸）。
    pub fn preheat_text(&self, device: &wgpu::Device, queue: &wgpu::Queue, physical_width: u32, physical_height: u32) {
        self.text_ctx
            .borrow_mut()
            .preheat(device, queue, physical_width, physical_height);
    }

    /// Creates a material with no group 3 resources.
    ///
    /// Pipeline layout has only groups 0–2. The source must define
    /// `fn material_main(in: MaterialInput) -> vec4<f32>`. No `set_*` methods
    /// are available on the returned material.
    ///
    /// For group 3 resources, use [`create_material_with_resources`](Self::create_material_with_resources).
    pub fn create_material(&self, source: &str) -> Result<Arc<Material>, String> {
        self.create_material_inner(source, None, None)
    }

    /// Creates a material (no group 3) with a custom shape vertex shader.
    /// Text targets still use the engine vertex shader.
    pub fn create_material_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
    ) -> Result<Arc<Material>, String> {
        self.create_material_inner(source, Some(vertex_source.to_owned()), None)
    }

    /// Creates a material from resource descriptors (engine builds BGL, injects WGSL, AutoDefaults).
    pub fn create_material_with_resources(
        &self,
        source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<Material>, String> {
        self.create_material_inner(source, None, Some(resources))
    }

    /// Creates a material from resource descriptors with custom vertex shader.
    pub fn create_material_with_resources_and_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<Material>, String> {
        self.create_material_inner(source, Some(vertex_source.to_owned()), Some(resources))
    }

    /// Creates a material with user-provided BGL (caller must install `set_bind_group_provider` before draw).
    pub fn create_material_manual(
        &self,
        source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<Material>, String> {
        self.create_material_inner_manual(source, None, bgl)
    }

    /// Creates a material with user-provided BGL and custom vertex shader.
    pub fn create_material_manual_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<Material>, String> {
        self.create_material_inner_manual(source, Some(vertex_source.to_owned()), bgl)
    }

    fn create_material_inner(
        &self,
        source: &str,
        shape_vertex_source: Option<String>,
        resources: Option<crate::material::MaterialResources<'_>>,
    ) -> Result<Arc<Material>, String> {
        let source = crate::material::expand_includes(source)?;

        let raw_resources: Vec<crate::material::MaterialResource<'_>> = resources
            .map(|r| r.0.to_vec())
            .unwrap_or_default();

        let has_resources = !raw_resources.is_empty();

        // Build BGL from descriptors
        let material_bgl = if has_resources {
            crate::material::build_bgl_from_resources(&self.device, &raw_resources)?
        } else {
            None
        };

        // Inject WGSL at end
        let final_source = if has_resources {
            crate::material::inject_wgsl_resources(&source, &raw_resources)
        } else {
            source
        };

        // Validate pipelines compile
        let mut pipelines = FxHashMap::default();
        let bgl_ref = material_bgl.as_ref();
        for target in [MaterialTarget::Shape, MaterialTarget::Text] {
            let pipeline = self.create_material_pipeline_raw(
                &final_source,
                shape_vertex_source.as_deref(),
                target,
                1,
                false,
                false,
                false,
                0,
                bgl_ref,
            )?;
            pipelines.insert(
                material_pipeline_key(target, 1, false, false, false, 0),
                Arc::new(pipeline),
            );
        }

        if has_resources {
            let bgl = material_bgl.unwrap();
            let (slots, init_bg) = crate::material::build_auto_defaults(
                &self.device,
                &bgl,
                &raw_resources,
                &self.white_texture_view,
                &self.default_sampler,
                &self.non_filtering_sampler,
                &self.comparison_sampler,
            );
            Ok(Arc::new(Material::new_a(
                bgl,
                slots,
                init_bg,
                crate::material::CachePolicy::Dirty,
                final_source,
                shape_vertex_source,
                pipelines,
                self.device.clone(),
            )))
        } else {
            Ok(Arc::new(Material::new_zero_resource(
                final_source,
                shape_vertex_source,
                pipelines,
            )))
        }
    }

    fn create_material_inner_manual(
        &self,
        source: &str,
        shape_vertex_source: Option<String>,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<Material>, String> {
        let source = crate::material::expand_includes(source)?;
        let mut pipelines = FxHashMap::default();
        let bgl_ref = Some(bgl);
        for target in [MaterialTarget::Shape, MaterialTarget::Text] {
            let pipeline = self.create_material_pipeline_raw(
                &source,
                shape_vertex_source.as_deref(),
                target,
                1,
                false,
                false,
                false,
                0,
                bgl_ref,
            )?;
            pipelines.insert(
                material_pipeline_key(target, 1, false, false, false, 0),
                Arc::new(pipeline),
            );
        }

        Ok(Arc::new(Material::new_b(
            bgl.clone(),
            None,
            source.to_owned(),
            shape_vertex_source,
            pipelines,
        )))
    }

    pub(crate) fn ensure_material_pipeline(
        &self,
        material: &Material,
        target: MaterialTarget,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        stencil_mode: bool,
        stencil_op: u32,
    ) -> Arc<wgpu::RenderPipeline> {
        let ssaa = ssaa
            && target == MaterialTarget::Shape
            && material.shape_vertex_source.is_none();
        let key = material_pipeline_key(
            target,
            sample_count,
            alpha_to_coverage,
            ssaa,
            stencil_mode,
            stencil_op,
        );
        {
            let pipes = material.pipelines.borrow();
            if let Some(p) = pipes.get(&key) {
                return p.clone();
            }
        }
        let pipeline = self.create_material_pipeline_raw(
            &material.source,
            material.shape_vertex_source.as_deref(),
            target,
            sample_count,
            alpha_to_coverage,
            ssaa,
            stencil_mode,
            stencil_op,
            material.bgl(),
        ).expect("material WGSL was validated by create_material");
        let arc = Arc::new(pipeline);
        let mut pipes = material.pipelines.borrow_mut();
        pipes.entry(key).or_insert(arc).clone()
    }

    fn create_material_pipeline_raw(
        &self,
        source: &str,
        shape_vertex_source: Option<&str>,
        target: MaterialTarget,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        stencil_mode: bool,
        stencil_op: u32,
        material_bgl: Option<&wgpu::BindGroupLayout>,
    ) -> Result<wgpu::RenderPipeline, String> {
        let ssaa = ssaa
            && target == MaterialTarget::Shape
            && shape_vertex_source.is_none();
        let _scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (fragment_source_str, user_offset) = material_fragment_source(source, target, ssaa);
        let user_line_count = source.split('\n').count() as u32;
        let depth_stencil = if !stencil_mode {
            None
        } else if target == MaterialTarget::Text {
            if stencil_op == 2 { crate::text::stencil_text_ds_test() } else { crate::text::stencil_text_ds_pass() }
        } else {
            let (face, read_mask, write_mask) = match stencil_op {
            0 => (wgpu::StencilFaceState::IGNORE, 0u32, 0u32),
            1 | 4 => (
                wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementClamp,
                },
                0xff,
                0xff,
            ),
            2 => (
                wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::Keep,
                },
                0xff,
                0xff,
            ),
            _ => (
                wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::DecrementClamp,
                },
                0xff,
                0xff,
            ),
            };
            Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState { front: face, back: face, read_mask, write_mask },
                bias: wgpu::DepthBiasState::default(),
            })
        };
        let multisample = wgpu::MultisampleState {
            count: sample_count,
            alpha_to_coverage_enabled: alpha_to_coverage,
            ..Default::default()
        };
        let pipeline = match target {
            MaterialTarget::Shape => self.create_shape_material_pipeline(
                &fragment_source_str,
                shape_vertex_source
                    .map(str::to_owned)
                    .unwrap_or_else(|| default_shape_vertex_wgsl(ssaa))
                    .as_str(),
                multisample,
                depth_stencil,
                stencil_op,
                material_bgl,
            ),
            MaterialTarget::Text => self.text_ctx.borrow().text_atlas.create_material_pipeline(
                &self.device,
                material_bgl,
                &fragment_source_str,
                multisample,
                depth_stencil,
            ),
        };

        let err = pollster::block_on(_scope.pop());
        if let Some(e) = err {
            let adjusted = offset_naga_error(&e.to_string(), user_offset, user_line_count);
            return Err(format!("material {:?} pipeline error: {}", target, adjusted));
        }
        Ok(pipeline)
    }

    fn create_shape_material_pipeline(
        &self,
        fragment_source: &str,
        vertex_source: &str,
        multisample: wgpu::MultisampleState,
        depth_stencil: Option<wgpu::DepthStencilState>,
        stencil_op: u32,
        material_bgl: Option<&wgpu::BindGroupLayout>,
    ) -> wgpu::RenderPipeline {
        let vertex = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("material shape vertex"), source: wgpu::ShaderSource::Wgsl(vertex_source.into()),
        });
        let fragment = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("material shape fragment"), source: wgpu::ShaderSource::Wgsl(fragment_source.into()),
        });
        let bgls: Vec<Option<&wgpu::BindGroupLayout>> = if material_bgl.is_some() {
            vec![Some(&self.camera_bind_group_layout), Some(&self.texture_bind_group_layout),
                 Some(&self.engine_storage_bind_group_layout), material_bgl]
        } else {
            vec![Some(&self.camera_bind_group_layout), Some(&self.texture_bind_group_layout),
                 Some(&self.engine_storage_bind_group_layout)]
        };
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("material shape layout"),
            bind_group_layouts: &bgls,
            immediate_size: 0,
        });
        self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("material shape pipeline"), layout: Some(&layout),
            vertex: wgpu::VertexState { module: &vertex, entry_point: Some("vs_main"), buffers: &[Some(Vertex::desc())], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &fragment, entry_point: Some("fs_main"), targets: &[Some(wgpu::ColorTargetState {
                format: self.surface_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: if stencil_op == 3 || stencil_op == 4 { wgpu::ColorWrites::empty() } else { wgpu::ColorWrites::ALL },
            })], compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil, multisample, multiview_mask: None, cache: None,
        })
    }

}

#[cfg(test)]
mod custom_material_tests {
    use super::*;

    #[test]
    fn target_pipeline_keys_are_distinct() {
        let shape = material_pipeline_key(MaterialTarget::Shape, 4, false, false, false, 0);
        let text = material_pipeline_key(MaterialTarget::Text, 4, false, false, false, 0);
        assert_ne!(shape, text);
    }

    #[test]
    fn material_stencil_key_ignores_unused_flags() {
        // 同 sample/atc/op 必须同 key
        assert_eq!(
            material_pipeline_key(MaterialTarget::Shape, 4, true, false, true, 2),
            material_pipeline_key(MaterialTarget::Shape, 4, true, false, true, 2)
        );
        assert_ne!(
            material_pipeline_key(MaterialTarget::Shape, 4, false, false, true, 1),
            material_pipeline_key(MaterialTarget::Shape, 4, false, false, true, 2)
        );
        assert_ne!(
            material_pipeline_key(MaterialTarget::Shape, 1, false, false, true, 1),
            material_pipeline_key(MaterialTarget::Shape, 4, false, false, true, 1)
        );
        // 不同 target 必须不同 key
        assert_ne!(
            material_pipeline_key(MaterialTarget::Shape, 4, false, false, true, 1),
            material_pipeline_key(MaterialTarget::Text, 4, false, false, true, 1)
        );
    }

    #[test]
    fn material_shape_ssaa_pipeline_key_is_distinct() {
        assert_ne!(
            material_pipeline_key(MaterialTarget::Shape, 4, false, false, false, 0),
            material_pipeline_key(MaterialTarget::Shape, 4, false, true, false, 0),
        );
        assert_eq!(
            material_pipeline_key(MaterialTarget::Text, 4, false, false, false, 0),
            material_pipeline_key(MaterialTarget::Text, 4, false, true, false, 0),
        );
    }

    #[test]
    fn default_material_shape_vertex_preserves_sample_interpolation_for_ssaa() {
        assert!(default_shape_vertex_wgsl(true).contains("@interpolate(linear, sample)"));
        assert!(!default_shape_vertex_wgsl(false).contains("@interpolate(linear, sample)"));
    }

    #[test]
    fn material_shape_fragment_matches_ssaa_interpolation() {
        let shader = "fn material_main(in: MaterialInput) -> vec4<f32> { return in.color; }";
        assert!(material_fragment_source(shader, MaterialTarget::Shape, true).0
            .contains("@interpolate(linear, sample) local_pos"));
        assert!(!material_fragment_source(shader, MaterialTarget::Shape, false).0
            .contains("@interpolate(linear, sample) local_pos"));
    }

    #[test]
    fn material_helpers_exist_for_both_targets() {
        let shader = "fn material_main(in: MaterialInput) -> vec4<f32> { return vireo_base_color(in); }";
        let shape = material_fragment_source(shader, MaterialTarget::Shape, false).0;
        let text = material_fragment_source(shader, MaterialTarget::Text, false).0;
        for src in [shape, text] {
            assert!(src.contains("fn vireo_base_sample(uv: vec2<f32>) -> vec4<f32>"));
            assert!(src.contains("fn vireo_base_color(in: MaterialInput) -> vec4<f32>"));
            assert!(src.contains("fn vireo_has_base_sample() -> bool"));
            assert!(src.contains("fn vireo_has_local_pos() -> bool"));
            assert!(src.contains("fn vireo_has_sdf_data() -> bool"));
        }
    }

    #[test]
    fn material_input_exposes_base_uv() {
        assert!(MATERIAL_INPUT_WGSL.contains("base_uv: vec2<f32>"));
        assert!(MATERIAL_INPUT_WGSL.contains("const VIREO_TARGET_SHAPE: u32 = 0u;"));
        assert!(MATERIAL_INPUT_WGSL.contains("const VIREO_TARGET_TEXT: u32 = 1u;"));
    }

    #[test]
    fn material_target_constants_match_rust() {
        assert_eq!(VIREO_TARGET_SHAPE, 0);
        assert_eq!(VIREO_TARGET_TEXT, 1);
    }
}

/// 2D 顶点（68 字节）。
///
/// 变换矩阵不再存储于顶点，而是通过 `transform_index` 索引 `transforms` storage buffer。
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    /// SDF 形状参数，含义由 `sdf_type` 决定：
    /// 1 circle/ellipse: (cx,cy,rx,ry)
    /// 2 rect/rounded_rect: (cx,cy,hw,hh); uv.x=corner_radius
    /// 3 line: (x1,y1,x2,y2); uv.x=half_thickness
    /// 4 triangle: (x1,y1,x2,y2); uv=(x3,y3)
    /// 5 arc: (cx,cy,r,0); uv=(start_angle, end_angle)
    /// 6 polygon: (start_idx_f32, count_f32, 0, 0); 边数据在 storage buffer（每边 vec4: nx,ny,offset,0）
    /// 7 line_chain: (start_idx_f32, count_f32, half_thickness, 0); segment 数据在 storage buffer（每段 vec4: x1,y1,x2,y2）
    pub sdf_params: [f32; 4],
    /// 0=none, 1=circle, 2=rect, 3=line, 4=triangle, 5=arc, 6=polygon, 7=line_chain
    pub sdf_type: u32,
    /// SDF 柔边宽度（逻辑像素）
    pub sdf_feather: f32,
    /// SDF 额外参数，含义由 sdf_type 决定：
    /// 2 rect/rounded_rect: (corner_radius, 0)
    /// 3 line: (half_thickness, 0)
    /// 4 triangle: (x3, y3)
    /// 5 arc: (start_angle, end_angle)
    /// 其余 type 未使用。
    pub sdf_extra: [f32; 2],
    /// 变换矩阵索引，指向 `transforms` storage buffer（group 2 binding 0）。
    /// 0 = 恒等矩阵（默认）。
    pub transform_index: u32,
}

impl Vertex {
    pub fn new(x: f32, y: f32, color: crate::color::Color) -> Self {
        Self {
            position: [x, y], uv: [0.0; 2], color: [color.r, color.g, color.b, color.a],
            sdf_params: [0.0; 4], sdf_type: 0, sdf_feather: 0.0,
            sdf_extra: [0.0; 2],
            transform_index: 0,
        }
    }

    pub fn new_uv(x: f32, y: f32, u: f32, v: f32, color: crate::color::Color) -> Self {
        Self {
            position: [x, y], uv: [u, v], color: [color.r, color.g, color.b, color.a],
            sdf_params: [0.0; 4], sdf_type: 0, sdf_feather: 0.0,
            sdf_extra: [0.0; 2],
            transform_index: 0,
        }
    }

    /// 带 transform 索引的 UV 顶点（热路径，避免二次赋值）。
    #[inline]
    pub fn new_uv_xform(
        x: f32, y: f32, u: f32, v: f32,
        color: crate::color::Color, transform_index: u32,
    ) -> Self {
        Self {
            position: [x, y], uv: [u, v], color: [color.r, color.g, color.b, color.a],
            sdf_params: [0.0; 4], sdf_type: 0, sdf_feather: 0.0,
            sdf_extra: [0.0; 2],
            transform_index,
        }
    }

    /// 设置 transform 索引（构建器模式）。
    pub fn with_transform_index(mut self, idx: u32) -> Self {
        self.transform_index = idx;
        self
    }

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        const S2: wgpu::BufferAddress = std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress;
        const S4: wgpu::BufferAddress = std::mem::size_of::<[f32; 4]>() as wgpu::BufferAddress;
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { offset: 0, format: wgpu::VertexFormat::Float32x2, shader_location: 0 },
                wgpu::VertexAttribute { offset: S2, format: wgpu::VertexFormat::Float32x2, shader_location: 1 },
                wgpu::VertexAttribute { offset: S2 * 2, format: wgpu::VertexFormat::Float32x4, shader_location: 2 },
                wgpu::VertexAttribute { offset: S2 * 2 + S4, format: wgpu::VertexFormat::Float32x4, shader_location: 3 },
                wgpu::VertexAttribute { offset: S2 * 2 + S4 * 2, format: wgpu::VertexFormat::Uint32, shader_location: 4 },
                wgpu::VertexAttribute { offset: S2 * 2 + S4 * 2 + 4, format: wgpu::VertexFormat::Float32, shader_location: 5 },
                wgpu::VertexAttribute { offset: S2 * 2 + S4 * 2 + 8, format: wgpu::VertexFormat::Float32x2, shader_location: 6 },
                wgpu::VertexAttribute { offset: S2 * 2 + S4 * 2 + 8 + S2, format: wgpu::VertexFormat::Uint32, shader_location: 7 },
            ],
        }
    }
}
