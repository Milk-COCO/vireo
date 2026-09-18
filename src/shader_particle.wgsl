struct QuadVertex {
    @location(0) corner: vec2<f32>,
};

struct ParticleInput {
    @location(1) pos_size: vec4<f32>,
    @location(2) vel_time: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) uv_rect: vec4<f32>,
    @location(5) fade_misc: vec4<f32>,
    @location(6) @interpolate(flat) xform: u32,
};

// 与 SDF instance 的 `VertexOutput` 逐字段一致，供 custom material FS 复用
//（SDF 字段恒 0，`vireo_apply_sdf` 早退）。
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
    time_secs: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;
@group(2) @binding(1) var<storage> polygon_edges: array<vec4<f32>>;

@vertex
fn vs_main(vertex: QuadVertex, instance: ParticleInput) -> VertexOutput {
    var out: VertexOutput;
    let t = vertex.corner * 0.5 + vec2<f32>(0.5);
    let birth = instance.vel_time.z;
    let life = instance.vel_time.w;
    let age = camera.time_secs - birth;
    let dead = age < 0.0 || (life > 0.0 && age > life);
    let pos = instance.pos_size.xy + instance.vel_time.xy * max(age, 0.0);
    let half_size = instance.pos_size.zw;
    let local_pos = mix(pos - half_size, pos + half_size, t);
    let world_pos = transforms[instance.xform] * vec3<f32>(local_pos, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    if (dead) {
        // 退化到裁剪区外：零光栅化、无 blend 副作用
        out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    }
    out.uv = mix(instance.uv_rect.xy, instance.uv_rect.zw, t);
    let fade_in = instance.fade_misc.x;
    let fade_out = instance.fade_misc.y;
    var fade = smoothstep(0.0, max(fade_in, 1e-6), max(age, 0.0));
    // fade_out <= 0 = 到 life 硬切（`dead` 分支已退化剔除，此处跳过）。
    // 注意：edge0 == edge1 的 smoothstep 未定义（实测某后端直接作废像素），必须守卫。
    if (life > 0.0 && fade_out > 0.0) {
        fade *= 1.0 - smoothstep(life - fade_out, life, age);
    }
    out.color = vec4<f32>(instance.color.rgb, instance.color.a * fade);
    out.sdf_params = vec4<f32>(0.0);
    out.local_pos = local_pos;
    out.sdf_type = 0u;
    out.sdf_feather = 0.0;
    out.sdf_extra = vec2<f32>(0.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(tex, tex_sampler, in.uv);
    return tex_color * in.color;
}
