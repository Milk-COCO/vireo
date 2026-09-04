use std::sync::Arc;

use crate::glyphon::{
    Buffer, PrepareError, Shaping, TextArea, TextAtlas, TextBounds,
};
use crate::glyphon::Metrics;
use crate::color::Color;
use crate::render::Transform;
use crate::gpu::GpuContext;

use super::*;

/// 文本区域元数据（entry 遍历阶段收集，第二次循环消费）。
/// `buf` 用 enum 携带 buffer 来源：
/// - `Slot(u32)`：cache 槽索引（prepare 期间 `text_ctx` 持锁，不会被 evict）
/// - `Stable(Arc<Buffer>)`：StableText 持有，不在 cache 中；
///   由 `metas` 持有 Arc 引用，延寿到第二次循环消费完。
pub(super) struct AreaMeta {
    pub(super) buf: MetaBuf,
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) color: crate::glyphon::Color,
    pub(super) bounds: TextBounds,
    pub(super) transform_index: u32,
    pub(super) base_uv_rect: [f32; 4],
    pub(super) texture_state: TextTextureState,
}

pub(super) enum MetaBuf {
    Slot(u32),
    Stable(Arc<Buffer>),
    Resolved(Arc<ResolvedTextGlyph>),
}

/// 读取 transform_table[ti] 的列，越界返 None。
pub(super) fn table_cols(table: &[f32], ti: u32) -> Option<([f32; 3], [f32; 3], [f32; 3])> {
    let base = ti as usize * 12;
    if base + 12 > table.len() { return None; }
    let t = &table[base..base + 12];
    Some(([t[0], t[1], 0.0], [t[4], t[5], 0.0], [t[8], t[9], 1.0]))
}

/// 计算 entry 的**物理空间**列向量（线性 + 平移已 × scale）。
/// `override` 存在时 = table[ti] * override；否则 = table[ti]。
/// 退化为恒等返 None。
pub(super) fn composed_phys_cols(
    ti: u32,
    table: &[f32],
    ov: Option<&Transform>,
    scale: f32,
) -> Option<([f32; 3], [f32; 3], [f32; 3])> {
    let m = if let Some((c0, c1, c2)) = table_cols(table, ti) {
        Transform::matrix(c0[0], c1[0], c0[1], c1[1], c2[0], c2[1])
    } else {
        Transform::IDENTITY
    };
    let composed = match ov {
        Some(o) => m.then(o),
        None => m,
    };
    let (c0, c1, c2) = composed.to_cols();
    // to_cols() 总产出 [a c 0; b d 0; tx ty 1]，padding 三行/第三列固定；
    // 比 6 float 足够判定 identity，padding 不参与语义。
    let is_identity = c0[0] == 1.0 && c0[1] == 0.0
        && c1[0] == 0.0 && c1[1] == 1.0
        && c2[0] == 0.0 && c2[1] == 0.0;
    if is_identity { None } else {
        Some(([c0[0], c0[1], 0.0], [c1[0], c1[1], 0.0], [c2[0] * scale, c2[1] * scale, 1.0]))
    }
}

/// 把 composed 列写入 global_transforms，返回新 index；identity 返 0。
pub(super) fn push_phys(global_transforms: &mut Vec<f32>, cols: ([f32; 3], [f32; 3], [f32; 3])) -> u32 {
    let idx = (global_transforms.len() / 12) as u32;
    let (c0, c1, c2) = cols;
    global_transforms.extend_from_slice(&[
        c0[0], c0[1], 0.0, 0.0,
        c1[0], c1[1], 0.0, 0.0,
        c2[0], c2[1], 1.0, 0.0,
    ]);
    idx
}

/// 逻辑 TextBounds → 物理（与 left/top × scale 一致）。全屏 default 不缩放。
pub(super) fn scale_text_bounds(b: TextBounds, scale: f32) -> TextBounds {
    if b == TextBounds::default() {
        return b;
    }
    TextBounds {
        left: (b.left as f32 * scale).round() as i32,
        top: (b.top as f32 * scale).round() as i32,
        right: (b.right as f32 * scale).round() as i32,
        bottom: (b.bottom as f32 * scale).round() as i32,
    }
}

/// 把物理 TextBounds 的四个角过 `cols` 变换后取 AABB。
/// 用于让旋转/缩放文字的 clip 跟随变换（避免「旋转文字被未旋转的方框裁掉」）。
pub(super) fn transform_bounds(
    b: TextBounds,
    cols: ([f32; 3], [f32; 3], [f32; 3]),
) -> TextBounds {
    let (c0, c1, c2) = cols;
    let corners = [
        (b.left as f32, b.top as f32),
        (b.right as f32, b.top as f32),
        (b.left as f32, b.bottom as f32),
        (b.right as f32, b.bottom as f32),
    ];
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (cx, cy) in corners {
        let wx = c0[0] * cx + c1[0] * cy + c2[0];
        let wy = c0[1] * cx + c1[1] * cy + c2[1];
        if wx < min_x { min_x = wx; }
        if wx > max_x { max_x = wx; }
        if wy < min_y { min_y = wy; }
        if wy > max_y { max_y = wy; }
    }
    TextBounds {
        left: min_x.round() as i32,
        top: min_y.round() as i32,
        right: max_x.round() as i32,
        bottom: max_y.round() as i32,
    }
}

impl TextEntryList {
    /// 准备文本条目（glyphon prepare），按入队时冻结的贴图状态分段返回。
    ///
    /// 连续相同 [`TextTextureState::generation`] 的条目合并为一段
    /// [`PreparedTextSegment`]；段内 glyph 写入全局 glyphon 顶点缓冲，
    /// 由 Renderer 按段绑定 base texture 后 `render_range`。
    ///
    /// - `transform_table`：batch 本地变换表（12 f32 / mat3x3）
    /// - `global_transforms`：全局物理空间矩阵表（本函数会追加）
    /// - `scale`：逻辑→物理像素
    /// - `batch_text_clip` / `batch_color`：默认裁切与颜色（可被 `TextOverride` 覆盖）
    ///
    /// 空列表返回空 `Vec`。
    pub(crate) fn prepare_texts(
        &self,
        gpu: &GpuContext,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
        transform_table: &[f32],
        global_transforms: &mut Vec<f32>,
        batch_text_clip: Option<TextBounds>,
        batch_color: Color,
    ) -> Vec<PreparedTextSegment> {
        if self.entries.is_empty() {
            return Vec::new();
        }

        let mut text_ctx = gpu.text_ctx.lock().unwrap();
        text_ctx.begin_prepare_pins();

        text_ctx.ensure_viewport(&gpu.queue, physical_width, physical_height);

        let mut metas: Vec<AreaMeta> = Vec::with_capacity(self.entries.len() * 2);
        // 注：StableText 的 Arc<Buffer> 直接放 metas.buf.Stable 里，延寿到第二次循环消费完。
        // 不再需要平行 stable_bufs vec + hi 计数器。

        for entry in &self.entries {
            let ov = entry.override_();
            let mut texture_state = entry.texture_state().clone();
            // uv / bind_group 覆盖：与 ShapeOverride 共享 semantics
            let effective_uv = ov.uv.unwrap_or(texture_state.uv);
            let batch_base_uv = [effective_uv.u0, effective_uv.v0, effective_uv.u1, effective_uv.v1];
            // bind_group 覆盖：按覆盖后的状态分段
            if let Some(bg_opt) = &ov.bind_group {
                texture_state.bind_group = bg_opt.clone();
                texture_state.view = None;
                let bg_hash = bg_opt.as_ref().map(|bg| {
                    use std::hash::{Hash, Hasher};
                    let mut h = rustc_hash::FxHasher::default();
                    bg.hash(&mut h);
                    h.finish()
                }).unwrap_or(0x9E3779B97F4A7C15);
                texture_state.generation = texture_state.generation.wrapping_add(1).wrapping_add(bg_hash);
                texture_state.uv = effective_uv;
            } else if ov.uv.is_some() {
                texture_state.uv = effective_uv;
            }
            // color: override > batch_color
            let color_rgb = entry.override_().color.unwrap_or(batch_color);
            let color = crate::glyphon::Color::rgba(
                (color_rgb.r * 255.0) as u8,
                (color_rgb.g * 255.0) as u8,
                (color_rgb.b * 255.0) as u8,
                (color_rgb.a * 255.0) as u8,
            );
            // clip：逻辑像素 → 物理（× scale）
            // clip：逻辑像素 → 物理（× scale），再过文字的物理变换 → 旋转/缩放下跟随文字
            let (bounds, phys_idx) = {
                let raw_bounds = match entry.override_().clip {
                    Some(Some(b)) => scale_text_bounds(b, scale),
                    Some(None) => TextBounds::default(),
                    None => scale_text_bounds(batch_text_clip.unwrap_or_default(), scale),
                };
                // 计算 entry 的物理列（含 override），同步用于 bounds 与 phys_idx
                let phys_cols = composed_phys_cols(
                    entry.transform_index(),
                    transform_table,
                    entry.override_().transform.as_ref(),
                    scale,
                );
                let new_bounds = match phys_cols {
                    Some(cols) if raw_bounds != TextBounds::default() => transform_bounds(raw_bounds, cols),
                    _ => raw_bounds,
                };
                let idx = match phys_cols {
                    Some(cols) => push_phys(global_transforms, cols),
                    None => 0,
                };
                (new_bounds, idx)
            };
            let top = entry.pos().y * scale;

            match entry {
                TextEntry::Stable { pos, .. } => {
                    if let TextEntry::Stable { resolved_glyphs, .. } = entry {
                        for glyph in resolved_glyphs.iter() {
                            metas.push(AreaMeta {
                                buf: MetaBuf::Resolved(glyph.clone()),
                                left: pos.x * scale,
                                top,
                                color,
                                bounds,
                                transform_index: phys_idx,
                                base_uv_rect: batch_base_uv,
                                texture_state: texture_state.clone(),
                            });
                        }
                    }
                }
                TextEntry::Parts { pos, def, parts, .. } => {
                    // HUD 多段：逻辑 x 横拼，再 * scale；每段可用 resolve_def 覆盖字号等
                    let mut cursor_x = pos.x;
                    for part in parts {
                        match part {
                            TextPart::Normal(s, _) => {
                                if s.is_empty() {
                                    continue;
                                }
                                let pdef = part.resolve_def(def);
                                let slot = text_ctx.get_or_shape_text(s, pdef);
                                let w = text_ctx.slot_line_width(slot);
                                metas.push(AreaMeta {
                                    buf: MetaBuf::Slot(slot),
                                    left: cursor_x * scale,
                                    top,
                                    color,
                                    bounds,
                                    transform_index: phys_idx,
                                    base_uv_rect: batch_base_uv,
                                    texture_state: texture_state.clone(),
                                });
                                cursor_x += w;
                            }
                            TextPart::Dynamic(s, _) => {
                                if s.is_empty() {
                                    continue;
                                }
                                let pdef = part.resolve_def(def);
                                let opts = TextDef {
                                    max_width: None,
                                    align: TextAlign::Left,
                                    ..pdef.clone()
                                };
                                let key = ShapeKey::from_text(s, &opts);
                                let (area_meta, w) = match text_ctx.peek_shape_slot(&key) {
                                    Some(si) => {
                                        text_ctx.touch_slot(si);
                                        let w = text_ctx.slot_line_width(si);
                                        (AreaMeta {
                                            buf: MetaBuf::Slot(si),
                                            left: cursor_x * scale,
                                            top,
                                            color,
                                            bounds,
                                            transform_index: phys_idx,
                                            base_uv_rect: batch_base_uv,
                                            texture_state: texture_state.clone(),
                                        }, w)
                                    }
                                    None => {
                                        let metrics =
                                            Metrics::new(opts.font_size, opts.font_size * 1.2);
                                        let mut buffer = text_ctx.take_buffer(metrics);
                                        let attrs = opts
                                            .attrs
                                            .as_ref()
                                            .map(|a| a.as_attrs())
                                            .unwrap_or_else(Attrs::new);
                                        buffer.set_size(None, None);
                                        buffer.set_text(s, &attrs, Shaping::Advanced, None);
                                        buffer.shape_until_scroll(&mut text_ctx.font_system, false);
                                        let lw = buffer
                                            .line_layout(&mut text_ctx.font_system, 0)
                                            .map(|layout| layout.iter().map(|run| run.w).sum::<f32>())
                                            .unwrap_or(0.0);
                                        (AreaMeta {
                                            buf: MetaBuf::Stable(Arc::new(buffer)),
                                            left: cursor_x * scale,
                                            top,
                                            color,
                                            bounds,
                                            transform_index: phys_idx,
                                            base_uv_rect: batch_base_uv,
                                            texture_state: texture_state.clone(),
                                        }, lw)
                                    }
                                };
                                metas.push(area_meta);
                                cursor_x += w;
                            }
                            TextPart::Stable(h) => {
                                for glyph in h.resolved_glyphs.iter() {
                                    metas.push(AreaMeta {
                                            buf: MetaBuf::Resolved(glyph.clone()),
                                        left: cursor_x * scale,
                                        top,
                                        color,
                                        bounds,
                                        transform_index: phys_idx,
                                        base_uv_rect: batch_base_uv,
                                        texture_state: texture_state.clone(),
                                    });
                                }
                                cursor_x += h.line_width();
                            }
                            TextPart::Glyphs(s, _) => {
                                let pdef = part.resolve_def(def);
                                for ch in s.chars() {
                                    let cluster = text_ctx.resolve_glyph(ch, pdef);
                                    for glyph in cluster.glyphs.iter() {
                                        metas.push(AreaMeta {
                                            buf: MetaBuf::Resolved(glyph.clone()),
                                            left: cursor_x * scale,
                                            top,
                                            color,
                                            bounds,
                                            transform_index: phys_idx,
                                            base_uv_rect: batch_base_uv,
                                            texture_state: texture_state.clone(),
                                        });
                                    }
                                    cursor_x += cluster.advance;
                                }
                            }
                        }
                    }
                }
                TextEntry::Normal { pos, text, def, .. } => {
                    let key = ShapeKey::from_text(text, def);
                    let slot = text_ctx.get_or_shape(key, def);
                    metas.push(AreaMeta {
                        buf: MetaBuf::Slot(slot),
                        left: pos.x * scale,
                        top,
                        color,
                        bounds,
                        transform_index: phys_idx,
                        base_uv_rect: batch_base_uv,
                        texture_state: texture_state.clone(),
                    });
                }
            }
        }

        let mut segments = Vec::new();
        {
            let TextContext {
                ref mut font_system,
                ref mut swash_cache,
                ref mut text_atlas,
                ref mut text_renderer,
                ref viewport,
                ref shape_slots,
                ref cache,
                ref base_texture,
                ref base_sampler,
                ..
            } = *text_ctx;

            let mut first = 0;
            while first < metas.len() {
                let generation = metas[first].texture_state.generation;
                let mut end = first + 1;
                while end < metas.len() && metas[end].texture_state.generation == generation {
                    end += 1;
                }

                let vertex_start = text_renderer.glyph_vertex_count();
                let mut run_start = first;
                while run_start < end {
                    let resolved = matches!(metas[run_start].buf, MetaBuf::Resolved(_));
                    let mut run_end = run_start + 1;
                    while run_end < end
                        && matches!(metas[run_end].buf, MetaBuf::Resolved(_)) == resolved
                    {
                        run_end += 1;
                    }

                    if resolved {
                        let mut attempt = 0;
                        loop {
                            let glyphs = metas[run_start..run_end].iter().filter_map(|meta| {
                                let MetaBuf::Resolved(resolved) = &meta.buf else { return None };
                                Some(crate::glyphon::ResolvedGlyphArea {
                                    glyph: &resolved.glyph,
                                    line_y: resolved.line_y,
                                    line_top: resolved.line_top,
                                    line_height: resolved.line_height,
                                    left: meta.left,
                                    top: meta.top,
                                    scale,
                                    bounds: meta.bounds,
                                    default_color: meta.color,
                                    transform_index: meta.transform_index,
                                    base_uv_rect: meta.base_uv_rect,
                                })
                            });
                            match text_renderer.prepare_resolved_glyphs(
                                &gpu.device,
                                &gpu.queue,
                                font_system,
                                text_atlas,
                                viewport,
                                glyphs,
                                swash_cache,
                            ) {
                                Ok(()) => break,
                                Err(PrepareError::AtlasFull) if attempt == 0 => {
                                    *text_atlas = TextAtlas::with_color_mode(
                                        &gpu.device,
                                        &gpu.queue,
                                        cache,
                                        text_atlas.format,
                                        text_atlas.color_mode,
                                        base_texture,
                                        base_sampler,
                                    );
                                    attempt += 1;
                                }
                                Err(PrepareError::AtlasFull) => {
                                    log::warn!("text atlas full after rebuild, skipping segment");
                                    break;
                                }
                            }
                        }
                    } else {
                        let mut attempt = 0;
                        loop {
                            let areas = metas[run_start..run_end].iter().filter_map(|meta| {
                                let buf: &Buffer = match &meta.buf {
                                    MetaBuf::Stable(arc) => arc,
                                    MetaBuf::Slot(si) => {
                                        debug_assert!((*si as usize) < shape_slots.len());
                                        &*shape_slots[*si as usize].buffer
                                    }
                                    MetaBuf::Resolved(_) => return None,
                                };
                                Some(TextArea {
                                    buffer: buf,
                                    left: meta.left,
                                    top: meta.top,
                                    scale,
                                    bounds: meta.bounds,
                                    default_color: meta.color,
                                    custom_glyphs: &[],
                                    transform_index: meta.transform_index,
                                    base_uv_rect: meta.base_uv_rect,
                                })
                            });
                            match text_renderer.prepare(
                                &gpu.device,
                                &gpu.queue,
                                font_system,
                                text_atlas,
                                viewport,
                                areas,
                                swash_cache,
                            ) {
                                Ok(()) => break,
                                Err(PrepareError::AtlasFull) if attempt == 0 => {
                                    *text_atlas = TextAtlas::with_color_mode(
                                        &gpu.device,
                                        &gpu.queue,
                                        cache,
                                        text_atlas.format,
                                        text_atlas.color_mode,
                                        base_texture,
                                        base_sampler,
                                    );
                                    attempt += 1;
                                }
                                Err(PrepareError::AtlasFull) => {
                                    log::warn!("text atlas full after rebuild, skipping segment");
                                    break;
                                }
                            }
                        }
                    }
                    run_start = run_end;
                }
                let vertex_count = text_renderer.glyph_vertex_count() - vertex_start;
                if vertex_count > 0 {
                    segments.push(PreparedTextSegment {
                        vertex_start,
                        vertex_count,
                        texture_view: metas[first].texture_state.view.clone(),
                        bind_group: metas[first].texture_state.bind_group.clone(),
                    });
                }
                first = end;
            }
        }

        segments
    }
}
