//! Pipeline cache, WGSL generation, and material pipeline creation.

use std::sync::Arc;

use crate::material::Material;

use super::{GeoInstance, GeoVertex, GpuContext, ParticleInstance, QuadVertex, Vertex};

// ---------------------------------------------------------------------------
// WGSL constants and helper functions
// ---------------------------------------------------------------------------

pub(crate) const SHAPE_VERTEX_OUTPUT_WGSL: &str = r#"
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

/// 弱 GLES 后端回退：剥掉 `local_pos` varying 的 `linear`/`sample` 插值修饰，
/// 只留默认透视插值。正交投影下 w 恒 1，透视插值与线性插值逐位一致；
/// `flat` 的 u32 varying（naga-GLSL 全版本支持，不请求任何特性）原样保留。
/// 长形式先换（短形式是其前缀，直接换短的会留下 `, sample)` 残片）。
pub(crate) fn strip_noflat_interpolation(src: &str) -> String {
    src.replace("@interpolate(linear, sample) local_pos", "local_pos")
        .replace("@interpolate(linear) local_pos", "local_pos")
}

/// 弱 GLES 回退其二：剥掉 `@invariant`（ANGLE/ES 编译器拒收 fragment 侧的
/// invariant 限定，glyphon `position` 与文字材质 `VertexOutput` 各一处）。
/// 非 noflat 路径原样保留（桌面/真机 Vulkan 无此限制）。
pub(crate) fn strip_noflat_invariant(src: &str) -> String {
    src.replace("@invariant ", "")
}

pub(crate) fn default_shape_vertex_wgsl(ssaa: bool, noflat: bool) -> String {
    let source = include_str!("../shader.wgsl");
    let base = if ssaa {
        source.to_owned()
    } else {
        source.replace("@interpolate(linear, sample)", "@interpolate(linear)")
    };
    if noflat {
        strip_noflat_interpolation(&base)
    } else {
        base
    }
}

/// 默认 SDF instance VS（`shader_instance.wgsl`），按 `ssaa` 同步 local_pos 插值。
/// 与 material FS 的 `SHAPE_VERTEX_OUTPUT_WGSL` 保持一致：
///   - ssaa=true：  VS/FS 双方 `@interpolate(linear, sample)`
///   - ssaa=false： VS/FS 双方 `@interpolate(linear)`
/// `noflat=true` 时双方统一退为默认插值（见 `strip_noflat_interpolation`）。
pub(crate) fn default_sdf_instance_vertex_wgsl(ssaa: bool, noflat: bool) -> String {
    let source = include_str!("../shader_instance.wgsl");
    let base = if ssaa {
        source.to_owned()
    } else {
        source.replace("@interpolate(linear, sample)", "@interpolate(linear)")
    };
    if noflat {
        strip_noflat_interpolation(&base)
    } else {
        base
    }
}

/// 默认 Geo instance VS（`shader_geo_instance.wgsl`），按 `ssaa` 同步 local_pos 插值。
/// geo shape 本无 SDF 字段，VS 输出 local_pos 仅为 material FS 可读参考。
pub(crate) fn default_geo_instance_vertex_wgsl(ssaa: bool, noflat: bool) -> String {
    let source = include_str!("../shader_geo_instance.wgsl");
    let base = if ssaa {
        // geo VS 当前用 `@interpolate(linear)`（per-pixel），提升为 per-sample 与 FS 对齐
        source.replace(
            "@interpolate(linear) local_pos",
            "@interpolate(linear, sample) local_pos",
        )
    } else {
        source.to_owned()
    };
    if noflat {
        strip_noflat_interpolation(&base)
    } else {
        base
    }
}

/// 默认粒子 VS（`shader_particle.wgsl`），按 `ssaa` 同步 local_pos 插值。
/// 与 material FS 的 `SHAPE_VERTEX_OUTPUT_WGSL` 保持一致（SDF 字段恒 0，
/// `vireo_apply_sdf` 早退；`local_pos` 为 material FS 可读参考）。
pub(crate) fn default_particle_vertex_wgsl(ssaa: bool, noflat: bool) -> String {
    let source = include_str!("../shader_particle.wgsl");
    let base = if ssaa {
        source.to_owned()
    } else {
        source.replace("@interpolate(linear, sample)", "@interpolate(linear)")
    };
    if noflat {
        strip_noflat_interpolation(&base)
    } else {
        base
    }
}

/// 返回 (final_source, user_source_line_offset) 其中 offset 是用户代码起始行号（1-indexed）。
pub(crate) fn material_fragment_source(
    source: &str,
    target: MaterialTarget,
    ssaa: bool,
    noflat: bool,
) -> (String, u32) {
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
            let vertex_out = if noflat {
                strip_noflat_interpolation(&vertex_out)
            } else {
                vertex_out
            };
            let offset = line_count(&vertex_out)
                + line_count(MATERIAL_INPUT_WGSL)
                + line_count(SHAPE_FRAGMENT_SUPPORT_WGSL);
            // MRT：用户声明 `fn material_main(...) -> MaterialOutput` 时生成多输出包装。
            // 契约：`MaterialOutput` 第 0 个字段必须名为 `color`（引擎在其上应用
            // alpha / SDF），其余 location 原样输出，由用户 fragment shader 填写。
            let fs_wrapper = if source.contains("-> MaterialOutput") {
                r#"
@fragment
fn fs_main(in: VertexOutput) -> MaterialOutput {
    let base = textureSample(vireo_base_texture, vireo_base_sampler, in.uv) * in.color;
    let material_in = MaterialInput(
        in.uv, in.uv, vec4<f32>(base.rgb, 1.0), in.local_pos, in.sdf_params, in.sdf_extra,
        in.sdf_type, in.sdf_feather, 0u, 0u,
    );
    var out = material_main(material_in);
    out.color = vireo_apply_sdf(in, vec4<f32>(out.color.rgb, out.color.a * base.a));
    return out;
}
"#
            } else {
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
            };
            (
                format!(
                    "{}\n{}\n{}\n{}\n{}",
                    vertex_out,
                    MATERIAL_INPUT_WGSL,
                    SHAPE_FRAGMENT_SUPPORT_WGSL,
                    source,
                    fs_wrapper
                ),
                offset,
            )
        }
        MaterialTarget::Text => {
            let text_vertex_out = if noflat {
                strip_noflat_invariant(TEXT_VERTEX_OUTPUT_WGSL)
            } else {
                TEXT_VERTEX_OUTPUT_WGSL.to_owned()
            };
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
            let offset = line_count(&text_vertex_out)
                + line_count(MATERIAL_INPUT_WGSL)
                + line_count(text_support);
            // 文字管线只有单颜色输出：若用户按 MRT 契约返回 `MaterialOutput`，
            // 这里只取第 0 个 `color` 输出（text 在 MRT pass 中会被跳过）。
            let text_fs_wrapper = if source.contains("-> MaterialOutput") {
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
    var out_color = material_main(material_in).color;
    out_color.a *= base.a;
    return out_color;
}
"#
            } else {
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
            };
            (
                format!(
                    "{}\n{}\n{}\n{}\n{}",
                    text_vertex_out, MATERIAL_INPUT_WGSL, text_support, source, text_fs_wrapper
                ),
                offset,
            )
        }
    }
}

/// 解析 naga 错误字符串，将行号偏移回用户原始代码。
/// `user_start` = 用户代码在最终 WGSL 里起始行（1-indexed）。
/// `user_len` = 用户代码行数。
pub(crate) fn offset_naga_error(msg: &str, user_start: u32, user_len: u32) -> String {
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
/// Material target discriminator: text.
pub const VIREO_TARGET_TEXT: u32 = 1;

pub(crate) const MATERIAL_INPUT_WGSL: &str = r#"
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
//          alpha is reapplied by the engine wrapper after material_main;
//          particle fade is baked into instance color and composes through that re-application:
//          materials return constant/modulated alpha and never need to read base.a)
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

pub(crate) const SHAPE_FRAGMENT_SUPPORT_WGSL: &str = r#"
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

pub(crate) const TEXT_VERTEX_OUTPUT_WGSL: &str = r#"
struct VertexOutput {
    @invariant @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) content_type: u32,
    @location(3) base_uv: vec2<f32>,
};
"#;

// ---------------------------------------------------------------------------
// Enums and pipeline key helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaterialTarget {
    Shape = 0,
    Text = 1,
}

/// Shape Material 的顶点布局。同一 Material FS 可绑定不同顶点布局，
/// 按 batch 类型自动选最优路径（mesh / SDF instance / Geo instance）。
///
/// - `Mesh`：单 buffer `[Vertex]`（68B），用于 `DrawBatch.vertices` / Area 全屏 quad
///   / 直接 push 顶点的用户绘制。
/// - `SdfInstance`：单 buffer `[QuadVertex]`（8B）+ group 2 binding 2 的
///   `InstanceData` storage（vertex-pulling，VS 按 `instance_index` 读取）；
///   1 dc 跨参数合并（round-38+ 父提交基线）。
///   custom VS 须按 SDF 实例契约读取（`QuadVertex` + `instances` storage）。
/// - `GeoInstance`：双 buffer `[GeoVertex, GeoInstance]`（16B + 32B），
///   共享模板 + 每实例 color/transform；支持 `merge_geo_templates` 合并。
///   custom VS 须按几何实例契约读取（`GeoVertex` + `GeoInstance`）。
/// - `Particle`：双 buffer `[QuadVertex, ParticleInstance]`（8B + 152B），
///   共享 unit quad + 每实例 spawn 参数；VS 按 `camera.time` 积分（pos/fade），
///   FS 极简（tex×color，无 SDF）。**仅 fragment-only Material 可用**（custom VS 在
///   `draw_particles` 记录期烘焙为 mesh）。
///
/// custom VS 的布局支持集由创建期预验证决定，记录期按绘制路径检查。
/// `material_pipeline_key` 用 bit 8-9 区分 layout。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShapeVertexLayout {
    Mesh = 0,
    SdfInstance = 1,
    GeoInstance = 2,
    Particle = 3,
}

/// 把 `TextureFormat` 折叠成管线缓存键用的低位数值。
/// `TextureFormat` 含带字段变体（`Astc { .. }`），不能用 `as` cast 取判别值；
/// 改用其 `Hash` 实现取低 12 位。每个 GpuContext 同时只活一种格式（窗口 surface
/// 与 offscreen 均派生自 `surface_format`），缓存键只需区分「同步前默认格式」与
/// 「同步后真实格式」的条目；同步时还会清空共享 `pipelines` 表兜底。
pub(crate) fn surface_format_bits(format: wgpu::TextureFormat) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    format.hash(&mut h);
    (h.finish() as u32) & 0x0FFF
}

/// 把多个颜色附件的混合状态折叠成管线缓存键的高位数值。
pub(crate) fn blend_states_hash(blends: &[Option<wgpu::BlendState>]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    (blends.len() as u64).hash(&mut h);
    for b in blends {
        b.hash(&mut h);
    }
    h.finish() & 0x0000_FFFF_FFFF_FFFF
}

pub(crate) fn material_pipeline_key(
    target: MaterialTarget,
    sample_count: u32,
    alpha_to_coverage: bool,
    ssaa: bool,
    ds: bool,
    depth_write: bool,
    depth_compare: wgpu::CompareFunction,
    stencil_mode: bool,
    stencil_op: u32,
    shape_layout: ShapeVertexLayout,
    format: wgpu::TextureFormat,
    num_targets: u32,
    blend_hash: u64,
) -> u64 {
    let ssaa = ssaa && target == MaterialTarget::Shape;
    let layout_bits = if target == MaterialTarget::Shape {
        shape_layout as u64
    } else {
        0
    };
    // num_targets 占 bits 40-43（wgpu 最大 8 个 color target，4 位足够）；
    // blend_hash 占 bits 44-63（所有目标混合状态的哈希）。
    target as u64
        | ((sample_count as u64) << 4)
        | ((layout_bits) << 8)
        | ((alpha_to_coverage as u64) << 12)
        | ((stencil_mode as u64) << 13)
        | ((ssaa as u64) << 14)
        | ((ds as u64) << 15)
        | ((stencil_op.min(4) as u64) << 16)
        | ((surface_format_bits(format) as u64) << 20)
        | ((depth_write as u64) << 32)
        | ((crate::material::compare_to_u8(depth_compare) as u64) << 33)
        | ((num_targets as u64) << 40)
        | (blend_hash << 44)
}

/// 中性 depth-stencil 管线状态：深度 `Always`/不写、模板 `IGNORE`/不读写。
///
/// 用于「pass 挂载了 DS 但本管线不需要 stencil 测试」的场景（wgpu 要求管线
/// 状态与 pass attachment 匹配）。深度写/比较由调用方（材质）声明；
/// 无需求时用 `(false, Always)`——与 `depth_stencil: None` 行为等价。
pub(crate) fn neutral_depth_stencil_state(
    depth_write: bool,
    depth_compare: wgpu::CompareFunction,
) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        depth_write_enabled: Some(depth_write),
        depth_compare: Some(depth_compare),
        stencil: wgpu::StencilState {
            front: wgpu::StencilFaceState::IGNORE,
            back: wgpu::StencilFaceState::IGNORE,
            read_mask: 0,
            write_mask: 0,
        },
        bias: wgpu::DepthBiasState::default(),
    }
}

// ---------------------------------------------------------------------------
// Pipeline cache methods (impl GpuContext)
// ---------------------------------------------------------------------------

impl GpuContext {
    /// 无 DS attachment 的热路径管线（无 `clips_children` 时使用）。
    /// `geometry`: true 时使用无 SDF 分支的几何着色器，忽略 ssaa 参数。
    /// `ds`: pass 挂载了 DS（中性路径，无 stencil 测试）时置 true，管线补中性 DS 状态。
    pub fn ensure_pipeline(
        &self,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        ds: bool,
        geometry: bool,
        blend: wgpu::BlendState,
    ) -> wgpu::RenderPipeline {
        // bit19 = use_stencil=0 → 与 stencil 管线缓存键不冲突。
        // bit25 = 中性 DS（该位其余生成器均未使用）。
        // bits4-15 = surface format（macOS Metal 为 Bgra8UnormSrgb），格式不同
        // 的管线键不同，避免复用首窗口同步前的默认 Rgba8 管线。
        // blend 不进 bit（`BlendState` 全 Hash，进元组键后半）。
        let key = (
            sample_count
                | ((alpha_to_coverage as u32) << 16)
                | ((ssaa as u32) << 17)
                | ((geometry as u32) << 18)
                | ((ds as u32) << 25)
                | (surface_format_bits(self.surface_format()) << 4),
            blend,
        );
        let mut pipes = self.pipelines.lock().unwrap();
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
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vireo pipeline layout"),
                bind_group_layouts: &[
                    Some(&self.camera_bind_group_layout),
                    Some(&self.texture_bind_group_layout),
                    Some(&self.engine_storage_bind_group_layout),
                ],
                immediate_size: 0,
            });
        let p = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                        format: self.surface_format(),
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: if ds {
                    Some(neutral_depth_stencil_state(
                        false,
                        wgpu::CompareFunction::Always,
                    ))
                } else {
                    None
                },
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
        blend: wgpu::BlendState,
    ) -> wgpu::RenderPipeline {
        let op = stencil_op.min(4);
        // op3/4 不写颜色（blend=None）：blend 归一化回 Alpha，防建重复管线。
        let no_color = op == 3 || op == 4;
        let key_blend = if no_color {
            wgpu::BlendState::ALPHA_BLENDING
        } else {
            blend
        };
        // bit19 = use_stencil=1；bits20-22 = stencil_op；bits4-15 = surface format
        let key = (
            sample_count
                | ((alpha_to_coverage as u32) << 16)
                | ((ssaa as u32) << 17)
                | ((geometry as u32) << 18)
                | (1u32 << 19)
                | (op << 20)
                | (surface_format_bits(self.surface_format()) << 4),
            key_blend,
        );
        let mut pipes = self.pipelines.lock().unwrap();
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
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
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
        let frag_entry = "fs_main";
        let color_target = if no_color {
            Some(wgpu::ColorTargetState {
                format: self.surface_format(),
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })
        } else {
            Some(wgpu::ColorTargetState {
                format: self.surface_format(),
                blend: Some(blend),
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

        let p = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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

    pub(crate) fn ensure_instance_pipeline(
        &self,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        ds: bool,
        use_stencil: bool,
        stencil_op: u32,
        blend: wgpu::BlendState,
    ) -> wgpu::RenderPipeline {
        let op = stencil_op.min(2);
        // bit18 = 中性 DS（该位在 instance/geo/particle 生成器中空闲）。
        let key = (
            sample_count
                | ((alpha_to_coverage as u32) << 16)
                | ((ssaa as u32) << 17)
                | ((ds as u32) << 18)
                | ((use_stencil as u32) << 19)
                | (op << 20)
                | (1u32 << 23)
                | (surface_format_bits(self.surface_format()) << 4),
            blend,
        );
        let mut pipes = self.pipelines.lock().unwrap();
        if let Some(p) = pipes.get(&key) {
            return p.clone();
        }
        let module = if ssaa {
            &self.shader_instance_ssaa
        } else {
            &self.shader_instance
        };
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vireo instance pipeline layout"),
                bind_group_layouts: &[
                    Some(&self.camera_bind_group_layout),
                    Some(&self.texture_bind_group_layout),
                    Some(&self.engine_storage_bind_group_layout),
                ],
                immediate_size: 0,
            });
        let depth_stencil = if use_stencil {
            let face = match op {
                1 => wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementClamp,
                },
                2 => wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::Keep,
                },
                _ => wgpu::StencilFaceState::IGNORE,
            };
            Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState {
                    front: face,
                    back: face,
                    read_mask: if op == 0 { 0 } else { 0xff },
                    write_mask: if op == 1 { 0xff } else { 0 },
                },
                bias: Default::default(),
            })
        } else if ds {
            Some(neutral_depth_stencil_state(
                false,
                wgpu::CompareFunction::Always,
            ))
        } else {
            None
        };
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vireo instance pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(QuadVertex::desc())],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.surface_format(),
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
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
        pipes.insert(key, pipeline.clone());
        pipeline
    }

    /// 几何模板实例化管线：共享模板顶点/索引 buffer + per-instance color/transform。
    /// 无 SDF 分支；无 per-sample 插值（单模块，MSAA/SSAA 同源）。
    pub(crate) fn ensure_geo_instance_pipeline(
        &self,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        ds: bool,
        use_stencil: bool,
        stencil_op: u32,
        blend: wgpu::BlendState,
    ) -> wgpu::RenderPipeline {
        let op = stencil_op.min(2);
        let key = (
            sample_count
                | ((alpha_to_coverage as u32) << 16)
                | ((ssaa as u32) << 17)
                | ((ds as u32) << 18)
                | ((use_stencil as u32) << 19)
                | (op << 20)
                | (2u32 << 23)
                | (surface_format_bits(self.surface_format()) << 4),
            blend,
        );
        let mut pipes = self.pipelines.lock().unwrap();
        if let Some(p) = pipes.get(&key) {
            return p.clone();
        }
        let module = &self.shader_geo_instance;
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vireo geo instance pipeline layout"),
                bind_group_layouts: &[
                    Some(&self.camera_bind_group_layout),
                    Some(&self.texture_bind_group_layout),
                    Some(&self.engine_storage_bind_group_layout),
                ],
                immediate_size: 0,
            });
        let depth_stencil = if use_stencil {
            let face = match op {
                1 => wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementClamp,
                },
                2 => wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::Keep,
                },
                _ => wgpu::StencilFaceState::IGNORE,
            };
            Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState {
                    front: face,
                    back: face,
                    read_mask: if op == 0 { 0 } else { 0xff },
                    write_mask: if op == 1 { 0xff } else { 0 },
                },
                bias: Default::default(),
            })
        } else if ds {
            Some(neutral_depth_stencil_state(
                false,
                wgpu::CompareFunction::Always,
            ))
        } else {
            None
        };
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vireo geo instance pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(GeoVertex::desc()), Some(GeoInstance::desc())],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.surface_format(),
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
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
        pipes.insert(key, pipeline.clone());
        pipeline
    }

    /// 粒子实例化管线：共享 unit quad + per-instance spawn 参数，VS 按 `camera.time`
    /// 积分。FS 极简（tex×color，无 SDF 分支）；`local_pos` 仅供 material FS 参考。
    /// 缓存键镜像 instance 管线，标记位 `3 << 23`（instance=1, geo=2）。
    pub(crate) fn ensure_particle_pipeline(
        &self,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        ds: bool,
        use_stencil: bool,
        stencil_op: u32,
        blend: wgpu::BlendState,
    ) -> wgpu::RenderPipeline {
        let op = stencil_op.min(2);
        let key = (
            sample_count
                | ((alpha_to_coverage as u32) << 16)
                | ((ssaa as u32) << 17)
                | ((ds as u32) << 18)
                | ((use_stencil as u32) << 19)
                | (op << 20)
                | (3u32 << 23)
                | (surface_format_bits(self.surface_format()) << 4),
            blend,
        );
        let mut pipes = self.pipelines.lock().unwrap();
        if let Some(p) = pipes.get(&key) {
            return p.clone();
        }
        let module = if ssaa {
            &self.shader_particle_ssaa
        } else {
            &self.shader_particle
        };
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vireo particle pipeline layout"),
                bind_group_layouts: &[
                    Some(&self.camera_bind_group_layout),
                    Some(&self.texture_bind_group_layout),
                    Some(&self.engine_storage_bind_group_layout),
                ],
                immediate_size: 0,
            });
        let depth_stencil = if use_stencil {
            let face = match op {
                1 => wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementClamp,
                },
                2 => wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::Keep,
                },
                _ => wgpu::StencilFaceState::IGNORE,
            };
            Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState {
                    front: face,
                    back: face,
                    read_mask: if op == 0 { 0 } else { 0xff },
                    write_mask: if op == 1 { 0xff } else { 0 },
                },
                bias: Default::default(),
            })
        } else if ds {
            Some(neutral_depth_stencil_state(
                false,
                wgpu::CompareFunction::Always,
            ))
        } else {
            None
        };
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("vireo particle pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers: &[Some(QuadVertex::desc()), Some(ParticleInstance::desc())],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.surface_format(),
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
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
        pipes.insert(key, pipeline.clone());
        pipeline
    }

    // -----------------------------------------------------------------------
    // Material pipeline creation
    // -----------------------------------------------------------------------

    pub(crate) fn ensure_material_pipeline(
        &self,
        material: &Material,
        target: MaterialTarget,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        ds: bool,
        stencil_mode: bool,
        stencil_op: u32,
        shape_layout: ShapeVertexLayout,
        // MRT：每个颜色附件的混合状态；第 0 个对应 batch.blend，其余取自 RenderTarget。
        blends: &[Option<wgpu::BlendState>],
    ) -> Result<Arc<wgpu::RenderPipeline>, String> {
        let ssaa =
            ssaa && target == MaterialTarget::Shape && material.shape_vertex_source.is_none();
        // 深度写/比较仅在中性 DS（无 stencil 测试）且 shape 目标时取材质声明；
        // 其余恒 (false, Always)，与 `depth_stencil: None` 行为等价。
        let (depth_write, depth_compare) = if ds && !stencil_mode && target == MaterialTarget::Shape
        {
            (material.depth_write(), material.depth_compare())
        } else {
            (false, wgpu::CompareFunction::Always)
        };
        // 空颜色掩码（shape stencil cover/erase）不写颜色：blend 归一化防重复管线。
        let key_blend = if stencil_mode && (stencil_op == 3 || stencil_op == 4) {
            wgpu::BlendState::ALPHA_BLENDING
        } else {
            blends[0].unwrap_or(wgpu::BlendState::ALPHA_BLENDING)
        };
        let key = (
            material_pipeline_key(
                target,
                sample_count,
                alpha_to_coverage,
                ssaa,
                ds,
                depth_write,
                depth_compare,
                stencil_mode,
                stencil_op,
                shape_layout,
                self.surface_format(),
                blends.len() as u32,
                blend_states_hash(blends),
            ),
            key_blend,
        );
        let mut pipes = material.pipelines.lock().unwrap();
        if let Some(p) = pipes.get(&key) {
            return Ok(p.clone());
        }
        // 确定 material bind group entries 和是否包含引擎组。
        let (material_bgl_entries, include_engine_groups) = match &material.state {
            crate::material::MaterialState::SelfManaged { entries, .. } => {
                let es: Vec<(u32, &wgpu::BindGroupLayout)> =
                    entries.iter().map(|e| (e.group_index, &e.bgl)).collect();
                (es, false)
            }
            _ => {
                // State A / B / ZeroResource：material BGL（若有）固定放在 group 3，
                // 引擎组 0/1/2 始终包含。
                let entries: Vec<(u32, &wgpu::BindGroupLayout)> =
                    material.bgl().map(|b| (3u32, b)).into_iter().collect();
                (entries, true)
            }
        };
        let pipeline = self.create_material_pipeline_raw(
            &material.source,
            material.shape_vertex_source.as_deref(),
            target,
            sample_count,
            alpha_to_coverage,
            ssaa,
            ds,
            depth_write,
            depth_compare,
            stencil_mode,
            stencil_op,
            &material_bgl_entries,
            include_engine_groups,
            shape_layout,
            blends,
        )?;
        let arc = Arc::new(pipeline);
        Ok(pipes.entry(key).or_insert(arc).clone())
    }

    pub(crate) fn create_material_pipeline_raw(
        &self,
        source: &str,
        shape_vertex_source: Option<&str>,
        target: MaterialTarget,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        ds: bool,
        depth_write: bool,
        depth_compare: wgpu::CompareFunction,
        stencil_mode: bool,
        stencil_op: u32,
        material_bgl_entries: &[(u32, &wgpu::BindGroupLayout)],
        include_engine_groups: bool,
        shape_layout: ShapeVertexLayout,
        // MRT：每个颜色附件的混合状态；第 0 个对应 batch.blend，其余取自 RenderTarget。
        blends: &[Option<wgpu::BlendState>],
    ) -> Result<wgpu::RenderPipeline, String> {
        let ssaa = ssaa && target == MaterialTarget::Shape && shape_vertex_source.is_none();
        let _scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (fragment_source_str, user_offset) =
            material_fragment_source(source, target, ssaa, self.noflat);
        let user_line_count = source.split('\n').count() as u32;
        let depth_stencil = if !ds {
            None
        } else if target == MaterialTarget::Text {
            // 文字不参与深度测试：stencil 模式用既有文字状态；
            // 中性 DS 时恒 Always/不写（忽略材质声明）。
            if stencil_mode && stencil_op == 2 {
                crate::text::stencil_text_ds_test()
            } else {
                crate::text::stencil_text_ds_pass()
            }
        } else if stencil_mode {
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
                stencil: wgpu::StencilState {
                    front: face,
                    back: face,
                    read_mask,
                    write_mask,
                },
                bias: wgpu::DepthBiasState::default(),
            })
        } else {
            // 中性 DS：无 stencil 测试，深度写/比较由材质声明（shape 目标）。
            Some(neutral_depth_stencil_state(depth_write, depth_compare))
        };
        let multisample = wgpu::MultisampleState {
            count: sample_count,
            alpha_to_coverage_enabled: alpha_to_coverage,
            ..Default::default()
        };
        let pipeline = match target {
            MaterialTarget::Shape => {
                // layout 决定默认 VS：Mesh 用 default_shape，instance path 用各自 VS。
                // custom VS 按绘制路径契约读取（Mesh/SDF/Geo）；Particle 仅 fragment-only
                //（粒子 path 记录期已烘焙为 mesh，到此即错误用法）。
                let default_vs = match shape_layout {
                    ShapeVertexLayout::Mesh => default_shape_vertex_wgsl(ssaa, self.noflat),
                    ShapeVertexLayout::SdfInstance => {
                        default_sdf_instance_vertex_wgsl(ssaa, self.noflat)
                    }
                    ShapeVertexLayout::GeoInstance => {
                        default_geo_instance_vertex_wgsl(ssaa, self.noflat)
                    }
                    ShapeVertexLayout::Particle => default_particle_vertex_wgsl(ssaa, self.noflat),
                };
                let vs_source = match shape_layout {
                    ShapeVertexLayout::Mesh
                    | ShapeVertexLayout::SdfInstance
                    | ShapeVertexLayout::GeoInstance => {
                        shape_vertex_source.map(str::to_owned).unwrap_or(default_vs)
                    }
                    ShapeVertexLayout::Particle => {
                        if shape_vertex_source.is_some() {
                            return Err(format!(
                                "material: custom vertex shader is not supported with \
                                 {:?} layout; use a fragment-only material",
                                shape_layout
                            ));
                        }
                        default_vs
                    }
                };
                self.create_shape_material_pipeline(
                    &fragment_source_str,
                    vs_source.as_str(),
                    multisample,
                    depth_stencil,
                    stencil_op,
                    material_bgl_entries,
                    include_engine_groups,
                    shape_layout,
                    blends,
                )
            }
            MaterialTarget::Text => self
                .text_ctx
                .lock()
                .unwrap()
                .text_atlas
                .create_material_pipeline(
                    &self.device,
                    material_bgl_entries.first().map(|(_, b)| *b),
                    &fragment_source_str,
                    multisample,
                    depth_stencil,
                    blends[0].unwrap_or(wgpu::BlendState::ALPHA_BLENDING),
                ),
        };

        let err = pollster::block_on(_scope.pop());
        if let Some(e) = err {
            let adjusted = offset_naga_error(&e.to_string(), user_offset, user_line_count);
            return Err(format!(
                "material {:?} pipeline error: {}",
                target, adjusted
            ));
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
        material_bgl_entries: &[(u32, &wgpu::BindGroupLayout)],
        include_engine_groups: bool,
        shape_layout: ShapeVertexLayout,
        // MRT：每个颜色附件的混合状态
        blends: &[Option<wgpu::BlendState>],
    ) -> wgpu::RenderPipeline {
        let vertex = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("material shape vertex"),
                source: wgpu::ShaderSource::Wgsl(vertex_source.into()),
            });
        let fragment = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("material shape fragment"),
                source: wgpu::ShaderSource::Wgsl(fragment_source.into()),
            });
        // 构建 pipeline layout 的 bind group 列表。
        // 旧行为：group 0 camera、group 1 texture、group 2 engine storage、group 3 material。
        // 自管模式：仅用户 entries，不含引擎组。
        let bgls: Vec<Option<&wgpu::BindGroupLayout>> = if include_engine_groups {
            let mut v = vec![
                Some(&self.camera_bind_group_layout),
                Some(&self.texture_bind_group_layout),
                Some(&self.engine_storage_bind_group_layout),
            ];
            if let Some((gi, bgl)) = material_bgl_entries.first() {
                // 确保数组足够大，放在指定 group_index 处
                let needed = (*gi as usize) + 1;
                if v.len() < needed {
                    v.resize(needed, None);
                }
                v[*gi as usize] = Some(*bgl);
            }
            v
        } else {
            // 自管模式：仅用户 entries
            let max_group = material_bgl_entries
                .iter()
                .map(|(gi, _)| *gi as usize)
                .max()
                .unwrap_or(0);
            let mut v = vec![None; max_group + 1];
            for (gi, bgl) in material_bgl_entries {
                v[*gi as usize] = Some(*bgl);
            }
            v
        };
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("material shape layout"),
                bind_group_layouts: &bgls,
                immediate_size: 0,
            });
        let buffers: Vec<Option<wgpu::VertexBufferLayout<'static>>> = match shape_layout {
            ShapeVertexLayout::Mesh => vec![Some(Vertex::desc())],
            ShapeVertexLayout::SdfInstance => vec![Some(QuadVertex::desc())],
            ShapeVertexLayout::GeoInstance => {
                vec![Some(GeoVertex::desc()), Some(GeoInstance::desc())]
            }
            ShapeVertexLayout::Particle => {
                vec![Some(QuadVertex::desc()), Some(ParticleInstance::desc())]
            }
        };
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("material shape pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &vertex,
                    entry_point: Some("vs_main"),
                    buffers: &buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &fragment,
                    entry_point: Some("fs_main"),
                    targets: &blends
                        .iter()
                        .map(|blend| {
                            Some(wgpu::ColorTargetState {
                                format: self.surface_format(),
                                blend: *blend,
                                write_mask: if stencil_op == 3 || stencil_op == 4 {
                                    wgpu::ColorWrites::empty()
                                } else {
                                    wgpu::ColorWrites::ALL
                                },
                            })
                        })
                        .collect::<Vec<_>>(),
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil,
                multisample,
                multiview_mask: None,
                cache: None,
            })
    }
}

#[cfg(test)]
mod custom_material_tests {
    use super::*;

    /// 旧 8 参调用（ds 中性）的测试包装，保持既有断言可读。
    fn mk_key(
        target: MaterialTarget,
        sample_count: u32,
        alpha_to_coverage: bool,
        ssaa: bool,
        stencil_mode: bool,
        stencil_op: u32,
        shape_layout: ShapeVertexLayout,
        format: wgpu::TextureFormat,
    ) -> u64 {
        material_pipeline_key(
            target,
            sample_count,
            alpha_to_coverage,
            ssaa,
            false,
            false,
            wgpu::CompareFunction::Always,
            stencil_mode,
            stencil_op,
            shape_layout,
            format,
            1,
            0,
        )
    }

    #[test]
    fn material_pipeline_key_distinguishes_ds_and_depth_state() {
        // ds 开启 / 深度写 / 深度比较 任一不同必须产生不同 key
        let neutral = material_pipeline_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            false,
            wgpu::CompareFunction::Always,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            1,
            0,
        );
        assert_eq!(
            mk_key(
                MaterialTarget::Shape,
                1,
                false,
                false,
                false,
                0,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
            neutral
        );

        let ds = material_pipeline_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            true,
            false,
            wgpu::CompareFunction::Always,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            1,
            0,
        );
        assert_ne!(neutral, ds);

        let write = material_pipeline_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            true,
            true,
            wgpu::CompareFunction::Always,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            1,
            0,
        );
        assert_ne!(ds, write);

        let compare = material_pipeline_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            true,
            true,
            wgpu::CompareFunction::Less,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            1,
            0,
        );
        assert_ne!(write, compare);
    }

    #[test]
    fn target_pipeline_keys_are_distinct() {
        let shape = mk_key(
            MaterialTarget::Shape,
            4,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let text = mk_key(
            MaterialTarget::Text,
            4,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        assert_ne!(shape, text);
    }

    #[test]
    fn material_stencil_key_ignores_unused_flags() {
        // 同 sample/atc/op 必须同 key
        assert_eq!(
            mk_key(
                MaterialTarget::Shape,
                4,
                true,
                false,
                true,
                2,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
            mk_key(
                MaterialTarget::Shape,
                4,
                true,
                false,
                true,
                2,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb
            )
        );
        assert_ne!(
            mk_key(
                MaterialTarget::Shape,
                4,
                false,
                false,
                true,
                1,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
            mk_key(
                MaterialTarget::Shape,
                4,
                false,
                false,
                true,
                2,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb
            )
        );
        assert_ne!(
            mk_key(
                MaterialTarget::Shape,
                1,
                false,
                false,
                true,
                1,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
            mk_key(
                MaterialTarget::Shape,
                4,
                false,
                false,
                true,
                1,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb
            )
        );
        // 不同 target 必须不同 key
        assert_ne!(
            mk_key(
                MaterialTarget::Shape,
                4,
                false,
                false,
                true,
                1,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
            mk_key(
                MaterialTarget::Text,
                4,
                false,
                false,
                true,
                1,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb
            )
        );
    }

    #[test]
    fn material_shape_ssaa_pipeline_key_is_distinct() {
        assert_ne!(
            mk_key(
                MaterialTarget::Shape,
                4,
                false,
                false,
                false,
                0,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
            mk_key(
                MaterialTarget::Shape,
                4,
                false,
                false,
                true,
                1,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
        );
        assert_eq!(
            mk_key(
                MaterialTarget::Text,
                4,
                false,
                false,
                false,
                0,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
            mk_key(
                MaterialTarget::Text,
                4,
                false,
                true,
                false,
                0,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
        );
    }

    #[test]
    fn material_shape_layout_pipeline_keys_are_distinct() {
        // Mesh / SdfInstance / GeoInstance / Particle 必须产生不同 key（shape target）
        let mesh = mk_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let sdf = mk_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::SdfInstance,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let geo = mk_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::GeoInstance,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let particle = mk_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::Particle,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        assert_ne!(mesh, sdf);
        assert_ne!(mesh, geo);
        assert_ne!(sdf, geo);
        assert_ne!(mesh, particle);
        assert_ne!(sdf, particle);
        assert_ne!(geo, particle);
        // Text target 不受 layout 影响
        assert_eq!(
            mk_key(
                MaterialTarget::Text,
                1,
                false,
                false,
                false,
                0,
                ShapeVertexLayout::Mesh,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
            mk_key(
                MaterialTarget::Text,
                1,
                false,
                false,
                false,
                0,
                ShapeVertexLayout::SdfInstance,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ),
        );
    }

    #[test]
    fn material_pipeline_key_no_overlap_sample_and_layout() {
        // 旧 bug: layout<<6 与 sample<<4 重叠，sample=4/Mesh 键 == sample=1/Sdf
        let mesh4 = mk_key(
            MaterialTarget::Shape,
            4,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let sdf1 = mk_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::SdfInstance,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        assert_ne!(mesh4, sdf1);
        let geo8 = mk_key(
            MaterialTarget::Shape,
            8,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::GeoInstance,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let mesh1 = mk_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        assert_ne!(geo8, mesh1);
    }

    #[test]
    fn default_material_shape_vertex_preserves_sample_interpolation_for_ssaa() {
        assert!(default_shape_vertex_wgsl(true, false).contains("@interpolate(linear, sample)"));
        assert!(!default_shape_vertex_wgsl(false, false).contains("@interpolate(linear, sample)"));
    }

    #[test]
    fn noflat_strips_linear_and_sample_but_keeps_flat() {
        // 长形式先剥（短形式是其前缀），flat 的 u32 varying 原样保留。
        let src = "@location(3) @interpolate(linear, sample) local_pos: vec2<f32>,\n    @location(4) @interpolate(flat) sdf_type: u32,";
        let out = strip_noflat_interpolation(src);
        assert!(out.contains("@location(3) local_pos: vec2<f32>"));
        assert!(out.contains("@location(4) @interpolate(flat) sdf_type: u32"));
        assert!(!out.contains("interpolate(linear"));
        assert!(!out.contains("sample)"));
        let plain = "@location(3) @interpolate(linear) local_pos: vec2<f32>";
        assert_eq!(
            strip_noflat_interpolation(plain),
            "@location(3) local_pos: vec2<f32>"
        );
    }

    #[test]
    fn noflat_strips_invariant_but_keeps_positions() {
        let src = "@invariant @builtin(position) position: vec4<f32>";
        assert_eq!(
            strip_noflat_invariant(src),
            "@builtin(position) position: vec4<f32>"
        );
        // 文字材质路径 noflat 下无 invariant，flat 保留。
        let shader = "fn material_main(in: MaterialInput) -> vec4<f32> { return in.color; }";
        let text = material_fragment_source(shader, MaterialTarget::Text, false, true).0;
        assert!(!text.contains("@invariant"));
        assert!(text.contains("@interpolate(flat) content_type: u32"));
        let text_plain = material_fragment_source(shader, MaterialTarget::Text, false, false).0;
        assert!(text_plain.contains("@invariant @builtin(position)"));
    }

    #[test]
    fn noflat_vertex_sources_have_no_linear_interpolation() {
        // 四条 local_pos 路径在 noflat 下全部退为默认插值，flat 不动。
        for src in [
            default_shape_vertex_wgsl(true, true),
            default_shape_vertex_wgsl(false, true),
            default_sdf_instance_vertex_wgsl(true, true),
            default_geo_instance_vertex_wgsl(true, true),
            default_geo_instance_vertex_wgsl(false, true),
            default_particle_vertex_wgsl(true, true),
        ] {
            assert!(!src.contains("@interpolate(linear"), "{}", &src[..80]);
            assert!(src.contains("@interpolate(flat)"));
        }
        // 非 noflat 路径行为不变。
        assert!(default_shape_vertex_wgsl(true, false).contains("@interpolate(linear, sample)"));
    }

    #[test]
    fn material_pipeline_key_distinguishes_surface_format() {
        // 不同 surface format（macOS Bgra8 vs 默认 Rgba8）必须产生不同 key，
        // 否则首窗同步后可能复用旧格式管线导致 Validation panic。
        let rgba = mk_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        let bgra = mk_key(
            MaterialTarget::Shape,
            1,
            false,
            false,
            false,
            0,
            ShapeVertexLayout::Mesh,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        );
        assert_ne!(rgba, bgra);
        assert_eq!(rgba, rgba);
    }

    #[test]
    fn material_shape_fragment_matches_ssaa_interpolation() {
        let shader = "fn material_main(in: MaterialInput) -> vec4<f32> { return in.color; }";
        assert!(
            material_fragment_source(shader, MaterialTarget::Shape, true, false)
                .0
                .contains("@interpolate(linear, sample) local_pos")
        );
        assert!(
            !material_fragment_source(shader, MaterialTarget::Shape, false, false)
                .0
                .contains("@interpolate(linear, sample) local_pos")
        );
        // noflat 下 ssaa 也不保留 linear/sample，flat 保留。
        let noflat = material_fragment_source(shader, MaterialTarget::Shape, true, true).0;
        assert!(!noflat.contains("@interpolate(linear"));
        assert!(noflat.contains("@interpolate(flat) sdf_type: u32"));
    }

    #[test]
    fn material_helpers_exist_for_both_targets() {
        let shader =
            "fn material_main(in: MaterialInput) -> vec4<f32> { return vireo_base_color(in); }";
        let shape = material_fragment_source(shader, MaterialTarget::Shape, false, false).0;
        let text = material_fragment_source(shader, MaterialTarget::Text, false, false).0;
        for src in [shape, text] {
            assert!(src.contains("fn vireo_base_sample(uv: vec2<f32>) -> vec4<f32>"));
            assert!(src.contains("fn vireo_base_color(in: MaterialInput) -> vec4<f32>"));
            assert!(src.contains("fn vireo_has_base_sample() -> bool"));
            assert!(src.contains("fn vireo_has_local_pos() -> bool"));
            assert!(src.contains("fn vireo_has_sdf_data() -> bool"));
        }
    }
}
