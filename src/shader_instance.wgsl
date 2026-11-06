struct QuadVertex {
    @location(0) corner: vec2<f32>,
};

struct InstanceData {
    bounds: vec4<f32>,
    uv_bounds: vec4<f32>,
    uv_rect: vec4<f32>,
    color: vec4<f32>,
    sdf_params: vec4<f32>,
    sdf_extra: vec2<f32>,
    sdf_type: u32,
    sdf_feather: f32,
    transform_index: u32,
    /// 渐变索引：`0` = 无渐变（旧行为），否则 `色带基址 + 1`。
    /// 跨全部 SDF 类型通用（曾经的填充位，布局/大小不变）。
    grad: u32,
    _pad2: vec2<u32>,
};

@group(2) @binding(2) var<storage> instances: array<InstanceData>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) sdf_params: vec4<f32>,
    @location(3) @interpolate(linear, sample) local_pos: vec2<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
    @location(6) sdf_extra: vec2<f32>,
    /// kind 0（轴向）的渐变坐标：位置仿射函数，顶点算 + 插值 = 逐像素精确。
    /// 用默认（透视）插值而非 `linear`：正交投影下 w 恒 1，两者逐位一致，而弱 GLES 的
    /// noflat 变体会剥掉 `linear` 修饰（见 `strip_noflat_interpolation`）。
    @location(7) grad_t: f32,
    /// 渐变索引（flat：索引不可插值），片元用它取色带基址。
    @location(8) @interpolate(flat) grad: u32,
};

struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;
@group(2) @binding(1) var<storage> polygon_edges: array<vec4<f32>>;
// 渐变色带表（VS 读轴向行，FS 读停点）。布局与 CPU 侧 `encode_ramp` 严格对应：
//   base + 0       : (count, kind, 0, 0)   —— kind 0 = 轴向，1 = 弧长，2 = 线段参数
//   base + 1       : (cosθ, sinθ, 0, 0)    —— 仅 kind 0；其余填 0
//   base + 2 + 2k  : (at_k, 0, 0, 0)       —— kind0/2 归一化 0..1；kind1 为 px 弧长
//   base + 3 + 2k  : 预乘 rgba_k
@group(2) @binding(3) var<storage> ramp_table: array<vec4<f32>>;

/// 沿色带坐标 `t` 取色：预乘空间插值，再反预乘回直通 alpha（blend state 是 ALPHA_BLENDING）。
///
/// 预乘的理由：两端 alpha 差很大时（例如 0.9 → 0.0）在直通空间插值会在中段压暗，
/// 出现一条暗边。CPU 侧 DP 简化也按预乘误差度量，所以这里与简化是同一套度量。
fn vireo_ramp_color(base: u32, t: f32) -> vec4<f32> {
    let count = u32(ramp_table[base].x);
    if (count == 0u) { return vec4<f32>(1.0); }
    var idx = 0u;
    for (var k = 1u; k < count; k++) {
        if (ramp_table[base + 2u + 2u * k].x <= t) { idx = k; }
    }
    let next = min(idx + 1u, count - 1u);
    let a_at = ramp_table[base + 2u + 2u * idx].x;
    let b_at = ramp_table[base + 2u + 2u * next].x;
    let span = b_at - a_at;
    let f = select(0.0, clamp((t - a_at) / span, 0.0, 1.0), abs(span) > 1e-6);
    let mixed = mix(ramp_table[base + 3u + 2u * idx], ramp_table[base + 3u + 2u * next], f);
    if (mixed.a <= 1e-6) { return vec4<f32>(0.0); }
    return vec4<f32>(mixed.rgb / mixed.a, mixed.a);
}

@vertex
fn vs_main(vertex: QuadVertex, @builtin(instance_index) inst_idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let inst = instances[inst_idx];
    let t = vertex.corner * 0.5 + vec2<f32>(0.5);
    let local_pos = mix(inst.bounds.xy, inst.bounds.zw, t);
    let world_pos = transforms[inst.transform_index] * vec3<f32>(local_pos, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    let uv_t = (local_pos - inst.uv_bounds.xy) / (inst.uv_bounds.zw - inst.uv_bounds.xy);
    out.uv = mix(inst.uv_rect.xy, inst.uv_rect.zw, uv_t);
    out.color = inst.color;
    out.sdf_params = inst.sdf_params;
    out.local_pos = local_pos;
    out.sdf_type = inst.sdf_type;
    out.sdf_feather = inst.sdf_feather;
    out.sdf_extra = inst.sdf_extra;
    out.grad = inst.grad;

    // kind 0（轴向）：渐变坐标 = 沿 `(cosθ, sinθ)` 在形状盒上的归一化投影。
    // `uv_bounds` 恒为形状本地盒（各 SDF 类型的发射器都这么传），所以这里不需要知道类型。
    // 坐标是位置的仿射函数 ⇒ 顶点算 + 插值 = 逐像素精确。
    // kind 1/2 的 `grad_t` 不被使用（它们的坐标在各 case 内从几何直接得出）。
    var grad_t = 0.0;
    if (inst.grad > 0u) {
        let base = inst.grad - 1u;
        let d = ramp_table[base + 1u].xy;
        let size = inst.uv_bounds.zw - inst.uv_bounds.xy;
        // 盒沿轴的半跨度（支撑函数）×2。为 0 时退化为中点色，不产生 NaN。
        let e = size.x * abs(d.x) + size.y * abs(d.y);
        grad_t = clamp(dot(local_pos - (inst.uv_bounds.xy + inst.uv_bounds.zw) * 0.5, d) / max(e, 1e-6) + 0.5, 0.0, 1.0);
    }
    out.grad_t = grad_t;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(tex, tex_sampler, in.uv);
    var out_color = tex_color * in.color;
    let feather = in.sdf_feather / camera.dpi_scale;
    var d: f32;
    // 边缘覆盖度：各 case 只判 `discard` 并把它算出来，末尾统一乘到 alpha 上。
    var cov = 1.0;
    // kind 1（折线弧长 / px）与 kind 2（线段参数 / 0..1）的坐标，由对应 case 写入。
    var arc_t = 0.0;
    var seg_t = 0.0;
    switch in.sdf_type {
        case 1u: {
            d = length((in.local_pos - in.sdf_params.xy) / in.sdf_params.zw);
            if d >= 1.0 { discard; }
            if feather > 0.0 {
                let k = feather / max(in.sdf_params.z, in.sdf_params.w);
                cov = 1.0 - smoothstep(1.0 - k, 1.0, d);
            }
        }
        case 2u: {
            let r = in.sdf_extra.x;
            d = length(max(abs(in.local_pos - in.sdf_params.xy) - (in.sdf_params.zw - vec2<f32>(r)), vec2<f32>(0.0))) - r;
            if feather > 0.0 { if d >= feather { discard; } cov = 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
        case 3u: {
            let a = in.sdf_params.xy; let b = in.sdf_params.zw;
            let ab = b - a;
            let t = clamp(dot(in.local_pos - a, ab) / max(dot(ab, ab), 1e-8), 0.0, 1.0);
            seg_t = t;
            d = length(in.local_pos - (a + t * ab)) - in.sdf_extra.x;
            if feather > 0.0 { if d >= feather { discard; } cov = 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
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
            if feather > 0.0 { if d >= feather { discard; } cov = 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
        case 5u: {
            let center = in.sdf_params.xy; let r = in.sdf_params.z;
            let to_p = in.local_pos - center; let d_circle = length(to_p) - r;
            let sa = in.sdf_extra.x; let ea = in.sdf_extra.y; let raw_span = ea - sa;
            let ccw_span = select(raw_span, raw_span + 6.283185307, raw_span < 0.0);
            let d_start = dot(to_p, vec2(sin(sa), -cos(sa)));
            let d_end = dot(to_p, vec2(-sin(ea), cos(ea)));
            let d_edge = select(-max(dot(to_p, vec2(sin(ea), -cos(ea))), dot(to_p, vec2(-sin(sa), cos(sa)))), max(d_start, d_end), ccw_span <= 3.14159265);
            d = max(d_circle, d_edge);
            if feather > 0.0 { if d >= feather { discard; } cov = 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
        case 6u: {
            let start = u32(in.sdf_params.x); let count = u32(in.sdf_params.y);
            var d_max = -1e10; var d_min = 1e10; var inside = true;
            for (var i = start; i < start + count; i++) { let e = polygon_edges[i]; let sd = dot(in.local_pos, e.xy) - e.z; if sd < -0.0001 { inside = false; } d_max = max(d_max, -sd); d_min = min(d_min, sd); }
            d = select(d_max, -d_min, inside);
            if feather > 0.0 { if d >= feather { discard; } cov = 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
        case 7u: {
            // 折线：`sdf_params = (edge_start, edge_count, half_thickness, 0)`
            // 渐变坐标 = 最近边的**弧长**（px），kind 1。
            let start = u32(in.sdf_params.x); let count = u32(in.sdf_params.y); let h = in.sdf_params.z;
            d = 1e10;
            if (in.grad > 0u) {
                var arc = 0.0;
                for (var i = start; i < start + count; i++) {
                    let seg = polygon_edges[i]; let a = seg.xy; let b = seg.zw; let ab = b - a;
                    let ab_len2 = max(dot(ab, ab), 1e-8);
                    let t = clamp(dot(in.local_pos - a, ab) / ab_len2, 0.0, 1.0);
                    let dd = length(in.local_pos - (a + t * ab));
                    if (dd < d) { d = dd; arc_t = arc + t * sqrt(ab_len2); }
                    arc += sqrt(ab_len2);
                }
            } else {
                for (var i = start; i < start + count; i++) { let seg = polygon_edges[i]; let a = seg.xy; let b = seg.zw; let ab = b - a; let t = clamp(dot(in.local_pos - a, ab) / max(dot(ab, ab), 1e-8), 0.0, 1.0); d = min(d, length(in.local_pos - (a + t * ab))); }
            }
            d -= h;
            if feather > 0.0 { if d >= feather { discard; } cov = 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
        default: {
            // 未知类型：不画。否则参数会被当成边索引读（`count` 可能上千 → 卡死 GPU）。
            discard;
        }
    }
    // 渐变：**唯一**应用点（8 种形状走同一条路）。色带**替换**实例色，贴图仍乘进来。
    if (in.grad > 0u) {
        let base = in.grad - 1u;
        let kind = u32(ramp_table[base].y);
        var t = in.grad_t;
        if (kind == 1u) { t = arc_t; }
        if (kind == 2u) { t = seg_t; }
        out_color = tex_color * vireo_ramp_color(base, t);
    }
    if feather > 0.0 { out_color.a *= cov; }
    return out_color;
}
