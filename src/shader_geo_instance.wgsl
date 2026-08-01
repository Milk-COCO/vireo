struct TemplateVertex {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct GeoInstanceInput {
    @location(2) color: vec4<f32>,
    @location(3) @interpolate(flat) transform_index: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;

@vertex
fn vs_main(vertex: TemplateVertex, instance: GeoInstanceInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = transforms[instance.transform_index] * vec3<f32>(vertex.position, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    out.uv = vertex.uv;
    out.color = instance.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(tex, tex_sampler, in.uv) * in.color;
}

@fragment
fn fs_stencil_only() -> @location(0) vec4<f32> {
    return vec4(0.0);
}
