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
    /// 图集里的 texel 尺寸 `(w<<16)|h`（0 = 与 quad 同尺寸，即精确 SDF 路径）
    @location(10) @interpolate(flat) entry_dim: u32,
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
    /// 该字形 quad 的设备像素尺寸 —— 层带偏移时把采样夹紧在自己的矩形内。
    @location(7) @interpolate(flat) glyph_size: vec2<f32>,
    /// 图集条目的 texel 尺寸；MSDF 时 `glyph_size / entry_texels` = 目标/生成 比例。
    @location(8) @interpolate(flat) entry_texels: vec2<f32>,
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

/// MSDF 图集：RGB 三通道各存一组边的距离，取中位数还原真实距离。
@group(0) @binding(5)
var msdf_atlas_texture: texture_2d<f32>;

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

    // UV 的推进量是**图集条目的 texel 尺寸**，不是 quad 的设备像素尺寸 ——
    // MSDF 的 quad 会按「目标/生成」比例缩放，两者不再相等；用错会把字放大 1/scale。
    let entry_u = select(
        vec2<u32>(width, height),
        vec2<u32>(in_vert.entry_dim >> 16u, in_vert.entry_dim & 0xffffu),
        in_vert.entry_dim != 0u,
    );
    uv = uv + entry_u * corner_position;
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
        case 3u: {
            dim = textureDimensions(msdf_atlas_texture);
            break;
        }
        default: {}
    }

    vert_output.content_type = content_type;
    vert_output.style_index = in_vert.style_index;
    vert_output.sdf_band = in_vert.sdf_band;
    vert_output.local01 = vec2<f32>(corner_position);
    vert_output.glyph_size = vec2<f32>(f32(width), f32(height));
    // entry_dim == 0 ⇒ 图集条目与 quad 同尺寸（精确 SDF / 覆盖度路径，行为不变）
    let ed = vec2<u32>(in_vert.entry_dim >> 16u, in_vert.entry_dim & 0xffffu);
    vert_output.entry_texels = select(
        vec2<f32>(f32(width), f32(height)),
        vec2<f32>(f32(ed.x), f32(ed.y)),
        in_vert.entry_dim != 0u,
    );

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

/// 三通道取**中位数**：每个通道存的是「该色边子集」的有符号距离，两条最近的边由两位
/// 掩码保证落在两个通道里，第三个通道更远 —— 有符号值的中位数就是真实距离
/// （内部同样成立；用 min 会被更远的通道拉走，内描边的环会整条消失）。
/// 诊断开关：`VIREO_SHADER_DEBUG_D=1` 时把解码出的距离画到红通道（d * 0.02，0.5 对应 25px）。
const DEBUG_D: bool = false;
/// 诊断开关：`VIREO_SHADER_DEBUG_COV=1` 时把最终覆盖率画到红通道（绿=层趟/填充趟标记）。
const DEBUG_COV: bool = false;

fn median3(v: vec3<f32>) -> f32 {
    return max(min(v.x, v.y), min(max(v.x, v.y), v.z));
}

/// 采样一个有符号距离（单位统一为**目标设备像素**）。
///
/// `d_range` 已由 CPU 换算到目标设备像素（`band_gen * quad_scale` / `range_gen * scale`），
/// 所以这里只做解码，不再乘任何比例 —— 用 `glyph_size / entry_texels` 推比例是错的：
/// quad 被 bounds 裁剪后会算小，描边会整块糊成矩形。
fn sample_d(uv: vec2<f32>, content_type: u32, d_range: f32) -> f32 {
    if (content_type == 3u) {
        let rgb = textureSampleLevel(msdf_atlas_texture, atlas_sampler, uv, 0.0).rgb;
        return (median3(rgb) * 2.0 - 1.0) * d_range;
    }
    return decode_sdf(
        textureSampleLevel(mask_atlas_texture, atlas_sampler, uv, 0.0).x,
        d_range,
    );
}

/// 一条样式的某一层：取区间 → 偏移采样 → 合成。
///
/// - `want_above`：这一趟画"填充之上"的层；层自己的 `above` 标志不匹配就跳过；
/// - `flags` bit0 = 有 cutoff（区间 `(cutoff, threshold]`），bit1 = 画在填充之上，
///   bit2 = **内阴影**（`字形内部 − 偏移后的覆盖率`，需要两个采样点）；
/// - 采样 UV **夹紧到本字形的图集矩形内**，否则带偏移的层会读到隔壁字形的数据（边缘细框）。
fn apply_layer(
    base: u32,
    content_type: u32,
    d_range: f32,
    msdf_scale: f32,
    at: vec2<f32>,
    glyph01: vec2<f32>,
    page: vec2<f32>,
    want_above: bool,
    entry_min: vec2<f32>,
    entry_max: vec2<f32>,
    out: vec4<f32>,
) -> vec4<f32> {
    let paint = text_layers[base + 1u];
    let flags = paint.w;
    let is_above = (flags & 2u) != 0u;
    if (is_above != want_above) {
        return out;
    }
    let geom = text_layers[base];
    let off = vec2<f32>(bitcast<f32>(geom.x), bitcast<f32>(geom.y));
    let soft = bitcast<f32>(geom.z);
    let thr = bitcast<f32>(geom.w);
    // 偏移量是**目标设备像素**，要换算成条目 texel：(off / msdf_scale) / dims。
    // dims 必须取**当前字形所在的图集**（MSDF 是独立 RGBA 图集，尺寸不同）——
    // 用错图集的尺寸会让偏移被放大/缩小，层带偏移的采样在条目边界被 clamp 成一条直线
    // （投影/发光看起来像"被矩形裁掉"）。
    let dims = select(
        vec2<f32>(textureDimensions(mask_atlas_texture)),
        vec2<f32>(textureDimensions(msdf_atlas_texture)),
        content_type == 3u,
    );
    let off_uv = off / (max(msdf_scale, 1e-3) * dims);
    let uv = clamp(at + off_uv, entry_min, entry_max);
    let d = sample_d(uv, content_type, d_range);
    // 距离场在条目边缘是**饱和值**（= d_range）。饱和区必须当作"远处"处理：
    // 否则当样式的阈值恰好 ≥ d_range（大描边 / 高 DPI 把外扩顶到带宽上限）时，
    // 整块 padding 会解出 d ≈ thr → 覆盖率 ≈ 0.5，肉眼就是"文字后面一个半透明矩形"。
    if (d >= d_range - 0.5) {
        return out;
    }
    if (DEBUG_D) {
        // 层趟诊断：红 = clamp(d*0.02+0.25)，绿 = thr/64，蓝 = soft/64
        return vec4<f32>(
            clamp(d * 0.02 + 0.25, 0.0, 1.0),
            clamp(thr / 64.0, 0.0, 1.0),
            clamp(soft / 64.0, 0.0, 1.0),
            1.0,
        );
    }
    var cov = coverage(d, thr, soft);
    if ((flags & 4u) != 0u) {
        // 内阴影：字形内部（未偏移）减去偏移后的覆盖率 —— 两个采样点，不是同一采样点的两个阈值
        let d0 = sample_d(at, content_type, d_range);
        cov = clamp(coverage(d0, 0.0, 0.0) - cov, 0.0, 1.0);
    }
    if ((flags & 1u) != 0u) {
        // 区间：再减掉 cutoff 之外的（`paint.z` 存 cutoff 的位模式）
        let cut = bitcast<f32>(paint.z);
        cov = cov - coverage(d, cut, soft);
    }
    cov = clamp(cov, 0.0, 1.0);
    if (DEBUG_COV) {
        // 层趟：红 = 覆盖率，绿 = thr/64
        return vec4<f32>(cov, clamp(thr / 64.0, 0.0, 1.0), 0.0, 1.0);
    }
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
        case 2u, 3u: {
            // 距离场字形（2 = 精确按尺寸 SDF，3 = MSDF）：每一层一趟 + 填充一趟（跨字形按层排序）。
            // 合成全程在**预乘**空间，出口再反预乘（blend state 是 ALPHA_BLENDING）。
            let pass_id = in_frag.style_index >> 24u;
            let style_id = in_frag.style_index & 0xffffffu;
            // MSDF：quad 与图集条目不是 1:1，距离要按「目标/生成」比例换算
            let msdf_scale = select(
                1.0,
                in_frag.glyph_size.x / max(in_frag.entry_texels.x, 1.0),
                in_frag.content_type == 3u,
            );
            let at = in_frag.uv;
            let entry_dims = select(
                vec2<f32>(textureDimensions(mask_atlas_texture)),
                vec2<f32>(textureDimensions(msdf_atlas_texture)),
                in_frag.content_type == 3u,
            );
            let entry0 = at - in_frag.local01 * in_frag.entry_texels / entry_dims;
            let entry_min = entry0 + 0.5 / entry_dims;
            let entry_max = entry0 + (in_frag.entry_texels - 0.5) / entry_dims;

            // 无样式（`style_id == 0`）：纯顶点色覆盖 —— 无样式文字也可以走 MSDF。
            if (style_id == 0u) {
                let d = sample_d(at, in_frag.content_type, in_frag.sdf_band);
                let cov = coverage(d, 0.0, 0.0);
                if (in_frag.content_type == 3u) {
                    return vec4<f32>(in_frag.color.rgb, in_frag.color.a * cov);
                }
                return vec4<f32>(0.0);
            }

            let sbase = (style_id - 1u) * 3u;
            let style_meta = text_styles[sbase];
            // `style_meta.x` 是**层下标**，而 `text_layers` 按 vec4 寻址（每层 4 个 vec4）
            let layer_base = style_meta.x * 4u;
            let layer_count = min(style_meta.y, 8u);

            var out = vec4<f32>(0.0);
            if (pass_id == 9u) {
                // 填充趟
                var filled = vec4<f32>(0.0);
                let fill_kind = style_meta.z;
                if (fill_kind != 4u) {
                    let d0 = sample_d(at, in_frag.content_type, in_frag.sdf_band);
                    if (DEBUG_D) {
                        // 填充趟在诊断时**不画**，这样读回的就是层趟的数据
                        return vec4<f32>(0.0);
                    }
                    let cov = select(
                        coverage(d0, 0.0, 0.0),
                        0.0,
                        d0 >= in_frag.sdf_band - 0.5,
                    );
                    if (fill_kind == 0u) {
                        let a = in_frag.color.a * cov;
                        filled = vec4<f32>(in_frag.color.rgb * a, a);
                    } else {
                        let painted = paint_color(
                            fill_kind,
                            text_styles[sbase + 1u],
                            text_styles[sbase + 2u],
                            in_frag.local01,
                            frag_pos.xy,
                        );
                        let a = painted.a * cov;
                        filled = vec4<f32>(painted.rgb * cov, a);
                    }
                    if (DEBUG_COV) {
                        // 填充趟：红 = 覆盖率，蓝 = 0.5 标记
                        return vec4<f32>(clamp(cov, 0.0, 1.0), 0.0, 0.5, 1.0);
                    }
                }
                out = over(out, filled);
            } else {
                // 单层趟：pass_id 1..8 = 填充之下第 pass_id-1 层；10..17 = 填充之上第 pass_id-10 层
                let want_above = pass_id >= 10u;
                let selected = select(pass_id - 1u, pass_id - 10u, want_above);
                if (selected < layer_count) {
                    out = apply_layer(
                        layer_base + selected * 4u,
                        in_frag.content_type,
                        in_frag.sdf_band,
                        msdf_scale,
                        at,
                        in_frag.local01,
                        frag_pos.xy,
                        want_above,
                        entry_min,
                        entry_max,
                        out,
                    );
                }
            }
            if (out.a <= 1e-6) {
                return vec4<f32>(0.0);
            }
            return vec4<f32>(out.rgb / out.a, out.a);
        }
        default: {
            return vec4<f32>(0.0);
        }
    }
}
