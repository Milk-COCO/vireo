use std::sync::Arc;

use crate::gpu::{GeoInstance, GeoVertex, MaterialTarget, ShapeInstance, Vertex};
use crate::math::{left_mul_view_table, Transform, IDENTITY_TRANSFORM_ROW};
use crate::material::Material;

use super::{BatchShapeCommand, DrawBatch, DrawEvent, EventInfo, GeoInstanceSegment, InstanceSegment, OrderedShapeSegment, RenderTarget, Renderer, ShapeInfo, ShapeSegment, TextRenderSegment};
use super::prepare_culling;

impl Renderer {
    /// 编码渲染命令到 `CommandBuffer`，**不** submit/present。
    ///
    /// 调用方负责在持有目标 surface/texture 帧循环的线程上：
    /// ```ignore
    /// queue.submit([cmd_buf]);
    /// queue.present(surface_texture);
    /// ```
    ///
    /// 返回的 `CommandBuffer` 持有对 `target.view` 的引用（`TextureView`），
    /// 在 `submit` 之前 `target.view` 必须保持有效（即 `SurfaceTexture` 未被销毁）。
    ///
    /// 当前窗口路径由渲染线程在 `VireoWindow::draw` 内完成 acquire、调用本方法、
    /// submit 和 present；winit owner 线程不参与逐帧 surface 提交。离屏调用方则自行 submit。
    pub fn draw(
        &self,
        target: &RenderTarget,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) -> wgpu::CommandBuffer {
        // ---- 前序展开子树（含 Pop 事件 + Area 事件）----
        // 可见 = 祖先 stencil ∧ batch 自身有效 Area。
        // Area 编译为掩码 op（无色）：AreaSetup 在 batch 前盖、AreaCleanup 在子树后擦。
        // Area 存在时，batch 自身 content 在 base+1 测（Area∩base），子树按 clips_children 走。
        // clips_children + Area：Push at base+1（content level），子看 base+2；Pop 回 base+1。
        let mut events: Vec<DrawEvent> = Vec::new();
        let (_viewport, uses_stencil) = prepare_culling(
            batches,
            self.logical_width,
            self.logical_height,
            &self.scratch_aabb_map,
            &self.scratch_view_map,
            &mut events,
        );

        let has_content = clear_color.is_some()
            || events.iter().any(|e| matches!(e, DrawEvent::Batch(b) if !b.vertices.is_empty() || !b.instances.is_empty() || !b.geo_instances.is_empty() || !b.texts.entries.is_empty()));
        if !has_content {
            // 无内容：返回空 cmd_buf（不创建 render pass 即可）
            self.last_draw_calls.set(0);
            let empty_encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vireo empty encoder"),
            });
            return empty_encoder.finish();
        }

        let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vireo encoder"),
        });

        let load = match clear_color {
            Some(c) => wgpu::LoadOp::Clear(wgpu::Color {
                r: c.r as f64,
                g: c.g as f64,
                b: c.b as f64,
                a: c.a as f64,
            }),
            None => wgpu::LoadOp::Load,
        };

        let target_view = &target.view;
        // 相机为逻辑像素正交；Pop 全屏四边形也用逻辑尺寸
        let lw = self.logical_width;
        let lh = self.logical_height;

        // ---- 在 pass 外写入所有 batch 的 vertex/index 数据 ----
        let mut event_infos = self.scratch_event_infos.borrow_mut();
        event_infos.clear();
        let vertex_count: u32 = 0;
        let ndx_accum: u32 = 0;

        // ---- 单次扫描：合并 transform/poly + 统计顶点数 ----
        let mut global_transforms = self.scratch_transforms.borrow_mut();
        global_transforms.clear();
        // 全局表槽 0 = 单位阵（与 batch `transform_table` 槽 0 约定一致）。
        // `draw_text` / glyphon 默认 transform_index=0 表示恒等；batch 表上传时
        // `transform_base` 会偏移局部 index，故全局槽 0 仍须单独预留，不能被首个 batch 占用。
        global_transforms.extend_from_slice(&IDENTITY_TRANSFORM_ROW);
        let mut polygon_edges_global = self.scratch_poly_edges.borrow_mut();
        polygon_edges_global.clear();

        // stencil 两路计数（不可混用）：
        // - `clip_depth`：仅 clips_children 的 Push 层数（不含 Area）
        // - `area_depth`：仍打开的 Area 框架数
        // content_level = clip_depth + area_depth_ancestors + has_own_area
        // Push@content_level 后 buffer 为 content_level+1；clip_depth+=1，
        // 子节点 content = (clip_depth) + area… 不会把 Area 算两次。
        fn compute_stencil_at_level(
            batch: &DrawBatch,
            content_level: u32,
            ref_stack: &mut Vec<u32>,
        ) -> (u32, u32) {
            let has_geom = !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.geo_instances.is_empty();
            let has_draw = has_geom || !batch.texts.entries.is_empty();
            if batch.clips_children && (has_geom || batch.scissor.is_some()) {
                // Push: Test content_level → Inc；ref_stack 存抬升后绝对值供 Pop
                let push_ref = content_level;
                ref_stack.push(push_ref + 1);
                (1u32, push_ref)
            } else {
                // `clips_children && !has_geom && scissor.is_none()` 是 no-op；
                // dev 模式立刻提示用户，release 保持原行为（静默跳过）
                debug_assert!(
                    !batch.clips_children || has_geom || batch.scissor.is_some(),
                    "clips_children=true 但 batch 无几何且无显式 scissor；裁切不会生效。请提供几何裁切形状或显式设置 batch.scissor"
                );
                if content_level > 0 {
                    if batch.inherit.clipped && has_draw {
                        (2u32, content_level) // Test
                    } else {
                        (0u32, 0)
                    }
                } else {
                    (0u32, 0)
                }
            }
        }

        let mut ref_stack = self.scratch_ref_stack.borrow_mut();
        ref_stack.clear();
        let mut clip_depth: u32 = 0;
        let mut area_depth: u32 = 0;
        // 连续 cleanup AreaOp 只 -1 一次（compile_erase 可多 op）
        let mut prev_area_cleanup = false;

        for event in &events {
            match event {
                DrawEvent::Batch(batch) => {
                    prev_area_cleanup = false;
                    let has_own_area = batch
                        .effective_area()
                        .as_ref()
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    let ancestors_area_depth = area_depth;
                    if has_own_area {
                        area_depth += 1;
                    }
                    let content_level =
                        clip_depth + ancestors_area_depth + (has_own_area as u32);
                    // 与 flatten_events 共用条件（见 DrawBatch::uses_scissor_path）
                    let use_scissor = batch.uses_scissor_path(has_own_area);
                    let (stencil_op, stencil_ref) = if use_scissor {
                        let has_draw =
                            !batch.vertices.is_empty() || !batch.instances.is_empty() || !batch.geo_instances.is_empty() || !batch.texts.entries.is_empty();
                        if content_level > 0 && batch.inherit.clipped && has_draw {
                            (2u32, content_level) // Test 祖先，不 Push
                        } else {
                            (0u32, 0u32)
                        }
                    } else {
                        compute_stencil_at_level(batch, content_level, &mut *ref_stack)
                    };
                    if stencil_op == 1 {
                        // 只增加 clip 层，不含 Area（Area 已在 content_level 里）
                        clip_depth += 1;
                    }
                    let custom_mat = batch.custom_material.clone();
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op,
                        stencil_ref,
                        area_op: None,
                        scissor_push: None,
                        scissor_pop: false,
                        custom_material: custom_mat,
                        custom_text_pipeline: None,
                        dynamic_offsets: batch.dynamic_offsets.clone(),
                    });
                }
                DrawEvent::StencilPop => {
                    prev_area_cleanup = false;
                    let popped = ref_stack.pop();
                    clip_depth = clip_depth.saturating_sub(1);
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op: 3,
                        stencil_ref: popped.unwrap_or(0),
                        area_op: None,
                        scissor_push: None,
                        scissor_pop: false,
                        custom_material: None,
                        custom_text_pipeline: None,
                        dynamic_offsets: Vec::new(),
                    });
                }
                DrawEvent::AreaOp { op, is_setup } => {
                    // Area 单 op：cover (op 4) 在 batch 前，erase (op 3) 在子树后。
                    // setup 在 Batch 事件里 +1；cleanup 连续多 op 只 -1 一次。
                    let pipe_op = op.stencil_pipeline_op(); // 3 or 4
                    let r = op.stencil_ref();
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op: pipe_op,
                        stencil_ref: r,
                        area_op: Some(pipe_op),
                        scissor_push: None,
                        scissor_pop: false,
                        custom_material: None,
                        custom_text_pipeline: None,
                        dynamic_offsets: Vec::new(),
                    });
                    if !is_setup {
                        if !prev_area_cleanup {
                            area_depth = area_depth.saturating_sub(1);
                        }
                        prev_area_cleanup = true;
                    } else {
                        prev_area_cleanup = false;
                    }
                }
                DrawEvent::ScissorPush(rect) => {
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op: 0,
                        stencil_ref: 0,
                        area_op: None,
                        scissor_push: Some(*rect),
                        scissor_pop: false,
                        custom_material: None,
                        custom_text_pipeline: None,
                        dynamic_offsets: Vec::new(),
                    });
                }
                DrawEvent::ScissorPop => {
                    event_infos.push(EventInfo {
                        shape: None,
                        text: Vec::new(),
                        stencil_op: 0,
                        stencil_ref: 0,
                        area_op: None,
                        scissor_push: None,
                        scissor_pop: true,
                        custom_material: None,
                        custom_text_pipeline: None,
                        dynamic_offsets: Vec::new(),
                    });
                }
            }
        }

        // 收集 transform/poly 信息
        let mut batch_transform_bases = self.scratch_batch_transform_bases.borrow_mut();
        batch_transform_bases.clear();
        let mut batch_poly_base = self.scratch_batch_poly_base.borrow_mut();
        batch_poly_base.clear();
        let mut total_vcount: u32 = 0;
        let mut total_icount: u32 = 0;
        let mut poly_offset: u32 = 0;
        let mut pop_screen_verts: u32 = 0; // 全屏 Pop 顶点数
        let mut pop_screen_idx: u32 = 0;

        let mut combined_geo_vertices = self.scratch_geo_vertices.borrow_mut();
        let mut combined_geo_indices = self.scratch_geo_indices.borrow_mut();
        combined_geo_vertices.clear();
        combined_geo_indices.clear();
        let mut batch_geo_vertex_base = self.scratch_batch_geo_vertex_base.borrow_mut();
        let mut batch_geo_index_base = self.scratch_batch_geo_index_base.borrow_mut();
        batch_geo_vertex_base.clear();
        batch_geo_index_base.clear();
        {
            let view_map = self.scratch_view_map.borrow();
            let mut view_table = self.scratch_view_table.borrow_mut();

            for (ei, event) in events.iter().enumerate() {
                if let DrawEvent::Batch(batch) = event {
                    let _e = &mut event_infos[ei];
                    batch_transform_bases.push((global_transforms.len() / 12) as u32);
                    // 左乘有效视图：几何与文字共用同一张表（见 `flatten_events` view_map）。
                let eff = view_map
                    .get(&(*batch as *const DrawBatch as *const () as usize))
                    .copied()
                    .unwrap_or(Transform::IDENTITY);
                    left_mul_view_table(&eff, &batch.transform_table, &mut view_table);
                    global_transforms.extend_from_slice(&view_table);
                    batch_poly_base.push(poly_offset);
                    poly_offset += batch.polygon_edges.len() as u32 / 4;
                    polygon_edges_global.extend_from_slice(&batch.polygon_edges);
                    total_vcount += batch.vertices.len() as u32;
                    total_icount += batch.indices.len() as u32;
                    if batch.custom_material.is_some() {
                        total_vcount += batch.instances.len() as u32 * 4;
                        total_icount += batch.instances.len() as u32 * 6;
                    }
                    batch_geo_vertex_base.push(combined_geo_vertices.len() as u32);
                    batch_geo_index_base.push(combined_geo_indices.len() as u32);
                    combined_geo_vertices.extend_from_slice(&batch.geo_template_vertices);
                    combined_geo_indices.extend_from_slice(&batch.geo_template_indices);
                } else if let DrawEvent::StencilPop = event {
                    // Pop 事件：添加全屏四边形（2 三角，6 索引）
                    pop_screen_verts += 4;
                    pop_screen_idx += 6;
                    batch_transform_bases.push(0);
                    batch_poly_base.push(poly_offset);
                    batch_geo_vertex_base.push(0);
                    batch_geo_index_base.push(0);
                } else if let DrawEvent::ScissorPush(_) | DrawEvent::ScissorPop = event {
                    // Scissor 事件不需要 transform/poly，但保留索引对齐
                    batch_transform_bases.push(0);
                    batch_poly_base.push(poly_offset);
                    batch_geo_vertex_base.push(0);
                    batch_geo_index_base.push(0);
                } else if let DrawEvent::AreaOp { op, .. } = event {
                    // Area 事件：Full → 全屏 4v/6i；Geom → AreaGeom 自带 v/i。
                    if let Some(geom) = op.geom() {
                        total_vcount += geom.vertices.len() as u32;
                        total_icount += geom.indices.len() as u32;
                        // 空表：顶点 index 走全局槽 0（单位阵），不追加、不 patch 偏移。
                        if geom.transform_table.is_empty() {
                            batch_transform_bases.push(0);
                        } else {
                            batch_transform_bases.push((global_transforms.len() / 12) as u32);
                            global_transforms.extend_from_slice(&geom.transform_table);
                        }
                        batch_poly_base.push(poly_offset);
                        poly_offset += geom.polygon_edges.len() as u32 / 4;
                        polygon_edges_global.extend_from_slice(&geom.polygon_edges);
                    } else {
                        pop_screen_verts += 4;
                        pop_screen_idx += 6;
                        batch_transform_bases.push(0);
                        batch_poly_base.push(poly_offset);
                    }
                    batch_geo_vertex_base.push(0);
                    batch_geo_index_base.push(0);
                }
            }
        }

        let total_vbytes = (total_vcount + pop_screen_verts) as u64 * size_of::<Vertex>() as u64;
        let total_ibytes = (total_icount + pop_screen_idx) as u64 * 4;
        self.ensure_vertex_buffer(total_vbytes);
        self.ensure_index_buffer(total_ibytes);
        let mut combined_vdata = self.scratch_vdata.borrow_mut();
        let mut combined_idata = self.scratch_idata.borrow_mut();
        let mut combined_instances = self.scratch_instances.borrow_mut();
        let mut combined_geo_instances = self.scratch_geo_instances.borrow_mut();
        combined_vdata.clear();
        combined_idata.clear();
        combined_instances.clear();
        combined_geo_instances.clear();
        let cap_v = total_vbytes as usize;
        let cap_i = total_ibytes as usize;
        if combined_vdata.capacity() < cap_v {
            combined_vdata.reserve(cap_v);
        }
        if combined_idata.capacity() < cap_i {
            combined_idata.reserve(cap_i);
        }

        // 合并数据 + 为 Pop 事件添加全屏顶点
        let mut v_offset = vertex_count;
        let mut idx_offset = ndx_accum;
        // merge_geo 排序后每实例的 texture segment 索引（None = 本轮未启用重排）
        let mut geo_merge_sorted_seg = self.scratch_geo_merge_sorted.borrow_mut();
        for (ei, event) in events.iter().enumerate() {
            match event {
                DrawEvent::Batch(batch) => {
                    let info_idx = ei;
                    let instance_start = combined_instances.len() as u32;
                    // fragment-only custom material（无 custom VS）可走 SDF instance path；
                    // 带 custom VS 的 Material 必须落回 mesh（VS 与 instance 字段契约不一致）。
                    let fragment_only = batch
                        .custom_material
                        .as_ref()
                        .map(|m| !m.has_custom_vertex_shader())
                        .unwrap_or(true);
                    let use_instances = !batch.instances.is_empty() && fragment_only;
                    if use_instances {
                        let transform_base = batch_transform_bases[info_idx];
                        combined_instances.extend(batch.instances.iter().copied().map(|mut instance| {
                            instance.transform_index += transform_base;
                            if instance.sdf_type == 6 || instance.sdf_type == 7 {
                                instance.sdf_params[0] += batch_poly_base[info_idx] as f32;
                            }
                            instance
                        }));
                    }
                    let geo_instance_start = combined_geo_instances.len() as u32;
                    let use_geo = !batch.geo_instances.is_empty() && fragment_only;
                    // merge_geo_templates：把同模板实例重排到连续范围以便合并 draw call。
                    // 按（texture segment, 模板）分组重排，多纹理 batch 也可合并——
                    // 每个 texture segment 内部按模板聚拢，段间仍保持各自 bind group。
                    let merge_geo = use_geo && batch.merge_geo_templates;
                    if use_geo {
                        let transform_base = batch_transform_bases[info_idx];
                        let gv_base = batch_geo_vertex_base[info_idx];
                        let gi_base = batch_geo_index_base[info_idx];
                        if merge_geo {
                            // 每实例原始下标 → texture segment 索引（超出段尾 → segments.len()，走 batch.bind_group）
                            let seg_count = batch.geo_instance_texture_segments.len() as u32;
                            let mut per_inst_seg = self.scratch_geo_merge_per_inst_seg.borrow_mut();
                            per_inst_seg.clear();
                            per_inst_seg.resize(batch.geo_instances.len(), seg_count);
                            for (si, seg) in batch.geo_instance_texture_segments.iter().enumerate() {
                                for k in seg.instance_start..seg.instance_start + seg.instance_count {
                                    per_inst_seg[k as usize] = si as u32;
                                }
                            }
                            let mut order = self.scratch_geo_merge_order.borrow_mut();
                            order.clear();
                            order.extend(0..batch.geo_instances.len() as u32);
                            order.sort_by_key(|&i| {
                                let g = &batch.geo_instances[i as usize];
                                (
                                    per_inst_seg[i as usize],
                                    g.template_vertex_start,
                                    g.template_index_start,
                                    g.index_count,
                                )
                            });
                            let sorted_seg = geo_merge_sorted_seg.get_or_insert_with(Vec::new);
                            sorted_seg.clear();
                            combined_geo_instances.extend(order.iter().copied().map(|i| {
                                sorted_seg.push(per_inst_seg[i as usize]);
                                let mut instance = batch.geo_instances[i as usize];
                                instance.template_vertex_start += gv_base;
                                instance.template_index_start += gi_base;
                                instance.transform_index += transform_base;
                                instance
                            }));
                        } else {
                            combined_geo_instances.extend(batch.geo_instances.iter().copied().map(|mut instance| {
                                instance.template_vertex_start += gv_base;
                                instance.template_index_start += gi_base;
                                instance.transform_index += transform_base;
                                instance
                            }));
                        }
                    }
                    let resolve_bg = |bg: Option<wgpu::BindGroup>| {
                        bg.unwrap_or_else(|| self.gpu.white_bind_group.as_ref().clone())
                    };
                    let instance_segments = if !use_instances {
                        Vec::new()
                    } else if batch.instance_texture_segments.is_empty() {
                        vec![InstanceSegment {
                            instance_start,
                            instance_count: batch.instances.len() as u32,
                            bind_group: resolve_bg(batch.bind_group.clone()),
                        }]
                    } else {
                        let mut segments: Vec<InstanceSegment> = batch.instance_texture_segments.iter().map(|segment| InstanceSegment {
                            instance_start: instance_start + segment.instance_start,
                            instance_count: segment.instance_count,
                            bind_group: resolve_bg(segment.bind_group.clone()),
                        }).collect();
                        let last_end = segments.last().map_or(instance_start, |s| s.instance_start + s.instance_count);
                        let total_end = instance_start + batch.instances.len() as u32;
                        if last_end < total_end {
                            segments.push(InstanceSegment {
                                instance_start: last_end,
                                instance_count: total_end - last_end,
                                bind_group: resolve_bg(batch.bind_group.clone()),
                            });
                        }
                        segments
                    };
                    let geo_segments = if !use_geo {
                        Vec::new()
                    } else if merge_geo {
                        // 实例已按（texture segment, 模板）重排：扫描排序后的连续范围，
                        // 每个 (segment, 模板) 组合一段，段用对应 segment 的 bind group。
                        let total = batch.geo_instances.len() as u32;
                        let mk_seg = |start: u32, count: u32, bg: wgpu::BindGroup| -> GeoInstanceSegment {
                            let tpl = combined_geo_instances[start as usize];
                            GeoInstanceSegment {
                                geo_instance_start: start,
                                geo_instance_count: count,
                                template_vertex_start: tpl.template_vertex_start,
                                template_index_start: tpl.template_index_start,
                                index_count: tpl.index_count,
                                bind_group: bg,
                            }
                        };
                        let seg_count = batch.geo_instance_texture_segments.len() as u32;
                        let sorted_seg = geo_merge_sorted_seg.as_deref().unwrap_or(&[]);
                        let resolve_seg_bg = |si: u32| -> wgpu::BindGroup {
                            if si < seg_count {
                                resolve_bg(batch.geo_instance_texture_segments[si as usize].bind_group.clone())
                            } else {
                                resolve_bg(batch.bind_group.clone())
                            }
                        };
                        let mut segments: Vec<GeoInstanceSegment> = Vec::new();
                        let mut i = 0u32;
                        while i < total {
                            let tpl_start = geo_instance_start + i;
                            let seg_i = sorted_seg.get(i as usize).copied().unwrap_or(seg_count);
                            let key = (
                                combined_geo_instances[tpl_start as usize].template_vertex_start,
                                combined_geo_instances[tpl_start as usize].template_index_start,
                                combined_geo_instances[tpl_start as usize].index_count,
                            );
                            let mut j = i + 1;
                            while j < total {
                                let seg_j = sorted_seg.get(j as usize).copied().unwrap_or(seg_count);
                                let g = &combined_geo_instances[(geo_instance_start + j) as usize];
                                if seg_j != seg_i
                                    || (g.template_vertex_start, g.template_index_start, g.index_count) != key
                                {
                                    break;
                                }
                                j += 1;
                            }
                            segments.push(mk_seg(tpl_start, j - i, resolve_seg_bg(seg_i)));
                            i = j;
                        }
                        segments
                    } else {
                        let mk_seg = |start: u32, count: u32, bg: wgpu::BindGroup| -> GeoInstanceSegment {
                            let tpl = combined_geo_instances[start as usize];
                            GeoInstanceSegment {
                                geo_instance_start: start,
                                geo_instance_count: count,
                                template_vertex_start: tpl.template_vertex_start,
                                template_index_start: tpl.template_index_start,
                                index_count: tpl.index_count,
                                bind_group: bg,
                            }
                        };
                        if batch.geo_instance_texture_segments.is_empty() {
                            vec![mk_seg(geo_instance_start, batch.geo_instances.len() as u32, resolve_bg(batch.bind_group.clone()))]
                        } else {
                            let mut segments: Vec<GeoInstanceSegment> = batch.geo_instance_texture_segments.iter().map(|segment| {
                                mk_seg(geo_instance_start + segment.instance_start, segment.instance_count, resolve_bg(segment.bind_group.clone()))
                            }).collect();
                            let last_end = segments.last().map_or(geo_instance_start, |s| s.geo_instance_start + s.geo_instance_count);
                            let total_end = geo_instance_start + batch.geo_instances.len() as u32;
                            if last_end < total_end {
                                segments.push(mk_seg(last_end, total_end - last_end, resolve_bg(batch.bind_group.clone())));
                            }
                            segments
                        }
                    };
                    let shape = if !batch.vertices.is_empty()
                        || !batch.instances.is_empty()
                        || !batch.geo_instances.is_empty()
                    {
                        let transform_base = batch_transform_bases[info_idx];
                        let poly_base = batch_poly_base[info_idx] as f32;
                        let needs_patch = !batch.polygon_edges.is_empty() || transform_base > 0;
                        if needs_patch {
                            let has_poly = !batch.polygon_edges.is_empty();
                            for mut v in batch.vertices.iter().copied() {
                                if transform_base > 0 {
                                    v.transform_index += transform_base;
                                }
                                if has_poly && (v.sdf_type == 6 || v.sdf_type == 7) {
                                    v.sdf_params[0] += poly_base;
                                }
                                combined_vdata.extend_from_slice(bytemuck::bytes_of(&v));
                            }
                        } else {
                            combined_vdata.extend_from_slice(bytemuck::cast_slice(&batch.vertices));
                        }
                        combined_idata.extend_from_slice(bytemuck::cast_slice(&batch.indices));
                        let mut mesh_index_count = batch.indices.len() as u32;
                        if !use_instances {
                            for (instance_index, instance) in batch.instances.iter().enumerate() {
                                let base = batch.vertices.len() as u32 + instance_index as u32 * 4;
                                let [x0, y0, x1, y1] = instance.bounds;
                                let [ux0, uy0, ux1, uy1] = instance.uv_bounds;
                                let [u0, v0, u1, v1] = instance.uv_rect;
                                let uv_at = |x: f32, y: f32| {
                                    (
                                        u0 + (x - ux0) / (ux1 - ux0) * (u1 - u0),
                                        v0 + (y - uy0) / (uy1 - uy0) * (v1 - v0),
                                    )
                                };
                                let (uv00, uv01) = uv_at(x0, y0);
                                let (uv10, uv11) = uv_at(x1, y0);
                                let (uv20, uv21) = uv_at(x1, y1);
                                let (uv30, uv31) = uv_at(x0, y1);
                                let color = crate::color::Color::new(
                                    instance.color[0], instance.color[1], instance.color[2], instance.color[3],
                                );
                                let mut verts = [
                                    Vertex::new_uv_xform(x0, y0, uv00, uv01, color, instance.transform_index + transform_base),
                                    Vertex::new_uv_xform(x1, y0, uv10, uv11, color, instance.transform_index + transform_base),
                                    Vertex::new_uv_xform(x1, y1, uv20, uv21, color, instance.transform_index + transform_base),
                                    Vertex::new_uv_xform(x0, y1, uv30, uv31, color, instance.transform_index + transform_base),
                                ];
                                for vertex in &mut verts {
                                    vertex.sdf_params = instance.sdf_params;
                                    if instance.sdf_type == 6 || instance.sdf_type == 7 {
                                        vertex.sdf_params[0] += poly_base;
                                    }
                                    vertex.sdf_extra = instance.sdf_extra;
                                    vertex.sdf_type = instance.sdf_type;
                                    vertex.sdf_feather = instance.sdf_feather;
                                }
                                combined_vdata.extend_from_slice(bytemuck::cast_slice(&verts));
                                combined_idata.extend_from_slice(bytemuck::cast_slice(&[
                                    base, base + 1, base + 2, base, base + 2, base + 3,
                                ]));
                                mesh_index_count += 6;
                            }
                        }

                        let segs: Vec<ShapeSegment> = if batch.texture_segments.is_empty() {
                            let bg = resolve_bg(batch.bind_group.clone());
                            vec![ShapeSegment { ndx_start: idx_offset, ndx_count: mesh_index_count, bind_group: bg }]
                        } else {
                            let mut v: Vec<ShapeSegment> = batch.texture_segments.iter().map(|s| ShapeSegment {
                                ndx_start: idx_offset + s.ndx_start,
                                ndx_count: s.ndx_count,
                                bind_group: resolve_bg(s.bind_group.clone()),
                            }).collect();
                            let last_end = v.last().map(|s| s.ndx_start + s.ndx_count).unwrap_or(idx_offset);
                            let total_end = idx_offset + mesh_index_count;
                            if last_end < total_end {
                                let bg = resolve_bg(batch.bind_group.clone());
                                v.push(ShapeSegment { ndx_start: last_end, ndx_count: total_end - last_end, bind_group: bg });
                            }
                            v
                        };
                        // merge_geo 时 ordered 路径需要 geo_segments 的克隆（原值移入 ShapeInfo）
                        let geo_segments_for_ordered = merge_geo.then(|| geo_segments.clone());
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: !batch.has_sdf && batch.sdf_feather.is_none(),
                            instances: instance_segments,
                            geo_instances: geo_segments,
                            ordered: if batch.shape_commands.is_empty() || !batch.shape_commands_valid() {
                                Vec::new()
                            } else {
                                let mut ordered = Vec::with_capacity(batch.shape_commands.len() + 1);
                                // merge_geo：shape_commands 的 GeoInstances 用原始局部偏移，
                                // 重排后失效 → 改用 `geo_segments` 的分组段（已在合并 buffer 上按模板分组）。
                                let geo_merged = merge_geo;
                                let mut geo_pushed = false;
                                for command in &batch.shape_commands {
                                    match command {
                                        BatchShapeCommand::Mesh { ndx_start, ndx_count, bind_group, geometry, .. } => {
                                            ordered.push(OrderedShapeSegment::Mesh {
                                                ndx_start: idx_offset + *ndx_start,
                                                ndx_count: *ndx_count,
                                                bind_group: resolve_bg(bind_group.clone()),
                                                geometry: *geometry,
                                            });
                                        }
                                        BatchShapeCommand::Instances { instance_start: local_start, instance_count, bind_group, .. } => {
                                            if use_instances {
                                                ordered.push(OrderedShapeSegment::Instances(InstanceSegment {
                                                    instance_start: instance_start + *local_start,
                                                    instance_count: *instance_count,
                                                    bind_group: resolve_bg(bind_group.clone()),
                                                }));
                                            } else {
                                                ordered.push(OrderedShapeSegment::Mesh {
                                                    ndx_start: idx_offset + batch.indices.len() as u32 + *local_start * 6,
                                                    ndx_count: *instance_count * 6,
                                                    bind_group: resolve_bg(bind_group.clone()),
                                                    geometry: false,
                                                });
                                            }
                                        }
                                        BatchShapeCommand::GeoInstances { geo_instance_start: local_start, geo_instance_count, bind_group, .. } => {
                                            if use_geo {
                                                if geo_merged {
                                                    if !geo_pushed {
                                                        ordered.extend(geo_segments_for_ordered.as_deref().unwrap_or(&[]).iter().map(|s| {
                                                            OrderedShapeSegment::GeoInstances(s.clone())
                                                        }));
                                                        geo_pushed = true;
                                                    }
                                                } else {
                                                    let tpl = combined_geo_instances[(geo_instance_start + *local_start) as usize];
                                                    ordered.push(OrderedShapeSegment::GeoInstances(GeoInstanceSegment {
                                                        geo_instance_start: geo_instance_start + *local_start,
                                                        geo_instance_count: *geo_instance_count,
                                                        template_vertex_start: tpl.template_vertex_start,
                                                        template_index_start: tpl.template_index_start,
                                                        index_count: tpl.index_count,
                                                        bind_group: resolve_bg(bind_group.clone()),
                                                    }));
                                                }
                                            }
                                        }
                                    }
                                }
                                if batch.shape_mesh_end < batch.indices.len() as u32 {
                                    ordered.push(OrderedShapeSegment::Mesh {
                                        ndx_start: idx_offset + batch.shape_mesh_end,
                                        ndx_count: batch.indices.len() as u32 - batch.shape_mesh_end,
                                        bind_group: resolve_bg(batch.bind_group.clone()),
                                        geometry: !batch.has_sdf && batch.sdf_feather.is_none(),
                                    });
                                }
                                if !batch.preserve_order {
                                    // 允许重排：按（种类, geometry, bind group）稳定排序，
                                    // 再把 pipeline 状态相同且范围连续的相邻段合并，
                                    // 减少 pipeline 切换与 draw call。
                                    ordered.sort_by_key(OrderedShapeSegment::sort_key);
                                    let mut merged: Vec<OrderedShapeSegment> =
                                        Vec::with_capacity(ordered.len());
                                    for segment in ordered {
                                        if let Some(last) = merged.last() {
                                            if let Some(combined) = last.try_merge(&segment) {
                                                *merged.last_mut().unwrap() = combined;
                                                continue;
                                            }
                                        }
                                        merged.push(segment);
                                    }
                                    ordered = merged;
                                }
                                ordered
                            },
                        };
                        v_offset += batch.vertices.len() as u32
                            + if use_instances { 0 } else { batch.instances.len() as u32 * 4 };
                        idx_offset += mesh_index_count;
                        Some(info)
                    } else if !instance_segments.is_empty() || !geo_segments.is_empty() {
                        Some(ShapeInfo {
                            base_vertex: 0,
                            segments: Vec::new(),
                            geometry: false,
                            instances: instance_segments,
                            geo_instances: geo_segments,
                            ordered: Vec::new(),
                        })
                    } else {
                        None
                    };
                    event_infos[ei].shape = shape;
                }
                DrawEvent::StencilPop => {
                    // 全屏四边形（逻辑像素）；索引相对 base_vertex；单位矩阵
                    // 复用全局槽 0（恒为单位阵，见 `Renderer::draw` 初始化），避免深嵌套浪费 transform 槽
                    let id_idx = 0u32;
                    let verts = [
                        Vertex::new_uv_xform(0.0, 0.0, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        Vertex::new_uv_xform(lw, 0.0, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        Vertex::new_uv_xform(lw, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        Vertex::new_uv_xform(0.0, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                    ];
                    combined_vdata.extend_from_slice(bytemuck::cast_slice(&verts));
                    let indices = [0u32, 1, 2, 0, 2, 3];
                    combined_idata.extend_from_slice(bytemuck::cast_slice(&indices));

                    let bg = self.gpu.white_bind_group.as_ref().clone();
                    let segs = vec![ShapeSegment {
                        ndx_start: idx_offset,
                        ndx_count: 6,
                        bind_group: bg,
                    }];
                    let si = ShapeInfo {
                        base_vertex: v_offset as i32,
                        segments: segs,
                        geometry: true,
                        instances: Vec::new(),
                        geo_instances: Vec::new(),
                        ordered: Vec::new(),
                    };
                    event_infos[ei].shape = Some(si);
                    v_offset += 4;
                    idx_offset += 6;
                }
                DrawEvent::ScissorPush(_) | DrawEvent::ScissorPop => {}
                DrawEvent::AreaOp { op, .. } => {
                    // Area 掩码：Full → 全屏 4v/6i；Geom → AreaGeom 自带 v/i。
                    // 走 stencil 管线 op 3/4（无色），由 pass 内 `area_op` 决定管线 key。
                    let transform_base = batch_transform_bases[ei];
                    let poly_base = batch_poly_base[ei] as f32;
                    let si = if let Some(geom) = op.geom() {
                        let needs_patch = !geom.polygon_edges.is_empty() || transform_base > 0;
                        if needs_patch {
                            let has_poly = !geom.polygon_edges.is_empty();
                            for mut v in geom.vertices.iter().copied() {
                                if transform_base > 0 {
                                    v.transform_index += transform_base;
                                }
                                if has_poly && (v.sdf_type == 6 || v.sdf_type == 7) {
                                    v.sdf_params[0] += poly_base;
                                }
                                combined_vdata.extend_from_slice(bytemuck::bytes_of(&v));
                            }
                        } else {
                            combined_vdata.extend_from_slice(bytemuck::cast_slice(&geom.vertices));
                        }
                        combined_idata.extend_from_slice(bytemuck::cast_slice(&geom.indices));
                        let n = geom.indices.len() as u32;
                        let bg = self.gpu.white_bind_group.as_ref().clone();
                        let segs = vec![ShapeSegment {
                            ndx_start: idx_offset,
                            ndx_count: n,
                            bind_group: bg,
                        }];
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: !geom.has_sdf && geom.sdf_feather.is_none(),
                            instances: Vec::new(),
                            geo_instances: Vec::new(),
                            ordered: Vec::new(),
                        };
                        v_offset += geom.vertices.len() as u32;
                        idx_offset += n;
                        info
                    } else {
                        // Full：全屏四边形 + 单位矩阵；复用全局槽 0（恒为单位阵）
                        let id_idx = 0u32;
                        let verts = [
                            Vertex::new_uv_xform(0.0, 0.0, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                            Vertex::new_uv_xform(lw, 0.0, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                            Vertex::new_uv_xform(lw, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                            Vertex::new_uv_xform(0.0, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        ];
                        combined_vdata.extend_from_slice(bytemuck::cast_slice(&verts));
                        let indices = [0u32, 1, 2, 0, 2, 3];
                        combined_idata.extend_from_slice(bytemuck::cast_slice(&indices));
                        let bg = self.gpu.white_bind_group.as_ref().clone();
                        let segs = vec![ShapeSegment {
                            ndx_start: idx_offset,
                            ndx_count: 6,
                            bind_group: bg,
                        }];
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: true,
                            instances: Vec::new(),
                            geo_instances: Vec::new(),
                            ordered: Vec::new(),
                        };
                        v_offset += 4;
                        idx_offset += 6;
                        info
                    };
                    event_infos[ei].shape = Some(si);
                }
            }
        }

        // ---- 合并上传 ----
        if !combined_vdata.is_empty() {
            let vbuf = self.vertex_buf.borrow();
            self.gpu.queue.write_buffer(&vbuf.as_ref().unwrap().0, 0, &combined_vdata);
        }
        if !combined_idata.is_empty() {
            let ibuf = self.index_buf.borrow();
            self.gpu.queue.write_buffer(&ibuf.as_ref().unwrap().0, 0, &combined_idata);
        }
        if !combined_instances.is_empty() {
            let size = (combined_instances.len() * size_of::<ShapeInstance>()) as u64;
            self.ensure_instance_buffer(size);
            let instance_buf = self.instance_buf.borrow();
            self.gpu.queue.write_buffer(
                &instance_buf.as_ref().unwrap().0,
                0,
                bytemuck::cast_slice(&combined_instances),
            );
        }

        // ---- 上传几何模板顶点/索引 ----
        if !combined_geo_vertices.is_empty() {
            let size = (combined_geo_vertices.len() * size_of::<GeoVertex>()) as u64;
            self.ensure_geo_template_vertex_buffer(size);
            let buf = self.geo_template_vertex_buf.borrow();
            self.gpu.queue.write_buffer(&buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&combined_geo_vertices));
        }
        if !combined_geo_indices.is_empty() {
            let size = (combined_geo_indices.len() * 4) as u64;
            self.ensure_geo_template_index_buffer(size);
            let buf = self.geo_template_index_buf.borrow();
            self.gpu.queue.write_buffer(&buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&combined_geo_indices));
        }

        // ---- 上传几何实例 ----
        if !combined_geo_instances.is_empty() {
            let size = (combined_geo_instances.len() * size_of::<GeoInstance>()) as u64;
            self.ensure_geo_instance_buffer(size);
            let buf = self.geo_instance_buf.borrow();
            self.gpu.queue.write_buffer(&buf.as_ref().unwrap().0, 0, bytemuck::cast_slice(&combined_geo_instances));
        }

        // ---- 上传多边形边数据 ----
        if !polygon_edges_global.is_empty() {
            let size = (polygon_edges_global.len() * 4) as u64;
            self.ensure_polygon_edge_buffer(size);
            {
                let buf = self.polygon_edge_buf.borrow();
                let buf_ref = buf.as_ref().unwrap();
                self.gpu.queue.write_buffer(&buf_ref.0, 0, bytemuck::cast_slice(&polygon_edges_global));
            }
        }

        // ---- 准备所有文本（DS 与本帧 attachment 一致）----
        {
            let mut tc = self.gpu.text_ctx.lock().unwrap();
            tc.ensure_sample_count(&self.gpu.device, self.sample_count);
            tc.ensure_text_ds(&self.gpu.device, uses_stencil);
        }
        let mut text_ctx = self.gpu.text_ctx.lock().unwrap();
        text_ctx.text_renderer.begin_frame();
        text_ctx.advance_frame();
        drop(text_ctx);
        for (ei, event) in events.iter().enumerate() {
            if let DrawEvent::Batch(batch) = event {
                if !batch.texts.entries.is_empty() {
                    // layout_follow 时用虚拟新物理尺寸（screen_resolution uniform 补偿 DXGI 拉伸）
                    let (tw, th) = self.text_viewport_override.get()
                        .unwrap_or((self.physical_width, self.physical_height));
                    // 文字与几何共用同一张表：左乘有效视图，保证 view 同时作用于文字。
                    let mut view_table = self.scratch_view_table.borrow_mut();
                    let eff = self
                        .scratch_view_map
                        .borrow()
                        .get(&(*batch as *const DrawBatch as *const () as usize))
                        .copied()
                        .unwrap_or(Transform::IDENTITY);
                    left_mul_view_table(&eff, &batch.transform_table, &mut view_table);
                    let prepared = batch.texts.prepare_texts(
                        &self.gpu,
                        tw,
                        th,
                        self.scale,
                        &view_table,
                        &mut global_transforms,
                        batch.text_clip,
                        batch.color,
                    );
                    drop(view_table);
                    let text_ctx = self.gpu.text_ctx.lock().unwrap();
                    event_infos[ei].text = prepared
                        .into_iter()
                        .map(|segment| {
                            let bind_group = if let Some(bg) = segment.bind_group.clone() {
                                Some(bg)
                            } else {
                                segment.texture_view.as_ref().map(|view| {
                                    text_ctx
                                        .text_atlas
                                        .bind_group_for_base_texture(&self.gpu.device, view)
                                })
                            };
                            TextRenderSegment {
                                vertex_start: segment.vertex_start,
                                vertex_count: segment.vertex_count,
                                bind_group,
                            }
                        })
                        .collect();
                    drop(text_ctx);
                    if let Some(material) = batch.custom_material.as_ref() {
                        let text_tests_stencil = uses_stencil
                            && (event_infos[ei].stencil_op == 1
                                || event_infos[ei].stencil_op == 2
                                || event_infos[ei].area_op.is_some());
                        event_infos[ei].custom_text_pipeline = Some(
                            self.gpu.ensure_material_pipeline(
                                material,
                                MaterialTarget::Text,
                                self.sample_count,
                                self.alpha_to_coverage,
                                false,
                                uses_stencil,
                                if text_tests_stencil { 2 } else { 0 },
                                crate::gpu::ShapeVertexLayout::Mesh,
                            ),
                        );
                    }
                }
            }
        }
        self.gpu
            .text_ctx
            .lock()
            .unwrap()
            .text_renderer
            .finish_frame(&self.gpu.device, &self.gpu.queue);

        // ---- 上传 transform 数据 ----
        if !global_transforms.is_empty() {
            let size = (global_transforms.len() * 4) as u64;
            self.ensure_transform_buffer(size);
            {
                let buf = self.transform_buf.borrow();
                let buf_ref = buf.as_ref().unwrap();
                self.gpu.queue.write_buffer(&buf_ref.0, 0, bytemuck::cast_slice(&global_transforms));
            }
        }
        let engine_storage_bind_group = {
            let mut cache = self.engine_storage_bind_group_cache.borrow_mut();
            if cache.is_none() {
                let transforms = self.transform_buf.borrow();
                let polygons = self.polygon_edge_buf.borrow();
                let transform_buf = transforms
                    .as_ref()
                    .map(|(buf, _)| buf)
                    .unwrap_or(&self.gpu.transform_dummy_buf);
                let polygon_buf = polygons
                    .as_ref()
                    .map(|(buf, _)| buf)
                    .unwrap_or(&self.gpu.polygon_dummy_buf);
                *cache = Some(self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("engine storage bind group"),
                    layout: &self.gpu.engine_storage_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: transform_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: polygon_buf.as_entire_binding() },
                    ],
                }));
            }
            cache.clone().unwrap()
        };

        // ---- 单 pass：仅 clips_children 帧挂 DS（热路径无 DS 开销）----
        let has_any_content = event_infos.iter().any(|e| e.shape.is_some() || !e.text.is_empty());
        // clear-only draw 也必须开启 pass，否则 LoadOp::Clear 不会执行。
        let mut shape_draw_calls: u32 = 0;
        if has_any_content || clear_color.is_some() {
            let msaa_view = self.msaa_view(self.gpu.surface_format());
            let (color_view, resolve): (&wgpu::TextureView, Option<&wgpu::TextureView>) = match &msaa_view {
                Some(msaa) => (msaa, Some(target_view)),
                None => (target_view, None),
            };
            let dv;
            let ds_attachment = if uses_stencil {
                dv = self.ds_view();
                // depth 也 Clear：部分后端在 depth_ops=None 时对未定义 depth 行为异常，
                // 且 glyphon 写 depth=0，需可预测的 depth 缓冲。
                // 每次 draw 独立建立并清理 stencil；multi-draw 只复用颜色 attachment。
                Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Discard,
                    }),
                })
            } else {
                None
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vireo render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target: resolve,
                    ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: ds_attachment,
                ..Default::default()
            });

            let vbuf = self.vertex_buf.borrow();
            let ibuf = self.index_buf.borrow();
            let instance_buf = self.instance_buf.borrow();
            let geo_template_vbuf = self.geo_template_vertex_buf.borrow();
            let geo_template_ibuf = self.geo_template_index_buf.borrow();
            let geo_instance_buf = self.geo_instance_buf.borrow();
            let mut text_ctx = self.gpu.text_ctx.lock().unwrap();
            let engine_bg = &engine_storage_bind_group;
            let mut shapes_bound = false;
            let mut last_geometry: Option<bool> = None;
            let mut last_stencil_op: u32 = u32::MAX;
            let mut last_custom_ptr: *const Material = std::ptr::null();
            let mut last_dynamic_offsets = self.scratch_last_dynamic_offsets.borrow_mut();
            last_dynamic_offsets.clear();
            let mut last_text_mode: Option<crate::text::TextStencilMode> = None;
            let mut scissor_stack = self.scratch_scissor_stack.borrow_mut();
            scissor_stack.clear();
            scissor_stack.push((0, 0, self.physical_width, self.physical_height));

            for info in event_infos.iter() {
                // ScissorPush: 计算物理像素 scissor rect，与当前 scissor 求交
                if let Some(scissor_rect) = info.scissor_push {
                    let sx = self.physical_width as f32 / self.logical_width.max(1.0);
                    let sy = self.physical_height as f32 / self.logical_height.max(1.0);
                    let fw = self.physical_width as f32;
                    let fh = self.physical_height as f32;
                    // 负坐标 / 越界：先 float 裁到视口再转 u32，避免 as u32 回绕
                    let x0 = (scissor_rect.x * sx).clamp(0.0, fw);
                    let y0 = (scissor_rect.y * sy).clamp(0.0, fh);
                    let x1 = ((scissor_rect.x + scissor_rect.w) * sx).clamp(0.0, fw);
                    let y1 = ((scissor_rect.y + scissor_rect.h) * sy).clamp(0.0, fh);
                    let px = x0.floor() as u32;
                    let py = y0.floor() as u32;
                    let pr = x1.ceil() as u32;
                    let pb = y1.ceil() as u32;
                    let (cx, cy, cw, ch) = *scissor_stack.last().unwrap_or(&(0, 0, self.physical_width, self.physical_height));
                    let ix = px.max(cx);
                    let iy = py.max(cy);
                    let ir = pr.min(cx + cw);
                    let ib = pb.min(cy + ch);
                    let (nx, ny, nw, nh) = if ir > ix && ib > iy {
                        (ix, iy, ir - ix, ib - iy)
                    } else {
                        (0u32, 0u32, 0u32, 0u32)
                    };
                    pass.set_scissor_rect(nx, ny, nw, nh);
                    scissor_stack.push((nx, ny, nw, nh));
                }
                if info.scissor_pop {
                    scissor_stack.pop();
                    let (cx, cy, cw, ch) = *scissor_stack.last().unwrap_or(&(0, 0, self.physical_width, self.physical_height));
                    pass.set_scissor_rect(cx, cy, cw, ch);
                }

                // 整批材质 bind group：有 group 3（非 ZeroResource）的材质若绑定失败
                // （纹理槽未 set_texture 等），整批跳过——custom pipeline 引用 group 3，
                // 不绑会触发 wgpu validation error。ZeroResource 无 group 3，None 合法。
                let custom_bg: Option<wgpu::BindGroup> = match info.custom_material.as_ref() {
                    Some(m) if m.bgl().is_some() => m.ensure_bind_group(
                        &self.gpu.device,
                        &self.gpu.queue,
                        &self.gpu.bind_group_pool,
                    ),
                    _ => None,
                };
                if info.custom_material.is_some()
                    && info.custom_material.as_ref().map_or(false, |m| m.bgl().is_some())
                    && custom_bg.is_none()
                {
                    continue;
                }

                if let Some(ref shape) = info.shape {
                    // Area 事件：op 3/4 来自 area_op；普通 batch/StencilPop：op 0..3 来自 stencil_op。
                    let pipe_op = info.area_op.unwrap_or(info.stencil_op);
                    let custom_ptr: *const Material = info.custom_material
                        .as_ref()
                        .map_or(std::ptr::null(), |m| Arc::as_ptr(m));
                    let use_custom = info.custom_material.is_some();
                    let has_custom_vs = info
                        .custom_material
                        .as_ref()
                        .map(|m| m.has_custom_vertex_shader())
                        .unwrap_or(false);
                    // instance 段仅在 fragment-only material 时可走对应 layout pipeline；
                    // custom VS 必须 mesh。
                    let use_custom_instance = use_custom && !has_custom_vs;
                    if !shape.ordered.is_empty() {
                        for segment in &shape.ordered {
                            match segment {
                                OrderedShapeSegment::Mesh {
                                    ndx_start,
                                    ndx_count,
                                    bind_group,
                                    geometry,
                                } => {
                                    let need_rebind = !shapes_bound
                                        || custom_ptr != last_custom_ptr
                                        || (!use_custom && last_geometry != Some(*geometry))
                                        || (uses_stencil && pipe_op != last_stencil_op)
                                        || info.dynamic_offsets != *last_dynamic_offsets;
                                    if need_rebind {
                                        let tmp_pipe: wgpu::RenderPipeline;
                                        let custom_pipe: Arc<wgpu::RenderPipeline>;
                                        let pipe: &wgpu::RenderPipeline = if use_custom {
                                            let mat = info.custom_material.as_ref().unwrap();
                                            custom_pipe = self.gpu.ensure_material_pipeline(
                                                mat,
                                                MaterialTarget::Shape,
                                                self.sample_count,
                                                self.alpha_to_coverage,
                                                self.ssaa,
                                                uses_stencil,
                                                if uses_stencil { pipe_op.min(4) } else { 0 },
                                                crate::gpu::ShapeVertexLayout::Mesh,
                                            );
                                            &custom_pipe
                                        } else if uses_stencil {
                                            tmp_pipe = self.gpu.ensure_stencil_pipeline(
                                                self.sample_count,
                                                self.alpha_to_coverage,
                                                self.ssaa,
                                                *geometry,
                                                pipe_op.min(4),
                                            );
                                            &tmp_pipe
                                        } else {
                                            tmp_pipe = self.gpu.ensure_pipeline(
                                                self.sample_count,
                                                self.alpha_to_coverage,
                                                self.ssaa,
                                                *geometry,
                                            );
                                            &tmp_pipe
                                        };
                                        pass.set_pipeline(pipe);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        if use_custom {
                                            if let Some(bg) = custom_bg.as_ref() {
                                                pass.set_bind_group(3, bg, &info.dynamic_offsets);
                                            }
                                        }
                                        pass.set_vertex_buffer(0, vbuf.as_ref().unwrap().0.slice(..));
                                        pass.set_index_buffer(
ibuf.as_ref().unwrap().0.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        shapes_bound = true;
                                        last_custom_ptr = custom_ptr;
                                        last_geometry = Some(*geometry);
                                        last_stencil_op = pipe_op;
                                        last_dynamic_offsets.clone_from(&info.dynamic_offsets);
                                    }
                                    if uses_stencil {
                                        pass.set_stencil_reference(info.stencil_ref);
                                    }
                                    pass.set_bind_group(1, bind_group, &[]);
                                    pass.draw_indexed(
                                        *ndx_start..*ndx_start + *ndx_count,
                                        shape.base_vertex,
                                        0..1,
                                    );
                                    shape_draw_calls += 1;
                                }
                                OrderedShapeSegment::Instances(segment) => {
                                    if use_custom_instance {
                                        let mat = info.custom_material.as_ref().unwrap();
                                        let custom_pipe = self.gpu.ensure_material_pipeline(
                                            mat,
                                            MaterialTarget::Shape,
                                            self.sample_count,
                                            self.alpha_to_coverage,
                                            self.ssaa,
                                            uses_stencil,
                                            if uses_stencil { pipe_op.min(4) } else { 0 },
                                            crate::gpu::ShapeVertexLayout::SdfInstance,
                                        );
                                        pass.set_pipeline(&custom_pipe);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(1, &segment.bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        if let Some(bg) = custom_bg.as_ref() {
                                            pass.set_bind_group(3, bg, &info.dynamic_offsets);
                                        }
                                        pass.set_vertex_buffer(
                                            0,
                                            self.gpu.instance_quad_vertex_buf.slice(..),
                                        );
                                        pass.set_vertex_buffer(
                                            1,
                                            instance_buf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_index_buffer(
                                            self.gpu.instance_quad_index_buf.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        if uses_stencil {
                                            pass.set_stencil_reference(info.stencil_ref);
                                        }
                                        pass.draw_indexed(
                                            0..6,
                                            0,
                                            segment.instance_start
                                                ..segment.instance_start + segment.instance_count,
                                        );
                                        shape_draw_calls += 1;
                                    } else {
                                        let instance_pipeline = self.gpu.ensure_instance_pipeline(
                                            self.sample_count,
                                            self.alpha_to_coverage,
                                            self.ssaa,
                                            uses_stencil,
                                            pipe_op,
                                        );
                                        pass.set_pipeline(&instance_pipeline);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(1, &segment.bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        pass.set_vertex_buffer(
                                            0,
                                            self.gpu.instance_quad_vertex_buf.slice(..),
                                        );
                                        pass.set_vertex_buffer(
                                            1,
                                            instance_buf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_index_buffer(
                                            self.gpu.instance_quad_index_buf.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        if uses_stencil {
                                            pass.set_stencil_reference(info.stencil_ref);
                                        }
                                        pass.draw_indexed(
                                            0..6,
                                            0,
                                            segment.instance_start
                                                ..segment.instance_start + segment.instance_count,
                                        );
                                        shape_draw_calls += 1;
                                    }
                                    shapes_bound = false;
                                    last_geometry = None;
                                }
                                OrderedShapeSegment::GeoInstances(segment) => {
                                    if use_custom_instance {
                                        let mat = info.custom_material.as_ref().unwrap();
                                        let custom_pipe = self.gpu.ensure_material_pipeline(
                                            mat,
                                            MaterialTarget::Shape,
                                            self.sample_count,
                                            self.alpha_to_coverage,
                                            self.ssaa,
                                            uses_stencil,
                                            if uses_stencil { pipe_op.min(4) } else { 0 },
                                            crate::gpu::ShapeVertexLayout::GeoInstance,
                                        );
                                        pass.set_pipeline(&custom_pipe);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(1, &segment.bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        if let Some(bg) = custom_bg.as_ref() {
                                            pass.set_bind_group(3, bg, &info.dynamic_offsets);
                                        }
                                        pass.set_vertex_buffer(
                                            0,
                                            geo_template_vbuf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_vertex_buffer(
                                            1,
                                            geo_instance_buf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_index_buffer(
                                            geo_template_ibuf.as_ref().unwrap().0.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        if uses_stencil {
                                            pass.set_stencil_reference(info.stencil_ref);
                                        }
                                        pass.draw_indexed(
                                            segment.template_index_start
                                                ..segment.template_index_start + segment.index_count,
                                            segment.template_vertex_start as i32,
                                            segment.geo_instance_start
                                                ..segment.geo_instance_start + segment.geo_instance_count,
                                        );
                                        shape_draw_calls += 1;
                                    } else {
                                        let geo_pipeline = self.gpu.ensure_geo_instance_pipeline(
                                            self.sample_count,
                                            self.alpha_to_coverage,
                                            self.ssaa,
                                            uses_stencil,
                                            pipe_op,
                                        );
                                        pass.set_pipeline(&geo_pipeline);
                                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                                        pass.set_bind_group(1, &segment.bind_group, &[]);
                                        pass.set_bind_group(2, engine_bg, &[]);
                                        pass.set_vertex_buffer(
                                            0,
                                            geo_template_vbuf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_vertex_buffer(
                                            1,
                                            geo_instance_buf.as_ref().unwrap().0.slice(..),
                                        );
                                        pass.set_index_buffer(
                                            geo_template_ibuf.as_ref().unwrap().0.slice(..),
                                            wgpu::IndexFormat::Uint32,
                                        );
                                        if uses_stencil {
                                            pass.set_stencil_reference(info.stencil_ref);
                                        }
                                        pass.draw_indexed(
                                            segment.template_index_start
                                                ..segment.template_index_start + segment.index_count,
                                            segment.template_vertex_start as i32,
                                            segment.geo_instance_start
                                                ..segment.geo_instance_start + segment.geo_instance_count,
                                        );
                                        shape_draw_calls += 1;
                                    }
                                    shapes_bound = false;
                                    last_geometry = None;
                                }
                            }
                        }
                    } else {
                    let need_rebind = !shapes_bound
                        || custom_ptr != last_custom_ptr
                        || (!use_custom && last_geometry != Some(shape.geometry))
                        || (uses_stencil && pipe_op != last_stencil_op)
                        || info.dynamic_offsets != *last_dynamic_offsets;
                    if need_rebind {
                        let tmp_pipe: wgpu::RenderPipeline;
                        let custom_pipe: Arc<wgpu::RenderPipeline>;
                        let pipe: &wgpu::RenderPipeline = if use_custom {
                            let mat = info.custom_material.as_ref().unwrap();
                            if uses_stencil {
                                custom_pipe = self.gpu.ensure_material_pipeline(
                                    mat,
                                    MaterialTarget::Shape,
                                    self.sample_count,
                                    self.alpha_to_coverage,
                                    self.ssaa,
                                    true,
                                    pipe_op.min(4),
                                    crate::gpu::ShapeVertexLayout::Mesh,
                                );
                            } else {
                                custom_pipe = self.gpu.ensure_material_pipeline(
                                    mat,
                                    MaterialTarget::Shape,
                                    self.sample_count,
                                    self.alpha_to_coverage,
                                    self.ssaa,
                                    false,
                                    0,
                                    crate::gpu::ShapeVertexLayout::Mesh,
                                );
                            }
                            &custom_pipe
                        } else if uses_stencil {
                            tmp_pipe = self.gpu.ensure_stencil_pipeline(
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                shape.geometry,
                                pipe_op.min(4),
                            );
                            &tmp_pipe
                        } else {
                            tmp_pipe = self.gpu.ensure_pipeline(
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                shape.geometry,
                            );
                            &tmp_pipe
                        };
                        pass.set_pipeline(pipe);
                        pass.set_bind_group(0, &self.camera_bind_group, &[]);
                        pass.set_bind_group(2, engine_bg, &[]);
                        if use_custom {
                            if let Some(bg) = custom_bg.as_ref() {
                                pass.set_bind_group(3, bg, &info.dynamic_offsets);
                            }
                        }
                        if let Some(vb) = vbuf.as_ref() {
                            pass.set_vertex_buffer(0, vb.0.slice(..));
                        }
                        if let Some(ib) = ibuf.as_ref() {
                            pass.set_index_buffer(ib.0.slice(..), wgpu::IndexFormat::Uint32);
                        }
                        shapes_bound = true;
                        last_custom_ptr = custom_ptr;
                        last_geometry = Some(shape.geometry);
                        last_stencil_op = pipe_op;
                        last_dynamic_offsets.clone_from(&info.dynamic_offsets);
                    }
                    if uses_stencil {
                        pass.set_stencil_reference(info.stencil_ref);
                    }
                    for seg in &shape.segments {
                        pass.set_bind_group(1, &seg.bind_group, &[]);
                        pass.draw_indexed(
                            seg.ndx_start..seg.ndx_start + seg.ndx_count,
                            shape.base_vertex,
                            0..1,
                        );
                        shape_draw_calls += 1;
                    }
                    if !shape.instances.is_empty() {
                        if use_custom_instance {
                            let mat = info.custom_material.as_ref().unwrap();
                            let custom_pipe = self.gpu.ensure_material_pipeline(
                                mat,
                                MaterialTarget::Shape,
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                uses_stencil,
                                if uses_stencil { pipe_op.min(4) } else { 0 },
                                crate::gpu::ShapeVertexLayout::SdfInstance,
                            );
                            pass.set_pipeline(&custom_pipe);
                            pass.set_bind_group(0, &self.camera_bind_group, &[]);
                            pass.set_bind_group(2, engine_bg, &[]);
                            if let Some(bg) = custom_bg.as_ref() {
                                pass.set_bind_group(3, bg, &info.dynamic_offsets);
                            }
                            pass.set_vertex_buffer(0, self.gpu.instance_quad_vertex_buf.slice(..));
                            pass.set_vertex_buffer(1, instance_buf.as_ref().unwrap().0.slice(..));
                            pass.set_index_buffer(
                                self.gpu.instance_quad_index_buf.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            if uses_stencil {
                                pass.set_stencil_reference(info.stencil_ref);
                            }
                            for segment in &shape.instances {
                                pass.set_bind_group(1, &segment.bind_group, &[]);
                                pass.draw_indexed(
                                    0..6,
                                    0,
                                    segment.instance_start
                                        ..segment.instance_start + segment.instance_count,
                                );
                                shape_draw_calls += 1;
                            }
                        } else {
                            let instance_pipeline = self.gpu.ensure_instance_pipeline(
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                uses_stencil,
                                pipe_op,
                            );
                            pass.set_pipeline(&instance_pipeline);
                            pass.set_bind_group(0, &self.camera_bind_group, &[]);
                            pass.set_bind_group(2, engine_bg, &[]);
                            pass.set_vertex_buffer(0, self.gpu.instance_quad_vertex_buf.slice(..));
                            pass.set_vertex_buffer(1, instance_buf.as_ref().unwrap().0.slice(..));
                            pass.set_index_buffer(
                                self.gpu.instance_quad_index_buf.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            if uses_stencil {
                                pass.set_stencil_reference(info.stencil_ref);
                            }
                            for segment in &shape.instances {
                                pass.set_bind_group(1, &segment.bind_group, &[]);
                                pass.draw_indexed(
                                    0..6,
                                    0,
                                    segment.instance_start
                                        ..segment.instance_start + segment.instance_count,
                                );
                                shape_draw_calls += 1;
                            }
                        }
                        shapes_bound = false;
                        last_geometry = None;
                    }
                    if !shape.geo_instances.is_empty() {
                        if use_custom_instance {
                            let mat = info.custom_material.as_ref().unwrap();
                            let custom_pipe = self.gpu.ensure_material_pipeline(
                                mat,
                                MaterialTarget::Shape,
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                uses_stencil,
                                if uses_stencil { pipe_op.min(4) } else { 0 },
                                crate::gpu::ShapeVertexLayout::GeoInstance,
                            );
                            pass.set_pipeline(&custom_pipe);
                            pass.set_bind_group(0, &self.camera_bind_group, &[]);
                            pass.set_bind_group(2, engine_bg, &[]);
                            if let Some(bg) = custom_bg.as_ref() {
                                pass.set_bind_group(3, bg, &info.dynamic_offsets);
                            }
                            pass.set_vertex_buffer(0, geo_template_vbuf.as_ref().unwrap().0.slice(..));
                            pass.set_vertex_buffer(1, geo_instance_buf.as_ref().unwrap().0.slice(..));
                            pass.set_index_buffer(
                                geo_template_ibuf.as_ref().unwrap().0.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            if uses_stencil {
                                pass.set_stencil_reference(info.stencil_ref);
                            }
                            for segment in &shape.geo_instances {
                                pass.set_bind_group(1, &segment.bind_group, &[]);
                                pass.draw_indexed(
                                    segment.template_index_start
                                        ..segment.template_index_start + segment.index_count,
                                    segment.template_vertex_start as i32,
                                    segment.geo_instance_start
                                        ..segment.geo_instance_start + segment.geo_instance_count,
                                );
                                shape_draw_calls += 1;
                            }
                        } else {
                            let geo_pipeline = self.gpu.ensure_geo_instance_pipeline(
                                self.sample_count,
                                self.alpha_to_coverage,
                                self.ssaa,
                                uses_stencil,
                                pipe_op,
                            );
                            pass.set_pipeline(&geo_pipeline);
                            pass.set_bind_group(0, &self.camera_bind_group, &[]);
                            pass.set_bind_group(2, engine_bg, &[]);
                            pass.set_vertex_buffer(0, geo_template_vbuf.as_ref().unwrap().0.slice(..));
                            pass.set_vertex_buffer(1, geo_instance_buf.as_ref().unwrap().0.slice(..));
                            pass.set_index_buffer(
                                geo_template_ibuf.as_ref().unwrap().0.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            if uses_stencil {
                                pass.set_stencil_reference(info.stencil_ref);
                            }
                            for segment in &shape.geo_instances {
                                pass.set_bind_group(1, &segment.bind_group, &[]);
                                pass.draw_indexed(
                                    segment.template_index_start
                                        ..segment.template_index_start + segment.index_count,
                                    segment.template_vertex_start as i32,
                                    segment.geo_instance_start
                                        ..segment.geo_instance_start + segment.geo_instance_count,
                                );
                                shape_draw_calls += 1;
                            }
                        }
                        shapes_bound = false;
                        last_geometry = None;
                    }
                    }
                }

                if !info.text.is_empty() {
                    // 有 DS 时：Push/Test 用 Equal；op=0（UI/unclipped）用 Always，避免误裁
                    // Area 存在时：当前文本在 Area content level，测 (Test)。
                    let has_area_at_text = info.area_op.is_some()
                        || info.stencil_op == 1
                        || info.stencil_op == 2;
                    let text_mode = if !uses_stencil {
                        crate::text::TextStencilMode::None
                    } else if has_area_at_text {
                        crate::text::TextStencilMode::Test
                    } else {
                        crate::text::TextStencilMode::Pass
                    };
                    if last_text_mode != Some(text_mode) {
                        text_ctx.ensure_text_stencil_mode(&self.gpu.device, text_mode);
                        last_text_mode = Some(text_mode);
                    }
                    // Push 后 mask 已 Inc：父文字测 new_level = ref+1
                    let text_ref = if info.stencil_op == 1 {
                        info.stencil_ref + 1
                    } else {
                        info.stencil_ref
                    };
                    // 必须在 set_pipeline（render_range 内）之后再 set_stencil_reference，
                    // 否则部分后端会把 ref 重置为 0。
                    // 复用循环顶部已计算的整批材质 bind group（ZeroResource 无 group 3 → None）。
                    let material_bg = custom_bg.as_ref();
                    for segment in &info.text {
                        if let Err(e) = text_ctx.text_renderer.render_range_with_material(
                            &text_ctx.text_atlas,
                            &text_ctx.viewport,
                            &mut pass,
                            engine_bg,
                            segment.vertex_start,
                            segment.vertex_count,
                            if uses_stencil { Some(text_ref) } else { None },
                            segment.bind_group.as_ref(),
                            info.custom_text_pipeline.as_deref(),
                            material_bg,
                            &info.dynamic_offsets,
                        ) {
                            log::warn!("glyphon text render failed (skipped segment): {:?}", e);
                        }
                    }
                    shapes_bound = false;
                    last_geometry = None;
                }
            }
        }

        self.last_draw_calls.set(shape_draw_calls);
        encoder.finish()
    }
}
