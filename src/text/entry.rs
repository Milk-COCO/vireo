use std::sync::Arc;

use crate::color::Color;
use crate::glyphon::Buffer;
use crate::render::Pos;
use crate::gpu::GpuContext;

use crate::text::{
    TextDef, TextOverride, TextPart, TextTextureState, StableText, ResolvedTextGlyph,
};
use super::split_hud;

/// 文本条目——三种变体，互斥字段不混存。
///
/// 各变体均含 `texture_state`：入队时从 [`TextEntryList`] 画笔 clone，
/// 供材质 `vireo_base_sample(in.base_uv)` 与分段绑定使用。
#[derive(Clone, Debug)]
pub enum TextEntry {
    Normal {
        text: String,
        pos: Pos,
        def: TextDef,
        override_: TextOverride,
        transform_index: u32,
        /// 入队时冻结的 batch 贴图状态。
        texture_state: TextTextureState,
    },
    Parts {
        pos: Pos,
        def: TextDef,
        parts: Vec<TextPart>,
        override_: TextOverride,
        transform_index: u32,
        /// 入队时冻结的 batch 贴图状态。
        texture_state: TextTextureState,
    },
    Stable {
        pos: Pos,
        override_: TextOverride,
        transform_index: u32,
        buffer: Arc<Buffer>,
        #[doc(hidden)]
        resolved_glyphs: Arc<[Arc<ResolvedTextGlyph>]>,
        font_size: f32,
        line_width: f32,
        line_count: u32,
        /// 入队时冻结的 batch 贴图状态。
        texture_state: TextTextureState,
    },
}

// 公共字段访问器，避免调用方 match
impl TextEntry {
    pub fn override_(&self) -> &TextOverride {
        match self {
            TextEntry::Normal { override_, .. }
            | TextEntry::Parts { override_, .. }
            | TextEntry::Stable { override_, .. } => override_,
        }
    }
    pub fn transform_index(&self) -> u32 {
        match self {
            TextEntry::Normal { transform_index, .. }
            | TextEntry::Parts { transform_index, .. }
            | TextEntry::Stable { transform_index, .. } => *transform_index,
        }
    }
    pub fn pos(&self) -> Pos {
        match self {
            TextEntry::Normal { pos, .. }
            | TextEntry::Parts { pos, .. }
            | TextEntry::Stable { pos, .. } => *pos,
        }
    }

    /// 该条目入队时冻结的 batch 贴图状态（只读）。
    ///
    /// 与后续 `DrawBatch::set_texture` / `set_uv` 无关；仅反映 push 瞬间的画笔。
    pub fn texture_state(&self) -> &TextTextureState {
        match self {
            TextEntry::Normal { texture_state, .. }
            | TextEntry::Parts { texture_state, .. }
            | TextEntry::Stable { texture_state, .. } => texture_state,
        }
    }

    /// 裁剪/culling 用：近似字号。
    pub(crate) fn approx_font_size(&self) -> f32 {
        match self {
            TextEntry::Normal { def, .. } => def.font_size,
            TextEntry::Parts { parts, def, .. } => parts.iter().fold(def.font_size, |max, part| {
                let size = match part {
                    TextPart::Normal(_, d) | TextPart::Dynamic(_, d) | TextPart::Glyphs(_, d) => {
                        d.as_ref().map(|d| d.font_size).unwrap_or(def.font_size)
                    }
                    TextPart::Stable(stable) => stable.font_size(),
                };
                max.max(size)
            }),
            TextEntry::Stable { font_size, .. } => *font_size,
        }
    }

    /// 裁剪/culling 用：近似逻辑宽度。
    /// - `Normal`：`max_width` 参与换行，用 `max_width` 宽度（单行最大宽）；未设则按字符估算。
    /// - `Parts`：`get_or_shape_text` 强制 `max_width=None`（单行 LTR 横拼，不换行），
    ///   所以 `def.max_width` 即使设置了也不会生效；按段长估算更准确。
    /// - `Stable`：构造时已记录 `line_width`。
    pub(crate) fn approx_width(&self) -> f32 {
        match self {
            TextEntry::Normal { text, def, .. } => {
                let fs = def.font_size;
                def.max_width
                    .unwrap_or_else(|| (text.chars().count() as f32) * fs * 0.6)
            }
            TextEntry::Parts { parts, def, .. } => {
                let mut w = 0.0f32;
                for p in parts {
                    match p {
                        TextPart::Normal(s, d) | TextPart::Dynamic(s, d) | TextPart::Glyphs(s, d) => {
                            let fs = d.as_ref().map(|x| x.font_size).unwrap_or(def.font_size);
                            w += s.chars().count() as f32 * fs * 0.6;
                        }
                        TextPart::Stable(h) => w += h.line_width(),
                    }
                }
                w
            }
            TextEntry::Stable { line_width, .. } => *line_width,
        }
    }

    /// 裁剪/culling 用：估算行数。
    /// - `Normal` 文本：按 `max_width` 折行（≥1）。
    /// - `Parts`：单行 LTR 横拼，永远 1。
    /// - `Stable`：使用 layout_runs 数出的实际行数。
    pub(crate) fn approx_line_count(&self) -> u32 {
        match self {
            TextEntry::Normal { text, def, .. } => {
                let max_w = match def.max_width {
                    Some(w) if w > 0.0 => w,
                    _ => return 1,
                };
                let natural = (text.chars().count() as f32) * def.font_size * 0.6;
                let lines = (natural / max_w).ceil() as u32;
                lines.max(1)
            }
            TextEntry::Parts { .. } => 1,
            TextEntry::Stable { line_count, .. } => (*line_count).max(1),
        }
    }
}

/// 文本条目列表，存储一组待渲染的文本。
///
/// 通过 `draw_text(&mut list, …)` / `push` 等添加条目；
/// 经 [`DrawBatch`] 时优先用 `batch.text` 等以捕获 transform。
///
/// 内部维护文字画笔 [`TextTextureState`]：由 batch 的 `set_texture` / `set_uv`
/// 更新，在每次 push 时冻结到条目。
#[derive(Clone)]
pub struct TextEntryList {
    pub entries: Vec<TextEntry>,
    pub(crate) texture_state: TextTextureState,
}

impl TextEntryList {
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(8),
            texture_state: TextTextureState::default(),
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.texture_state = TextTextureState::default();
    }

    /// 从另一个 TextEntryList 复制条目
    pub fn new_from_entries(other: &Self) -> Self {
        Self {
            entries: other.entries.clone(),
            texture_state: other.texture_state.clone(),
        }
    }

    /// 更新当前文字画笔的 batch 贴图（由 [`DrawBatch::set_texture`] / [`DrawBatch::set_bind_group`] 调用）。
    /// 递增 `generation`；之后 `push*` 的条目会冻结新状态。
    pub(crate) fn set_texture_state(&mut self, view: Option<wgpu::TextureView>) {
        self.texture_state.generation = self.texture_state.generation.wrapping_add(1);
        self.texture_state.view = view;
        self.texture_state.bind_group = None;
    }

    /// 更新当前文字画笔的 bind group（`TextOverride` 的 `bind_group` 共享位）。
    /// 递增 `generation`；之后 `push*` 的条目会冻结新状态。
    pub(crate) fn set_bind_group_state(&mut self, bg: Option<wgpu::BindGroup>) {
        self.texture_state.generation = self.texture_state.generation.wrapping_add(1);
        self.texture_state.bind_group = bg;
        self.texture_state.view = None;
    }

    /// 更新当前文字画笔的 UV 子区域（由 [`DrawBatch::set_uv`] / [`DrawBatch::clear_uv`] 调用）。
    /// 递增 `generation`；之后 `push*` 的条目会冻结新状态。
    pub(crate) fn set_uv_state(&mut self, uv: crate::render::UvRect) {
        self.texture_state.generation = self.texture_state.generation.wrapping_add(1);
        self.texture_state.uv = uv;
    }

    /// 添加文本条目。
    ///
    /// **默认 `transform_index = 0`**：约定为 batch / 全局 transform 表的**单位矩阵槽**
    ///（见 `DrawBatch::transform_table` 文档）。`pos` 为逻辑世界坐标（再 × scale → 物理 left/top）。
    /// 若需随 batch 画笔变换，用 `DrawBatch::text`（捕获 `current_transform_index`）。
    pub fn push(&mut self, text: &str, pos: Pos, def: TextDef, ov: TextOverride) {
        self.entries.push(TextEntry::Normal {
            text: text.to_string(),
            pos,
            def,
            override_: ov,
            transform_index: 0,
            texture_state: self.texture_state.clone(),
        });
    }

    /// 添加文本条目并指定 transform index。
    pub(crate) fn push_indexed(
        &mut self,
        text: &str,
        pos: Pos,
        def: TextDef,
        ov: TextOverride,
        transform_index: u32,
    ) {
        self.entries.push(TextEntry::Normal {
            text: text.to_string(),
            pos,
            def,
            override_: ov,
            transform_index,
            texture_state: self.texture_state.clone(),
        });
    }

    /// 使用预 shape 的 [`StableText`] 直接添加条目（跳过 cache 查询）。
    /// `def` 已在 `make_stable_text` 时定型，此处不需要。
    pub fn push_stable(&mut self, stable: &StableText, pos: Pos, ov: TextOverride) {
        self.push_stable_indexed(stable, pos, ov, 0);
    }

    pub(crate) fn push_stable_indexed(
        &mut self,
        stable: &StableText,
        pos: Pos,
        ov: TextOverride,
        transform_index: u32,
    ) {
        self.entries.push(TextEntry::Stable {
            pos,
            override_: ov,
            transform_index,
            buffer: stable.buffer.clone(),
            resolved_glyphs: stable.resolved_glyphs.clone(),
            font_size: stable.font_size,
            line_width: stable.line_width,
            line_count: stable.line_count,
            texture_state: self.texture_state.clone(),
        });
    }

    /// HUD 多段文字（默认无 transform）。`parts` 切片 clone 进 [`TextEntry::Parts`]。
    pub fn push_parts(&mut self, parts: &[TextPart], pos: Pos, def: TextDef, ov: TextOverride) {
        self.push_parts_indexed(parts, pos, def, ov, 0);
    }

    pub(crate) fn push_parts_indexed(
        &mut self,
        parts: &[TextPart],
        pos: Pos,
        def: TextDef,
        ov: TextOverride,
        transform_index: u32,
    ) {
        if parts.is_empty() {
            return;
        }
        self.entries.push(TextEntry::Parts {
            pos,
            def,
            parts: parts.to_vec(),
            override_: ov,
            transform_index,
            texture_state: self.texture_state.clone(),
        });
    }

    /// 按 [`split_hud`] 规则自动切分后加入（默认无 transform）。
    pub fn push_hud(&mut self, text: &str, pos: Pos, def: TextDef, ov: TextOverride) {
        self.push_hud_indexed(text, pos, def, ov, 0);
    }

    pub(crate) fn push_hud_indexed(
        &mut self,
        text: &str,
        pos: Pos,
        def: TextDef,
        ov: TextOverride,
        transform_index: u32,
    ) {
        let parts = split_hud(text);
        self.push_parts_indexed(&parts, pos, def, ov, transform_index);
    }

    /// prepare + render 所有文本条目到 render pass（单 batch 便利方法）。
    /// 不使用 transform（所有文字用恒等矩阵）。
    ///
    /// 不变量：本方法内的 prepare 与 render 之间，以及调用方 `Renderer::draw` 的整个执行期间，
    /// 不得改动 `text_ctx.viewport` 或 `text_ctx.text_atlas`（也不要响应窗口 resize）。
    /// 否则 glyphon 会返回 `RenderError::ScreenResolutionChanged` / `RemovedFromAtlas`，
    /// 该文字段的渲染将失败（此处按跳过 + 日志处理，不会 panic）。
    pub fn draw(
        &self,
        gpu: &GpuContext,
        render_pass: &mut wgpu::RenderPass<'_>,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
    ) {
        if self.entries.is_empty() {
            return;
        }
        let mut global_transforms = Vec::new();
        let _ = self.prepare_texts(
            gpu,
            physical_width,
            physical_height,
            scale,
            &[],
            &mut global_transforms,
            None,
            Color::new(1.0, 1.0, 1.0, 1.0),
        );
        let text_ctx = gpu.text_ctx.lock().unwrap();
        if let Err(e) = text_ctx
            .text_renderer
            .render(
                &text_ctx.text_atlas,
                &text_ctx.viewport,
                render_pass,
                &gpu.engine_storage_dummy_bind_group,
            )
        {
            log::warn!("glyphon text render failed (skipped this frame): {:?}", e);
        }
    }
}
