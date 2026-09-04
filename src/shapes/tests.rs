use super::*;
use crate::color::colors::{WHITE, RED, BLUE, GREEN};

fn test_batch() -> DrawBatch {
    let mut batch = DrawBatch::new();
    batch.sdf_feather = None;
    batch
}

#[test]
fn none_color_uses_batch_color() {
    let mut batch = test_batch();
    batch.set_color(GREEN);
    draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 10.0, 10.0, None);
    assert_eq!(batch.geo_instances[0].color, [GREEN.r, GREEN.g, GREEN.b, GREEN.a]);
    assert_eq!(batch.color, GREEN);
}

#[test]
fn some_color_does_not_write_batch_color() {
    let mut batch = test_batch();
    batch.set_color(WHITE);
    draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
    assert_eq!(batch.geo_instances[0].color, [RED.r, RED.g, RED.b, RED.a]);
    assert_eq!(batch.color, WHITE);
}

#[test]
fn opts_sdf_and_transform_restore_batch_state() {
    let mut batch = test_batch();
    batch.sdf_feather = None;
    batch.set_color(WHITE);
    batch.set_position(10.0, 20.0);
    draw_shape(
        &mut batch,
        &Shape::Circle { pos: Pos::new(100.0, 200.0), r: 5.0 },
        ShapeOverride::new()
            .color(RED)
            .sdf(1.0),
    );
    assert_eq!(batch.sdf_feather, None);
    assert_eq!(batch.color, WHITE);
    assert_eq!(batch.instances[0].sdf_type, 1);
    assert_eq!(batch.instances[0].color, [RED.r, RED.g, RED.b, RED.a]);
    // 覆盖后的 shape 与 batch 后续 draw 应使用不同 transform_index
    draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 1.0, 1.0, Some(BLUE));
    let idx_override = batch.instances[0].transform_index;
    let idx_restored = batch.geo_instances[0].transform_index;
    assert_ne!(idx_override, idx_restored);
}

#[test]
fn draw_shape_matches_draw_circle_sdf() {
    let mut a = test_batch();
    a.sdf_feather = Some(1.0);
    draw_circle(&mut a, Pos::new(50.0, 60.0), 20.0, Some(RED));
    let mut b = test_batch();
    b.sdf_feather = Some(1.0);
    draw_shape(
        &mut b,
        &Shape::Circle { pos: Pos::new(50.0, 60.0), r: 20.0 },
        ShapeOverride::from_color(Some(RED)),
    );
    assert_eq!(a.instances.len(), b.instances.len());
    assert_eq!(a.instances[0].sdf_type, 1);
    assert_eq!(a.instances[0].sdf_params, b.instances[0].sdf_params);
}

#[test]
fn rect_zero_size_skipped() {
    let mut batch = test_batch();
    draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 0.0, 100.0, Some(WHITE));
    draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 100.0, 0.0, Some(WHITE));
    assert!(batch.geo_instances.is_empty());
    assert!(batch.geo_template_indices.is_empty());
}

#[test]
fn rect_transparent_skipped() {
    let mut batch = test_batch();
    draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 100.0, 100.0, Some(Color::new(1.0, 0.0, 0.0, 0.0)));
    assert!(batch.geo_instances.is_empty());
}

#[test]
fn circle_sdf_produces_quad() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(0.0);
    draw_circle(&mut batch, Pos::new(100.0, 100.0), 50.0, Some(RED));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 1);
}

#[test]
fn circle_zero_radius_skipped() {
    let mut batch = test_batch();
    draw_circle(&mut batch, Pos::new(0.0, 0.0), 0.0, Some(RED));
    assert!(batch.geo_instances.is_empty());
}

#[test]
fn line_geometry_produces_caps() {
    let mut batch = test_batch();
    draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 2.0, Some(WHITE));
    assert!(batch.geo_template_vertices.len() > 4);
    assert!(batch.geo_template_indices.len() > 6);
}

#[test]
fn line_sdf_produces_quad() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(1.0);
    draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 2.0, Some(WHITE));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 3);
}

#[test]
fn line_zero_thickness_both_modes_combined() {
    // geometry mode: zero thickness → no geo instances
    let mut batch = test_batch();
    draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 0.0, Some(WHITE));
    assert!(batch.geo_instances.is_empty());

    // default SDF mode: zero thickness → no SDF instances
    let mut batch = DrawBatch::new();
    draw_line(&mut batch, 0.0, 0.0, 100.0, 0.0, 0.0, Some(WHITE));
    assert!(batch.instances.is_empty());
}

#[test]
fn line_zero_length_becomes_dot() {
    let mut batch = test_batch();
    draw_line(&mut batch, 50.0, 50.0, 50.0, 50.0, 2.0, Some(WHITE));
    // 零长线：无线段，只有端点圆弧 → 画半径 = thickness/2 的圆（点）
    assert!(!batch.geo_instances.is_empty());
}

#[test]
fn ellipse_geometry_produces_fan() {
    let mut batch = test_batch();
    draw_ellipse(&mut batch, Pos::new(0.0, 0.0), 30.0, 20.0, Some(BLUE));
    assert!(batch.geo_template_vertices.len() > 4);
    assert!(batch.geo_template_indices.len() > 6);
}

#[test]
fn ellipse_sdf_produces_quad() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(1.0);
    draw_ellipse(&mut batch, Pos::new(0.0, 0.0), 30.0, 20.0, Some(BLUE));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 1);
}

#[test]
fn ellipse_zero_radius_skipped() {
    let mut batch = test_batch();
    draw_ellipse(&mut batch, Pos::new(0.0, 0.0), 0.0, 10.0, Some(BLUE));
    draw_ellipse(&mut batch, Pos::new(0.0, 0.0), 10.0, 0.0, Some(BLUE));
    assert!(batch.geo_instances.is_empty());
}

#[test]
fn rounded_rect_geometry_produces_triangles() {
    let mut batch = test_batch();
    draw_rounded_rect(&mut batch, Pos::new(10.0, 10.0), 100.0, 60.0, 10.0, Some(GREEN));
    assert!(batch.geo_template_vertices.len() > 4);
    assert!(batch.geo_template_indices.len() > 6);
}

#[test]
fn rounded_rect_sdf_produces_quad() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(1.0);
    draw_rounded_rect(&mut batch, Pos::new(10.0, 10.0), 100.0, 60.0, 10.0, Some(GREEN));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 2);
}

#[test]
fn rounded_rect_zero_size_skipped() {
    let mut batch = test_batch();
    draw_rounded_rect(&mut batch, Pos::new(0.0, 0.0), 0.0, 100.0, 5.0, Some(WHITE));
    assert!(batch.geo_instances.is_empty());
}

#[test]
fn triangle_geometry_produces_one_triangle() {
    let mut batch = test_batch();
    draw_triangle(&mut batch, 0.0, 0.0, 100.0, 0.0, 50.0, 100.0, Some(RED));
    assert_eq!(batch.geo_template_vertices.len(), 3);
    assert_eq!(batch.geo_template_indices.len(), 3);
    assert_eq!(batch.geo_instances.len(), 1);
}

#[test]
fn triangle_sdf_produces_quad() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(1.0);
    draw_triangle(&mut batch, 0.0, 0.0, 100.0, 0.0, 50.0, 100.0, Some(RED));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 4);
}

#[test]
fn degenerate_triangle_skipped_in_default_sdf_mode() {
    let mut batch = DrawBatch::new();
    draw_triangle(&mut batch, 0.0, 0.0, 10.0, 0.0, 20.0, 0.0, Some(RED));
    assert!(batch.instances.is_empty());
}

#[test]
fn polygon_geometry_produces_fan() {
    let mut batch = test_batch();
    let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];
    draw_polygon(&mut batch, &pts, Some(WHITE));
    assert_eq!(batch.geo_template_vertices.len(), 4);
    assert_eq!(batch.geo_template_indices.len(), 6);
    assert_eq!(batch.geo_instances.len(), 1);
}

#[test]
fn polygon_sdf_produces_quad() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(1.0);
    let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)];
    draw_polygon(&mut batch, &pts, Some(WHITE));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 6);
}

#[test]
fn polygon_too_few_points_skipped() {
    let mut batch = test_batch();
    draw_polygon(&mut batch, &[(0.0, 0.0), (10.0, 10.0)], Some(WHITE));
    assert!(batch.geo_instances.is_empty());
}

#[test]
fn arc_geometry_produces_fan() {
    let mut batch = test_batch();
    draw_arc(&mut batch, Pos::new(0.0, 0.0), 50.0, 0.0, std::f32::consts::PI, Some(RED));
    assert!(batch.geo_template_vertices.len() > 4);
    assert!(batch.geo_template_indices.len() > 6);
}

#[test]
fn arc_sdf_produces_quad() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(1.0);
    draw_arc(&mut batch, Pos::new(0.0, 0.0), 50.0, 0.0, std::f32::consts::PI, Some(RED));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 5);
}

#[test]
fn arc_zero_radius_skipped() {
    let mut batch = test_batch();
    draw_arc(&mut batch, Pos::new(0.0, 0.0), 0.0, 0.0, 1.0, Some(RED));
    assert!(batch.geo_instances.is_empty());
}

#[test]
fn arc_zero_radius_and_span_skipped_in_default_sdf_mode() {
    let mut batch = DrawBatch::new();
    draw_arc(&mut batch, Pos::ZERO, 0.0, 0.0, 1.0, Some(RED));
    draw_arc(&mut batch, Pos::ZERO, 10.0, 1.0, 1.0, Some(RED));
    assert!(batch.instances.is_empty());
}

#[test]
fn negative_radius_and_size_skipped() {
    let mut batch = test_batch();
    draw_circle(&mut batch, Pos::new(0.0, 0.0), -5.0, Some(RED));
    draw_ellipse(&mut batch, Pos::new(0.0, 0.0), -3.0, 5.0, Some(RED));
    draw_rectangle(&mut batch, Pos::new(0.0, 0.0), -10.0, 10.0, Some(RED));
    draw_arc(&mut batch, Pos::new(0.0, 0.0), -5.0, 0.0, 1.0, Some(RED));
    assert!(batch.instances.is_empty());
    assert!(batch.geo_instances.is_empty());
}

#[test]
fn multiple_shapes_in_one_batch() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(0.0);
    draw_rectangle(&mut batch, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
    draw_circle(&mut batch, Pos::new(100.0, 100.0), 5.0, Some(BLUE));
    draw_triangle(&mut batch, 0.0, 0.0, 10.0, 0.0, 5.0, 10.0, Some(GREEN));

    assert_eq!(batch.instances.len(), 3);
    assert!(batch.vertices.is_empty());
    assert!(batch.indices.is_empty());
}

#[test]
fn rect_default_mode_is_sdf() {
    let mut batch = DrawBatch::new();
    draw_rectangle(&mut batch, Pos::new(10.0, 10.0), 100.0, 60.0, Some(GREEN));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 2);
}

#[test]
fn circle_geometry_mode_produces_triangle_fan() {
    let mut batch = test_batch();
    batch.sdf_feather = None;
    draw_circle(&mut batch, Pos::new(100.0, 100.0), 50.0, Some(RED));
    let n = 256u32;
    assert_eq!(batch.geo_template_vertices.len() as u32, 1 + n + 1);
    assert_eq!(batch.geo_template_indices.len() as u32, n * 3);
    assert_eq!(batch.geo_instances.len(), 1);
}

#[test]
fn geo_same_params_share_single_template() {
    let mut batch = test_batch();
    for i in 0..100u32 {
        draw_circle(&mut batch, Pos::new(i as f32, 0.0), 5.0, Some(RED));
    }
    assert_eq!(batch.geo_instances.len(), 100);
    assert_eq!(batch.geo_templates.len(), 1, "同参数圆应共享单个模板");
    assert!(batch.geo_template_vertices.len() < 100 * 258);
    assert!(batch.shape_vertex_count() < 100 * 258, "模板应复用");
}

#[test]
fn geo_different_params_emit_separate_templates() {
    let mut batch = test_batch();
    draw_circle(&mut batch, Pos::ZERO, 5.0, Some(RED));
    draw_circle(&mut batch, Pos::new(100.0, 0.0), 9.0, Some(BLUE));
    assert_eq!(batch.geo_instances.len(), 2);
    assert_eq!(batch.geo_templates.len(), 2, "不同半径应生成不同模板");
}

#[test]
fn line_chain_geometry_produces_segments() {
    let mut batch = test_batch();
    let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
    draw_line_chain(&mut batch, &pts, 2.0, Some(WHITE));
    assert!(batch.geo_template_vertices.len() >= 8);
    assert!(batch.geo_template_indices.len() >= 12);
    assert_eq!(batch.geo_instances.len(), 1);
}

#[test]
fn line_chain_duplicate_endpoint_has_finite_vertices() {
    let mut batch = test_batch();
    draw_line_chain(
        &mut batch,
        &[(0.0, 0.0), (0.0, 0.0), (20.0, 0.0)],
        2.0,
        Some(WHITE),
    );
    assert!(!batch.geo_template_vertices.is_empty());
    assert!(batch.geo_template_vertices.iter().all(|v| {
        v.position.iter().all(|x| x.is_finite())
            && v.uv.iter().all(|x| x.is_finite())
    }));
}

#[test]
fn line_chain_sdf_produces_quad() {
    let mut batch = test_batch();
    batch.sdf_feather = Some(1.0);
    let pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
    draw_line_chain(&mut batch, &pts, 2.0, Some(WHITE));
    assert_eq!(batch.instances.len(), 1);
    assert_eq!(batch.instances[0].sdf_type, 7);
}
