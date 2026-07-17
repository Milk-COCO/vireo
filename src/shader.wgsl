struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) circle: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) circle: vec4<f32>,
    @location(3) local_pos: vec2<f32>,
};

struct Camera {
    projection: mat4x4<f32>,
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
    out.circle = in.circle;
    out.local_pos = in.position;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(tex, tex_sampler, in.uv);
    var out_color = tex_color * in.color;
    // SDF shape: circle.z = rx, circle.w = ry（>0 触发）
    if in.circle.z > 0.0 {
        let d = length((in.local_pos - in.circle.xy) / vec2(in.circle.z, in.circle.w));
        if d > 1.0 { discard; }
        let alpha = 1.0 - smoothstep(0.98, 1.0, d);
        out_color.a *= alpha;
    }
    return out_color;
}
