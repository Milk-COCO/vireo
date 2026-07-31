struct QuadVertex {
    @location(0) corner: vec2<f32>,
};

struct InstanceInput {
    @location(1) bounds: vec4<f32>,
    @location(2) uv_bounds: vec4<f32>,
    @location(3) uv_rect: vec4<f32>,
    @location(4) color: vec4<f32>,
    @location(5) sdf_params: vec4<f32>,
    @location(6) sdf_extra: vec2<f32>,
    @location(7) @interpolate(flat) sdf_type: u32,
    @location(8) sdf_feather: f32,
    @location(9) @interpolate(flat) transform_index: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) sdf_params: vec4<f32>,
    @location(3) @interpolate(linear, sample) local_pos: vec2<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
    @location(6) sdf_extra: vec2<f32>,
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

@vertex
fn vs_main(vertex: QuadVertex, instance: InstanceInput) -> VertexOutput {
    var out: VertexOutput;
    let t = vertex.corner * 0.5 + vec2<f32>(0.5);
    let local_pos = mix(instance.bounds.xy, instance.bounds.zw, t);
    let world_pos = transforms[instance.transform_index] * vec3<f32>(local_pos, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    let uv_t = (local_pos - instance.uv_bounds.xy) / (instance.uv_bounds.zw - instance.uv_bounds.xy);
    out.uv = mix(instance.uv_rect.xy, instance.uv_rect.zw, uv_t);
    out.color = instance.color;
    out.sdf_params = instance.sdf_params;
    out.local_pos = local_pos;
    out.sdf_type = instance.sdf_type;
    out.sdf_feather = instance.sdf_feather;
    out.sdf_extra = instance.sdf_extra;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(tex, tex_sampler, in.uv);
    var out_color = tex_color * in.color;
    let feather = in.sdf_feather / camera.dpi_scale;
    var d: f32;
    switch in.sdf_type {
        case 1u: {
            d = length((in.local_pos - in.sdf_params.xy) / in.sdf_params.zw);
            if d >= 1.0 { discard; }
            if feather > 0.0 {
                let k = feather / max(in.sdf_params.z, in.sdf_params.w);
                out_color.a *= 1.0 - smoothstep(1.0 - k, 1.0, d);
            }
        }
        case 2u: {
            let r = in.sdf_extra.x;
            d = length(max(abs(in.local_pos - in.sdf_params.xy) - (in.sdf_params.zw - vec2<f32>(r)), vec2<f32>(0.0))) - r;
            if feather > 0.0 { if d >= feather { discard; } out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
        case 3u: {
            let a = in.sdf_params.xy; let b = in.sdf_params.zw;
            let ab = b - a;
            let t = clamp(dot(in.local_pos - a, ab) / max(dot(ab, ab), 1e-8), 0.0, 1.0);
            d = length(in.local_pos - (a + t * ab)) - in.sdf_extra.x;
            if feather > 0.0 { if d >= feather { discard; } out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
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
            if feather > 0.0 { if d >= feather { discard; } out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
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
            if feather > 0.0 { if d >= feather { discard; } out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
        case 6u: {
            let start = u32(in.sdf_params.x); let count = u32(in.sdf_params.y);
            var d_max = -1e10; var d_min = 1e10; var inside = true;
            for (var i = start; i < start + count; i++) { let e = polygon_edges[i]; let sd = dot(in.local_pos, e.xy) - e.z; if sd < -0.0001 { inside = false; } d_max = max(d_max, -sd); d_min = min(d_min, sd); }
            d = select(d_max, -d_min, inside);
            if feather > 0.0 { if d >= feather { discard; } out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
        default: {
            let start = u32(in.sdf_params.x); let count = u32(in.sdf_params.y); let h = in.sdf_params.z;
            d = 1e10;
            for (var i = start; i < start + count; i++) { let seg = polygon_edges[i]; let a = seg.xy; let b = seg.zw; let ab = b - a; let t = clamp(dot(in.local_pos - a, ab) / max(dot(ab, ab), 1e-8), 0.0, 1.0); d = min(d, length(in.local_pos - (a + t * ab))); }
            d -= h;
            if feather > 0.0 { if d >= feather { discard; } out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
            else if d > 0.0 { discard; }
        }
    }
    return out_color;
}
