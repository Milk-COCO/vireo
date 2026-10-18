use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::gpu::{
    ClockIndex, GeoInstance, GeoVertex, InstanceData, MaterialTarget, ParticleInstance, Vertex,
};
use crate::material::Material;
use crate::math::{IDENTITY_TRANSFORM_ROW, Transform, left_mul_view_table};

use super::prepare_culling;
use super::{
    BatchEncodeCtx, BatchEncodeInfo, BatchShapeCommand, ClipPath, DepthStencilOps,
    DepthStencilTarget, DrawBatch, DrawEvent, EventInfo, GeoInstanceSegment, InstanceSegment,
    OrderedShapeSegment, ParticleSegment, RenderTarget, Renderer, ShapeInfo, ShapeSegment,
    TextRenderSegment,
};

/// 同步 blend 常量：段用 `Constant` 系数时才调（小众路径；无条件设置，正确优先）。
#[inline]
fn sync_blend_constant(
    pass: &mut wgpu::RenderPass,
    blend: wgpu::BlendState,
    constant: wgpu::Color,
) {
    if blend.color.uses_constant() || blend.alpha.uses_constant() {
        pass.set_blend_constant(constant);
    }
}

/// DS 须为 `Depth24PlusStencil8`；本帧解析结果（不持借用，pass 构建处物化）。
enum ResolvedDsOps {
    /// 本帧无 DS attachment。
    None,
    /// 引擎 DS（默认或与用户 ops 合并后）。
    Engine(DepthStencilOps),
    /// 用户 view（ops 原样取自 `RenderTarget::depth_stencil`）。
    External,
}

/// 已准备的一帧：剔除 / flatten / 合并 / 上传 / 文字 prepare 全部完成。
///
/// 由窗口/离屏的 `draw_with_frame` 回调交出；生命周期内持有本帧的 `EventInfo`
/// 与附件计划。**只在回调期间有效**，不要保存到回调之外。
///
/// # 自定义 pass 的契约
///
/// `encode` 使用引擎的 pipeline（按 renderer 的格式 / 采样数构建），因此自建 pass 必须：
/// - 颜色附件使用 [`Self::target`] 里的同一组 view（`sample_count()` 为 1 时可直接用；
///   >1 时需自建 MSAA target 并 resolve 到该 view）；
/// - 附件数量与 [`Self::target`] 相同；
/// - [`Self::has_ds`] 为真时挂 `Depth24PlusStencil8` depth-stencil 附件；
/// - 尺寸与 renderer 一致（文字 viewport / scissor 换算依赖它）。
pub struct PreparedFrame<'a> {
    renderer: &'a Renderer,
    target: &'a RenderTarget,
    events: Vec<DrawEvent<'a>>,
    event_infos: Vec<EventInfo>,
    engine_bg: wgpu::BindGroup,
    target_blends: Vec<Option<wgpu::BlendState>>,
    loads: Vec<wgpu::LoadOp<wgpu::Color>>,
    ds: ResolvedDsOps,
    has_ds: bool,
    uses_stencil: bool,
    mrt: bool,
    wants_pass: bool,
    clip_paths: Vec<ClipPath>,
    draw_calls: std::cell::Cell<u32>,
    _active: FrameActiveGuard<'a>,
}

/// DS ops 问题进程级 warn-once，避免每帧刷屏。
static DS_WARNED: AtomicBool = AtomicBool::new(false);

#[inline]
fn ds_warn_once(msg: &str) {
    if !DS_WARNED.swap(true, Ordering::Relaxed) {
        log::warn!("{msg}");
    }
}

/// MRT（多颜色目标）pass 里引擎 shader / 文字管线只有单颜色输出，无法写入多目标。
/// 进程级 warn-once，内容跳过而不是触发 wgpu 校验错误。
static MRT_WARNED: AtomicBool = AtomicBool::new(false);

#[inline]
pub(crate) fn mrt_warn_once(what: &str) {
    if !MRT_WARNED.swap(true, Ordering::Relaxed) {
        log::warn!(
            "{what} skipped in a multi-target pass: engine shaders/text write a single color output; \
             use a custom material whose fragment shader declares matching outputs"
        );
    }
}

/// 把 `RenderTarget::depth_stencil` 与 stencil 需求解析成本帧 DS 计划。
///
/// - `None`（target 无 DS 配置）：uses_stencil 时用引擎默认（depth Clear(1.0)+Discard、
///   stencil Clear(0)+Discard），兼容历史行为；否则无 DS。
/// - `Engine(ops)`：与默认合并后使用引擎 DS 纹理；uses_stencil 时补齐缺失 aspect 的
///   Clear+Discard；`!uses_stencil` 原样使用；两 aspect 皆 `None` → warn + 无 DS。
/// - `External(att)`：直接使用用户 view；uses_stencil 且缺 aspect → 回退引擎默认 + warn；
///   两 ops 皆 `None` → warn + 当无 DS。
fn resolve_depth_stencil(target: &RenderTarget, uses_stencil: bool) -> ResolvedDsOps {
    let default_ops = DepthStencilOps {
        depth: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Discard,
        }),
        stencil: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(0),
            store: wgpu::StoreOp::Discard,
        }),
    };
    match target.depth_stencil.as_ref() {
        None => {
            if uses_stencil {
                ResolvedDsOps::Engine(default_ops)
            } else {
                ResolvedDsOps::None
            }
        }
        Some(DepthStencilTarget::Engine(ops)) => {
            if !uses_stencil {
                if ops.depth.is_none() && ops.stencil.is_none() {
                    ds_warn_once(
                        "engine depth-stencil ops are empty (both None); drawing without DS",
                    );
                    ResolvedDsOps::None
                } else {
                    ResolvedDsOps::Engine(DepthStencilOps {
                        depth: ops.depth,
                        stencil: ops.stencil,
                    })
                }
            } else {
                ResolvedDsOps::Engine(DepthStencilOps {
                    depth: ops.depth.or(default_ops.depth),
                    stencil: ops.stencil.or(default_ops.stencil),
                })
            }
        }
        Some(DepthStencilTarget::External(att)) => {
            if att.depth_ops.is_none() && att.stencil_ops.is_none() {
                ds_warn_once(
                    "external depth-stencil attachment ops are empty (both None); drawing without DS",
                );
                ResolvedDsOps::None
            } else if uses_stencil && (att.depth_ops.is_none() || att.stencil_ops.is_none()) {
                ds_warn_once(
                    "external depth-stencil attachment is missing an aspect required by stencil; falling back to engine DS",
                );
                ResolvedDsOps::Engine(default_ops)
            } else {
                ResolvedDsOps::External
            }
        }
    }
}

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
        clear_colors: &[Option<crate::color::Color>],
        batches: &[&DrawBatch],
    ) -> wgpu::CommandBuffer {
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vireo encoder"),
            });
        self.draw_into(&mut encoder, target, clear_colors, batches);
        encoder.finish()
    }

    /// 准备一帧：剔除 / flatten / 合并 / 上传 / 文字 prepare。
    ///
    /// 结果 [`PreparedFrame`] 可被编码进任意满足契约的 `RenderPass`；引擎自己的
    /// pass 由 [`PreparedFrame::begin_engine_pass`] 创建。
    pub(crate) fn prepare_frame<'a>(
        &'a self,
        target: &'a RenderTarget,
        clear_colors: &[Option<crate::color::Color>],
        batches: &'a [&'a DrawBatch],
    ) -> PreparedFrame<'a> {
        let active = FrameActiveGuard::acquire(&self.frame_active);
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

        // ---- depth-stencil 计划（与目标对齐；warn-once 收敛）----
        let resolved_ds = resolve_depth_stencil(target, uses_stencil);
        let has_ds = !matches!(resolved_ds, ResolvedDsOps::None);

        let loads: Vec<wgpu::LoadOp<wgpu::Color>> = (0..target.color_attachments.len())
            .map(|i| match clear_colors.get(i) {
                Some(Some(c)) => wgpu::LoadOp::Clear(wgpu::Color {
                    r: c.r as f64,
                    g: c.g as f64,
                    b: c.b as f64,
                    a: c.a as f64,
                }),
                _ => wgpu::LoadOp::Load,
            })
            .collect();

        // MRT：>1 个颜色附件的 pass。引擎 MSAA / 文字 / 引擎形状均只支持单目标，
        // MRT 由 custom material（多个 FS 输出）承担，其余内容跳过并 warn-once。
        let mrt = target.color_attachments.len() > 1;
        let target_blends: Vec<Option<wgpu::BlendState>> =
            target.color_attachments.iter().map(|a| a.blend).collect();
        // 相机为逻辑像素正交；Pop 全屏四边形也用逻辑尺寸
        let lw = self.logical_width;
        let lh = self.logical_height;

        // ---- 在 pass 外写入所有 batch 的 vertex/index 数据 ----
        let mut event_infos = std::mem::take(&mut *self.scratch_event_infos.lock());
        event_infos.clear();
        let vertex_count: u32 = 0;
        let ndx_accum: u32 = 0;

        // ---- 单次扫描：合并 transform/poly + 统计顶点数 ----
        let mut global_transforms = self.scratch_transforms.lock();
        global_transforms.clear();
        // 全局表槽 0 = 单位阵（与 batch `transform_table` 槽 0 约定一致）。
        // `draw_text` / glyphon 默认 transform_index=0 表示恒等；batch 表上传时
        // `transform_base` 会偏移局部 index，故全局槽 0 仍须单独预留，不能被首个 batch 占用。
        global_transforms.extend_from_slice(&IDENTITY_TRANSFORM_ROW);
        let mut polygon_edges_global = self.scratch_poly_edges.lock();
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
            let has_geom = !batch.vertices.is_empty()
                || !batch.instance_data.is_empty()
                || !batch.geo_instances.is_empty()
                || !batch.particles.is_empty();
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
                    "clips_children=true but the batch has no geometry and no explicit scissor; clipping will not take effect. Provide a geometry clip shape or set batch.scissor explicitly"
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

        let mut ref_stack = self.scratch_ref_stack.lock();
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
                    let content_level = clip_depth + ancestors_area_depth + (has_own_area as u32);
                    // 与 flatten_events 共用条件（见 DrawBatch::uses_scissor_path）
                    let use_scissor = batch.uses_scissor_path(has_own_area);
                    let (stencil_op, stencil_ref) = if use_scissor {
                        let has_draw = !batch.vertices.is_empty()
                            || !batch.instance_data.is_empty()
                            || !batch.geo_instances.is_empty()
                            || !batch.texts.entries.is_empty();
                        if content_level > 0 && batch.inherit.clipped && has_draw {
                            (2u32, content_level) // Test 祖先，不 Push
                        } else {
                            (0u32, 0u32)
                        }
                    } else {
                        compute_stencil_at_level(batch, content_level, &mut ref_stack)
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
                        dynamic_offsets: Vec::new(),
                    });
                }
            }
        }

        // 收集 transform/poly 信息
        let mut batch_transform_bases = self.scratch_batch_transform_bases.lock();
        batch_transform_bases.clear();
        let mut batch_poly_base = self.scratch_batch_poly_base.lock();
        batch_poly_base.clear();
        let mut total_vcount: u32 = 0;
        let mut total_icount: u32 = 0;
        let mut poly_offset: u32 = 0;
        let mut pop_screen_verts: u32 = 0; // 全屏 Pop 顶点数
        let mut pop_screen_idx: u32 = 0;

        let mut combined_geo_vertices = self.scratch_geo_vertices.lock();
        let mut combined_geo_indices = self.scratch_geo_indices.lock();
        combined_geo_vertices.clear();
        combined_geo_indices.clear();
        let mut batch_geo_vertex_base = self.scratch_batch_geo_vertex_base.lock();
        let mut batch_geo_index_base = self.scratch_batch_geo_index_base.lock();
        batch_geo_vertex_base.clear();
        batch_geo_index_base.clear();
        {
            let view_map = self.scratch_view_map.lock();
            let mut view_table = self.scratch_view_table.lock();

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
                        total_vcount += batch.instance_data.len() as u32 * 4;
                        total_icount += batch.instance_data.len() as u32 * 6;
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
        let mut combined_vdata = self.scratch_vdata.lock();
        let mut combined_idata = self.scratch_idata.lock();
        let mut combined_instances = self.scratch_instances.lock();
        let mut combined_particles = self.scratch_particles.lock();
        let mut combined_geo_instances = self.scratch_geo_instances.lock();
        combined_vdata.clear();
        combined_idata.clear();
        combined_instances.clear();
        combined_particles.clear();
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
        let mut geo_merge_sorted_seg = self.scratch_geo_merge_sorted.lock();
        for (ei, event) in events.iter().enumerate() {
            match event {
                DrawEvent::Batch(batch) => {
                    let info_idx = ei;
                    let instance_start = combined_instances.len() as u32;
                    // custom material 按绘制路径契约参与实例化（supported layout 校验在
                    // 创建期完成）；记录期已按路径分派，实例数据与材质布局一致。
                    let use_instances = !batch.instance_data.is_empty();
                    // 实例按状态重排（opt-in）：把同 (bind group, material, blend,
                    // blend constant) 的实例范围排到连续，使 ordered 段可合并 dc。
                    // 仅在 preserve_order=false 且 shape_commands 有效时生效。
                    let mut instance_remap: rustc_hash::FxHashMap<u32, u32> =
                        rustc_hash::FxHashMap::default();
                    if use_instances {
                        let transform_base = batch_transform_bases[info_idx];
                        let poly_base = batch_poly_base[info_idx] as f32;
                        let patch = |mut instance: InstanceData| -> InstanceData {
                            instance.transform_index += transform_base;
                            if instance.sdf_type == 6 || instance.sdf_type == 7 {
                                instance.sdf_params[0] += poly_base;
                            }
                            instance
                        };
                        if batch.merge_instance_states
                            && !batch.preserve_order
                            && !batch.shape_commands.is_empty()
                            && batch.shape_commands_valid()
                        {
                            let mut ranges: Vec<(u32, u32, u64)> = Vec::new();
                            for command in &batch.shape_commands {
                                if let BatchShapeCommand::Instances {
                                    instance_start: local_start,
                                    instance_count,
                                    bind_group,
                                    material,
                                    blend,
                                    blend_constant,
                                    ..
                                } = command
                                {
                                    let bg = bind_group.clone().unwrap_or_else(|| {
                                        self.gpu.white_bind_group.as_ref().clone()
                                    });
                                    let key = super::segment_state_hash(
                                        material.as_ref().map(Arc::as_ptr),
                                        blend,
                                        *blend_constant,
                                        super::bind_group_id(&bg),
                                    );
                                    ranges.push((*local_start, *instance_count, key));
                                }
                            }
                            ranges.sort_by_key(|r| r.2);
                            for (local_start, count, _) in ranges {
                                instance_remap.insert(local_start, combined_instances.len() as u32);
                                combined_instances.extend(
                                    batch.instance_data
                                        [local_start as usize..(local_start + count) as usize]
                                        .iter()
                                        .copied()
                                        .map(&patch),
                                );
                            }
                        } else {
                            combined_instances
                                .extend(batch.instance_data.iter().copied().map(&patch));
                        }
                    }
                    // 粒子恒走 instance 路径（record 期 custom VS 已烘焙为 mesh，
                    // `particles` 非空即实例安全，无需 fragment_only 门）。
                    let particle_start = combined_particles.len() as u32;
                    if !batch.particles.is_empty() {
                        let transform_base = batch_transform_bases[info_idx];
                        combined_particles.extend(batch.particles.iter().copied().map(
                            |mut particle| {
                                particle.transform_index += transform_base;
                                particle
                            },
                        ));
                    }
                    let geo_instance_start = combined_geo_instances.len() as u32;
                    let use_geo = !batch.geo_instances.is_empty();
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
                            let mut per_inst_seg = self.scratch_geo_merge_per_inst_seg.lock();
                            per_inst_seg.clear();
                            per_inst_seg.resize(batch.geo_instances.len(), seg_count);
                            for (si, seg) in batch.geo_instance_texture_segments.iter().enumerate()
                            {
                                for k in seg.instance_start..seg.instance_start + seg.instance_count
                                {
                                    per_inst_seg[k as usize] = si as u32;
                                }
                            }
                            let mut order = self.scratch_geo_merge_order.lock();
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
                            combined_geo_instances.extend(batch.geo_instances.iter().copied().map(
                                |mut instance| {
                                    instance.template_vertex_start += gv_base;
                                    instance.template_index_start += gi_base;
                                    instance.transform_index += transform_base;
                                    instance
                                },
                            ));
                        }
                    }
                    let resolve_bg = |bg: Option<wgpu::BindGroup>| {
                        bg.unwrap_or_else(|| self.gpu.white_bind_group.as_ref().clone())
                    };
                    let batch_material = batch.custom_material.clone();
                    let batch_blend = batch.blend_state;
                    let batch_blend_constant = batch.blend_constant;
                    let instance_segments = if !use_instances {
                        Vec::new()
                    } else if batch.instance_texture_segments.is_empty() {
                        vec![InstanceSegment {
                            instance_start,
                            instance_count: batch.instance_data.len() as u32,
                            bind_group: resolve_bg(batch.bind_group.clone()),
                            material: batch_material.clone(),
                            blend: batch_blend,
                            blend_constant: batch_blend_constant,
                        }]
                    } else {
                        let mut segments: Vec<InstanceSegment> = batch
                            .instance_texture_segments
                            .iter()
                            .map(|segment| InstanceSegment {
                                instance_start: instance_start + segment.instance_start,
                                instance_count: segment.instance_count,
                                bind_group: resolve_bg(segment.bind_group.clone()),
                                material: batch_material.clone(),
                                blend: batch_blend,
                                blend_constant: batch_blend_constant,
                            })
                            .collect();
                        let last_end = segments
                            .last()
                            .map_or(instance_start, |s| s.instance_start + s.instance_count);
                        let total_end = instance_start + batch.instance_data.len() as u32;
                        if last_end < total_end {
                            segments.push(InstanceSegment {
                                instance_start: last_end,
                                instance_count: total_end - last_end,
                                bind_group: resolve_bg(batch.bind_group.clone()),
                                material: batch_material.clone(),
                                blend: batch_blend,
                                blend_constant: batch_blend_constant,
                            });
                        }
                        segments
                    };
                    // 粒子段：与 instance 段同机理（texture segment 切分 + 尾段兜底），
                    // 下标进 `combined_particles`。material 取 batch 级（legacy 路径用；
                    // ordered 路径用命令自带 material，见下）。
                    let batch_particle_material = batch.custom_material.clone();
                    let particle_segments = if batch.particles.is_empty() {
                        Vec::new()
                    } else if batch.particle_texture_segments.is_empty() {
                        vec![ParticleSegment {
                            particle_start,
                            particle_count: batch.particles.len() as u32,
                            bind_group: resolve_bg(batch.bind_group.clone()),
                            material: batch_particle_material.clone(),
                            blend: batch_blend,
                            blend_constant: batch_blend_constant,
                        }]
                    } else {
                        let mut segments: Vec<ParticleSegment> = batch
                            .particle_texture_segments
                            .iter()
                            .map(|segment| ParticleSegment {
                                particle_start: particle_start + segment.instance_start,
                                particle_count: segment.instance_count,
                                bind_group: resolve_bg(segment.bind_group.clone()),
                                material: batch_particle_material.clone(),
                                blend: batch_blend,
                                blend_constant: batch_blend_constant,
                            })
                            .collect();
                        let last_end = segments
                            .last()
                            .map_or(particle_start, |s| s.particle_start + s.particle_count);
                        let total_end = particle_start + batch.particles.len() as u32;
                        if last_end < total_end {
                            segments.push(ParticleSegment {
                                particle_start: last_end,
                                particle_count: total_end - last_end,
                                bind_group: resolve_bg(batch.bind_group.clone()),
                                material: batch_particle_material.clone(),
                                blend: batch_blend,
                                blend_constant: batch_blend_constant,
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
                        let batch_material = batch.custom_material.clone();
                        let mk_seg =
                            |start: u32, count: u32, bg: wgpu::BindGroup| -> GeoInstanceSegment {
                                let tpl = combined_geo_instances[start as usize];
                                GeoInstanceSegment {
                                    geo_instance_start: start,
                                    geo_instance_count: count,
                                    template_vertex_start: tpl.template_vertex_start,
                                    template_index_start: tpl.template_index_start,
                                    index_count: tpl.index_count,
                                    bind_group: bg,
                                    material: batch_material.clone(),
                                    blend: batch_blend,
                                    blend_constant: batch_blend_constant,
                                }
                            };
                        let seg_count = batch.geo_instance_texture_segments.len() as u32;
                        let sorted_seg = geo_merge_sorted_seg.as_deref().unwrap_or(&[]);
                        let resolve_seg_bg = |si: u32| -> wgpu::BindGroup {
                            if si < seg_count {
                                resolve_bg(
                                    batch.geo_instance_texture_segments[si as usize]
                                        .bind_group
                                        .clone(),
                                )
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
                                let seg_j =
                                    sorted_seg.get(j as usize).copied().unwrap_or(seg_count);
                                let g = &combined_geo_instances[(geo_instance_start + j) as usize];
                                if seg_j != seg_i
                                    || (
                                        g.template_vertex_start,
                                        g.template_index_start,
                                        g.index_count,
                                    ) != key
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
                        let batch_material = batch.custom_material.clone();
                        let mk_seg =
                            |start: u32, count: u32, bg: wgpu::BindGroup| -> GeoInstanceSegment {
                                let tpl = combined_geo_instances[start as usize];
                                GeoInstanceSegment {
                                    geo_instance_start: start,
                                    geo_instance_count: count,
                                    template_vertex_start: tpl.template_vertex_start,
                                    template_index_start: tpl.template_index_start,
                                    index_count: tpl.index_count,
                                    bind_group: bg,
                                    material: batch_material.clone(),
                                    blend: batch_blend,
                                    blend_constant: batch_blend_constant,
                                }
                            };
                        if batch.geo_instance_texture_segments.is_empty() {
                            vec![mk_seg(
                                geo_instance_start,
                                batch.geo_instances.len() as u32,
                                resolve_bg(batch.bind_group.clone()),
                            )]
                        } else {
                            let mut segments: Vec<GeoInstanceSegment> = batch
                                .geo_instance_texture_segments
                                .iter()
                                .map(|segment| {
                                    mk_seg(
                                        geo_instance_start + segment.instance_start,
                                        segment.instance_count,
                                        resolve_bg(segment.bind_group.clone()),
                                    )
                                })
                                .collect();
                            let last_end = segments.last().map_or(geo_instance_start, |s| {
                                s.geo_instance_start + s.geo_instance_count
                            });
                            let total_end = geo_instance_start + batch.geo_instances.len() as u32;
                            if last_end < total_end {
                                segments.push(mk_seg(
                                    last_end,
                                    total_end - last_end,
                                    resolve_bg(batch.bind_group.clone()),
                                ));
                            }
                            segments
                        }
                    };
                    let shape = if !batch.vertices.is_empty()
                        || !batch.instance_data.is_empty()
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
                            for (instance_index, instance) in batch.instance_data.iter().enumerate()
                            {
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
                                    instance.color[0],
                                    instance.color[1],
                                    instance.color[2],
                                    instance.color[3],
                                );
                                let mut verts = [
                                    Vertex::new_uv_xform(
                                        x0,
                                        y0,
                                        uv00,
                                        uv01,
                                        color,
                                        instance.transform_index + transform_base,
                                    ),
                                    Vertex::new_uv_xform(
                                        x1,
                                        y0,
                                        uv10,
                                        uv11,
                                        color,
                                        instance.transform_index + transform_base,
                                    ),
                                    Vertex::new_uv_xform(
                                        x1,
                                        y1,
                                        uv20,
                                        uv21,
                                        color,
                                        instance.transform_index + transform_base,
                                    ),
                                    Vertex::new_uv_xform(
                                        x0,
                                        y1,
                                        uv30,
                                        uv31,
                                        color,
                                        instance.transform_index + transform_base,
                                    ),
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
                                    base,
                                    base + 1,
                                    base + 2,
                                    base,
                                    base + 2,
                                    base + 3,
                                ]));
                                mesh_index_count += 6;
                            }
                        }

                        let segs: Vec<ShapeSegment> = if batch.texture_segments.is_empty() {
                            let bg = resolve_bg(batch.bind_group.clone());
                            vec![ShapeSegment {
                                ndx_start: idx_offset,
                                ndx_count: mesh_index_count,
                                bind_group: bg,
                                blend: batch_blend,
                                blend_constant: batch_blend_constant,
                            }]
                        } else {
                            let mut v: Vec<ShapeSegment> = batch
                                .texture_segments
                                .iter()
                                .map(|s| ShapeSegment {
                                    ndx_start: idx_offset + s.ndx_start,
                                    ndx_count: s.ndx_count,
                                    bind_group: resolve_bg(s.bind_group.clone()),
                                    blend: batch_blend,
                                    blend_constant: batch_blend_constant,
                                })
                                .collect();
                            let last_end = v
                                .last()
                                .map(|s| s.ndx_start + s.ndx_count)
                                .unwrap_or(idx_offset);
                            let total_end = idx_offset + mesh_index_count;
                            if last_end < total_end {
                                let bg = resolve_bg(batch.bind_group.clone());
                                v.push(ShapeSegment {
                                    ndx_start: last_end,
                                    ndx_count: total_end - last_end,
                                    bind_group: bg,
                                    blend: batch_blend,
                                    blend_constant: batch_blend_constant,
                                });
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
                            particles: particle_segments,
                            particle_clock: batch.particle_clock,
                            blend: batch.blend_state,
                            ordered: if batch.shape_commands.is_empty()
                                || !batch.shape_commands_valid()
                            {
                                Vec::new()
                            } else {
                                let mut ordered =
                                    Vec::with_capacity(batch.shape_commands.len() + 1);
                                // merge_geo：shape_commands 的 GeoInstances 用原始局部偏移，
                                // 重排后失效 → 改用 `geo_segments` 的分组段（已在合并 buffer 上按模板分组）。
                                let geo_merged = merge_geo;
                                let mut geo_pushed = false;
                                for command in &batch.shape_commands {
                                    match command {
                                        BatchShapeCommand::Mesh {
                                            ndx_start,
                                            ndx_count,
                                            bind_group,
                                            geometry,
                                            material,
                                            blend,
                                            blend_constant,
                                            ..
                                        } => {
                                            ordered.push(OrderedShapeSegment::Mesh {
                                                ndx_start: idx_offset + *ndx_start,
                                                ndx_count: *ndx_count,
                                                bind_group: resolve_bg(bind_group.clone()),
                                                geometry: *geometry,
                                                material: material.clone(),
                                                blend: *blend,
                                                blend_constant: *blend_constant,
                                            });
                                        }
                                        BatchShapeCommand::Instances {
                                            instance_start: local_start,
                                            instance_count,
                                            bind_group,
                                            material,
                                            blend,
                                            blend_constant,
                                            ..
                                        } => {
                                            if use_instances {
                                                // merge_instance_states 重排后，局部范围
                                                // 已不连续：按 local_start 查新绝对起点。
                                                let abs_start = instance_remap
                                                    .get(local_start)
                                                    .copied()
                                                    .unwrap_or(instance_start + *local_start);
                                                ordered.push(OrderedShapeSegment::Instances(
                                                    InstanceSegment {
                                                        instance_start: abs_start,
                                                        instance_count: *instance_count,
                                                        bind_group: resolve_bg(bind_group.clone()),
                                                        material: material.clone(),
                                                        blend: *blend,
                                                        blend_constant: *blend_constant,
                                                    },
                                                ));
                                            } else {
                                                ordered.push(OrderedShapeSegment::Mesh {
                                                    ndx_start: idx_offset
                                                        + batch.indices.len() as u32
                                                        + *local_start * 6,
                                                    ndx_count: *instance_count * 6,
                                                    bind_group: resolve_bg(bind_group.clone()),
                                                    geometry: false,
                                                    material: material.clone(),
                                                    blend: *blend,
                                                    blend_constant: *blend_constant,
                                                });
                                            }
                                        }
                                        BatchShapeCommand::Particles {
                                            particle_start: local_start,
                                            particle_count,
                                            bind_group,
                                            material,
                                            blend,
                                            blend_constant,
                                            ..
                                        } => {
                                            ordered.push(OrderedShapeSegment::Particles(
                                                ParticleSegment {
                                                    particle_start: particle_start + *local_start,
                                                    particle_count: *particle_count,
                                                    bind_group: resolve_bg(bind_group.clone()),
                                                    material: material.clone(),
                                                    blend: *blend,
                                                    blend_constant: *blend_constant,
                                                },
                                            ));
                                        }
                                        BatchShapeCommand::GeoInstances {
                                            geo_instance_start: local_start,
                                            geo_instance_count,
                                            bind_group,
                                            material,
                                            blend,
                                            blend_constant,
                                            ..
                                        } => {
                                            if use_geo {
                                                if geo_merged {
                                                    if !geo_pushed {
                                                        ordered.extend(geo_segments_for_ordered.as_deref().unwrap_or(&[]).iter().map(|s| {
                                                            OrderedShapeSegment::GeoInstances(s.clone())
                                                        }));
                                                        geo_pushed = true;
                                                    }
                                                } else {
                                                    let tpl = combined_geo_instances
                                                        [(geo_instance_start + *local_start)
                                                            as usize];
                                                    ordered.push(
                                                        OrderedShapeSegment::GeoInstances(
                                                            GeoInstanceSegment {
                                                                geo_instance_start:
                                                                    geo_instance_start
                                                                        + *local_start,
                                                                geo_instance_count:
                                                                    *geo_instance_count,
                                                                template_vertex_start: tpl
                                                                    .template_vertex_start,
                                                                template_index_start: tpl
                                                                    .template_index_start,
                                                                index_count: tpl.index_count,
                                                                bind_group: resolve_bg(
                                                                    bind_group.clone(),
                                                                ),
                                                                material: material.clone(),
                                                                blend: *blend,
                                                                blend_constant: *blend_constant,
                                                            },
                                                        ),
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                if batch.shape_mesh_end < batch.indices.len() as u32 {
                                    ordered.push(OrderedShapeSegment::Mesh {
                                        ndx_start: idx_offset + batch.shape_mesh_end,
                                        ndx_count: batch.indices.len() as u32
                                            - batch.shape_mesh_end,
                                        bind_group: resolve_bg(batch.bind_group.clone()),
                                        geometry: !batch.has_sdf && batch.sdf_feather.is_none(),
                                        material: batch.custom_material.clone(),
                                        blend: batch.blend_state,
                                        blend_constant: batch.blend_constant,
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
                                        if let Some(last) = merged.last()
                                            && let Some(combined) = last.try_merge(&segment)
                                        {
                                            *merged.last_mut().unwrap() = combined;
                                            continue;
                                        }
                                        merged.push(segment);
                                    }
                                    ordered = merged;
                                }
                                ordered
                            },
                        };
                        v_offset += batch.vertices.len() as u32
                            + if use_instances {
                                0
                            } else {
                                batch.instance_data.len() as u32 * 4
                            };
                        idx_offset += mesh_index_count;
                        Some(info)
                    } else if !instance_segments.is_empty()
                        || !geo_segments.is_empty()
                        || !particle_segments.is_empty()
                    {
                        Some(ShapeInfo {
                            base_vertex: 0,
                            segments: Vec::new(),
                            geometry: false,
                            instances: instance_segments,
                            geo_instances: geo_segments,
                            particles: particle_segments,
                            particle_clock: batch.particle_clock,
                            blend: batch.blend_state,
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
                        Vertex::new_uv_xform(
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            crate::color::colors::WHITE,
                            id_idx,
                        ),
                        Vertex::new_uv_xform(
                            lw,
                            0.0,
                            0.0,
                            0.0,
                            crate::color::colors::WHITE,
                            id_idx,
                        ),
                        Vertex::new_uv_xform(lw, lh, 0.0, 0.0, crate::color::colors::WHITE, id_idx),
                        Vertex::new_uv_xform(
                            0.0,
                            lh,
                            0.0,
                            0.0,
                            crate::color::colors::WHITE,
                            id_idx,
                        ),
                    ];
                    combined_vdata.extend_from_slice(bytemuck::cast_slice(&verts));
                    let indices = [0u32, 1, 2, 0, 2, 3];
                    combined_idata.extend_from_slice(bytemuck::cast_slice(&indices));

                    let bg = self.gpu.white_bind_group.as_ref().clone();
                    let segs = vec![ShapeSegment {
                        ndx_start: idx_offset,
                        ndx_count: 6,
                        bind_group: bg,
                        // stencil 掩码不写颜色：blend 归一化（与管线键归一化一致）。
                        blend: wgpu::BlendState::ALPHA_BLENDING,
                        blend_constant: wgpu::Color::TRANSPARENT,
                    }];
                    let si = ShapeInfo {
                        base_vertex: v_offset as i32,
                        segments: segs,
                        geometry: true,
                        instances: Vec::new(),
                        geo_instances: Vec::new(),
                        particles: Vec::new(),
                        particle_clock: ClockIndex::ZERO,
                        blend: wgpu::BlendState::ALPHA_BLENDING,
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
                            // stencil 掩码不写颜色：blend 归一化（与管线键归一化一致）。
                            blend: wgpu::BlendState::ALPHA_BLENDING,
                            blend_constant: wgpu::Color::TRANSPARENT,
                        }];
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: !geom.has_sdf && geom.sdf_feather.is_none(),
                            instances: Vec::new(),
                            geo_instances: Vec::new(),
                            particles: Vec::new(),
                            particle_clock: ClockIndex::ZERO,
                            blend: wgpu::BlendState::ALPHA_BLENDING,
                            ordered: Vec::new(),
                        };
                        v_offset += geom.vertices.len() as u32;
                        idx_offset += n;
                        info
                    } else {
                        // Full：全屏四边形 + 单位矩阵；复用全局槽 0（恒为单位阵）
                        let id_idx = 0u32;
                        let verts = [
                            Vertex::new_uv_xform(
                                0.0,
                                0.0,
                                0.0,
                                0.0,
                                crate::color::colors::WHITE,
                                id_idx,
                            ),
                            Vertex::new_uv_xform(
                                lw,
                                0.0,
                                0.0,
                                0.0,
                                crate::color::colors::WHITE,
                                id_idx,
                            ),
                            Vertex::new_uv_xform(
                                lw,
                                lh,
                                0.0,
                                0.0,
                                crate::color::colors::WHITE,
                                id_idx,
                            ),
                            Vertex::new_uv_xform(
                                0.0,
                                lh,
                                0.0,
                                0.0,
                                crate::color::colors::WHITE,
                                id_idx,
                            ),
                        ];
                        combined_vdata.extend_from_slice(bytemuck::cast_slice(&verts));
                        let indices = [0u32, 1, 2, 0, 2, 3];
                        combined_idata.extend_from_slice(bytemuck::cast_slice(&indices));
                        let bg = self.gpu.white_bind_group.as_ref().clone();
                        let segs = vec![ShapeSegment {
                            ndx_start: idx_offset,
                            ndx_count: 6,
                            bind_group: bg,
                            // stencil 掩码不写颜色：blend 归一化（与管线键归一化一致）。
                            blend: wgpu::BlendState::ALPHA_BLENDING,
                            blend_constant: wgpu::Color::TRANSPARENT,
                        }];
                        let info = ShapeInfo {
                            base_vertex: v_offset as i32,
                            segments: segs,
                            geometry: true,
                            instances: Vec::new(),
                            geo_instances: Vec::new(),
                            particles: Vec::new(),
                            particle_clock: ClockIndex::ZERO,
                            blend: wgpu::BlendState::ALPHA_BLENDING,
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
            let vbuf = self.vertex_buf.lock();
            self.gpu
                .queue
                .write_buffer(&vbuf.as_ref().unwrap().0, 0, &combined_vdata);
        }
        if !combined_idata.is_empty() {
            let ibuf = self.index_buf.lock();
            self.gpu
                .queue
                .write_buffer(&ibuf.as_ref().unwrap().0, 0, &combined_idata);
        }
        if !combined_instances.is_empty() {
            let size = (combined_instances.len() * size_of::<InstanceData>()) as u64;
            self.ensure_instance_storage_buffer(size);
            let instance_buf = self.gpu.instance_storage_buf.lock().unwrap();
            let buf = &(*instance_buf).as_ref().unwrap().0;
            self.gpu
                .queue
                .write_buffer(buf, 0, bytemuck::cast_slice(&combined_instances));
        }

        if !combined_particles.is_empty() {
            let size = (combined_particles.len() * size_of::<ParticleInstance>()) as u64;
            self.ensure_particle_buffer(size);
            let particle_buf = self.particle_buf.lock();
            self.gpu.queue.write_buffer(
                &particle_buf.as_ref().unwrap().0,
                0,
                bytemuck::cast_slice(&combined_particles),
            );
        }

        // ---- 上传几何模板顶点/索引 ----
        if !combined_geo_vertices.is_empty() {
            let size = (combined_geo_vertices.len() * size_of::<GeoVertex>()) as u64;
            self.ensure_geo_template_vertex_buffer(size);
            let buf = self.geo_template_vertex_buf.lock();
            self.gpu.queue.write_buffer(
                &buf.as_ref().unwrap().0,
                0,
                bytemuck::cast_slice(&combined_geo_vertices),
            );
        }
        if !combined_geo_indices.is_empty() {
            let size = (combined_geo_indices.len() * 4) as u64;
            self.ensure_geo_template_index_buffer(size);
            let buf = self.geo_template_index_buf.lock();
            self.gpu.queue.write_buffer(
                &buf.as_ref().unwrap().0,
                0,
                bytemuck::cast_slice(&combined_geo_indices),
            );
        }

        // ---- 上传几何实例 ----
        if !combined_geo_instances.is_empty() {
            let size = (combined_geo_instances.len() * size_of::<GeoInstance>()) as u64;
            self.ensure_geo_instance_buffer(size);
            let buf = self.geo_instance_buf.lock();
            self.gpu.queue.write_buffer(
                &buf.as_ref().unwrap().0,
                0,
                bytemuck::cast_slice(&combined_geo_instances),
            );
        }

        // ---- 上传多边形边数据 ----
        if !polygon_edges_global.is_empty() {
            let size = (polygon_edges_global.len() * 4) as u64;
            self.ensure_polygon_edge_buffer(size);
            {
                let buf = self.polygon_edge_buf.lock();
                let buf_ref = buf.as_ref().unwrap();
                self.gpu.queue.write_buffer(
                    &buf_ref.0,
                    0,
                    bytemuck::cast_slice(&polygon_edges_global),
                );
            }
        }

        // ---- 准备所有文本（DS 与本帧 attachment 一致）----
        {
            let mut tc = self.gpu.text_ctx.lock().unwrap();
            tc.ensure_sample_count(&self.gpu.device, self.sample_count);
            tc.ensure_text_stencil_mode(
                &self.gpu.device,
                if has_ds {
                    crate::text::TextStencilMode::Pass
                } else {
                    crate::text::TextStencilMode::None
                },
            );
        }
        let mut text_ctx = self.gpu.text_ctx.lock().unwrap();
        text_ctx.text_renderer.begin_frame();
        text_ctx.advance_frame();
        drop(text_ctx);
        for (ei, event) in events.iter().enumerate() {
            if let DrawEvent::Batch(batch) = event
                && !batch.texts.entries.is_empty()
            {
                // layout_follow 时用虚拟新物理尺寸（screen_resolution uniform 补偿 DXGI 拉伸）
                let (tw, th) = self
                    .text_viewport_override
                    .lock()
                    .unwrap_or((self.physical_width, self.physical_height));
                // 文字与几何共用同一张表：左乘有效视图，保证 view 同时作用于文字。
                let mut view_table = self.scratch_view_table.lock();
                let eff = self
                    .scratch_view_map
                    .lock()
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
                            blend: segment.blend,
                            blend_constant: segment.blend_constant,
                        }
                    })
                    .collect();
                drop(text_ctx);
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
                let buf = self.transform_buf.lock();
                let buf_ref = buf.as_ref().unwrap();
                self.gpu.queue.write_buffer(
                    &buf_ref.0,
                    0,
                    bytemuck::cast_slice(&global_transforms),
                );
            }
        }
        let engine_storage_bind_group = {
            let mut cache = self.engine_storage_bind_group_cache.lock();
            if cache.is_none() {
                let transforms = self.transform_buf.lock();
                let polygons = self.polygon_edge_buf.lock();
                let transform_buf = transforms
                    .as_ref()
                    .map(|(buf, _)| buf)
                    .unwrap_or(&self.gpu.transform_dummy_buf);
                let polygon_buf = polygons
                    .as_ref()
                    .map(|(buf, _)| buf)
                    .unwrap_or(&self.gpu.polygon_dummy_buf);
                // instance storage 是本帧上传阶段刚写入的 buffer；此处 clone 句柄
                // （绑定不持有所有权），避免临时值在表达式末尾被释放。
                let instance_buf = {
                    let instance_storage = self.gpu.instance_storage_buf.lock().unwrap();
                    (*instance_storage)
                        .as_ref()
                        .map(|(buf, _)| buf.clone())
                        .unwrap_or_else(|| self.gpu.transform_dummy_buf.clone())
                };
                *cache = Some(
                    self.gpu
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("engine storage bind group"),
                            layout: &self.gpu.engine_storage_bind_group_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: transform_buf.as_entire_binding(),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: polygon_buf.as_entire_binding(),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 2,
                                    resource: instance_buf.as_entire_binding(),
                                },
                            ],
                        }),
                );
            }
            cache.clone().unwrap()
        };

        // ---- 组装 PreparedFrame（pass 由调用方创建）----
        let has_any_content = event_infos
            .iter()
            .any(|e| e.shape.is_some() || !e.text.is_empty());
        let wants_pass =
            has_any_content || loads.iter().any(|l| matches!(l, wgpu::LoadOp::Clear(_)));
        let clip_paths: Vec<ClipPath> = events
            .iter()
            .filter_map(|event| match event {
                DrawEvent::Batch(batch) => Some(batch.clip_path()),
                _ => None,
            })
            .collect();
        PreparedFrame {
            renderer: self,
            target,
            events,
            event_infos,
            engine_bg: engine_storage_bind_group,
            target_blends,
            loads,
            ds: resolved_ds,
            has_ds,
            uses_stencil,
            mrt,
            wants_pass,
            clip_paths,
            draw_calls: std::cell::Cell::new(0),
            _active: active,
        }
    }

    /// 兼容入口：准备 + 引擎 pass + 编码 + finish 交给调用方。
    pub(crate) fn draw_into(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &RenderTarget,
        clear_colors: &[Option<crate::color::Color>],
        batches: &[&DrawBatch],
    ) {
        let frame = self.prepare_frame(target, clear_colors, batches);
        if frame.wants_pass() {
            let mut pass = frame.begin_engine_pass(encoder);
            frame.encode(&mut pass);
        }
    }
}

impl PreparedFrame<'_> {
    /// 本帧是否会产生任何 pass（有批次内容或某个颜色附件需要 clear）。
    pub fn is_empty(&self) -> bool {
        !self.wants_pass
    }

    /// 本帧渲染目标（自定义 pass 应复用其中的颜色 view）。
    pub fn target(&self) -> &RenderTarget {
        self.target
    }

    /// 引擎 pipeline 构建时使用的颜色格式。
    pub fn target_format(&self) -> wgpu::TextureFormat {
        self.renderer.gpu.surface_format()
    }

    /// 引擎 pipeline 的 sample count（`1` = 无 MSAA）。
    pub fn sample_count(&self) -> u32 {
        self.renderer.sample_count
    }

    /// `encode` 是否会按“有 depth-stencil”选择管线。
    ///
    /// 为 true 时自建 pass 必须挂 `Depth24PlusStencil8` 附件。
    pub fn has_ds(&self) -> bool {
        self.has_ds
    }

    pub(crate) fn wants_pass(&self) -> bool {
        self.wants_pass
    }

    /// 把已准备的内容编码进调用方的 `RenderPass`。
    ///
    /// 可重复调用（每次都重发全部绘制，不支持只编码部分批次）。
    /// 返回本次发出的 draw call 数；整帧累计值在帧结束时发布到
    /// `Renderer::last_draw_calls`。
    pub fn encode(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        let r = self.renderer;
        let mut shape_draw_calls: u32 = 0;
        let vbuf = r.vertex_buf.lock();
        let ibuf = r.index_buf.lock();
        let particle_buf = r.particle_buf.lock();
        let geo_template_vbuf = r.geo_template_vertex_buf.lock();
        let geo_template_ibuf = r.geo_template_index_buf.lock();
        let geo_instance_buf = r.geo_instance_buf.lock();
        let ctx = BatchEncodeCtx {
            renderer: r,
            gpu: &r.gpu,
            camera_bind_group: &r.camera_bind_group,
            engine_bg: &self.engine_bg,
            vertex_buf: vbuf.as_ref().map(|b| &b.0),
            index_buf: ibuf.as_ref().map(|b| &b.0),
            particle_buf: particle_buf.as_ref().map(|b| &b.0),
            geo_template_vbuf: geo_template_vbuf.as_ref().map(|b| &b.0),
            geo_template_ibuf: geo_template_ibuf.as_ref().map(|b| &b.0),
            geo_instance_buf: geo_instance_buf.as_ref().map(|b| &b.0),
            sample_count: r.sample_count,
            alpha_to_coverage: r.alpha_to_coverage,
            ssaa: r.ssaa,
        };
        let mut scissor_stack = r.scratch_scissor_stack.lock();
        scissor_stack.clear();
        scissor_stack.push((0, 0, r.physical_width, r.physical_height));

        for (ei, _event) in self.events.iter().enumerate() {
            let info = &self.event_infos[ei];
            // ScissorPush: 计算物理像素 scissor rect，与当前 scissor 求交
            if let Some(scissor_rect) = info.scissor_push {
                let sx = r.physical_width as f32 / r.logical_width.max(1.0);
                let sy = r.physical_height as f32 / r.logical_height.max(1.0);
                let fw = r.physical_width as f32;
                let fh = r.physical_height as f32;
                // 负坐标 / 越界：先 float 裁到视口再转 u32，避免 as u32 回绕
                let x0 = (scissor_rect.x * sx).clamp(0.0, fw);
                let y0 = (scissor_rect.y * sy).clamp(0.0, fh);
                let x1 = ((scissor_rect.x + scissor_rect.w) * sx).clamp(0.0, fw);
                let y1 = ((scissor_rect.y + scissor_rect.h) * sy).clamp(0.0, fh);
                let px = x0.floor() as u32;
                let py = y0.floor() as u32;
                let pr = x1.ceil() as u32;
                let pb = y1.ceil() as u32;
                let (cx, cy, cw, ch) =
                    *scissor_stack
                        .last()
                        .unwrap_or(&(0, 0, r.physical_width, r.physical_height));
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
                let (cx, cy, cw, ch) =
                    *scissor_stack
                        .last()
                        .unwrap_or(&(0, 0, r.physical_width, r.physical_height));
                pass.set_scissor_rect(cx, cy, cw, ch);
            }

            // 形状事件：`Batch` 自身的几何，以及 AreaOp 的掩码几何（op 3/4）。
            // 两者都由 `info.shape` 承载；`encode_into` 不依赖 batch 字段。
            if let Some(shape) = &info.shape {
                let pipe_op = info.area_op.unwrap_or(info.stencil_op);
                let encode_info = BatchEncodeInfo {
                    base_vertex: shape.base_vertex,
                    geometry: shape.geometry,
                    blend: shape.blend,
                    target_blends: &self.target_blends,
                    custom_material: &info.custom_material,
                    segments: &shape.segments,
                    instances: &shape.instances,
                    geo_instances: &shape.geo_instances,
                    particles: &shape.particles,
                    ordered: &shape.ordered,
                    particle_clock: shape.particle_clock,
                    stencil_ref: info.stencil_ref,
                    dynamic_offsets: &info.dynamic_offsets,
                    pipe_op,
                    uses_stencil: self.uses_stencil,
                    has_ds: self.has_ds,
                };
                shape_draw_calls += DrawBatch::encode_into(&mut *pass, &encode_info, &ctx);
            }
            if !info.text.is_empty() && self.mrt {
                mrt_warn_once("text");
            } else if !info.text.is_empty() {
                // 有 DS 时：Push/Test 用 Equal；op=0（UI/unclipped）用 Always，避免误裁
                // Area 存在时：当前文本在 Area content level，测 (Test)。
                let has_area_at_text =
                    info.area_op.is_some() || info.stencil_op == 1 || info.stencil_op == 2;
                let text_mode = if !self.has_ds {
                    crate::text::TextStencilMode::None
                } else if has_area_at_text {
                    crate::text::TextStencilMode::Test
                } else {
                    crate::text::TextStencilMode::Pass
                };
                // 管线由段循环按（mode, blend, material）逐段 ensure（见下），此处不预建。
                // Push 后 mask 已 Inc：父文字测 new_level = ref+1
                let text_ref = if info.stencil_op == 1 {
                    info.stencil_ref + 1
                } else {
                    info.stencil_ref
                };
                // 必须在 set_pipeline（render_range 内）之后再 set_stencil_reference，
                // 否则部分后端会把 ref 重置为 0。
                // 文字管线按（mode, blend, material）去重：段连续同状态只建一次
                //（custom material 的 glyphon 管线无缓存，重建很贵）。
                // 文字路径：材质取 batch 最终值（text 无 per-shape command）。
                let material_bg = match info.custom_material.as_ref() {
                    Some(m) if m.bgl().is_some() => {
                        m.ensure_bind_group(&r.gpu.device, &r.gpu.queue, &r.gpu.bind_group_pool)
                    }
                    _ => None,
                };
                let material_bg = material_bg.as_ref();
                let text_tests_stencil = self.uses_stencil
                    && (info.stencil_op == 1 || info.stencil_op == 2 || info.area_op.is_some());
                let mut last_text_key: Option<(
                    crate::text::TextStencilMode,
                    wgpu::BlendState,
                    Option<*const Material>,
                )> = None;
                let mut text_mat_pipe: Option<Arc<wgpu::RenderPipeline>> = None;
                for segment in &info.text {
                    let mat_key = info.custom_material.as_ref().map(Arc::as_ptr);
                    let key = (text_mode, segment.blend, mat_key);
                    if last_text_key != Some(key) {
                        // custom 管线先行：raw 的 Text 臂内部 lock text_ctx，
                        // 此处必须 guard-free（临时的 guard 逐句释放）。
                        text_mat_pipe = info.custom_material.as_ref().and_then(|m| {
                            match r.gpu.ensure_material_pipeline(
                                m,
                                MaterialTarget::Text,
                                r.sample_count,
                                r.alpha_to_coverage,
                                false,
                                self.has_ds,
                                self.uses_stencil,
                                if text_tests_stencil { 2 } else { 0 },
                                crate::gpu::ShapeVertexLayout::Mesh,
                                &[Some(segment.blend)],
                            ) {
                                Ok(p) => Some(p),
                                Err(err) => {
                                    log::warn!(
                                        "custom material pipeline unavailable (text): {err}"
                                    );
                                    None
                                }
                            }
                        });
                        r.gpu.text_ctx.lock().unwrap().ensure_text_pipeline(
                            &r.gpu.device,
                            text_mode,
                            segment.blend,
                        );
                        last_text_key = Some(key);
                    }
                    sync_blend_constant(&mut *pass, segment.blend, segment.blend_constant);
                    let text_ctx = r.gpu.text_ctx.lock().unwrap();
                    if let Err(e) = text_ctx.text_renderer.render_range_with_material(
                        &text_ctx.text_atlas,
                        &text_ctx.viewport,
                        &mut *pass,
                        &self.engine_bg,
                        segment.vertex_start,
                        segment.vertex_count,
                        if self.uses_stencil {
                            Some(text_ref)
                        } else {
                            None
                        },
                        segment.bind_group.as_ref(),
                        text_mat_pipe.as_deref(),
                        material_bg,
                        &info.dynamic_offsets,
                    ) {
                        log::warn!("glyphon text render failed (skipped segment): {:?}", e);
                    }
                }
            }
        }

        self.draw_calls
            .set(self.draw_calls.get().saturating_add(shape_draw_calls));
        shape_draw_calls
    }

    /// 用引擎的附件计划（颜色 / MSAA resolve / DS）创建 pass。
    ///
    /// 与 [`Self::encode`] 配合即“引擎 pass 内自定义绘制”：同一个 pass 里先画引擎
    /// 批次、再画用户内容。`draw_with_inline` 是它的便捷封装。
    pub fn begin_engine_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
    ) -> wgpu::RenderPass<'e> {
        let r = self.renderer;
        let msaa_view = r.msaa_view(r.gpu.surface_format());
        // 目标附件清单：MRT 直接使用用户的多个颜色附件（引擎 MSAA 仅单目标路径）；
        // 单目标路径保持 MSAA resolve 到 target 0 的既有行为。
        let mut color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> =
            Vec::with_capacity(self.target.color_attachments.len().max(1));
        if self.mrt {
            for (i, att) in self.target.color_attachments.iter().enumerate() {
                let load = self.loads.get(i).copied().unwrap_or(wgpu::LoadOp::Load);
                color_attachments.push(Some(wgpu::RenderPassColorAttachment {
                    view: &att.view,
                    resolve_target: att.resolve_target.as_ref(),
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                }));
            }
        } else {
            let target_view = &self.target.color_attachments[0].view;
            let load = self.loads.first().copied().unwrap_or(wgpu::LoadOp::Load);
            let (color_view, resolve): (&wgpu::TextureView, Option<&wgpu::TextureView>) =
                match &msaa_view {
                    Some(msaa) => (msaa, Some(target_view)),
                    None => (target_view, None),
                };
            color_attachments.push(Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                resolve_target: resolve,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            }));
        }
        let dv;
        let ds_attachment = match &self.ds {
            ResolvedDsOps::None => None,
            ResolvedDsOps::Engine(ops) => {
                dv = r.ds_view();
                // depth 也 Clear：部分后端在 depth_ops=None 时对未定义 depth 行为异常，
                // 且 glyphon 写 depth=0，需可预测的 depth 缓冲。
                // 每次 draw 独立建立并清理 stencil；multi-draw 只复用颜色 attachment。
                Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dv,
                    depth_ops: ops.depth,
                    stencil_ops: ops.stencil,
                })
            }
            ResolvedDsOps::External => {
                // resolve 处已断言 `target.depth_stencil` 为 External；此处取其 view。
                let att = match self.target.depth_stencil.as_ref() {
                    Some(DepthStencilTarget::External(att)) => att,
                    _ => unreachable!("External resolve without external target"),
                };
                Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &att.view,
                    depth_ops: att.depth_ops,
                    stencil_ops: att.stencil_ops,
                })
            }
        };
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vireo render pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment: ds_attachment,
            ..Default::default()
        })
    }
}

impl Drop for PreparedFrame<'_> {
    fn drop(&mut self) {
        let r = self.renderer;
        *r.last_draw_calls.lock() = self.draw_calls.get();
        *r.last_clip_paths.lock() = std::mem::take(&mut self.clip_paths);
        // 归还 scratch 容量，避免每帧重新分配 EventInfo 向量。
        *r.scratch_event_infos.lock() = std::mem::take(&mut self.event_infos);
    }
}

/// 防止 `prepare_frame` 重入（嵌套会破坏 scratch 状态）。
struct FrameActiveGuard<'a>(&'a AtomicBool);

impl<'a> FrameActiveGuard<'a> {
    fn acquire(flag: &'a AtomicBool) -> Self {
        assert!(
            !flag.swap(true, Ordering::AcqRel),
            "nested Renderer::prepare_frame is not supported (a PreparedFrame is still alive)"
        );
        Self(flag)
    }
}

impl Drop for FrameActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
