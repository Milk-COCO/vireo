struct VertexInput {
    @builtin(vertex_index) vertex_idx: u32,
    @location(0) pos: vec2<i32>,
    @location(1) dim: u32,
    @location(2) uv: u32,
    @location(3) color: u32,
    @location(4) content_type_with_srgb: u32,
    @location(5) depth: f32,
    @location(6) @interpolate(flat) transform_index: u32,
    @location(7) base_uv_rect: vec4<f32>,
    /// 样式表下标 + 1（0 = 无样式）
    @location(8) @interpolate(flat) style_index: u32,
    /// 距离场带宽（设备像素），仅 content_type == Sdf 时有意义
    @location(9) sdf_band: f32,
}

struct VertexOutput {
    @invariant @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) content_type: u32,
    @location(3) base_uv: vec2<f32>,
    @location(4) @interpolate(flat) style_index: u32,
    @location(5) sdf_band: f32,
    /// 字形 quad 内的归一化坐标（0..1），逐字形渐变/纹理填充用。
    @location(6) local01: vec2<f32>,
}

struct Params {
    screen_resolution: vec2<u32>,
    /// 逻辑像素 → 设备像素
    dpi_scale: f32,
    /// 本帧样式数量（0 = 无样式）
    style_count: u32,
};

@group(0) @binding(0)
var color_atlas_texture: texture_2d<f32>;

@group(0) @binding(1)
var mask_atlas_texture: texture_2d<f32>;

@group(0) @binding(2)
var atlas_sampler: sampler;

@group(0) @binding(3)
var base_texture: texture_2d<f32>;

@group(0) @binding(4)
var base_sampler: sampler;

@group(1) @binding(0)
var<uniform> params: Params;

/// 样式表：每条 3 个 vec4 —— `(layer_first, layer_count, fill_kind, 0)` + `fill_color` + 保留。
@group(1) @binding(1)
var<storage> text_styles: array<vec4<u32>>;

/// 图层表：每层 4 个 vec4 —— `geom` + `paint` + `color` + `aux`。
@group(1) @binding(2)
var<storage> text_layers: array<vec4<u32>>;

/// 渐变停点表：每停点 2 个 vec4 —— `(at,0,0,0)` + 预乘 rgba。
@group(1) @binding(3)
var<storage> text_stops: array<vec4<u32>>;

@group(2) @binding(0)
var<storage> transforms: array<mat3x3<f32>>;

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    } else {
        return pow((c + 0.055) / 1.055, 2.4);
    }
}

@vertex
fn vs_main(in_vert: VertexInput) -> VertexOutput {
    var pos = in_vert.pos;
    let width = in_vert.dim & 0xffffu;
    let height = (in_vert.dim & 0xffff0000u) >> 16u;
    let color = in_vert.color;
    var uv = vec2<u32>(in_vert.uv & 0xffffu, (in_vert.uv & 0xffff0000u) >> 16u);
    let v = in_vert.vertex_idx;

    let corner_position = vec2<u32>(
        in_vert.vertex_idx & 1u,
        (in_vert.vertex_idx >> 1u) & 1u,
    );

    let corner_offset = vec2<u32>(width, height) * corner_position;

    uv = uv + corner_offset;
    pos = pos + vec2<i32>(corner_offset);

    var vert_output: VertexOutput;

    let transform = transforms[in_vert.transform_index];
    let world_pos = transform * vec3<f32>(vec2<f32>(pos), 1.0);

    vert_output.position = vec4<f32>(
        2.0 * world_pos.xy / vec2<f32>(params.screen_resolution) - 1.0,
        in_vert.depth,
        1.0,
    );

    vert_output.position.y *= -1.0;

    let content_type = in_vert.content_type_with_srgb & 0xffffu;
    let srgb = (in_vert.content_type_with_srgb & 0xffff0000u) >> 16u;

    switch srgb {
        case 0u: {
            vert_output.color = vec4<f32>(
                f32((color & 0x00ff0000u) >> 16u) / 255.0,
                f32((color & 0x0000ff00u) >> 8u) / 255.0,
                f32(color & 0x000000ffu) / 255.0,
                f32((color & 0xff000000u) >> 24u) / 255.0,
            );
        }
        case 1u: {
            vert_output.color = vec4<f32>(
                srgb_to_linear(f32((color & 0x00ff0000u) >> 16u) / 255.0),
                srgb_to_linear(f32((color & 0x0000ff00u) >> 8u) / 255.0),
                srgb_to_linear(f32(color & 0x000000ffu) / 255.0),
                f32((color & 0xff000000u) >> 24u) / 255.0,
            );
        }
        default: {}
    }

    var dim: vec2<u32> = vec2(0u);
    switch content_type {
        case 0u: {
            dim = textureDimensions(color_atlas_texture);
            break;
        }
        // SDF 与覆盖度共用 mask 图集
        case 1u, 2u: {
            dim = textureDimensions(mask_atlas_texture);
            break;
        }
        default: {}
    }

    vert_output.content_type = content_type;
    vert_output.style_index = in_vert.style_index;
    vert_output.sdf_band = in_vert.sdf_band;
    vert_output.local01 = vec2<f32>(corner_position);

    vert_output.uv = vec2<f32>(uv) / vec2<f32>(dim);
    let base_uv0 = in_vert.base_uv_rect.xy;
    let base_uv1 = in_vert.base_uv_rect.zw;
    vert_output.base_uv = base_uv0 + vec2<f32>(corner_position) * (base_uv1 - base_uv0);

    return vert_output;
}

/// 覆盖率：`soft == 0` 时是 1px 解析 AA；否则用误差函数（距离场下"高斯模糊过的边缘"
/// 精确就是 erf，所以软投影/发光一次采样就够，不需要多 tap）。
fn coverage(d: f32, threshold: f32, soft: f32) -> f32 {
    let x = d - threshold;
    if soft <= 0.5 {
        return clamp(0.5 - x, 0.0, 1.0);
    }
    // erf 的 tanh 近似（误差 ~1e-3，对阴影足够）
    let u = x / (soft * 1.41421356);
    let t = tanh(1.1283791670955126 * u + 0.147 * u * u * u);
    return clamp(0.5 * (1.0 - t), 0.0, 1.0);
}

/// 预乘 over：`src` 盖在 `dst` 上。
fn over(dst: vec4<f32>, src: vec4<f32>) -> vec4<f32> {
    return src + dst * (1.0 - src.a);
}

fn decode_sdf(q: f32, band: f32) -> f32 {
    return (q * 2.0 - 1.0) * band;
}

/// 一条样式的某一层：取阈 → 偏移采样 → 合成（`band` 是字形距离场的带宽）。
fn apply_layer(
    base: u32,
    band: f32,
    at: vec2<f32>,
    glyph01: vec2<f32>,
    page: vec2<f32>,
    out: vec4<f32>,
) -> vec4<f32> {
    let geom = text_layers[base];
    let off = vec2<f32>(bitcast<f32>(geom.x), bitcast<f32>(geom.y));
    let soft = bitcast<f32>(geom.z);
    let thr = bitcast<f32>(geom.w);
    let dims = vec2<f32>(textureDimensions(mask_atlas_texture));
    let uv = at + off / dims;
    let d = decode_sdf(textureSampleLevel(mask_atlas_texture, atlas_sampler, uv, 0.0).x, band);
    let cov = coverage(d, thr, soft);
    let paint = text_layers[base + 1u];
    let col = paint_color(paint.x, text_layers[base + 2u], text_layers[base + 3u], glyph01, page);
    let a = col.a * cov;
    let src = vec4<f32>(col.rgb * cov, a);
    if paint.y == 1u {
        return out + src; // Add
    }
    return over(out, src);
}

fn unpack(v: vec4<u32>) -> vec4<f32> {
    return vec4<f32>(bitcast<f32>(v.x), bitcast<f32>(v.y), bitcast<f32>(v.z), bitcast<f32>(v.w));
}

/// 沿色带取色（预乘）——与形状渐变的 `vireo_ramp_color` 同一套布局。
fn ramp_at(first: u32, count: u32, t: f32) -> vec4<f32> {
    if (count == 0u) { return vec4<f32>(0.0); }
    var idx = 0u;
    for (var k = 1u; k < count; k = k + 1u) {
        if (bitcast<f32>(text_stops[first + k * 2u].x) <= t) { idx = k; }
    }
    let next = min(idx + 1u, count - 1u);
    let a_at = bitcast<f32>(text_stops[first + idx * 2u].x);
    let b_at = bitcast<f32>(text_stops[first + next * 2u].x);
    let span = b_at - a_at;
    let f = select(0.0, clamp((t - a_at) / span, 0.0, 1.0), abs(span) > 1e-6);
    let mixed = mix(
        unpack(text_stops[first + idx * 2u + 1u]),
        unpack(text_stops[first + next * 2u + 1u]),
        f,
    );
    if (mixed.a <= 1e-6) { return vec4<f32>(0.0); }
    return mixed; // 已是预乘
}

/// 一种填充的预乘颜色（`kind`: 0 默认色 / 1 纯色 / 2 渐变 / 3 纹理）。
fn paint_color(
    kind: u32,
    color: vec4<u32>,
    aux: vec4<u32>,
    glyph01: vec2<f32>,
    page: vec2<f32>,
) -> vec4<f32> {
    if (kind == 1u) {
        return vec4<f32>(
            bitcast<f32>(color.x),
            bitcast<f32>(color.y),
            bitcast<f32>(color.z),
            bitcast<f32>(color.w),
        );
    }
    if (kind == 2u) {
        let center = vec2<f32>(bitcast<f32>(color.x), bitcast<f32>(color.y));
        let span = bitcast<f32>(color.z);
        let dir = vec2<f32>(bitcast<f32>(aux.x), bitcast<f32>(aux.y));
        let first = aux.z;
        let count = aux.w;
        var t = 0.0;
        if (span > 0.0) {
            t = clamp(dot(page - center, dir) / span + 0.5, 0.0, 1.0);
        } else {
            // 逐字形：字形框内 0..1
            t = clamp(dot(glyph01 - vec2<f32>(0.5), dir) + 0.5, 0.0, 1.0);
        }
        return ramp_at(first, count, t);
    }
    if (kind == 3u) {
        // 纹理：uv 矩形 + 页面矩形（半宽/半高为 0 ⇒ 逐字形）
        let uv_rect = vec4<f32>(
            bitcast<f32>(color.x),
            bitcast<f32>(color.y),
            bitcast<f32>(color.z),
            bitcast<f32>(color.w),
        );
        let center = vec2<f32>(bitcast<f32>(aux.x), bitcast<f32>(aux.y));
        let half = vec2<f32>(bitcast<f32>(aux.z), bitcast<f32>(aux.w));
        var p01 = glyph01;
        if (half.x > 0.0 && half.y > 0.0) {
            p01 = clamp((page - center) / (half * 2.0) + vec2<f32>(0.5), vec2<f32>(0.0), vec2<f32>(1.0));
        }
        let uv = mix(uv_rect.xy, uv_rect.zw, p01);
        return textureSampleLevel(base_texture, base_sampler, uv, 0.0);
    }
    return vec4<f32>(0.0);
}

@fragment
fn fs_main(in_frag: VertexOutput) -> @location(0) vec4<f32> {
    // 片元在帧缓冲里的像素坐标（页面空间渐变用）
    let frag_pos = in_frag.position;
    switch in_frag.content_type {
        case 0u: {
            return textureSampleLevel(color_atlas_texture, atlas_sampler, in_frag.uv, 0.0);
        }
        case 1u: {
            return vec4<f32>(in_frag.color.rgb, in_frag.color.a * textureSampleLevel(mask_atlas_texture, atlas_sampler, in_frag.uv, 0.0).x);
        }
        case 2u: {
            // 距离场字形：逐层取阈 + 最后 fill。合成全程在**预乘**空间，出口再反预乘
            // （blend state 是 ALPHA_BLENDING，着色器必须输出直通 alpha）。
            let sbase = (in_frag.style_index - 1u) * 3u;
            let style_meta = text_styles[sbase];
            let layer_first = style_meta.x;
            let layer_count = min(style_meta.y, 8u);
            let d0 = decode_sdf(
                textureSampleLevel(mask_atlas_texture, atlas_sampler, in_frag.uv, 0.0).x,
                in_frag.sdf_band,
            );
            var out = vec4<f32>(0.0);
            var i = 0u;
            loop {
                if i >= layer_count { break; }
                out = apply_layer(layer_first + i * 4u, in_frag.sdf_band, in_frag.uv, in_frag.local01, frag_pos.xy, out);
                i = i + 1u;
            }
            // fill（style_meta.z: 0 = 用顶点色，1/2/3 = 纯色/渐变/纹理，4 = 不填）
            let fill_kind = style_meta.z;
            let cov = coverage(d0, 0.0, 0.0);
            if fill_kind == 0u {
                let a = in_frag.color.a * cov;
                out = over(out, vec4<f32>(in_frag.color.rgb * a, a));
            } else if fill_kind != 4u {
                let painted = paint_color(
                    fill_kind,
                    text_styles[sbase + 1u],
                    text_styles[sbase + 2u],
                    in_frag.local01,
                    frag_pos.xy,
                );
                let a = painted.a * cov;
                out = over(out, vec4<f32>(painted.rgb * cov, a));
            }
            if out.a <= 1e-6 {
                return vec4<f32>(0.0);
            }
            return vec4<f32>(out.rgb / out.a, out.a);
        }
        default: {
            return vec4<f32>(0.0);
        }
    }
}
