use super::{
    custom_glyph::CustomGlyphCacheKey, ColorMode, ContentType, FontSystem, GlyphDetails,
    GlyphToRender, GpuCacheStatus, PrepareError, RasterizeCustomGlyphRequest,
    RasterizedCustomGlyph, RenderError, ResolvedGlyphArea, State, SwashCache, SwashContent,
    TextArea, TextAtlas, Viewport,
};
use cosmic_text::{Color, SubpixelBin};
use std::slice;
use wgpu::{
    BindGroup, Buffer, BufferDescriptor, BufferUsages, DepthStencilState, Device, Extent3d,
    MultisampleState, Origin3d, Queue, RenderPass, RenderPipeline, TexelCopyBufferLayout,
    TexelCopyTextureInfo, TextureAspect, COPY_BUFFER_ALIGNMENT,
};

/// A text renderer that uses cached glyphs to render text into an existing render pass.
pub struct TextRenderer {
    vertex_buffer: Buffer,
    vertex_buffer_size: u64,
    pipeline: RenderPipeline,
    glyph_vertices: Vec<GlyphToRender>,
    defer_upload: bool,
}

impl TextRenderer {
    /// Creates a new `TextRenderer`.
    pub fn new(
        atlas: &mut TextAtlas,
        device: &Device,
        multisample: MultisampleState,
        depth_stencil: Option<DepthStencilState>,
    ) -> Self {
        let vertex_buffer_size = next_copy_buffer_size(4096);
        let vertex_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("glyphon vertices"),
            size: vertex_buffer_size,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pipeline = atlas.get_or_create_pipeline(device, multisample, depth_stencil);

        Self {
            vertex_buffer,
            vertex_buffer_size,
            pipeline,
            glyph_vertices: Vec::new(),
            defer_upload: false,
        }
    }

    /// Prepares all of the provided text areas for rendering.
    pub fn prepare<'a>(
        &mut self,
        device: &Device,
        queue: &Queue,
        font_system: &mut FontSystem,
        atlas: &mut TextAtlas,
        viewport: &Viewport,
        text_areas: impl IntoIterator<Item = TextArea<'a>>,
        cache: &mut SwashCache,
    ) -> Result<(), PrepareError> {
        self.prepare_with_depth_and_custom(
            device,
            queue,
            font_system,
            atlas,
            viewport,
            text_areas,
            cache,
            zero_depth,
            |_| None,
        )
    }

    /// Prepares all of the provided text areas for rendering.
    pub fn prepare_with_depth<'a>(
        &mut self,
        device: &Device,
        queue: &Queue,
        font_system: &mut FontSystem,
        atlas: &mut TextAtlas,
        viewport: &Viewport,
        text_areas: impl IntoIterator<Item = TextArea<'a>>,
        cache: &mut SwashCache,
        metadata_to_depth: impl FnMut(usize) -> f32,
    ) -> Result<(), PrepareError> {
        self.prepare_with_depth_and_custom(
            device,
            queue,
            font_system,
            atlas,
            viewport,
            text_areas,
            cache,
            metadata_to_depth,
            |_| None,
        )
    }

    /// Prepares all of the provided text areas for rendering.
    pub fn prepare_with_custom<'a>(
        &mut self,
        device: &Device,
        queue: &Queue,
        font_system: &mut FontSystem,
        atlas: &mut TextAtlas,
        viewport: &Viewport,
        text_areas: impl IntoIterator<Item = TextArea<'a>>,
        cache: &mut SwashCache,
        rasterize_custom_glyph: impl FnMut(RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph>,
    ) -> Result<(), PrepareError> {
        self.prepare_with_depth_and_custom(
            device,
            queue,
            font_system,
            atlas,
            viewport,
            text_areas,
            cache,
            zero_depth,
            rasterize_custom_glyph,
        )
    }

    /// Prepares all of the provided text areas for rendering.
    pub fn prepare_with_depth_and_custom<'a>(
        &mut self,
        device: &Device,
        queue: &Queue,
        font_system: &mut FontSystem,
        atlas: &mut TextAtlas,
        viewport: &Viewport,
        text_areas: impl IntoIterator<Item = TextArea<'a>>,
        cache: &mut SwashCache,
        mut metadata_to_depth: impl FnMut(usize) -> f32,
        mut rasterize_custom_glyph: impl FnMut(
            RasterizeCustomGlyphRequest,
        ) -> Option<RasterizedCustomGlyph>,
    ) -> Result<(), PrepareError> {
        let state = State { device, queue };
        let mut system = GlyphSystem {
            atlas,
            cache,
            font_system,
        };
        let resolution = viewport.resolution();

        for text_area in text_areas {
            // 恒等 transform：顶点坐标即屏幕物理像素，可裁到视口。
            // 非恒等：VS 才乘 transform（如 batch 父平移），此处仍是局部坐标；
            // 若再夹到 [0,res]，圆心附近 y<0 的字体会被切掉上半（text_batch_clip）。
            let bounds = if text_area.transform_index != 0 {
                GlyphBounds {
                    x: Bounds {
                        min: text_area.bounds.left,
                        max: text_area.bounds.right,
                    },
                    y: Bounds {
                        min: text_area.bounds.top,
                        max: text_area.bounds.bottom,
                    },
                }
            } else {
                let x_min = text_area.bounds.left.max(0);
                let y_min = text_area.bounds.top.max(0);
                GlyphBounds {
                    x: Bounds {
                        min: x_min,
                        max: text_area
                            .bounds
                            .right
                            .min(resolution.width as i32)
                            .max(x_min),
                    },
                    y: Bounds {
                        min: y_min,
                        max: text_area
                            .bounds
                            .bottom
                            .min(resolution.height as i32)
                            .max(y_min),
                    },
                }
            };

            for glyph in text_area.custom_glyphs.iter() {
                let x = text_area.left + (glyph.left * text_area.scale);
                let y = text_area.top + (glyph.top * text_area.scale);
                let width = (glyph.width * text_area.scale).round() as u16;
                let height = (glyph.height * text_area.scale).round() as u16;

                let (x, y, x_bin, y_bin) = if glyph.snap_to_physical_pixel {
                    (
                        x.round() as i32,
                        y.round() as i32,
                        SubpixelBin::Zero,
                        SubpixelBin::Zero,
                    )
                } else {
                    let (x, x_bin) = SubpixelBin::new(x);
                    let (y, y_bin) = SubpixelBin::new(y);
                    (x, y, x_bin, y_bin)
                };

                let cache_key = GlyphonCacheKey::Custom(CustomGlyphCacheKey {
                    glyph_id: glyph.id,
                    width,
                    height,
                    x_bin,
                    y_bin,
                });

                let color = glyph.color.unwrap_or(text_area.default_color);

                if let Some(glyph_to_render) = append_custom_glyph(
                    &state,
                    &mut system,
                    x,
                    y,
                    text_area.scale,
                    width,
                    height,
                    x_bin,
                    y_bin,
                    color,
                    glyph.metadata,
                    cache_key,
                    text_area.transform_index,
                    text_area.base_uv_rect,
                    bounds,
                    glyph.id,
                    &mut rasterize_custom_glyph,
                )? {
                    self.glyph_vertices.push(glyph_to_render);
                }
            }

            let is_run_visible = |run: &cosmic_text::LayoutRun| {
                let start_y_physical = (text_area.top + (run.line_top * text_area.scale)) as i32;
                let end_y_physical = start_y_physical + (run.line_height * text_area.scale) as i32;

                start_y_physical <= text_area.bounds.bottom
                    && text_area.bounds.top <= end_y_physical
            };

            let layout_runs = text_area
                .buffer
                .layout_runs()
                .skip_while(|run| !is_run_visible(run))
                .take_while(is_run_visible);

            for run in layout_runs {
                for glyph in run.glyphs.iter() {
                    if let Some(glyph_to_render) = append_text_glyph(
                        &state,
                        &mut system,
                        glyph,
                        run.line_y,
                        text_area.left,
                        text_area.top,
                        text_area.scale,
                        bounds,
                        text_area.default_color,
                        text_area.transform_index,
                        text_area.base_uv_rect,
                        &mut metadata_to_depth,
                        &mut rasterize_custom_glyph,
                    )? {
                        self.glyph_vertices.push(glyph_to_render);
                    }
                }
            }
        }

        if !self.defer_upload {
            self.upload(device, queue);
        }
        Ok(())
    }

    /// Prepare already-shaped glyphs directly, bypassing `TextArea` and
    /// `Buffer::layout_runs()`. Atlas lookup, rasterization, clipping and
    /// physical/subpixel positioning remain identical to the normal path.
    pub(crate) fn prepare_resolved_glyphs<'a>(
        &mut self,
        device: &Device,
        queue: &Queue,
        font_system: &mut FontSystem,
        atlas: &mut TextAtlas,
        viewport: &Viewport,
        glyphs: impl IntoIterator<Item = ResolvedGlyphArea<'a>>,
        cache: &mut SwashCache,
    ) -> Result<(), PrepareError> {
        let state = State { device, queue };
        let mut system = GlyphSystem { atlas, cache, font_system };
        let resolution = viewport.resolution();

        for area in glyphs {
            let start_y = (area.top + area.line_top * area.scale) as i32;
            let end_y = start_y + (area.line_height * area.scale) as i32;
            if start_y > area.bounds.bottom || area.bounds.top > end_y {
                continue;
            }
            let bounds = glyph_bounds(area.bounds, area.transform_index, resolution);
            if let Some(glyph_to_render) = append_text_glyph(
                &state,
                &mut system,
                area.glyph,
                area.line_y,
                area.left,
                area.top,
                area.scale,
                bounds,
                area.default_color,
                area.transform_index,
                area.base_uv_rect,
                zero_depth,
                |_| None,
            )? {
                self.glyph_vertices.push(glyph_to_render);
            }
        }

        if !self.defer_upload {
            self.upload(device, queue);
        }
        Ok(())
    }

    /// Upload all instances accumulated since [`clear`](Self::clear) in one
    /// write. Vireo prepares multiple interleaved text ranges per frame.
    pub(crate) fn upload(&mut self, device: &Device, queue: &Queue) {
        if self.glyph_vertices.is_empty() {
            return;
        }
        let all_raw = unsafe {
            slice::from_raw_parts(
                self.glyph_vertices.as_ptr() as *const u8,
                std::mem::size_of_val(self.glyph_vertices.as_slice()),
            )
        };
        let required_size = all_raw.len() as u64;

        if self.vertex_buffer_size < required_size {
            self.vertex_buffer.destroy();
            let (buffer, buffer_size) = create_oversized_buffer(
                device,
                Some("glyphon vertices"),
                all_raw,
                BufferUsages::VERTEX | BufferUsages::COPY_DST,
            );
            self.vertex_buffer = buffer;
            self.vertex_buffer_size = buffer_size;
        } else {
            queue.write_buffer(&self.vertex_buffer, 0, all_raw);
        }
    }

    /// Start Vireo's multi-range frame preparation. Instances are accumulated
    /// without intermediate buffer writes until [`finish_frame`](Self::finish_frame).
    pub(crate) fn begin_frame(&mut self) {
        self.glyph_vertices.clear();
        self.defer_upload = true;
    }

    pub(crate) fn finish_frame(&mut self, device: &Device, queue: &Queue) {
        self.upload(device, queue);
        self.defer_upload = false;
    }

    /// Clears all prepared glyph vertices. Call once per frame before the first `prepare()`.
    pub fn clear(&mut self) {
        self.glyph_vertices.clear();
    }

    /// Vireo 扩展：替换 pipeline（用于匹配 depth/stencil 状态）。
    pub fn set_pipeline(&mut self, pipeline: RenderPipeline) {
        self.pipeline = pipeline;
    }

    /// Returns the number of prepared glyph vertices.
    pub fn glyph_vertex_count(&self) -> u32 {
        self.glyph_vertices.len() as u32
    }

    /// Renders all layouts that were previously provided to `prepare`.
    pub fn render(
        &self,
        atlas: &TextAtlas,
        viewport: &Viewport,
        pass: &mut RenderPass<'_>,
        transform_bind_group: &BindGroup,
    ) -> Result<(), RenderError> {
        self.render_range(atlas, viewport, pass, transform_bind_group, 0, self.glyph_vertices.len() as u32)
    }

    /// Renders a sub-range of prepared glyph vertices. Use after multiple `prepare()` calls
    /// to draw specific text layers interleaved with shapes.
    pub fn render_range(
        &self,
        atlas: &TextAtlas,
        viewport: &Viewport,
        pass: &mut RenderPass<'_>,
        transform_bind_group: &BindGroup,
        vertex_start: u32,
        vertex_count: u32,
    ) -> Result<(), RenderError> {
        self.render_range_with_stencil_ref(
            atlas,
            viewport,
            pass,
            transform_bind_group,
            vertex_start,
            vertex_count,
            None,
        )
    }

    /// 同 [`render_range`]，但在 `set_pipeline` 之后设置 stencil reference（避免被重置）。
    pub fn render_range_with_stencil_ref(
        &self,
        atlas: &TextAtlas,
        viewport: &Viewport,
        pass: &mut RenderPass<'_>,
        transform_bind_group: &BindGroup,
        vertex_start: u32,
        vertex_count: u32,
        stencil_ref: Option<u32>,
    ) -> Result<(), RenderError> {
        self.render_range_with_material(
            atlas,
            viewport,
            pass,
            transform_bind_group,
            vertex_start,
            vertex_count,
            stencil_ref,
            None,
            None,
            None,
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_range_with_material(
        &self,
        atlas: &TextAtlas,
        viewport: &Viewport,
        pass: &mut RenderPass<'_>,
        transform_bind_group: &BindGroup,
        vertex_start: u32,
        vertex_count: u32,
        stencil_ref: Option<u32>,
        atlas_bind_group: Option<&BindGroup>,
        material_pipeline: Option<&RenderPipeline>,
        material_bind_group: Option<&BindGroup>,
        material_offsets: &[u32],
    ) -> Result<(), RenderError> {
        if vertex_count == 0 {
            return Ok(());
        }

        let byte_offset = vertex_start as u64 * std::mem::size_of::<GlyphToRender>() as u64;
        pass.set_pipeline(material_pipeline.unwrap_or(&self.pipeline));
        if let Some(r) = stencil_ref {
            pass.set_stencil_reference(r);
        }
        pass.set_bind_group(0, atlas_bind_group.unwrap_or(&atlas.bind_group), &[]);
        pass.set_bind_group(1, &viewport.bind_group, &[]);
        pass.set_bind_group(2, transform_bind_group, &[]);
        if let Some(bind_group) = material_bind_group {
            pass.set_bind_group(3, bind_group, material_offsets);
        }
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(byte_offset..));
        pass.draw(0..4, 0..vertex_count);

        Ok(())
    }
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum TextColorConversion {
    None = 0,
    ConvertToLinear = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum GlyphonCacheKey {
    Text(cosmic_text::CacheKey),
    Custom(CustomGlyphCacheKey),
}

fn next_copy_buffer_size(size: u64) -> u64 {
    let align_mask = COPY_BUFFER_ALIGNMENT - 1;
    ((size.next_power_of_two() + align_mask) & !align_mask).max(COPY_BUFFER_ALIGNMENT)
}

fn create_oversized_buffer(
    device: &Device,
    label: Option<&str>,
    contents: &[u8],
    usage: BufferUsages,
) -> (Buffer, u64) {
    let size = next_copy_buffer_size(contents.len() as u64);
    let buffer = device.create_buffer(&BufferDescriptor {
        label,
        size,
        usage,
        mapped_at_creation: true,
    });
    buffer
        .slice(..contents.len() as u64)
        .get_mapped_range_mut()
        .unwrap()
        .copy_from_slice(contents);
    buffer.unmap();
    (buffer, size)
}

fn zero_depth(_: usize) -> f32 {
    0f32
}

struct GetGlyphImageResult {
    content_type: ContentType,
    top: i16,
    left: i16,
    width: u16,
    height: u16,
    data: Vec<u8>,
}

struct GlyphMetadata {
    x: i32,
    y: i32,
    line_y: f32,
    scale_factor: f32,
    color: Color,
    metadata: usize,
    cache_key: GlyphonCacheKey,
    transform_index: u32,
    base_uv_rect: [f32; 4],
}

#[derive(Clone, Copy)]
struct Bounds {
    min: i32,
    max: i32,
}

#[derive(Clone, Copy)]
struct GlyphBounds {
    x: Bounds,
    y: Bounds,
}

struct GlyphSystem<'a> {
    atlas: &'a mut TextAtlas,
    cache: &'a mut SwashCache,
    font_system: &'a mut FontSystem,
}

fn prepare_glyph<R>(
    state: &State,
    system: &mut GlyphSystem,
    metadata: GlyphMetadata,
    bounds: GlyphBounds,
    get_glyph_image: impl FnOnce(&mut GlyphSystem, &mut R) -> Option<GetGlyphImageResult>,
    mut metadata_to_depth: impl FnMut(usize) -> f32,
    mut rasterize_custom_glyph: R,
) -> Result<Option<GlyphToRender>, PrepareError>
where
    R: FnMut(RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph>,
{
    let mask_generation = system.atlas.mask_atlas.generation;
    let color_generation = system.atlas.color_atlas.generation;

    let details = if let Some(details) = system
        .atlas
        .mask_atlas
        .glyph_cache
        .get_mut(&metadata.cache_key)
    {
        details.last_used = mask_generation;
        details
    } else if let Some(details) = system
        .atlas
        .color_atlas
        .glyph_cache
        .get_mut(&metadata.cache_key)
    {
        details.last_used = color_generation;
        details
    } else {
        let Some(image) = (get_glyph_image)(system, &mut rasterize_custom_glyph) else {
            return Ok(None);
        };

        let should_rasterize = image.width > 0 && image.height > 0;

        let (gpu_cache, atlas_id, inner) = if should_rasterize {
            let mut inner = system.atlas.inner_for_content_mut(image.content_type);

            // Find a position in the packer
            let allocation = loop {
                match inner.try_allocate(image.width as usize, image.height as usize) {
                    Some(a) => break a,
                    None => {
                        if !system.atlas.grow(
                            state,
                            system.font_system,
                            system.cache,
                            image.content_type,
                            metadata.scale_factor,
                            &mut rasterize_custom_glyph,
                        ) {
                            return Err(PrepareError::AtlasFull);
                        }

                        inner = system.atlas.inner_for_content_mut(image.content_type);
                    }
                }
            };
            let atlas_min = allocation.rectangle.min;

            state.queue.write_texture(
                TexelCopyTextureInfo {
                    texture: &inner.texture,
                    mip_level: 0,
                    origin: Origin3d {
                        x: atlas_min.x as u32,
                        y: atlas_min.y as u32,
                        z: 0,
                    },
                    aspect: TextureAspect::All,
                },
                &image.data,
                TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width as u32 * inner.num_channels() as u32),
                    rows_per_image: None,
                },
                Extent3d {
                    width: image.width as u32,
                    height: image.height as u32,
                    depth_or_array_layers: 1,
                },
            );

            (
                GpuCacheStatus::InAtlas {
                    x: atlas_min.x as u16,
                    y: atlas_min.y as u16,
                    content_type: image.content_type,
                },
                Some(allocation.id),
                inner,
            )
        } else {
            let inner = &mut system.atlas.color_atlas;
            (GpuCacheStatus::SkipRasterization, None, inner)
        };

        let generation = inner.generation;
        // Insert the glyph into the cache and return the details reference
        inner
            .glyph_cache
            .get_or_insert(metadata.cache_key, || GlyphDetails {
                width: image.width,
                height: image.height,
                gpu_cache,
                atlas_id,
                top: image.top,
                left: image.left,
                last_used: generation,
            })
    };

    let mut x = metadata.x + details.left as i32;
    let mut y =
        (metadata.line_y * metadata.scale_factor).round() as i32 + metadata.y - details.top as i32;

    let (mut atlas_x, mut atlas_y, content_type) = match details.gpu_cache {
        GpuCacheStatus::InAtlas { x, y, content_type } => (x, y, content_type),
        GpuCacheStatus::SkipRasterization => return Ok(None),
    };

    let mut width = details.width as i32;
    let mut height = details.height as i32;

    // Starts beyond right edge or ends beyond left edge
    let max_x = x + width;
    if x > bounds.x.max || max_x < bounds.x.min {
        return Ok(None);
    }

    // Starts beyond bottom edge or ends beyond top edge
    let max_y = y + height;
    if y > bounds.y.max || max_y < bounds.y.min {
        return Ok(None);
    }

    // Clip left ege
    if x < bounds.x.min {
        let right_shift = bounds.x.min - x;

        x = bounds.x.min;
        width = max_x - bounds.x.min;
        atlas_x += right_shift as u16;
    }

    // Clip right edge
    if x + width > bounds.x.max {
        width = bounds.x.max - x;
    }

    // Clip top edge
    if y < bounds.y.min {
        let bottom_shift = bounds.y.min - y;

        y = bounds.y.min;
        height = max_y - bounds.y.min;
        atlas_y += bottom_shift as u16;
    }

    // Clip bottom edge
    if y + height > bounds.y.max {
        height = bounds.y.max - y;
    }

    let depth = metadata_to_depth(metadata.metadata);

    Ok(Some(GlyphToRender {
        pos: [x, y],
        dim: [width as u16, height as u16],
        uv: [atlas_x, atlas_y],
        color: metadata.color.0,
        content_type_with_srgb: [
            content_type as u16,
            match system.atlas.color_mode {
                ColorMode::Accurate => TextColorConversion::ConvertToLinear,
                ColorMode::Web => TextColorConversion::None,
            } as u16,
        ],
        depth,
        transform_index: metadata.transform_index,
        base_uv_rect: metadata.base_uv_rect,
    }))
}

fn glyph_bounds(
    bounds: super::TextBounds,
    transform_index: u32,
    resolution: super::Resolution,
) -> GlyphBounds {
    if transform_index != 0 {
        GlyphBounds {
            x: Bounds { min: bounds.left, max: bounds.right },
            y: Bounds { min: bounds.top, max: bounds.bottom },
        }
    } else {
        let x_min = bounds.left.max(0);
        let y_min = bounds.top.max(0);
        GlyphBounds {
            x: Bounds {
                min: x_min,
                max: bounds.right.min(resolution.width as i32).max(x_min),
            },
            y: Bounds {
                min: y_min,
                max: bounds.bottom.min(resolution.height as i32).max(y_min),
            },
        }
    }
}

fn glyph_image(system: &mut GlyphSystem<'_>, key: cosmic_text::CacheKey) -> Option<GetGlyphImageResult> {
    let image = system.cache.get_image_uncached(system.font_system, key)?;
    let content_type = match image.content {
        SwashContent::Color => ContentType::Color,
        SwashContent::Mask | SwashContent::SubpixelMask => ContentType::Mask,
    };
    Some(GetGlyphImageResult {
        content_type,
        top: image.placement.top as i16,
        left: image.placement.left as i16,
        width: image.placement.width as u16,
        height: image.placement.height as u16,
        data: image.data,
    })
}

fn append_text_glyph<R>(
    state: &State,
    system: &mut GlyphSystem,
    glyph: &cosmic_text::LayoutGlyph,
    line_y: f32,
    left: f32,
    top: f32,
    scale: f32,
    bounds: GlyphBounds,
    default_color: Color,
    transform_index: u32,
    base_uv_rect: [f32; 4],
    metadata_to_depth: impl FnMut(usize) -> f32,
    rasterize_custom_glyph: R,
) -> Result<Option<GlyphToRender>, PrepareError>
where
    R: FnMut(RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph>,
{
    let physical_glyph = glyph.physical((left, top), scale);
    let color = glyph.color_opt.unwrap_or(default_color);
    prepare_glyph(
        state,
        system,
        GlyphMetadata {
            x: physical_glyph.x,
            y: physical_glyph.y,
            line_y,
            color,
            metadata: glyph.metadata,
            cache_key: GlyphonCacheKey::Text(physical_glyph.cache_key),
            scale_factor: scale,
            transform_index,
            base_uv_rect,
        },
        bounds,
        |system, _| glyph_image(system, physical_glyph.cache_key),
        metadata_to_depth,
        rasterize_custom_glyph,
    )
}

fn append_custom_glyph<R>(
    state: &State,
    system: &mut GlyphSystem,
    x: i32,
    y: i32,
    scale: f32,
    width: u16,
    height: u16,
    x_bin: SubpixelBin,
    y_bin: SubpixelBin,
    color: Color,
    metadata: usize,
    cache_key: GlyphonCacheKey,
    transform_index: u32,
    base_uv_rect: [f32; 4],
    bounds: GlyphBounds,
    custom_glyph_id: u16,
    rasterize_custom_glyph: R,
) -> Result<Option<GlyphToRender>, PrepareError>
where
    R: FnMut(RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph>,
{
    prepare_glyph(
        state,
        system,
        GlyphMetadata {
            x,
            y,
            line_y: 0.0,
            scale_factor: scale,
            color,
            metadata,
            cache_key,
            transform_index,
            base_uv_rect,
        },
        bounds,
        move |_system, rasterize_custom_glyph_fn| -> Option<GetGlyphImageResult> {
            if width == 0 || height == 0 {
                return None;
            }

            let input = RasterizeCustomGlyphRequest {
                id: custom_glyph_id,
                width,
                height,
                x_bin,
                y_bin,
                scale,
            };

            let output = (rasterize_custom_glyph_fn)(input)?;

            output.validate(&input, None);

            Some(GetGlyphImageResult {
                content_type: output.content_type,
                top: 0,
                left: 0,
                width,
                height,
                data: output.data,
            })
        },
        zero_depth,
        rasterize_custom_glyph,
    )
}
