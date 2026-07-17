struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) sdf_params: vec4<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) sdf_params: vec4<f32>,
    @location(3) @interpolate(linear, sample) local_pos: vec2<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
};

struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = camera.projection * vec4<f32>(in.position, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.sdf_params = in.sdf_params;
    out.local_pos = in.position;
    out.sdf_type = in.sdf_type;
    out.sdf_feather = in.sdf_feather;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(tex, tex_sampler, in.uv);
    var out_color = tex_color * in.color;
    if in.sdf_type > 0u {
        let feather = in.sdf_feather / camera.dpi_scale;
        if in.sdf_type == 1u {
            let d = length((in.local_pos - in.sdf_params.xy) / vec2(in.sdf_params.z, in.sdf_params.w));
            if feather > 0.0 { let k = feather / max(in.sdf_params.z, in.sdf_params.w); out_color.a *= 1.0 - smoothstep(1.0 - k, 1.0, d); }
            else { if d > 1.0 { out_color.a = 0.0; } }
        } else if in.sdf_type == 2u {
            let hw = in.sdf_params.z; let hh = in.sdf_params.w; let r = in.uv.x;
            let d = length(max(abs(in.local_pos - in.sdf_params.xy) - vec2(hw - r, hh - r), vec2(0.0))) - r;
            if feather > 0.0 { out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
            else { if d > 0.0 { out_color.a = 0.0; } }
        } else if in.sdf_type == 3u {
            let a = in.sdf_params.xy; let b = in.sdf_params.zw;
            let ab = b - a; let t = clamp(dot(in.local_pos - a, ab) / dot(ab, ab), 0.0, 1.0);
            let d = length(in.local_pos - (a + t * ab)) - in.uv.x;
            if feather > 0.0 { out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
            else { if d > 0.0 { out_color.a = 0.0; } }
        } else {
            // triangle (ty==4.0)
            let a = in.sdf_params.xy; let b = in.sdf_params.zw; let c = in.uv;
            let n_ab = normalize(vec2(-(b.y - a.y), b.x - a.x));
            let n_bc = normalize(vec2(-(c.y - b.y), c.x - b.x));
            let n_ca = normalize(vec2(-(a.y - c.y), a.x - c.x));
            let d_ab = dot(in.local_pos - a, n_ab); let d_bc = dot(in.local_pos - b, n_bc); let d_ca = dot(in.local_pos - c, n_ca);
            let inside = d_ab > -0.0001 && d_bc > -0.0001 && d_ca > -0.0001;
            let d = select(max(-d_ab, max(-d_bc, -d_ca)), -min(d_ab, min(d_bc, d_ca)), inside);
            if feather > 0.0 { out_color.a *= 1.0 - smoothstep(0.0, feather, d); }
            else { if d > 0.0 { out_color.a = 0.0; } }
        }
    }
    return out_color;
}
