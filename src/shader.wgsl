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
    @location(3) @interpolate(linear, sample) local_pos: vec2<f32>,
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
    out.circle = in.circle;
    out.local_pos = in.position;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(tex, tex_sampler, in.uv);
    var out_color = tex_color * in.color;
    // SDF shape (circle.z > 0 触发)
    //   circle.w > 0: circle/ellipse, feather=uv.x (edge at d=1)
    //   circle.w < 0: rounded rect, r=uv.x (edge at d=0, no feather)
    if in.circle.z > 0.0 {
        let is_circle = in.circle.w > 0.0;
        let circle_d = length((in.local_pos - in.circle.xy) / vec2(in.circle.z, in.circle.w));
        let hw = in.circle.z; let hh = -in.circle.w; let r = in.uv.x;
        let rect_d = length(max(abs(in.local_pos - in.circle.xy) - vec2(hw - r, hh - r), vec2(0.0))) - r;
        if is_circle {
            let feather = in.uv.x / camera.dpi_scale;
            if feather > 0.0 {
                let r_max = max(in.circle.z, in.circle.w);
                let k = feather / r_max;
                out_color.a *= 1.0 - smoothstep(1.0 - k, 1.0, circle_d);
            } else {
                if circle_d > 1.0 { out_color.a = 0.0; }
            }
        } else {
            let feather = in.uv.y / camera.dpi_scale;
            if feather > 0.0 {
                out_color.a *= 1.0 - smoothstep(0.0, feather, rect_d);
            } else {
                if rect_d > 0.0 { out_color.a = 0.0; }
            }
        }
    }
    return out_color;
}
