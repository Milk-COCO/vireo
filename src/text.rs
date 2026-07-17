use glyphon::{
    Buffer, Cache, FontSystem, Metrics, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
pub use cosmic_text::{AttrsOwned, Family, FamilyOwned, Style, Weight};
pub use glyphon::ColorMode;
pub use glyphon::Attrs;
use wgpu::{Device, MultisampleState, Queue, TextureFormat};

use crate::color::Color;
use crate::gpu::GpuContext;

pub struct TextContext {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub cache: Cache,
    pub text_atlas: TextAtlas,
    pub text_renderer: TextRenderer,
    pub viewport: Viewport,
}

impl TextContext {
    pub fn new(
        device: &Device,
        queue: &Queue,
        texture_format: TextureFormat,
        color_mode: ColorMode,
    ) -> Self {
        let mut font_system = FontSystem::new();
        font_system.db_mut().load_system_fonts();

        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let mut text_atlas =
            TextAtlas::with_color_mode(device, queue, &cache, texture_format, color_mode);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            device,
            MultisampleState::default(),
            None,
        );
        let viewport = Viewport::new(device, &cache);

        Self {
            font_system,
            swash_cache,
            cache,
            text_atlas,
            text_renderer,
            viewport,
        }
    }
}

/// 文本水平对齐
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    End,
    Justified,
}

impl From<TextAlign> for cosmic_text::Align {
    fn from(a: TextAlign) -> Self {
        match a {
            TextAlign::Left => cosmic_text::Align::Left,
            TextAlign::Center => cosmic_text::Align::Center,
            TextAlign::Right => cosmic_text::Align::Right,
            TextAlign::End => cosmic_text::Align::End,
            TextAlign::Justified => cosmic_text::Align::Justified,
        }
    }
}

/// 文本渲染选项
#[derive(Clone)]
pub struct TextOptions {
    pub x: f32,
    pub y: f32,
    pub font_size: f32,
    pub color: Color,
    /// 最大宽度，超过则换行。None 表示不换行。
    pub max_width: Option<f32>,
    /// 水平对齐。需要配合 max_width 使用才有效果。
    pub align: TextAlign,
    /// 字体属性（family、weight、style 等）。None 使用默认 Attrs。
    pub attrs: Option<AttrsOwned>,
    /// 裁剪区域 (left, top, right, bottom)，物理像素坐标。None 不裁剪。超出区域的文本会被隐藏。
    pub clip: Option<(i32, i32, i32, i32)>,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            font_size: 16.0,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
            max_width: None,
            align: TextAlign::Left,
            attrs: None,
            clip: None,
        }
    }
}

impl TextOptions {
    pub fn x(mut self, x: f32) -> Self {
        self.x = x;
        self
    }

    pub fn y(mut self, y: f32) -> Self {
        self.y = y;
        self
    }

    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn max_width(mut self, w: f32) -> Self {
        self.max_width = Some(w);
        self
    }

    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    pub fn clip(mut self, left: i32, top: i32, right: i32, bottom: i32) -> Self {
        self.clip = Some((left, top, right, bottom));
        self
    }

    pub fn with_family(mut self, family: Family<'_>) -> Self {
        self.attrs.get_or_insert_with(|| AttrsOwned::new(&Attrs::new()))
            .family_owned = FamilyOwned::new(family);
        self
    }

    pub fn with_weight(mut self, weight: Weight) -> Self {
        let attrs = self.attrs.get_or_insert_with(|| AttrsOwned::new(&Attrs::new()));
        attrs.weight = weight;
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        let attrs = self.attrs.get_or_insert_with(|| AttrsOwned::new(&Attrs::new()));
        attrs.style = style;
        self
    }
}

/// 文本条目，pushed by draw_text
#[derive(Clone)]
pub struct TextEntry {
    pub text: String,
    pub options: TextOptions,
}

/// 文本条目列表
pub struct TextEntryList {
    pub entries: Vec<TextEntry>,
}

impl TextEntryList {
    pub fn new() -> Self {
        Self { entries: vec![] }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 从另一个 TextEntryList 复制条目
    pub fn new_from_entries(other: &Self) -> Self {
        Self { entries: other.entries.clone() }
    }

    /// 添加文本条目（用户 API）
    pub fn push(&mut self, text: &str, options: TextOptions) {
        self.entries.push(TextEntry {
            text: text.to_string(),
            options,
        });
    }

    /// 准备文本条目（调用 glyphon prepare），返回 (vertex_start, vertex_count)，
    /// 用于后续 render_range() 分段绘制。多 batch 时逐个调用，最后用返回的范围渲染。
    pub fn prepare_texts(
        &self,
        gpu: &GpuContext,
        physical_width: u32,
        physical_height: u32,
        scale: f32,
    ) -> (u32, u32) {
        if self.entries.is_empty() {
            return (0, 0);
        }

        let mut text_ctx = gpu.text_ctx.borrow_mut();

        text_ctx.viewport.update(
            &gpu.queue,
            glyphon::Resolution {
                width: physical_width,
                height: physical_height,
            },
        );

        let mut buffers: Vec<Buffer> = vec![];

        for entry in &self.entries {
            let line_height = entry.options.font_size * 1.2;
            let metrics = Metrics::new(entry.options.font_size, line_height);
            let mut buffer = Buffer::new(&mut text_ctx.font_system, metrics);
            buffer.set_size(entry.options.max_width, None);

            let attrs = entry.options.attrs.as_ref()
                .map(|a| a.as_attrs())
                .unwrap_or_else(Attrs::new);

            buffer.set_text(
                &entry.text,
                &attrs,
                Shaping::Advanced,
                Some(entry.options.align.into()),
            );
            buffer.shape_until_scroll(&mut text_ctx.font_system, false);
            buffers.push(buffer);
        }

        let areas: Vec<TextArea> = buffers
            .iter()
            .enumerate()
            .map(|(i, buf)| {
                let entry = &self.entries[i];
                let color = glyphon::Color::rgba(
                    (entry.options.color.r * 255.0) as u8,
                    (entry.options.color.g * 255.0) as u8,
                    (entry.options.color.b * 255.0) as u8,
                    (entry.options.color.a * 255.0) as u8,
                );
                let bounds = match entry.options.clip {
                    Some((l, t, r, b)) => TextBounds { left: l, top: t, right: r, bottom: b },
                    None => TextBounds::default(),
                };
                TextArea {
                    buffer: buf,
                    left: entry.options.x * scale,
                    top: entry.options.y * scale,
                    scale,
                    bounds,
                    default_color: color,
                    custom_glyphs: &[],
                }
            })
            .collect();

        let vertex_start;
        let vertex_count;

        {
            vertex_start = text_ctx.text_renderer.glyph_vertex_count();

            let TextContext {
                ref mut font_system,
                ref mut swash_cache,
                ref mut text_atlas,
                ref mut text_renderer,
                ref viewport,
                ..
            } = *text_ctx;

            text_renderer.prepare(
                &gpu.device,
                &gpu.queue,
                font_system,
                text_atlas,
                viewport,
                areas,
                swash_cache,
            ).expect("glyphon prepare failed");

            vertex_count = text_renderer.glyph_vertex_count() - vertex_start;
        }

        (vertex_start, vertex_count)
    }

    /// prepare + render 所有文本条目到 render pass（单 batch 便利方法）
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
        let _ = self.prepare_texts(gpu, physical_width, physical_height, scale);
        let text_ctx = gpu.text_ctx.borrow();
        text_ctx.text_renderer.render(
            &text_ctx.text_atlas,
            &text_ctx.viewport,
            render_pass,
        ).expect("glyphon render failed");
    }
}

/// 单个字形的渲染四边形
#[derive(Debug, Clone, Copy)]
pub struct GlyphQuad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// 文本排版结果
pub struct TextLayout {
    pub quads: Vec<GlyphQuad>,
    /// 字形图集纹理的 bind group（指向 glyphon 的 atlas）
    pub atlas_bind_group: wgpu::BindGroup,
}

/// 往 batch.texts 中添加一条文本渲染指令
pub fn draw_text(list: &mut TextEntryList, text: &str, options: TextOptions) {
    list.push(text, options);
}
