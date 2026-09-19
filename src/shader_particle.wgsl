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
    @location(7) motion2: vec4<f32>,
    @location(8) end_rgba_size: vec4<f32>,
    @location(9) atlas_seq: vec4<f32>,
    @location(10) fade_shape: vec4<f32>,
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
    let damp = instance.motion2.x;
    let grav = instance.motion2.yz;
    let tt = max(age, 0.0);
    // 闭式积分：v(t) = v0*e^(dt) + g*(e^(dt)-1)/d。
    // f32 下 (e^q-1-q)/d² 在 q→0 灾难性抵消（WGSL 无 expm1），故按无量纲 q=d*t
    // 分段：|q|<0.01 用 Taylor（k1 到 q²、k2 到 q²，截断误差 O(q³) 无缝），外侧用 exp。
    let q = damp * tt;
    var travel: vec2<f32>;
    if (abs(q) < 0.01) {
        travel = instance.vel_time.xy * (tt * (1.0 + q * (0.5 + q / 6.0)))
            + grav * (tt * tt * (0.5 + q * (1.0 / 6.0 + q / 24.0)));
    } else {
        let e = exp(q);
        travel = instance.vel_time.xy * ((e - 1.0) / damp)
            + grav * ((e - 1.0 - q) / (damp * damp));
    }
    let pos = instance.pos_size.xy + travel;
    // 寿命期插值进度（不朽无分母，k 恒 0 =  stay 起点）。
    let k = select(0.0, clamp(age / max(life, 1e-6), 0.0, 1.0), life > 0.0);
    let half_size = instance.pos_size.zw * mix(1.0, instance.end_rgba_size.a, k);
    let ang = instance.motion2.w;
    let cs = cos(ang);
    let sn = sin(ang);
    let off = vertex.corner * half_size;
    let local_pos = pos + vec2<f32>(off.x * cs - off.y * sn, off.x * sn + off.y * cs);
    let world_pos = transforms[instance.xform] * vec3<f32>(local_pos, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    if (dead) {
        // 退化到裁剪区外：零光栅化、无 blend 副作用
        out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    }
    out.uv = mix(instance.uv_rect.xy, instance.uv_rect.zw, t);
    // 图集序列：uv_rect 当整表按 cols×rows 切分（row 0 = v0 边）；
    // cols*rows<=1 或 fps<=0 即静态（None 烘焙零值，零成本）。
    let cols = instance.atlas_seq.x;
    let rows = instance.atlas_seq.y;
    let nframes = cols * rows;
    if (nframes > 1.0 && instance.atlas_seq.z > 0.0) {
        let frame = (floor(max(age, 0.0) * instance.atlas_seq.z) + instance.atlas_seq.w) % nframes;
        let col = frame % cols;
        let row = floor(frame / cols);
        let span = instance.uv_rect.zw - instance.uv_rect.xy;
        let cell = span / vec2<f32>(cols, rows);
        let cell_min = instance.uv_rect.xy + vec2<f32>(col, row) * cell;
        out.uv = mix(cell_min, cell_min + cell, t);
    }
    let fade_in = instance.fade_misc.x;
    let fade_out = instance.fade_misc.y;
    var fade = smoothstep(0.0, max(fade_in, 1e-6), max(age, 0.0));
    // fade_out <= 0 = 到 life 硬切（`dead` 分支已退化剔除，此处跳过）。
    // 注意：edge0 == edge1 的 smoothstep 未定义（实测某后端直接作废像素），必须守卫。
    if (life > 0.0 && fade_out > 0.0) {
        let mid = instance.fade_shape.x;
        if (mid >= 0.0) {
            // 线性三点 1→mid→0（中点在段中央；fade_in 段不受影响）。
            let t0 = life - fade_out;
            let mc = clamp(mid, 0.0, 1.0);
            let f = clamp((age - t0) / max(fade_out, 1e-6), 0.0, 1.0);
            if (f > 0.5) {
                fade *= mix(mc, 0.0, f * 2.0 - 1.0);
            } else {
                fade *= mix(1.0, mc, f * 2.0);
            }
        } else {
            fade *= 1.0 - smoothstep(life - fade_out, life, age);
        }
    }
    out.color = vec4<f32>(instance.color.rgb, instance.color.a * fade);
    if (instance.fade_misc.w > 0.5) {
        out.color = vec4<f32>(
            mix(instance.color.rgb, instance.end_rgba_size.rgb, k),
            instance.color.a * fade,
        );
    }
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
