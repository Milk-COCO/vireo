struct TemplateVertex {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct GeoInstanceInput {
    @location(2) color: vec4<f32>,
    @location(3) @interpolate(flat) transform_index: u32,
};

// 完整 VertexOutput（兼容 material FS）。geo shape 无 SDF 字段，全部填 0
// （vireo_apply_sdf 在 sdf_type==0 时早退）。local_pos = 模板原始 position，
// 仅供 fragment-only material 参考；非 SDF-aware material 通常不用。
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
    out.sdf_params = vec4<f32>(0.0);
    out.local_pos = vertex.position;
    out.sdf_type = 0u;
    out.sdf_feather = 0.0;
    out.sdf_extra = vec2<f32>(0.0);
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
