    use super::*;
    use crate::area::Area;
    use crate::color::colors::*;
    use crate::math::{left_mul_view_table, transform_key};
    use crate::shapes::{draw_circle, draw_polygon, draw_rectangle, draw_rounded_rect};
    use crate::text::{TextDef, TextOverride};

    #[test]
    fn has_sdf_flag_set_on_sdf_shapes() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        assert!(b.has_sdf);
        b.clear();
        assert!(!b.has_sdf);
        b.sdf_feather = None;
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        assert!(!b.has_sdf);
    }

    #[test]
    fn with_override_applies_then_restores() {
        let mut b = DrawBatch::new();
        b.set_color(crate::color::Color::new(1.0, 0.0, 0.0, 1.0));
        let before = b.color();
        let mut seen: Option<crate::color::Color> = None;
        b.with_override(
            BatchOverride::default().color(crate::color::Color::new(0.0, 1.0, 0.0, 1.0)),
            |_b, c| {
                seen = Some(c);
            },
        );
        assert_eq!(b.color(), before);
        assert_eq!(
            seen,
            Some(crate::color::Color::new(0.0, 1.0, 0.0, 1.0))
        );
    }

    #[test]
    fn with_override_noop_passthrough() {
        let mut b = DrawBatch::new();
        b.set_color(crate::color::Color::new(1.0, 0.0, 0.0, 1.0));
        let before = b.color();
        let mut ran = false;
        b.with_override(
            BatchOverride::default(),
            |_b, c| {
                ran = true;
                assert_eq!(c, before);
            },
        );
        assert!(ran);
        assert_eq!(b.color(), before);
    }

    #[test]
    fn with_override_text_clip_and_shared_color() {
        let mut b = DrawBatch::new();
        let clip = crate::glyphon::TextBounds { left: 0, top: 0, right: 10, bottom: 10 };
        let green = crate::color::Color::new(0.0, 1.0, 0.0, 1.0);
        b.with_override(
            BatchOverride::default().color(green).text_clip(Some(clip)),
            |b, c| {
                assert_eq!(c, green);
                assert_eq!(b.color(), green);
                assert_eq!(b.text_clip, Some(clip));
                // 形状与文字共享 batch.color：闭包内形状与文字 fallback 同色
                b.text("hi", Pos::new(0.0, 0.0), TextDef::default(), TextOverride::default());
                // 文字未显式 color，prepare 将用 batch_color (=green) 兜底，此处 entry 仍为 None
                assert_eq!(b.texts.entries[0].override_().color, None);
            },
        );
        // 退出后恢复
        assert_eq!(b.text_clip, None);
        assert_eq!(b.color(), crate::color::Color::new(1.0, 1.0, 1.0, 1.0));
    }

    #[test]
    fn with_override_uv_and_text_texture_restores() {
        let mut b = DrawBatch::new();
        let uv0 = b.uv();
        let uv1 = UvRect { u0: 0.1, v0: 0.2, u1: 0.3, v1: 0.4 };
        b.with_override(BatchOverride::default().uv(uv1), |b, _| {
            assert_eq!(b.uv(), uv1);
            assert_eq!(b.texts.texture_state.uv, uv1);
        });
        assert_eq!(b.uv(), uv0);
        assert_eq!(b.texts.texture_state.uv, uv0);
    }

    #[test]
    fn transform_index_stable_across_same_transform() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(10.0, 20.0);
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 5.0, 5.0, Some(RED));
        draw_circle(&mut b, Pos::new(0.0, 0.0), 3.0, Some(BLUE));
        let idxs: Vec<u32> = b.instances.iter().map(|v| v.transform_index).collect();
        assert!(idxs.iter().all(|&i| i == idxs[0]));
        // 槽 0 = 单位阵 + 1 个平移
        assert_eq!(b.transform_table.len() / 12, 2);
    }

    #[test]
    fn transform_cache_invalidates_on_set_position() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        // 不同 Pos 应产生不同 transform entry
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 5.0, 5.0, Some(RED));
        draw_rectangle(&mut b, Pos::new(100.0, 0.0), 5.0, 5.0, Some(BLUE));
        let i0 = b.instances[0].transform_index;
        let i1 = b.instances[1].transform_index;
        assert_ne!(i0, i1);
        // 槽 0 = 单位阵 + 2 个不同平移（Pos(0,0) 复用槽 0）
        assert_eq!(b.transform_table.len() / 12, 2);
        assert_eq!(i0, 0);
        assert_eq!(i1, 1);
    }

    #[test]
    fn transform_slot_zero_is_identity_after_shape() {
        let mut b = DrawBatch::new();
        draw_rectangle(&mut b, Pos::new(100.0, 200.0), 50.0, 40.0, Some(WHITE));
        assert!(b.transform_table.len() >= 12);
        let t0 = &b.transform_table[0..12];
        assert_eq!(t0[0], 1.0);
        assert_eq!(t0[5], 1.0);
        assert_eq!(t0[8], 0.0);
        assert_eq!(t0[9], 0.0);
        assert_eq!(b.instances[0].transform_index, 1);
        let t1 = &b.transform_table[12..24];
        assert!((t1[8] - 100.0).abs() < 1e-4);
        assert!((t1[9] - 200.0).abs() < 1e-4);
        // draw_text 默认 index 0 → 恒等，不会吃到矩形的平移
        crate::text::draw_text(
            &mut b.texts,
            "hi",
            Pos::new(106.0, 204.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(WHITE),
        );
        assert_eq!(b.texts.entries[0].transform_index(), 0);
    }

    #[test]
    fn multi_batch_poly_base_patch_values() {
        // 模拟 Renderer 多 batch poly 偏移：第二 batch 的 type6 start 应加上第一 batch 边数
        let mut b0 = DrawBatch::new();
        b0.sdf_feather = Some(1.0);
        let pts = [(0., 0.), (10., 0.), (5., 8.)];
        draw_polygon(&mut b0, &pts, Some(RED));
        let edges0 = b0.polygon_edges.len() / 4;

        let mut b1 = DrawBatch::new();
        b1.sdf_feather = Some(1.0);
        draw_polygon(&mut b1, &pts, Some(BLUE));
        let start_local = b1.instances[0].sdf_params[0];
        assert_eq!(start_local, 0.0);

        let poly_base = edges0 as f32;
        let mut patched = b1.instances.clone();
        for v in &mut patched {
            if v.sdf_type == 6 || v.sdf_type == 7 {
                v.sdf_params[0] += poly_base;
            }
        }
        assert_eq!(patched[0].sdf_params[0], poly_base);
    }

    #[test]
    fn transform_key_distinguishes_similar_matrices() {
        let k1 = transform_key([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        let k2 = transform_key([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 1.0]);
        let k3 = transform_key([2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]);
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        assert_ne!(k2, k3);
    }

    #[test]
    fn clear_preserves_vertex_capacity() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        for i in 0..32 {
            b.set_position(i as f32, 0.0);
            draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(RED));
        }
        let cap_v = b.vertices.capacity();
        let cap_i = b.indices.capacity();
        b.clear();
        assert!(b.vertices.capacity() >= cap_v);
        assert!(b.indices.capacity() >= cap_i);
        assert!(b.vertices.is_empty());
        assert!(!b.has_sdf);
        assert_eq!(b.sdf_feather, Some(1.0)); // 与 new() 一致
    }

    #[test]
    fn sdf_instances_store_one_record_per_shape() {
        let mut batch = DrawBatch::new();
        for i in 0..1000 {
            batch.instance_rectangle(Pos::new(i as f32, 0.0), 8.0, 4.0, Some(RED));
        }
        assert_eq!(batch.instances.len(), 1000);
        assert!(batch.vertices.is_empty());
        assert!(batch.indices.is_empty());
        assert!(batch.has_sdf);
    }

    #[test]
    fn instance_shapes_capture_transform_and_position() {
        let mut batch = DrawBatch::new();
        batch.set_position(10.0, 20.0);
        batch.instance_circle(Pos::new(5.0, 6.0), 3.0, Some(WHITE));
        let instance = batch.instances[0];
        assert_ne!(instance.transform_index, 0);
        let (_, _, translation) = DrawBatch::table_cols_at(
            &batch.transform_table,
            instance.transform_index,
        );
        assert!((translation[0] - 15.0).abs() < 1e-5);
        assert!((translation[1] - 26.0).abs() < 1e-5);
    }

    #[test]
    fn instance_geometry_mode_falls_back_to_vertices() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = None;
        batch.instance_circle(Pos::ZERO, 5.0, Some(WHITE));
        assert!(batch.instances.is_empty());
        assert!(!batch.geo_instances.is_empty());
        assert!(!batch.geo_template_vertices.is_empty());
        assert!(!batch.geo_template_indices.is_empty());
    }

    #[test]
    fn to_area_expands_instances_to_legacy_quads() {
        let mut batch = DrawBatch::new();
        batch.instance_rectangle(Pos::new(3.0, 4.0), 10.0, 20.0, Some(GREEN));
        match batch.to_area() {
            Area::Geom(geom) => {
                assert_eq!(geom.vertices.len(), 4);
                assert_eq!(geom.indices.len(), 6);
                assert_eq!(geom.vertices[0].sdf_type, 2);
            }
            _ => panic!("expected Area::Geom"),
        }
    }

    #[test]
    fn extended_sdf_instances_do_not_expand_vertices() {
        let mut batch = DrawBatch::new();
        batch.instance_rounded_rect(Pos::new(1.0, 2.0), 20.0, 10.0, 3.0, Some(WHITE));
        batch.instance_line(0.0, 0.0, 10.0, 5.0, 2.0, Some(WHITE));
        batch.instance_triangle(0.0, 0.0, 10.0, 0.0, 5.0, 8.0, Some(WHITE));
        batch.instance_arc(Pos::new(20.0, 20.0), 8.0, 0.0, std::f32::consts::PI, Some(WHITE));
        batch.instance_polygon(&[(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)], Some(WHITE));
        batch.instance_line_chain(&[(0.0, 0.0), (4.0, 2.0), (8.0, 0.0)], 2.0, Some(WHITE));
        assert_eq!(batch.instances.len(), 6);
        assert!(batch.vertices.is_empty());
        assert_eq!(batch.instances[0].sdf_type, 2);
        assert_eq!(batch.instances[1].sdf_type, 3);
        assert_eq!(batch.instances[2].sdf_type, 4);
        assert_eq!(batch.instances[3].sdf_type, 5);
        assert_eq!(batch.instances[4].sdf_type, 6);
        assert_eq!(batch.instances[5].sdf_type, 7);
        assert!(!batch.polygon_edges.is_empty());
    }

    #[test]
    fn repeated_instance_polygon_reuses_edges() {
        let points = [(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)];
        let mut batch = DrawBatch::new();
        batch.instance_polygon(&points, Some(WHITE));
        batch.instance_polygon(&points, Some(WHITE));

        assert_eq!(batch.polygon_edges.len(), points.len() * 4);
        assert_eq!(batch.instances[0].sdf_params[0], 0.0);
        assert_eq!(batch.instances[1].sdf_params[0], 0.0);
    }

    #[test]
    fn polygon_and_line_chain_edges_use_distinct_templates() {
        let points = [(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)];
        let mut batch = DrawBatch::new();
        batch.instance_polygon(&points, Some(WHITE));
        batch.instance_line_chain(&points, 2.0, Some(WHITE));

        assert_eq!(batch.instances[0].sdf_params[0], 0.0);
        assert_eq!(batch.instances[1].sdf_params[0], 3.0);
        assert_eq!(batch.polygon_edges.len(), (3 + 2) * 4);
    }

    #[test]
    fn repeated_instance_line_chain_reuses_edges() {
        let points = [(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)];
        let mut batch = DrawBatch::new();
        batch.instance_line_chain(&points, 2.0, Some(WHITE));
        batch.instance_line_chain(&points, 4.0, Some(RED));

        assert_eq!(batch.polygon_edges.len(), 2 * 4);
        assert_eq!(batch.instances[0].sdf_params[0], 0.0);
        assert_eq!(batch.instances[1].sdf_params[0], 0.0);
        assert_eq!(batch.instances[1].sdf_params[2], 2.0);
    }

    #[test]
    fn edge_template_recovers_after_public_edge_buffer_mutation() {
        let points = [(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)];
        let mut batch = DrawBatch::new();
        batch.instance_polygon(&points, Some(WHITE));
        let expected = batch.polygon_edges.clone();

        batch.polygon_edges.clear();
        batch.instance_polygon(&points, Some(WHITE));

        assert_eq!(batch.polygon_edges, expected);
        assert_eq!(batch.instances[1].sdf_params[0], 0.0);
    }

    #[test]
    fn instance_shape_covers_positioned_and_outline_variants() {
        let mut batch = DrawBatch::new();
        batch.instance_shape(
            &crate::shapes::Shape::RoundedRect {
                pos: Pos::new(20.0, 30.0),
                w: 16.0,
                h: 8.0,
                radius: 2.0,
            },
            crate::shapes::ShapeOverride::default(),
        );
        batch.instance_shape(
            &crate::shapes::Shape::PolygonOutline {
                points: &[(0.0, 0.0), (8.0, 0.0), (4.0, 6.0)],
                thickness: 1.0,
            },
            crate::shapes::ShapeOverride::default(),
        );
        assert_eq!(batch.instances.len(), 2);
        assert!(batch.vertices.is_empty());
        let (_, _, translation) = DrawBatch::table_cols_at(
            &batch.transform_table,
            batch.instances[0].transform_index,
        );
        assert!((translation[0] - 20.0).abs() < 1e-5);
        assert!((translation[1] - 30.0).abs() < 1e-5);
    }

    #[test]
    fn shape_stats_separates_mesh_vertices_and_instances() {
        let mut batch = DrawBatch::new();
        // geo-instance 路径（sdf_feather=None）
        batch.sdf_feather = None;
        draw_circle(&mut batch, Pos::new(5.0, 5.0), 4.0, Some(RED));
        let geo_stats = batch.shape_stats();
        assert_eq!(geo_stats.mesh_vertices, 0, "geo 路径不应推 mesh 顶点");
        assert_eq!(geo_stats.sdf_instances, 0, "geo 路径不应有 SDF instance");
        assert!(geo_stats.geo_instances > 0, "geo 路径应推送 geo instance");
        assert_eq!(geo_stats.geo_instances, batch.geo_instances.len());
        assert!(geo_stats.geo_templates > 0, "geo 路径应有模板");
        assert_eq!(geo_stats.geo_templates, batch.geo_templates.len());
        assert!(geo_stats.geo_template_vertices > 0, "geo 路径应有模板顶点");
        assert_eq!(
            geo_stats.geo_template_vertices,
            batch.geo_template_vertices.len()
        );

        // instance 路径
        let mut batch2 = DrawBatch::new();
        batch2.sdf_feather = Some(1.0);
        draw_rectangle(&mut batch2, Pos::ZERO, 8.0, 8.0, Some(RED));
        draw_circle(&mut batch2, Pos::new(20.0, 0.0), 4.0, Some(BLUE));
        let sdf_stats = batch2.shape_stats();
        assert_eq!(sdf_stats.mesh_vertices, 0, "instance 路径不应推 mesh 顶点");
        assert_eq!(sdf_stats.sdf_instances, 2);
        assert_eq!(sdf_stats.geo_instances, 0);
        assert_eq!(sdf_stats.geo_templates, 0);
        assert_eq!(sdf_stats.geo_template_vertices, 0);
        // shape_vertex_count 仍是 4-顶点等价
        assert_eq!(sdf_stats.sdf_instances * 4, batch2.shape_vertex_count());
    }

    #[test]
    fn set_uv_propagates_to_text_entries() {
        let mut batch = DrawBatch::new();
        batch.set_uv(0.25, 0.25, 0.75, 0.75);
        let expected = UvRect { u0: 0.25, v0: 0.25, u1: 0.75, v1: 0.75 };
        assert_eq!((batch.uv.u0, batch.uv.u1), (expected.u0, expected.u1));
        // set_uv 同步到 text 画笔；之后入队条目冻结该 uv
        batch.texts.push(
            "H",
            Pos::ZERO,
            Default::default(),
            Default::default(),
        );
        let entries = batch.texts.entries.clone();
        let frozen = entries[0].texture_state().uv;
        assert_eq!(frozen.u0, 0.25);
        assert_eq!(frozen.u1, 0.75);
        // getter 与 setter 往返一致
        let got = batch.uv();
        assert_eq!((got.u0, got.v0, got.u1, got.v1), (0.25, 0.25, 0.75, 0.75));
        batch.clear_uv();
        let reset = batch.uv();
        assert_eq!((reset.u0, reset.v0, reset.u1, reset.v1), (0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn inherit_uv_propagates_to_child_text_brush() {
        // 回归：apply_inherit_from 的 uv 分支必须走 set_uv（传播到 texts.texture_state.uv），
        // 否则 child 继承 uv 后文字画笔仍默认 UV，形状/文字 UV 不一致。
        let mut parent = DrawBatch::new();
        parent.set_uv(0.25, 0.25, 0.75, 0.75);

        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::ALL;
        parent.push_child(child);

        // 继承后入队文字，冻结的画笔 UV 应为父值
        let mut inherited = DrawBatch::new();
        inherited.inherit = InheritFromParent::ALL;
        let mut p2 = DrawBatch::new();
        p2.set_uv(0.25, 0.25, 0.75, 0.75);
        p2.push_child(inherited);
        p2.children[0].texts.push(
            "H",
            Pos::ZERO,
            Default::default(),
            Default::default(),
        );
        let expected = UvRect { u0: 0.25, v0: 0.25, u1: 0.75, v1: 0.75 };
        let frozen = p2.children[0].texts.entries[0].texture_state().uv;
        assert_eq!((frozen.u0, frozen.u1), (expected.u0, expected.u1),
            "uv 继承必须同步到子 batch 文字画笔");
        _ = parent;
    }

    #[test]
    fn automatic_instances_merge_contiguous_commands() {
        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        draw_circle(&mut batch, Pos::new(10.0, 0.0), 4.0, Some(BLUE));

        assert_eq!(batch.instances.len(), 2);
        assert_eq!(batch.shape_commands.len(), 1);
        assert!(matches!(
            batch.shape_commands[0],
            BatchShapeCommand::Instances { instance_start: 0, instance_count: 2, .. }
        ));
    }

    #[test]
    fn ordered_commands_preserve_instance_mesh_instance_order() {
        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        batch.sdf_feather = None;
        draw_rectangle(&mut batch, Pos::new(2.0, 2.0), 8.0, 8.0, Some(GREEN));
        batch.sdf_feather = Some(1.0);
        draw_circle(&mut batch, Pos::new(10.0, 0.0), 4.0, Some(BLUE));

        assert_eq!(batch.shape_commands.len(), 3);
        assert!(matches!(batch.shape_commands[0], BatchShapeCommand::Instances { .. }));
        assert!(matches!(batch.shape_commands[1], BatchShapeCommand::GeoInstances { .. }));
        assert!(matches!(batch.shape_commands[2], BatchShapeCommand::Instances { .. }));
    }

    #[test]
    fn merge_decision_requires_same_state_and_contiguity() {
        // 状态一致 + 连续 → 合并
        let merged = merge_decision(true, 0, 6, 6, 6).unwrap();
        assert_eq!(merged, (0, 12));
        // 状态一致但中间有间隙 → 不合并
        assert!(merge_decision(true, 0, 6, 10, 6).is_none());
        // 状态不一致 → 不合并（即使连续）
        assert!(merge_decision(false, 0, 6, 6, 6).is_none());
        // 跨种类（mesh vs instances）→ 不合并
        assert!(merge_decision(false, 0, 6, 6, 6).is_none());
        // 三段连续合并
        let m1 = merge_decision(true, 0, 6, 6, 6).unwrap();
        let m2 = merge_decision(true, m1.0, m1.1, 12, 6).unwrap();
        assert_eq!(m2, (0, 18));
    }

    #[test]
    fn texture_generation_splits_instance_commands() {
        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        batch.advance_shape_texture_generation();
        draw_circle(&mut batch, Pos::new(10.0, 0.0), 4.0, Some(BLUE));

        assert_eq!(batch.shape_commands.len(), 2);
        assert!(matches!(batch.shape_commands[0], BatchShapeCommand::Instances { .. }));
        assert!(matches!(batch.shape_commands[1], BatchShapeCommand::Instances { .. }));
    }

    #[test]
    fn automatic_rectangle_bounds_include_feather() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = Some(2.0);
        draw_rectangle(&mut batch, Pos::ZERO, 10.0, 20.0, Some(WHITE));

        assert_eq!(batch.instances[0].bounds, [-2.0, -2.0, 12.0, 22.0]);
        assert_eq!(batch.instances[0].uv_bounds, [0.0, 0.0, 10.0, 20.0]);
    }

    #[test]
    fn geo_same_params_share_single_template() {
        let mut batch = DrawBatch::new();
        batch.sdf_feather = None;
        for i in 0..50u32 {
            batch.set_position(i as f32 * 2.0, 0.0);
            draw_circle(&mut batch, Pos::new(8.0, 8.0), 8.0, Some(RED));
        }
        for i in 0..50u32 {
            batch.set_position(i as f32 * 2.0, 40.0);
            draw_rounded_rect(&mut batch, Pos::ZERO, 20.0, 16.0, 4.0, Some(BLUE));
        }
        // 同模板圆合并为 1 个命令；圆角矩形独立模板 → 第 2 个命令
        assert_eq!(batch.shape_commands.len(), 2, "两个模板应各一个命令");
        assert!(matches!(batch.shape_commands[0], BatchShapeCommand::GeoInstances { geo_instance_count: 50, .. }));
        assert!(matches!(batch.shape_commands[1], BatchShapeCommand::GeoInstances { geo_instance_count: 50, .. }));
    }

    #[test]
    fn stale_commands_fall_back_after_geo_buffer_clear() {
        // geo_template_indices / geo_instances 清空后命令应失效
        let mut batch = DrawBatch::new();
        batch.sdf_feather = None;
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        assert!(batch.shape_commands_valid());
        batch.geo_template_indices.clear();
        assert!(!batch.shape_commands_valid());

        // geo_instances 清空亦然
        let mut batch2 = DrawBatch::new();
        batch2.sdf_feather = None;
        draw_rectangle(&mut batch2, Pos::ZERO, 8.0, 8.0, Some(RED));
        assert!(batch2.shape_commands_valid());
        batch2.geo_instances.clear();
        assert!(!batch2.shape_commands_valid());

        // custom_material fragment-only + sdf_feather=None 也走 geo 路径
        let mut batch3 = DrawBatch::new();
        batch3.sdf_feather = None;
        batch3.custom_material = Some(Arc::new(crate::material::Material::new_zero_resource(
            "fn material_main(in: crate_material_never) -> vec4<f32> { return vec4<f32>(1.0); }"
                .to_string(),
            None,
            rustc_hash::FxHashMap::default(),
        )));
        draw_rectangle(&mut batch3, Pos::ZERO, 8.0, 8.0, Some(RED));
        assert!(batch3.shape_commands_valid());
        batch3.geo_template_indices.clear();
        assert!(!batch3.shape_commands_valid());
    }

    #[test]
    fn clear_resets_and_rebuilds_ordered_commands() {
        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(RED));
        batch.sdf_feather = None;
        draw_rectangle(&mut batch, Pos::ZERO, 8.0, 8.0, Some(BLUE));
        assert_eq!(batch.shape_commands.len(), 2);

        batch.clear();
        assert!(batch.shape_commands.is_empty());
        draw_circle(&mut batch, Pos::ZERO, 4.0, Some(GREEN));
        assert_eq!(batch.shape_commands.len(), 1);
        assert!(batch.shape_commands_valid());
    }

    #[test]
    fn translate_and_draw_share_cached_index() {
        let mut b = DrawBatch::new();
        b.sdf_feather = Some(1.0);
        b.set_position(1.0, 2.0);
        let i0 = b.current_transform_index();
        let i1 = b.current_transform_index();
        assert_eq!(i0, i1);
        b.translate(3.0, 4.0);
        let i2 = b.current_transform_index();
        assert_ne!(i0, i2);
    }

    #[test]
    fn inherit_transform_left_muls_child_table() {
        let mut parent = DrawBatch::new();
        parent.set_position(100.0, 50.0);
        let mut child = DrawBatch::new();
        child.sdf_feather = Some(1.0);
        child.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        let idx = child.instances[0].transform_index as usize;
        let base = idx * 12;
        // 继承前局部表为恒等
        assert_eq!(child.transform_table[base], 1.0);
        assert_eq!(child.transform_table[base + 8], 0.0);
        parent.push_child(child);
        let c = &parent.children[0];
        let t = &c.transform_table[base..base + 12];
        assert!((t[8] - 100.0).abs() < 1e-4, "tx={}", t[8]);
        assert!((t[9] - 50.0).abs() < 1e-4, "ty={}", t[9]);
    }

    #[test]
    fn inherit_color_and_feather_on_push() {
        let mut parent = DrawBatch::new();
        parent.color = GREEN;
        parent.sdf_feather = Some(2.5);
        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::NONE.color().sdf_feather();
        assert_eq!(child.color, WHITE);
        parent.push_child(child);
        assert_eq!(parent.children[0].color, GREEN);
        assert_eq!(parent.children[0].sdf_feather, Some(2.5));
    }

    #[test]
    fn transform_then_composes() {
        let p = Transform::translation(10.0, 20.0);
        let c = Transform::translation(3.0, 4.0);
        let m = p.then(&c);
        let (_, _, t) = m.to_cols();
        assert!((t[0] - 13.0).abs() < 1e-5);
        assert!((t[1] - 24.0).abs() < 1e-5);
    }

    /// 三层 clips 的 flatten 顺序：root → mid → leaf → Pop → Pop
    #[test]
    fn nested_clips_flatten_emits_two_pops() {
        let mut root = DrawBatch::new();
        root.clips_children = true;
        draw_rectangle(&mut root, Pos::new(-10.0, -10.0), 20.0, 20.0, Some(RED));

        let mut mid = DrawBatch::new();
        mid.clips_children = true;
        mid.inherit = InheritFromParent::TRANSFORM;
        draw_circle(&mut mid, Pos::new(0.0, 0.0), 8.0, Some(GREEN));

        let mut leaf = DrawBatch::new();
        leaf.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut leaf, Pos::new(-2.0, -2.0), 4.0, 4.0, Some(BLUE));

        mid.push_child(leaf);
        root.push_child(mid);

        let mut flat: Vec<Option<&DrawBatch>> = Vec::new();
        root.flatten_with_pop(&mut flat);
        // root, mid, leaf, pop(mid), pop(root)
        assert_eq!(flat.len(), 5);
        assert!(flat[0].is_some());
        assert!(flat[1].is_some());
        assert!(flat[2].is_some());
        assert!(flat[3].is_none());
        assert!(flat[4].is_none());
        assert!(flat[0].unwrap().clips_children);
        assert!(flat[1].unwrap().clips_children);
        assert!(!flat[2].unwrap().clips_children);
    }

    /// 嵌套 push 时 ref 语义：root Push(0)→mid Push(1)→leaf Test(2)
    #[test]
    fn nested_clips_stencil_ref_sequence() {
        // 模拟 draw() 内 compute_stencil 的 ref 栈
        let mut root = DrawBatch::new();
        root.clips_children = true;
        draw_rectangle(&mut root, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        let mut mid = DrawBatch::new();
        mid.clips_children = true;
        mid.inherit = InheritFromParent::TRANSFORM;
        draw_circle(&mut mid, Pos::new(0.0, 0.0), 5.0, Some(GREEN));
        let mut leaf = DrawBatch::new();
        leaf.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut leaf, Pos::new(0.0, 0.0), 2.0, 2.0, Some(BLUE));
        mid.push_child(leaf);
        root.push_child(mid);

        let mut flat: Vec<Option<&DrawBatch>> = Vec::new();
        root.flatten_with_pop(&mut flat);

        let mut ref_stack: Vec<u32> = Vec::new();
        let mut active: Option<u32> = None;
        let mut ops: Vec<(u32, u32)> = Vec::new(); // (op, ref)
        for item in &flat {
            match item {
                Some(batch) => {
                    let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.geo_instances.is_empty();
                    let has_draw = has_geom || !batch.texts.entries.is_empty();
                    let (op, r) = if batch.clips_children && has_geom {
                        let push_ref = active.unwrap_or(0);
                        let new_lv = push_ref + 1;
                        ref_stack.push(new_lv);
                        active = Some(new_lv);
                        (1u32, push_ref)
                    } else if let Some(a) = active {
                        if batch.inherit.clipped && has_draw {
                            (2u32, a)
                        } else {
                            (0u32, 0)
                        }
                    } else {
                        (0u32, 0)
                    };
                    ops.push((op, r));
                }
                None => {
                    let popped = ref_stack.pop().unwrap_or(0);
                    active = if popped > 1 { Some(popped - 1) } else { None };
                    ops.push((3u32, popped));
                }
            }
        }
        assert_eq!(ops, vec![
            (1, 0), // root Push @0 → level 1
            (1, 1), // mid Push @1 → level 2
            (2, 2), // leaf Test @2
            (3, 2), // pop mid
            (3, 1), // pop root
        ]);
    }

    /// 读 transform 表第 `idx` 个 mat 的 (a,c,b,d,tx,ty)
    fn mat6(table: &[f32], idx: u32) -> (f32, f32, f32, f32, f32, f32) {
        let b = idx as usize * 12;
        assert!(b + 12 <= table.len(), "idx={idx} table_mats={}", table.len() / 12);
        (
            table[b],
            table[b + 1],
            table[b + 4],
            table[b + 5],
            table[b + 8],
            table[b + 9],
        )
    }

    /// 嵌套 Inherit TRANSFORM 后：leaf/mid 的形状与文字共用索引，且表内平移已含祖先
    #[test]
    fn nested_text_and_shape_share_composed_transform() {
        let mut root = DrawBatch::new();
        root.sdf_feather = Some(1.0);
        root.set_position(230.0, 270.0);
        root.clips_children = true;
        // 形状 Pos 为局部偏移；与 batch 平移组合后共享 transform entry
        draw_rounded_rect(&mut root, Pos::new(0.0, 0.0), 300.0, 260.0, 28.0, Some(RED));

        let mut mid = DrawBatch::new();
        mid.sdf_feather = Some(1.0);
        mid.set_position(40.0, 0.0);
        mid.clips_children = true;
        mid.inherit = InheritFromParent::TRANSFORM;
        // mid 形状用局部原点，与 batch 平移一致 → 共享 transform entry
        draw_circle(&mut mid, Pos::new(0.0, 0.0), 90.0, Some(GREEN));

        let mut leaf = DrawBatch::new();
        leaf.sdf_feather = Some(1.0);
        leaf.inherit = InheritFromParent::TRANSFORM;
        // leaf 无独立平移，形状用局部原点与 batch 一致
        draw_circle(&mut leaf, Pos::new(0.0, 0.0), 18.0, Some(WHITE));
        leaf.text(
            "LEAF",
            Pos::new(-40.0, -14.0),
            TextDef::default().font_size(28.0),
            TextOverride::from_color(BLACK),
        );

        mid.push_child(leaf);
        mid.text(
            "MID",
            Pos::new(-36.0, -34.0),
            TextDef::default().font_size(26.0),
            TextOverride::from_color(YELLOW),
        );
        root.push_child(mid);
        root.text(
            "ROOT",
            Pos::new(-50.0, -58.0),
            TextDef::default().font_size(26.0),
            TextOverride::from_color(SKYBLUE),
        );

        // --- root ---
        assert_eq!(root.texts.entries.len(), 1);
        let rti = root.texts.entries[0].transform_index();
        let rvi = root.instances[0].transform_index;
        assert_eq!(rti, rvi, "root text/shape index");
        let (_, _, _, _, rtx, rty) = mat6(&root.transform_table, rti);
        assert!((rtx - 230.0).abs() < 1e-3, "root tx={rtx}");
        assert!((rty - 270.0).abs() < 1e-3, "root ty={rty}");

        // --- mid（继承后应为 root∘mid_local = (270, 270)）---
        let mid = &root.children[0];
        assert_eq!(mid.texts.entries.len(), 1);
        let mti = mid.texts.entries[0].transform_index();
        let mvi = mid.instances[0].transform_index;
        assert_eq!(mti, mvi, "mid text/shape index");
        let (_, _, _, _, mtx, mty) = mat6(&mid.transform_table, mti);
        assert!(
            (mtx - 270.0).abs() < 1e-3 && (mty - 270.0).abs() < 1e-3,
            "mid composed tx,ty=({mtx},{mty}) want (270,270)"
        );

        // --- leaf（继承后应与 mid 同世界原点 (270,270)）---
        let leaf = &root.children[0].children[0];
        assert_eq!(leaf.texts.entries.len(), 1);
        let lti = leaf.texts.entries[0].transform_index();
        let lvi = leaf.instances[0].transform_index;
        assert_eq!(lti, lvi, "leaf text/shape index");
        let (_, _, _, _, ltx, lty) = mat6(&leaf.transform_table, lti);
        assert!(
            (ltx - 270.0).abs() < 1e-3 && (lty - 270.0).abs() < 1e-3,
            "leaf composed tx,ty=({ltx},{lty}) want (270,270)"
        );

        // 文字局部坐标：LEAF 在 leaf 原点附近，变换后世界 ≈ (230,256) 仍在 mid 圆内
        let wx = ltx + leaf.texts.entries[0].pos().x;
        let wy = lty + leaf.texts.entries[0].pos().y;
        let dx = wx - 270.0;
        let dy = wy - 270.0;
        let dist = (dx * dx + dy * dy).sqrt();
        assert!(
            dist < 90.0,
            "LEAF text world ({wx},{wy}) dist_from_mid_center={dist} should be inside r=90"
        );
    }

    /// 仅文字、无形状的子：inherit 后 transform_table 仍应被左乘
    #[test]
    fn inherit_transform_text_only_child_gets_table_entry() {
        let mut parent = DrawBatch::new();
        parent.set_position(100.0, 50.0);
        draw_rectangle(&mut parent, Pos::new(-10.0, -10.0), 20.0, 20.0, Some(RED));

        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::TRANSFORM;
        child.text(
            "hi",
            Pos::new(0.0, 0.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(WHITE),
        );
        // text() 会注册当前 transform（恒等）到 table
        assert!(!child.transform_table.is_empty());
        let ti_before = child.texts.entries[0].transform_index();
        let (_, _, _, _, tx0, ty0) = mat6(&child.transform_table, ti_before);
        assert!(tx0.abs() < 1e-5 && ty0.abs() < 1e-5);

        parent.push_child(child);
        let c = &parent.children[0];
        let ti = c.texts.entries[0].transform_index();
        let (_, _, _, _, tx, ty) = mat6(&c.transform_table, ti);
        assert!((tx - 100.0).abs() < 1e-3, "tx={tx}");
        assert!((ty - 50.0).abs() < 1e-3, "ty={ty}");
    }

    /// 文字在 draw 形状之前 push：索引仍应与之后形状一致（同画笔）
    #[test]
    fn text_before_shape_shares_transform_index() {
        let mut b = DrawBatch::new();
        b.set_position(12.0, 34.0);
        b.text(
            "A",
            Pos::new(0.0, 0.0),
            TextDef::default().font_size(12.0),
            TextOverride::from_color(WHITE),
        );
        // 形状 Pos 为局部原点，与 batch 平移组合 → 共享 transform entry
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(RED));
        assert_eq!(
            b.texts.entries[0].transform_index(),
            b.instances[0].transform_index
        );
        let (_, _, _, _, tx, ty) = mat6(&b.transform_table, b.texts.entries[0].transform_index());
        assert!((tx - 12.0).abs() < 1e-4 && (ty - 34.0).abs() < 1e-4);
    }

    /// 绘制顺序：父 shapes+texts 先于子；父文字会被不透明子盖住
    #[test]
    fn draw_order_parent_text_before_children() {
        let mut root = DrawBatch::new();
        root.clips_children = true;
        draw_rectangle(&mut root, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        root.text("R", Pos::new(0.0, 0.0), TextDef::default().font_size(12.0), TextOverride::from_color(WHITE));

        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 10.0, 10.0, Some(BLUE));
        child.text("C", Pos::new(0.0, 0.0), TextDef::default().font_size(12.0), TextOverride::from_color(WHITE));
        root.push_child(child);

        let mut flat: Vec<Option<&DrawBatch>> = Vec::new();
        root.flatten_with_pop(&mut flat);
        // root(有字) → child(有字) → Pop
        assert_eq!(flat.len(), 3);
        assert!(!flat[0].unwrap().texts.entries.is_empty());
        assert!(!flat[1].unwrap().texts.entries.is_empty());
        assert!(flat[2].is_none());
        // 子在父之后 → 同区域会盖住父文字（文档化行为，非 bug）
        assert!(flat[0].unwrap().clips_children);
    }

    /// 单 batch 含 area_include → flatten 输出 AreaOp(setup) + Batch + AreaOp(cleanup)
    #[test]
    fn area_flatten_include_emits_cover_and_erase() {
        let mut b = DrawBatch::new();
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        // 用一个简单矩形作 include
        let mut include_batch = DrawBatch::new();
        draw_rectangle(&mut include_batch, Pos::new(0.0, 0.0), 100.0, 100.0, Some(WHITE));
        b.area_include = Some(include_batch.to_area());

        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // 1 cover op + 1 Batch + 1 erase op = 3 events
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], DrawEvent::AreaOp { is_setup: true, .. }));
        assert!(matches!(events[1], DrawEvent::Batch(_)));
        assert!(matches!(events[2], DrawEvent::AreaOp { is_setup: false, .. }));
    }

    /// 嵌套 clips_children + Area：AreaOp 套住子树，clips Push/Pop 仍在子树内部
    #[test]
    fn area_flatten_with_clips_children() {
        let mut root = DrawBatch::new();
        root.clips_children = true;
        draw_rectangle(&mut root, Pos::new(0.0, 0.0), 10.0, 10.0, Some(RED));
        // root 加 area_include
        let mut incl = DrawBatch::new();
        draw_rectangle(&mut incl, Pos::new(0.0, 0.0), 100.0, 100.0, Some(RED));
        root.area_include = Some(incl.to_area());

        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::TRANSFORM;
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 4.0, 4.0, Some(GREEN));
        root.push_child(child);

        let mut events: Vec<DrawEvent> = Vec::new();
        root.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // 1 setup + Batch + child.Batch + StencilPop + 1 cleanup = 5
        assert_eq!(events.len(), 5);
        assert!(matches!(events[0], DrawEvent::AreaOp { is_setup: true, .. }));
        assert!(matches!(events[1], DrawEvent::Batch(_)));
        assert!(matches!(events[2], DrawEvent::Batch(_)));
        assert!(matches!(events[3], DrawEvent::StencilPop));
        assert!(matches!(events[4], DrawEvent::AreaOp { is_setup: false, .. }));
    }

    /// effective Area = Empty → 不发 AreaOp
    #[test]
    fn area_flatten_empty_skips_ops() {
        let mut b = DrawBatch::new();
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        // empty 几何 → Area::Empty
        b.area_include = Some(Area::Empty);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // 仅 Batch（无 AreaOp）
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DrawEvent::Batch(_)));
    }

    /// Area + clips_children：子 Test ref = cover 后 buffer（2），不是双重计数 3
    #[test]
    fn area_plus_clips_child_stencil_ref_not_double_counted() {
        let mut parent = DrawBatch::new();
        parent.clips_children = true;
        draw_rectangle(&mut parent, Pos::new(0.0, 0.0), 100.0, 100.0, Some(RED));
        let mut incl = DrawBatch::new();
        draw_circle(&mut incl, Pos::new(50.0, 50.0), 40.0, Some(WHITE));
        parent.area_include = Some(incl.to_area());
        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::NONE; // clipped 默认 true
        draw_rectangle(&mut child, Pos::new(10.0, 10.0), 20.0, 20.0, Some(GREEN));
        parent.push_child(child);

        let mut events: Vec<DrawEvent> = Vec::new();
        parent.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());

        // 模拟 draw 路径：clip_depth 与 area_depth 分离
        let mut clip_depth = 0u32;
        let mut area_depth = 0u32;
        let mut prev_cleanup = false;
        let mut child_test_ref: Option<u32> = None;
        let mut parent_push_ref: Option<u32> = None;

        for ev in &events {
            match ev {
                DrawEvent::Batch(batch) => {
                    prev_cleanup = false;
                    let has_own = batch
                        .effective_area()
                        .as_ref()
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    let anc = area_depth;
                    if has_own {
                        area_depth += 1;
                    }
                    let content = clip_depth + anc + (has_own as u32);
                    let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.geo_instances.is_empty();
                    if batch.clips_children && has_geom {
                        parent_push_ref = Some(content);
                        clip_depth += 1;
                    } else if content > 0 && batch.inherit.clipped && has_geom {
                        child_test_ref = Some(content);
                    }
                }
                DrawEvent::StencilPop => {
                    prev_cleanup = false;
                    clip_depth = clip_depth.saturating_sub(1);
                }
                DrawEvent::AreaOp { is_setup, .. } => {
                    if !*is_setup {
                        if !prev_cleanup {
                            area_depth = area_depth.saturating_sub(1);
                        }
                        prev_cleanup = true;
                    } else {
                        prev_cleanup = false;
                    }
                }
                _ => {}
            }
        }
        // cover@0 → buffer 1；parent content=1 Push@1 → buffer 2；child Test@2
        assert_eq!(parent_push_ref, Some(1), "parent Push ref");
        assert_eq!(child_test_ref, Some(2), "child Test must be 2 not 3");
    }

    // ---- culling tests ----

    #[test]
    fn bounds_culls_offscreen_subtree() {
        let mut b = DrawBatch::new();
        b.bounds = Some(Some(Rect::new(9999.0, 9999.0, 10.0, 10.0)));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        assert!(events.is_empty());
    }

    #[test]
    fn bounds_keeps_onscreen_subtree() {
        let mut b = DrawBatch::new();
        b.bounds = Some(Some(Rect::new(100.0, 100.0, 50.0, 50.0)));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DrawEvent::Batch(_)));
    }

    #[test]
    fn empty_container_with_offscreen_children_recurse() {
        // 空容器无 bounds → 不能剪，自身体现为 event（无顶点）
        // 子屏外 → 子被剪
        let mut parent = DrawBatch::new(); // 无顶点
        let mut child = DrawBatch::new();
        child.inherit = InheritFromParent::TRANSFORM;
        child.set_position(9999.0, 9999.0);
        draw_rectangle(&mut child, Pos::ZERO, 4.0, 4.0, Some(WHITE));
        parent.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        parent.flatten_events(&mut events, 0, Some(Rect::new(0.0, 0.0, 800.0, 600.0)), &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // parent 空容器 → 自身 event（无顶点），子剪掉
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], DrawEvent::Batch(b) if b.vertices.is_empty()));
    }

    #[test]
    fn scissor_emits_scissor_events() {
        let mut b = DrawBatch::new();
        b.clips_children = true;
        b.scissor = Some(Rect::new(10.0, 10.0, 200.0, 150.0));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
        assert!(matches!(&events[1], DrawEvent::ScissorPush(r) if *r == Rect::new(10.0, 10.0, 200.0, 150.0)));
        assert!(matches!(&events[2], DrawEvent::Batch(_)));
        assert!(matches!(&events[3], DrawEvent::ScissorPop));
    }

    #[test]
    fn scissor_without_clips_children_still_emits_scissor() {
        let mut b = DrawBatch::new();
        b.scissor = Some(Rect::new(0.0, 0.0, 100.0, 100.0));
        b.clips_children = false;
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // scissor 现在不依赖 clips_children，仍会为子节点发 ScissorPush/Pop
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
        assert!(matches!(&events[1], DrawEvent::ScissorPush(_)));
        assert!(matches!(&events[2], DrawEvent::Batch(_)));
        assert!(matches!(&events[3], DrawEvent::ScissorPop));
    }

    #[test]
    fn scissor_does_not_set_uses_stencil() {
        let mut b = DrawBatch::new();
        b.clips_children = true;
        b.scissor = Some(Rect::new(0.0, 0.0, 100.0, 100.0));
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 10.0, 10.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        let uses_stencil = events.iter().any(|e| matches!(e, DrawEvent::StencilPop | DrawEvent::AreaOp { .. }));
        assert!(!uses_stencil);
    }

    #[test]
    fn auto_scissor_detects_single_rect() {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        b.clips_children = true;
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 100.0, 50.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // 自动检测为矩形 → ScissorPush/Pop 代替 StencilPop
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[1], DrawEvent::ScissorPush(r) if (r.w - 100.0).abs() < 1e-4));
        assert!(matches!(&events[3], DrawEvent::ScissorPop));
        assert!(events.iter().all(|e| !matches!(e, DrawEvent::StencilPop)));
    }

    #[test]
    fn auto_scissor_ignores_nonrect() {
        let mut b = DrawBatch::new();
        b.clips_children = true;
        // 三角形（3 顶点），不是矩形
        crate::shapes::draw_triangle(&mut b, 0.0, 0.0, 100.0, 0.0, 0.0, 50.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // 非矩形 → 走 stencil
        assert!(events.iter().any(|e| matches!(e, DrawEvent::StencilPop)));
    }

    #[test]
    fn auto_scissor_no_children_skips_scissor_events() {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        b.clips_children = true;
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 100.0, 50.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // 无子：不发空 scissor Push/Pop
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
    }

    #[test]
    fn geo_instance_clip_emits_stencil_pop() {
        // 回归：flatten_events 的 has_geom 此前漏了 geo_instances。
        // sdf_feather=None 的几何走 geo_instance_shape → 只有 geo_instances
        //（vertices/instances 均空）；clips_children=true 时 draw 阶段会 Push
        // 但 flatten 不发 StencilPop → clip_depth 泄漏、后续 batch stencil ref 偏移。
        let mut g = DrawBatch::new();
        g.sdf_feather = None;
        g.clips_children = true;
        crate::shapes::draw_triangle(&mut g, 0.0, 0.0, 100.0, 0.0, 0.0, 50.0, Some(WHITE));
        assert!(!g.geo_instances.is_empty());
        assert!(g.vertices.is_empty() && g.instances.is_empty());
        let mut events: Vec<DrawEvent> = Vec::new();
        g.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        assert!(events.iter().any(|e| matches!(e, DrawEvent::StencilPop)));
    }

    #[test]
    fn auto_scissor_requires_clips_children() {
        let mut b = DrawBatch::new();
        b.clips_children = false;
        draw_rectangle(&mut b, Pos::new(0.0, 0.0), 100.0, 50.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
        assert!(matches!(&events[1], DrawEvent::Batch(_)));
    }

    #[test]
    fn stencil_pop_when_children_all_culled() {
        // 非矩形父 → stencil 路径；子全 cull 仍要 Pop（与 Push 成对）
        let mut parent = DrawBatch::new();
        parent.clips_children = true;
        crate::shapes::draw_triangle(
            &mut parent,
            0.0, 0.0, 100.0, 0.0, 0.0, 50.0,
            Some(RED),
        );
        let mut child = DrawBatch::new();
        child.bounds = Some(Some(Rect::new(9999.0, 9999.0, 4.0, 4.0)));
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 4.0, 4.0, Some(WHITE));
        parent.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        parent.flatten_events(
            &mut events,
            0,
            Some(Rect::new(0.0, 0.0, 800.0, 600.0)),
            &FxHashMap::default(),
            &Transform::IDENTITY,
            &mut FxHashMap::default(),
        );
        assert!(matches!(&events[0], DrawEvent::Batch(_)));
        assert!(
            events.iter().any(|e| matches!(e, DrawEvent::StencilPop)),
            "culled children must still emit StencilPop"
        );
    }

    #[test]
    fn auto_aabb_culls_offscreen_pos() {
        // Pos 进表、画笔 restore 后：AABB 仍应按世界位置裁
        let mut b = DrawBatch::new();
        draw_rectangle(&mut b, Pos::new(9999.0, 9999.0), 4.0, 4.0, Some(WHITE));
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(
            &mut events,
            0,
            Some(Rect::new(0.0, 0.0, 800.0, 600.0)),
            &FxHashMap::default(),
            &Transform::IDENTITY,
            &mut FxHashMap::default(),
        );
        assert!(events.is_empty());
    }

    #[test]
    fn auto_scissor_uses_pos_world_rect() {
        let mut b = DrawBatch::new();
        b.sdf_feather = None;
        b.clips_children = true;
        draw_rectangle(&mut b, Pos::new(50.0, 60.0), 100.0, 50.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        assert_eq!(events.len(), 4);
        match &events[1] {
            DrawEvent::ScissorPush(r) => {
                assert!((r.x - 50.0).abs() < 1e-3, "x={}", r.x);
                assert!((r.y - 60.0).abs() < 1e-3, "y={}", r.y);
                assert!((r.w - 100.0).abs() < 1e-3);
                assert!((r.h - 50.0).abs() < 1e-3);
            }
            _ => panic!("expected ScissorPush"),
        }
    }

    #[test]
    fn auto_scissor_skips_sdf_circle() {
        let mut b = DrawBatch::new();
        b.clips_children = true;
        b.sdf_feather = Some(1.0);
        draw_circle(&mut b, Pos::new(100.0, 100.0), 40.0, Some(WHITE));
        let mut child = DrawBatch::new();
        draw_rectangle(&mut child, Pos::new(0.0, 0.0), 5.0, 5.0, Some(WHITE));
        b.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(&mut events, 0, None, &FxHashMap::default(), &Transform::IDENTITY, &mut FxHashMap::default());
        // SDF 圆不得 auto-scissor → StencilPop
        assert!(events.iter().any(|e| matches!(e, DrawEvent::StencilPop)));
        assert!(events.iter().all(|e| !matches!(e, DrawEvent::ScissorPush(_))));
    }

    #[test]
    fn text_only_batch_not_culled_when_onscreen() {
        let mut b = DrawBatch::new();
        b.text(
            "hi",
            Pos::new(10.0, 10.0),
            TextDef::default().font_size(16.0),
            TextOverride::from_color(WHITE),
        );
        let mut events: Vec<DrawEvent> = Vec::new();
        b.flatten_events(
            &mut events,
            0,
            Some(Rect::new(0.0, 0.0, 800.0, 600.0)),
            &FxHashMap::default(),
            &Transform::IDENTITY,
            &mut FxHashMap::default(),
        );
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn transform_then_rotation_matches_table_compose() {
        // 验证 then 与列主序表布局一致（文字 override 依赖此）
        let m = Transform::translation(10.0, 20.0);
        let o = Transform::trs(0.0, 0.0, 0.0, 0.0, std::f32::consts::FRAC_PI_2, 1.0, 1.0);
        let c = m.then(&o);
        let (c0, c1, c2) = c.to_cols();
        // 90° 顺时针：a=0,b=-1,c=1,d=0；平移 (10,20)
        assert!(c0[0].abs() < 1e-5);
        assert!((c1[0] + 1.0).abs() < 1e-5);
        assert!((c0[1] - 1.0).abs() < 1e-5);
        assert!(c1[1].abs() < 1e-5);
        assert!((c2[0] - 10.0).abs() < 1e-3);
        assert!((c2[1] - 20.0).abs() < 1e-3);
    }

    #[test]
    fn draw_batch_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<DrawBatch>();
    }

    #[test]
    fn draw_batch_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<DrawBatch>();
    }

    #[test]
    fn flatten_records_effective_view_for_batch_and_children() {
        let mut parent = DrawBatch::new();
        parent.view = Transform::translation(100.0, 200.0);
        let mut child = DrawBatch::new();
        child.view = Transform::translation(5.0, 7.0);
        parent.push_child(child);
        let mut events: Vec<DrawEvent> = Vec::new();
        let mut view_map = FxHashMap::default();
        parent.flatten_events(
            &mut events,
            0,
            None,
            &FxHashMap::default(),
            &Transform::IDENTITY,
            &mut view_map,
        );
        assert_eq!(events.len(), 2);
        let pkey = &(&parent as *const DrawBatch as *const () as usize);
        let child_ref = match &events[1] {
            DrawEvent::Batch(cb) => *cb,
            _ => unreachable!(),
        };
        let ckey = &(child_ref as *const DrawBatch as *const () as usize);
        // 父子 view 有效值：父 = 自身 view；子 = 父 view × 子 view（左乘）。
        let pe = view_map[pkey];
        assert_eq!(pe, Transform::translation(100.0, 200.0));
        let ce = view_map[ckey];
        assert_eq!(ce, Transform::translation(105.0, 207.0));
    }

    #[test]
    fn left_mul_view_table_applies_view_to_rows() {
        // 单位视图：整表原样
        let table = vec![
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 12.0, 34.0, 1.0, 0.0,
        ];
        let mut out = Vec::new();
        left_mul_view_table(&Transform::IDENTITY, &table, &mut out);
        assert_eq!(out, table);
        // 平移视图：tx/ty 列被左乘（单位线性部分）
        let view = Transform::translation(50.0, 60.0);
        left_mul_view_table(&view, &table, &mut out);
        assert_eq!(out.len(), 12);
        // 平移列 = (50+12, 60+34)（view 线性部分为单位阵）
        assert!((out[8] - 62.0).abs() < 1e-4);
        assert!((out[9] - 94.0).abs() < 1e-4);
    }
