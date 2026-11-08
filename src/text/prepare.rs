use std::sync::Arc;

use crate::color::Color;
use crate::glyphon::Metrics;
use crate::glyphon::{Buffer, PrepareError, Shaping, TextArea, TextAtlas, TextBounds};
use crate::gpu::GpuContext;
use crate::render::Transform;

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
    pub(super) blend: wgpu::BlendState,
    pub(super) blend_constant: wgpu::Color,
    /// 样式表下标 + 1（`0` = 无样式）。
    pub(super) style_index: u32,
    /// 只画图层的样式下标 + 1（彩色字形用）。
    pub(super) style_layers_only: u32,
    /// 距离场带宽（设备像素）。
    pub(super) sdf_band: f32,
    /// 该样式「画在填充之上」的层数（> 0 才需要 above 那一趟）。
    pub(super) style_layers: u32,
    /// 位图：第 i 位 = 第 i 层画在填充之上。
    pub(super) style_above_mask: u32,
    /// 样式向字形外侧的最大外扩（设备像素）。
    pub(super) style_reach: f32,
    /// 几何描边（真 miter/bevel）；画在填充之下。
    pub(super) vector_stroke: Option<crate::text::TextStrokePx>,
}

/// 位图：第 i 位 = 第 i 层画在填充之上（最多 8 层）。
pub(super) fn above_mask(style: &std::sync::Arc<crate::text::TextStyle>) -> u32 {
    let mut mask = 0u32;
    for (i, layer) in style.layers.iter().take(8).enumerate() {
        if layer.above_fill {
            mask |= 1 << i;
        }
    }
    mask
}

/// 层数（最多 8）。
fn layer_count_of(style: &std::sync::Arc<crate::text::TextStyle>) -> u32 {
    style.layers.len().min(8) as u32
}

/// 几何描边：折算成设备像素（`None` = 该样式没有几何描边）。
pub(super) fn stroke_px(
    style: &crate::text::TextStyle,
    dpi: f64,
) -> Option<crate::text::TextStrokePx> {
    use crate::dpi::ToPx;
    style
        .vector_stroke
        .as_ref()
        .map(|s| crate::text::TextStrokePx {
            width: s.width.to_px(dpi).0 as f32,
            color: s.color,
            join: match s.join {
                crate::text::TextJoin::Miter => 0,
                crate::text::TextJoin::Bevel => 1,
                crate::text::TextJoin::Round => 2,
            },
            cap: match s.cap {
                crate::text::TextCap::Butt => 0,
                crate::text::TextCap::Square => 1,
                crate::text::TextCap::Round => 2,
            },
            miter_limit: s.miter_limit,
        })
}

pub(super) enum MetaBuf {
    Slot(u32),
    Stable(Arc<Buffer>),
    /// resolved 字形 + 它所属的烘焙块（`None` = 该条目没有固化句柄）。
    ///
    /// 只在**烘焙失效**的那一帧用：逐字形 prepare 一遍，顺便把实例捕获进块。
    Resolved {
        glyph: Arc<ResolvedTextGlyph>,
        block: Option<Arc<super::hud::BakedHandle>>,
    },
    /// 命中烘焙块：整个块只占**一条** meta，拷贝时一次写完全部实例。
    ///
    /// `dx/dy` 是整数位移（加进实例坐标），`slot` 是"平移 × 批次变换"的变换槽下标。
    Baked {
        block: Arc<super::hud::BakedHandle>,
        dx: i32,
        dy: i32,
        slot: u32,
    },
}

/// 读取 transform_table[ti] 的列，越界返 None。
pub(super) fn table_cols(table: &[f32], ti: u32) -> Option<([f32; 3], [f32; 3], [f32; 3])> {
    let base = ti as usize * 12;
    if base + 12 > table.len() {
        return None;
    }
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
    let is_identity = c0[0] == 1.0
        && c0[1] == 0.0
        && c1[0] == 0.0
        && c1[1] == 1.0
        && c2[0] == 0.0
        && c2[1] == 0.0;
    if is_identity {
        None
    } else {
        Some((
            [c0[0], c0[1], 0.0],
            [c1[0], c1[1], 0.0],
            [c2[0] * scale, c2[1] * scale, 1.0],
        ))
    }
}

/// 把 composed 列写入 global_transforms，返回新 index；identity 返 0。
pub(super) fn push_phys(
    global_transforms: &mut Vec<f32>,
    cols: ([f32; 3], [f32; 3], [f32; 3]),
) -> u32 {
    let idx = (global_transforms.len() / 12) as u32;
    let (c0, c1, c2) = cols;
    global_transforms.extend_from_slice(&[
        c0[0], c0[1], 0.0, 0.0, c1[0], c1[1], 0.0, 0.0, c2[0], c2[1], 1.0, 0.0,
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
pub(super) fn transform_bounds(b: TextBounds, cols: ([f32; 3], [f32; 3], [f32; 3])) -> TextBounds {
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
        if wx < min_x {
            min_x = wx;
        }
        if wx > max_x {
            max_x = wx;
        }
        if wy < min_y {
            min_y = wy;
        }
        if wy > max_y {
            max_y = wy;
        }
    }
    TextBounds {
        left: min_x.round() as i32,
        top: min_y.round() as i32,
        right: max_x.round() as i32,
        bottom: max_y.round() as i32,
    }
}

/// 图集布局代数（拿不到 `TextContext` 的全部借用时用）。
fn text_atlas_epoch(ctx: &crate::text::TextContext) -> (u64, u64) {
    ctx.text_atlas.layout_epoch()
}

/// 烘焙块是否**仍然有效**（DPI/裁剪/变换模式/图集布局都没变）；有效则返回它的整数原点。
///
/// 位置**不在**判定里：实例是本地坐标，绘制位置由平移变换槽给。
fn baked_state(
    handle: &super::hud::BakedHandle,
    bounds: crate::glyphon::TextBounds,
    transform_index: u32,
    scale: f32,
    epoch: (u64, u64),
) -> Option<(i32, i32)> {
    let key = super::hud::BakeKey {
        mask_epoch: epoch.0,
        color_epoch: epoch.1,
        scale_bits: scale.to_bits(),
        bounds: [bounds.left, bounds.top, bounds.right, bounds.bottom],
        transform_is_zero: transform_index == 0,
    };
    handle.with_block(|block| {
        let block = block?;
        (block.key == key).then_some(block.origin)
    })
}

/// 把"想画在 `(left, top)`"拆成：整数位移（加进实例坐标）+ 平移量（进变换槽）。
///
/// 烘焙块的原点是**整数物理像素**，所以平移量 = 原点 + 小数部分，最后落点
/// `本地坐标 + 整数位移 + 平移量` 精确等于 `(left, top)`。
fn translate_parts(left: f32, top: f32, origin: (i32, i32)) -> (i32, i32, f32, f32) {
    let dx = left - origin.0 as f32;
    let dy = top - origin.1 as f32;
    let dxi = dx.floor();
    let dyi = dy.floor();
    (
        dxi as i32,
        dyi as i32,
        origin.0 as f32 + (dx - dxi),
        origin.1 as f32 + (dy - dyi),
    )
}

/// 追加一个"平移 × 批次物理变换"的变换槽，返回下标。
///
/// 复用已有的 `global_transforms` 机制（每个条目本来就会 push 自己的槽），
/// 所以任意小数位移都不需要重烘实例。
fn push_translate_slot(
    global_transforms: &mut Vec<f32>,
    phys_cols: Option<([f32; 3], [f32; 3], [f32; 3])>,
    tx: f32,
    ty: f32,
) -> u32 {
    let mut cols = phys_cols.unwrap_or(([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]));
    cols.2[0] += tx;
    cols.2[1] += ty;
    push_phys(global_transforms, cols)
}

/// 一个 meta 所属的烘焙句柄（只有 `TextEntry::Stable` / `TextPart::Stable` 有）。
fn block_of(meta: &AreaMeta) -> Option<Arc<super::hud::BakedHandle>> {
    match &meta.buf {
        MetaBuf::Resolved { block, .. } => block.clone(),
        _ => None,
    }
}

fn same_handle(
    a: &Option<Arc<super::hud::BakedHandle>>,
    b: &Option<Arc<super::hud::BakedHandle>>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

/// 一个 meta 属于哪条处理分支（用于把连续同类 meta 归成一个运行区间）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Normal,
    Resolved,
    Baked,
}

fn run_kind(meta: &AreaMeta) -> RunKind {
    match &meta.buf {
        MetaBuf::Resolved { .. } => RunKind::Resolved,
        MetaBuf::Baked { .. } => RunKind::Baked,
        _ => RunKind::Normal,
    }
}

/// 用一个 meta 的位置/裁剪/样式下标拼出烘焙失效键。
fn bake_key_of(meta: &AreaMeta, scale: f32, epoch: (u64, u64)) -> super::hud::BakeKey {
    super::hud::BakeKey {
        mask_epoch: epoch.0,
        color_epoch: epoch.1,
        scale_bits: scale.to_bits(),
        bounds: [
            meta.bounds.left,
            meta.bounds.top,
            meta.bounds.right,
            meta.bounds.bottom,
        ],
        transform_is_zero: meta.transform_index == 0,
    }
}

/// 把一个烘焙块的实例拷进本帧实例数组并打补丁。
///
/// 实例是**本地坐标**：位置只加整数位移（`dx/dy`），小数部分由变换槽 `slot` 给，
/// 所以任意小数位移都不用重烘。补丁：颜色（只改哨兵）、变换槽、batch UV、
/// 样式表下标（只改"带样式"的那些，`pass` 位原样保留）。
fn copy_baked_instances(
    block: &super::hud::BakedBlock,
    dx: i32,
    dy: i32,
    slot: u32,
    color: u32,
    base_uv_rect: [f32; 4],
    style_index: u32,
    text_renderer: &mut crate::glyphon::TextRenderer,
) {
    // 实例的 `style_index` = 帧内样式表下标 | (pass << PASS_SHIFT)：
    // 下标要换成当前帧的，pass 位原样保留。填充/几何描边下标是 0，不动。
    const PASS_MASK: u32 = 0xFF << crate::text::PASS_SHIFT;
    let table_bits = !PASS_MASK;
    text_renderer.reserve_instances(block.instances.len());
    for src in &block.instances {
        let mut inst = *src;
        inst.pos[0] += dx;
        inst.pos[1] += dy;
        if inst.color == super::hud::BAKED_FILL_SENTINEL {
            inst.color = color;
        }
        inst.transform_index = slot;
        inst.base_uv_rect = base_uv_rect;
        if inst.style_index & table_bits != 0 {
            inst.style_index = (inst.style_index & PASS_MASK) | (style_index & table_bits);
        }
        text_renderer.push_instance(inst);
    }
}

/// 命中块（整块一条 meta）时的拷贝。失效判定已在建 meta 时做过。
fn append_baked_meta(meta: &AreaMeta, text_renderer: &mut crate::glyphon::TextRenderer) {
    let MetaBuf::Baked {
        block,
        dx,
        dy,
        slot,
    } = &meta.buf
    else {
        return;
    };
    let style_index = meta.style_index;
    let color = meta.color.0;
    let base_uv_rect = meta.base_uv_rect;
    block.with_block(|block| {
        let Some(block) = block else { return };
        copy_baked_instances(
            block,
            *dx,
            *dy,
            *slot,
            color,
            base_uv_rect,
            style_index,
            text_renderer,
        );
    });
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

        let mut text_ctx = gpu.text_ctx();
        text_ctx.begin_prepare_pins();

        text_ctx.ensure_viewport(&gpu.queue, physical_width, physical_height, scale);

        let mut metas: Vec<AreaMeta> = Vec::with_capacity(self.entries.len() * 2);
        // 样式表：本批次的样式去重后整块上传（`group(1)` 的 storage）。
        let mut style_table = crate::text::style::StyleTable::new();
        // 注：StableText 的 Arc<Buffer> 直接放 metas.buf.Stable 里，延寿到第二次循环消费完。
        // 不再需要平行 stable_bufs vec + hi 计数器。

        for entry in &self.entries {
            let ov = entry.override_();
            let mut texture_state = entry.texture_state().clone();
            // uv / bind_group 覆盖：与 ShapeOverride 共享 semantics
            let effective_uv = ov.uv.unwrap_or(texture_state.uv);
            let batch_base_uv = [
                effective_uv.u0,
                effective_uv.v0,
                effective_uv.u1,
                effective_uv.v1,
            ];
            // bind_group 覆盖：按覆盖后的状态分段
            if let Some(bg_opt) = &ov.bind_group {
                texture_state.bind_group = bg_opt.clone();
                texture_state.view = None;
                let bg_hash = bg_opt
                    .as_ref()
                    .map(|bg| {
                        use std::hash::{Hash, Hasher};
                        let mut h = rustc_hash::FxHasher::default();
                        bg.hash(&mut h);
                        h.finish()
                    })
                    .unwrap_or(0x9E3779B97F4A7C15);
                texture_state.generation = texture_state
                    .generation
                    .wrapping_add(1)
                    .wrapping_add(bg_hash);
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
            let (bounds, phys_idx, phys_cols) = {
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
                    Some(cols) if raw_bounds != TextBounds::default() => {
                        transform_bounds(raw_bounds, cols)
                    }
                    _ => raw_bounds,
                };
                let idx = match phys_cols {
                    Some(cols) => push_phys(global_transforms, cols),
                    None => 0,
                };
                (new_bounds, idx, phys_cols)
            };
            let top = entry.pos().y * scale;

            // 行级样式（`TextPart` 逐段覆盖的话，下面在段内重新算一次并 shadow）。
            let (row_style_index, row_style_layers_only, row_sdf_band) = match entry.style() {
                Some(style) if style.needs_style() => {
                    let (a, band) = style_table.register(style, scale as f64);
                    let (b, _) = style_table.register_layers_only(style, scale as f64);
                    (a, b, band)
                }
                _ => (0, 0, 0.0),
            };
            let row_vector_stroke = entry.style().and_then(|s| stroke_px(s, scale as f64));
            let row_style_layers = entry.style().map(layer_count_of).unwrap_or(0);
            let row_style_above_mask = entry.style().map(above_mask).unwrap_or(0);
            let row_style_reach = entry
                .style()
                .map(|s| s.reach_px(scale as f64))
                .unwrap_or(0.0);
            let mut style_index = row_style_index;
            let mut style_layers_only = row_style_layers_only;
            let mut sdf_band = row_sdf_band;
            let mut style_layers = row_style_layers;
            let mut style_above_mask = row_style_above_mask;
            let mut style_reach = row_style_reach;
            let mut vector_stroke = row_vector_stroke;

            match entry {
                TextEntry::Stable { pos, baked, .. } => {
                    if let TextEntry::Stable {
                        resolved_glyphs, ..
                    } = entry
                    {
                        let left = pos.x * scale;
                        // 命中烘焙块 ⇒ 整个块一条 meta（稳态不再逐字形建 AreaMeta）。
                        // 位置**不进实例**：实例是本地坐标，位置由一次性的平移变换槽给，
                        // 所以任意小数位移都不需要重烘。
                        let hit = baked_state(
                            baked,
                            bounds,
                            phys_idx,
                            scale,
                            text_atlas_epoch(&text_ctx),
                        );
                        if let Some(origin) = hit {
                            let (dx, dy, tx, ty) = translate_parts(left, top, origin);
                            let slot = if phys_cols.is_some() || tx != 0.0 || ty != 0.0 {
                                push_translate_slot(global_transforms, phys_cols, tx, ty)
                            } else {
                                phys_idx
                            };
                            metas.push(AreaMeta {
                                buf: MetaBuf::Baked {
                                    block: baked.clone(),
                                    dx,
                                    dy,
                                    slot,
                                },
                                left,
                                top,
                                color,
                                bounds,
                                transform_index: phys_idx,
                                base_uv_rect: batch_base_uv,
                                texture_state: texture_state.clone(),
                                blend: entry.blend(),
                                blend_constant: entry.blend_constant(),
                                style_index,
                                style_layers_only,
                                sdf_band,
                                style_layers,
                                style_above_mask,
                                style_reach,
                                vector_stroke,
                            });
                        } else {
                            for glyph in resolved_glyphs.iter() {
                                metas.push(AreaMeta {
                                    buf: MetaBuf::Resolved {
                                        glyph: glyph.clone(),
                                        block: Some(baked.clone()),
                                    },
                                    left,
                                    top,
                                    color,
                                    bounds,
                                    transform_index: phys_idx,
                                    base_uv_rect: batch_base_uv,
                                    texture_state: texture_state.clone(),
                                    blend: entry.blend(),
                                    blend_constant: entry.blend_constant(),
                                    style_index,
                                    style_layers_only,
                                    sdf_band,
                                    style_layers,
                                    style_above_mask,
                                    style_reach,
                                    vector_stroke,
                                });
                            }
                        }
                    }
                }
                TextEntry::Parts {
                    pos, def, parts, ..
                } => {
                    // HUD 多段：逻辑 x 横拼，再 * scale；每段可用 resolve_def 覆盖字号等
                    let mut cursor_x = pos.x;
                    for part in parts {
                        // 每段从行级样式起算（上一段的设置不能渗到下一段）。
                        style_index = row_style_index;
                        style_layers_only = row_style_layers_only;
                        sdf_band = row_sdf_band;
                        style_layers = row_style_layers;
                        style_above_mask = row_style_above_mask;
                        style_reach = row_style_reach;
                        vector_stroke = row_vector_stroke;
                        match part {
                            TextPart::Normal(s, _) => {
                                if s.is_empty() {
                                    continue;
                                }
                                let pdef = part.resolve_def(def);
                                // 逐段样式：本段 `TextDef` 覆盖行级（与字号/字体同一套语义）。
                                let (si, lo, band) = match pdef.style.as_ref() {
                                    Some(st) if st.needs_style() => {
                                        let (a, bd) = style_table.register(st, scale as f64);
                                        let (b, _) =
                                            style_table.register_layers_only(st, scale as f64);
                                        (a, b, bd)
                                    }
                                    _ => (0, 0, 0.0),
                                };
                                style_index = si;
                                style_layers_only = lo;
                                sdf_band = band;
                                vector_stroke =
                                    pdef.style.as_ref().and_then(|s| stroke_px(s, scale as f64));
                                style_above_mask = pdef.style.as_ref().map(above_mask).unwrap_or(0);
                                style_layers = pdef.style.as_ref().map(layer_count_of).unwrap_or(0);
                                style_reach = pdef
                                    .style
                                    .as_ref()
                                    .map(|s| s.reach_px(scale as f64))
                                    .unwrap_or(0.0);
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
                                    blend: entry.blend(),
                                    blend_constant: entry.blend_constant(),
                                    style_index,
                                    style_layers_only,
                                    sdf_band,
                                    style_layers,
                                    style_above_mask,
                                    style_reach,
                                    vector_stroke,
                                });
                                cursor_x += w;
                            }
                            TextPart::Dynamic(s, _) => {
                                if s.is_empty() {
                                    continue;
                                }
                                let pdef = part.resolve_def(def);
                                // 逐段样式：本段 `TextDef` 覆盖行级（与字号/字体同一套语义）。
                                let (si, lo, band) = match pdef.style.as_ref() {
                                    Some(st) if st.needs_style() => {
                                        let (a, bd) = style_table.register(st, scale as f64);
                                        let (b, _) =
                                            style_table.register_layers_only(st, scale as f64);
                                        (a, b, bd)
                                    }
                                    _ => (0, 0, 0.0),
                                };
                                style_index = si;
                                style_layers_only = lo;
                                sdf_band = band;
                                vector_stroke =
                                    pdef.style.as_ref().and_then(|s| stroke_px(s, scale as f64));
                                style_above_mask = pdef.style.as_ref().map(above_mask).unwrap_or(0);
                                style_layers = pdef.style.as_ref().map(layer_count_of).unwrap_or(0);
                                style_reach = pdef
                                    .style
                                    .as_ref()
                                    .map(|s| s.reach_px(scale as f64))
                                    .unwrap_or(0.0);
                                let opts = TextDef {
                                    max_width: None,
                                    align: TextAlign::Left,
                                    ..pdef.clone()
                                };
                                let key = TypesetKey::from_text(s, &opts);
                                let (area_meta, w) = match text_ctx.peek_shape_slot(&key) {
                                    Some(si) => {
                                        text_ctx.touch_slot(si);
                                        let w = text_ctx.slot_line_width(si);
                                        (
                                            AreaMeta {
                                                buf: MetaBuf::Slot(si),
                                                left: cursor_x * scale,
                                                top,
                                                color,
                                                bounds,
                                                transform_index: phys_idx,
                                                base_uv_rect: batch_base_uv,
                                                texture_state: texture_state.clone(),
                                                blend: entry.blend(),
                                                blend_constant: entry.blend_constant(),
                                                style_index,
                                                style_layers_only,
                                                sdf_band,
                                                style_layers,
                                                style_above_mask,
                                                style_reach,
                                                vector_stroke,
                                            },
                                            w,
                                        )
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
                                            .map(|layout| {
                                                layout.iter().map(|run| run.w).sum::<f32>()
                                            })
                                            .unwrap_or(0.0);
                                        (
                                            AreaMeta {
                                                buf: MetaBuf::Stable(Arc::new(buffer)),
                                                left: cursor_x * scale,
                                                top,
                                                color,
                                                bounds,
                                                transform_index: phys_idx,
                                                base_uv_rect: batch_base_uv,
                                                texture_state: texture_state.clone(),
                                                blend: entry.blend(),
                                                blend_constant: entry.blend_constant(),
                                                style_index,
                                                style_layers_only,
                                                sdf_band,
                                                style_layers,
                                                style_above_mask,
                                                style_reach,
                                                vector_stroke,
                                            },
                                            lw,
                                        )
                                    }
                                };
                                metas.push(area_meta);
                                cursor_x += w;
                            }
                            TextPart::Stable(h) => {
                                for glyph in h.resolved_glyphs.iter() {
                                    metas.push(AreaMeta {
                                        buf: MetaBuf::Resolved {
                                            glyph: glyph.clone(),
                                            block: Some(h.baked.clone()),
                                        },
                                        left: cursor_x * scale,
                                        top,
                                        color,
                                        bounds,
                                        transform_index: phys_idx,
                                        base_uv_rect: batch_base_uv,
                                        texture_state: texture_state.clone(),
                                        blend: entry.blend(),
                                        blend_constant: entry.blend_constant(),
                                        style_index,
                                        style_layers_only,
                                        sdf_band,
                                        style_layers,
                                        style_above_mask,
                                        style_reach,
                                        vector_stroke,
                                    });
                                }
                                cursor_x += h.line_width();
                            }
                            TextPart::Glyphs(s, _) => {
                                let pdef = part.resolve_def(def);
                                // 逐段样式：本段 `TextDef` 覆盖行级（与字号/字体同一套语义）。
                                let (si, lo, band) = match pdef.style.as_ref() {
                                    Some(st) if st.needs_style() => {
                                        let (a, bd) = style_table.register(st, scale as f64);
                                        let (b, _) =
                                            style_table.register_layers_only(st, scale as f64);
                                        (a, b, bd)
                                    }
                                    _ => (0, 0, 0.0),
                                };
                                style_index = si;
                                style_layers_only = lo;
                                sdf_band = band;
                                vector_stroke =
                                    pdef.style.as_ref().and_then(|s| stroke_px(s, scale as f64));
                                style_above_mask = pdef.style.as_ref().map(above_mask).unwrap_or(0);
                                style_layers = pdef.style.as_ref().map(layer_count_of).unwrap_or(0);
                                style_reach = pdef
                                    .style
                                    .as_ref()
                                    .map(|s| s.reach_px(scale as f64))
                                    .unwrap_or(0.0);
                                for ch in s.chars() {
                                    let cluster = text_ctx.resolve_glyph(ch, pdef);
                                    for glyph in cluster.glyphs.iter() {
                                        metas.push(AreaMeta {
                                            buf: MetaBuf::Resolved {
                                                glyph: glyph.clone(),
                                                block: None,
                                            },
                                            left: cursor_x * scale,
                                            top,
                                            color,
                                            bounds,
                                            transform_index: phys_idx,
                                            base_uv_rect: batch_base_uv,
                                            texture_state: texture_state.clone(),
                                            blend: entry.blend(),
                                            blend_constant: entry.blend_constant(),
                                            style_index,
                                            style_layers_only,
                                            sdf_band,
                                            style_layers,
                                            style_above_mask,
                                            style_reach,
                                            vector_stroke,
                                        });
                                    }
                                    cursor_x += cluster.advance;
                                }
                            }
                        }
                    }
                }
                TextEntry::Normal { pos, text, def, .. } => {
                    let key = TypesetKey::from_text(text, def);
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
                        blend: entry.blend(),
                        blend_constant: entry.blend_constant(),
                        style_index,
                        style_layers_only,
                        sdf_band,
                        style_layers,
                        style_above_mask,
                        style_reach,
                        vector_stroke,
                    });
                }
            }
        }

        // 样式表：整块上传（group 1），并按需重建 bind group。无样式时两张表都是空的。
        let style_cache = text_ctx.cache.clone();
        text_ctx.ensure_styles(&gpu.device, &gpu.queue, &style_cache, &style_table);

        let mut segments = Vec::new();
        {
            let TextContext {
                ref mut font_system,
                ref mut swash_cache,
                ref mut text_atlas,
                ref mut text_renderer,
                ref viewport,
                ref typeset_slots,
                ref cache,
                ref base_texture,
                ref base_sampler,
                ..
            } = *text_ctx;

            let mut first = 0;
            while first < metas.len() {
                let generation = metas[first].texture_state.generation;
                let first_blend = metas[first].blend;
                let first_constant = metas[first].blend_constant;
                let mut end = first + 1;
                while end < metas.len()
                    && metas[end].texture_state.generation == generation
                    && metas[end].blend == first_blend
                    && metas[end].blend_constant == first_constant
                {
                    end += 1;
                }

                let vertex_start = text_renderer.glyph_vertex_count();
                let mut run_start = first;
                while run_start < end {
                    let kind = run_kind(&metas[run_start]);
                    let mut run_end = run_start + 1;
                    while run_end < end && run_kind(&metas[run_end]) == kind {
                        run_end += 1;
                    }

                    if kind == RunKind::Baked {
                        // 稳态快路径：整块一条 meta，直接拷实例。
                        for meta in &metas[run_start..run_end] {
                            append_baked_meta(meta, text_renderer);
                        }
                    } else if kind == RunKind::Resolved {
                        // 再按"烘焙块句柄"切分：同一个 StableText 的连续字形一起处理。
                        // 走到这里说明**烘焙失效或还没烘过**，所以一律 prepare 一遍并捕获。
                        let mut sub = run_start;
                        while sub < run_end {
                            let handle = block_of(&metas[sub]);
                            let mut sub_end = sub + 1;
                            while sub_end < run_end
                                && same_handle(&handle, &block_of(&metas[sub_end]))
                            {
                                sub_end += 1;
                            }
                            {
                                let cap_start = text_renderer.glyph_vertex_count();
                                let mut attempt = 0;
                                loop {
                                    let glyphs = metas[sub..sub_end].iter().filter_map(|meta| {
                                        let MetaBuf::Resolved { glyph, .. } = &meta.buf else {
                                            return None;
                                        };
                                        Some(crate::glyphon::ResolvedGlyphArea {
                                            glyph: &glyph.glyph,
                                            line_y: glyph.line_y,
                                            line_top: glyph.line_top,
                                            line_height: glyph.line_height,
                                            left: meta.left,
                                            top: meta.top,
                                            scale,
                                            bounds: meta.bounds,
                                            default_color: meta.color,
                                            transform_index: meta.transform_index,
                                            base_uv_rect: meta.base_uv_rect,
                                            style_index: meta.style_index,
                                            sdf_band: meta.sdf_band,
                                            style_layers: meta.style_layers,
                                            style_above_mask: meta.style_above_mask,
                                            style_reach: meta.style_reach,
                                            vector_stroke: meta.vector_stroke,
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
                                            log::warn!(
                                                "text atlas full after rebuild, skipping segment"
                                            );
                                            break;
                                        }
                                    }
                                }
                                // 捕获刚产生的实例写进块（本次绘制直接用同一批实例）。
                                if let Some(h) = &handle {
                                    let first_meta = &metas[sub];
                                    let key =
                                        bake_key_of(first_meta, scale, text_atlas.layout_epoch());
                                    let entry_color = first_meta.color.0;
                                    // 转成**本地坐标**：减去整数原点。之后任何位置都靠
                                    // "整数位移 + 平移变换槽"表达，实例不再依赖位置。
                                    let ox = first_meta.left.floor() as i32;
                                    let oy = first_meta.top.floor() as i32;
                                    let mut instances =
                                        text_renderer.instances_since(cap_start).to_vec();
                                    // 只把"跟随条目颜色"的实例改成哨兵：几何描边用自己的颜色、
                                    // 逐字覆盖（`color_opt`）也各是各的，复用不能一律覆盖。
                                    for inst in &mut instances {
                                        inst.pos[0] -= ox;
                                        inst.pos[1] -= oy;
                                        if inst.color == entry_color {
                                            inst.color = super::hud::BAKED_FILL_SENTINEL;
                                        }
                                    }
                                    if !instances.is_empty() {
                                        h.store(Some(super::hud::BakedBlock {
                                            origin: (ox, oy),
                                            instances,
                                            key,
                                        }));
                                    }
                                }
                            }
                            sub = sub_end;
                        }
                    } else {
                        let mut attempt = 0;
                        loop {
                            let areas = metas[run_start..run_end].iter().filter_map(|meta| {
                                let buf: &Buffer = match &meta.buf {
                                    MetaBuf::Stable(arc) => arc,
                                    MetaBuf::Slot(si) => {
                                        debug_assert!((*si as usize) < typeset_slots.len());
                                        &typeset_slots[*si as usize].buffer
                                    }
                                    MetaBuf::Resolved { .. } | MetaBuf::Baked { .. } => {
                                        return None;
                                    }
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
                                    style_index: meta.style_index,
                                    style_layers_only: meta.style_layers_only,
                                    sdf_band: meta.sdf_band,
                                    style_layers: meta.style_layers,
                                    style_above_mask: meta.style_above_mask,
                                    style_reach: meta.style_reach,
                                    vector_stroke: meta.vector_stroke,
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
                        blend: metas[first].blend,
                        blend_constant: metas[first].blend_constant,
                    });
                }
                first = end;
            }
        }

        segments
    }
}
