use vireo::prelude::*;

const VERTEX_WGSL: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) sdf_params: vec4<f32>,
    @location(4) @interpolate(flat) sdf_type: u32,
    @location(5) sdf_feather: f32,
    @location(6) sdf_extra: vec2<f32>,
    @location(7) @interpolate(flat) transform_index: u32,
};

struct Camera {
    projection: mat4x4<f32>,
    dpi_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(2) @binding(0) var<storage> transforms: array<mat3x3<f32>>;

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
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let wobble = 1.0 + 0.10 * sin(in.position.y * 0.08);
    let local = vec3<f32>(in.position.x * wobble, in.position.y, 1.0);
    let world = transforms[in.transform_index] * local;
    out.position = camera.projection * vec4<f32>(world.xy, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.sdf_params = in.sdf_params;
    out.local_pos = in.position;
    out.sdf_type = in.sdf_type;
    out.sdf_feather = in.sdf_feather;
    out.sdf_extra = in.sdf_extra;
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

fn main() {
    let mut app = App::new();
    let idx = app.window(
        WindowDesc::new("Custom Vertex Shader", 640, 420),
        None::<fn()>,
    );
    let material = app
        .material_with_vertex_shader(FRAGMENT_WGSL, VERTEX_WGSL)
        .expect("custom vertex WGSL compile");

    app.run(move |app| {
        let win = app.window_ref(&idx).unwrap();

        let mut batch = DrawBatch::new();
        batch.custom_material = Some(material.clone());
        batch.set_position(320.0, 210.0);
        batch.set_rad(0.15);
        draw_rectangle(&mut batch, Pos::new(-170.0, -110.0), 340.0, 220.0, Some(WHITE));

        let mut title = DrawBatch::new();
        draw_text(
            &mut title.texts,
            "custom VS: vertex wobble + custom FS stripes",
            Pos::new(18.0, 18.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(Color::new(0.04, 0.05, 0.09, 1.0), &[&batch, &title]);
        true
    });
}
