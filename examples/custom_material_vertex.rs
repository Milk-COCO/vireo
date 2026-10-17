//! 自定义顶点着色器（custom VS）示例：演示实例化契约下的两种绘制路径。
//!
//! - **SDF 实例路径**（`sdf_feather = Some(..)`，默认）：顶点缓冲仅 unit quad corner（8B），
//!   形状参数从 `@group(2) @binding(2)` 的实例 storage 读取（按 `@builtin(instance_index)`），
//!   VS 按 `bounds` 展开角点；写出的 `local_pos` 同时参与片元 SDF 求值。
//! - **Geo 模板实例路径**（`clear_sdf_feather()`）：顶点缓冲为模板 `TemplateVertex`
//!  （`@0 position` / `@1 uv`），每实例属性为 `GeoInstanceInput`（`@2 color` / `@3 transform_index`）。
//!
//! 两套契约均需输出同一 `VertexOutput`（@0-@6）供 `material_main` 使用；
//! custom VS 插值限定必须用普通 `@interpolate(linear)`（`linear, sample` 在
//! sample_count=1 验证会失败 → 布局支持丢失）。布局不匹配时形状被跳过 + 警告日志（不回落 mesh）。

use vireo::prelude::*;

/// SDF 实例契约：QuadVertex（corner）+ 实例 storage（InstanceData，112B）。
const SDF_VERTEX_WGSL: &str = r#"
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
    _pad: u32,
    _pad2: vec2<u32>,
};

struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;
@group(2) @binding(2) var<storage> instances: array<InstanceData>;

// 必须提供完整 VertexOutput（auto-generated material FS 契约）
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

@vertex
fn vs_main(vertex: QuadVertex, @builtin(instance_index) inst_idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let inst = instances[inst_idx];
    let t = vertex.corner * 0.5 + vec2<f32>(0.5);
    var local_pos = mix(inst.bounds.xy, inst.bounds.zw, t);
    // 顶点抖动：沿 y 正弦偏移 x（local_pos 同时进入片元 SDF 求值）
    local_pos.x += 6.0 * sin(local_pos.y * 0.05);
    let world_pos = transforms[inst.transform_index] * vec3<f32>(local_pos, 1.0);
    out.position = camera.projection * vec4<f32>(world_pos.xy, 0.0, 1.0);
    let span = max(inst.uv_bounds.zw - inst.uv_bounds.xy, vec2<f32>(1e-6));
    let uv_t = (local_pos - inst.uv_bounds.xy) / span;
    out.uv = mix(inst.uv_rect.xy, inst.uv_rect.zw, uv_t);
    out.color = inst.color;
    out.sdf_params = inst.sdf_params;
    out.local_pos = local_pos;
    out.sdf_type = inst.sdf_type;
    out.sdf_feather = inst.sdf_feather;
    out.sdf_extra = inst.sdf_extra;
    return out;
}
"#;

/// Geo 模板契约：TemplateVertex（position/uv）+ GeoInstanceInput（color/transform_index）。
const GEO_VERTEX_WGSL: &str = r#"
struct TemplateVertex {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct GeoInstanceInput {
    @location(2) color: vec4<f32>,
    @location(3) @interpolate(flat) transform_index: u32,
};

struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;

// 必须提供完整 VertexOutput（auto-generated material FS 契约）
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

@vertex
fn vs_main(vertex: TemplateVertex, instance: GeoInstanceInput) -> VertexOutput {
    var out: VertexOutput;
    var local_pos = vertex.position;
    // 顶点抖动：同 SDF 路径，作用于模板顶点
    local_pos.x += 6.0 * sin(local_pos.y * 0.05);
    let world_pos = transforms[instance.transform_index] * vec3<f32>(local_pos, 1.0);
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
"#;

const FRAGMENT_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let stripe = 0.5 + 0.5 * sin(in.local_pos.x * 0.12);
    let tint = vec3<f32>(0.15 + stripe * 0.75, 0.25, 0.95 - stripe * 0.55);
    let base = vireo_base_sample(in.base_uv);
    return vec4<f32>(base.rgb * tint, 0.95);
}
"#;

#[vireo::main]
async fn main() {
    let idx = app.window(
        WindowDesc::new("Custom Vertex Shader", 640, 420),
        None::<fn()>,
    );
    let sdf_material = app
        .material_with_vertex_shader(FRAGMENT_WGSL, SDF_VERTEX_WGSL)
        .expect("sdf instance custom vertex WGSL compile");
    let geo_material = app
        .material_with_vertex_shader(FRAGMENT_WGSL, GEO_VERTEX_WGSL)
        .expect("geo template custom vertex WGSL compile");

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };

        let mut batch = DrawBatch::new();

        // 左：SDF 实例路径（默认 feather），custom VS 按实例 storage 展开
        batch.set_custom_material(Some(sdf_material.clone()));
        batch.set_sdf_feather(Some(1.0));
        batch.set_position(190.0, 250.0);
        batch.set_rad(-0.08);
        draw_rectangle(
            &mut batch,
            Pos::new(-130.0, -120.0),
            260.0,
            240.0,
            Some(WHITE),
        );

        // 右：Geo 模板实例路径，custom VS 直接变换模板顶点
        batch.set_custom_material(Some(geo_material.clone()));
        batch.clear_sdf_feather();
        batch.set_position(450.0, 250.0);
        batch.set_rad(0.08);
        draw_rectangle(
            &mut batch,
            Pos::new(-130.0, -120.0),
            260.0,
            240.0,
            Some(WHITE),
        );

        let mut title = DrawBatch::new();
        draw_text(
            &mut title.texts,
            "custom VS: SDF instance contract (left) / geo template contract (right)",
            Pos::new(18.0, 18.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(WHITE),
        );
        draw_text(
            &mut title.texts,
            "vertex wobble runs in both contracts; same FS stripes via local_pos",
            Pos::new(18.0, 42.0),
            TextDef::default().font_size(14.0),
            TextOverride::from_color(Color::new(0.7, 0.75, 0.85, 1.0)),
        );

        win.draw(Color::new(0.04, 0.05, 0.09, 1.0), &[&batch, &title]);
        true
    })
    .await
    .unwrap();
}
