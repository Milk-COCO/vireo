use super::{Cache, Params, Resolution};
use std::{mem, slice};
use wgpu::{BindGroup, Buffer, BufferDescriptor, BufferUsages, Device, Queue};

/// 空样式表 / 空图层表的最小尺寸（1 个 vec4 = 16B，storage 绑定不能空）。
const EMPTY_STYLE_BYTES: u64 = 16;

/// Controls the visible area of all text for a given renderer. Any text outside the visible
/// area will be clipped.
///
/// Many projects will only ever need a single `Viewport`, but it is possible to create multiple
/// `Viewport`s if you want to render text to specific areas within a window (without having to)
/// bound each `TextArea`).
#[derive(Debug)]
pub struct Viewport {
    params: Params,
    params_buffer: Buffer,
    /// 样式表 / 图层表（`group(1) @binding(1..2)`）。空表时用 16B 的占位缓冲。
    styles_buffer: Buffer,
    layers_buffer: Buffer,
    pub(crate) bind_group: BindGroup,
}

impl Viewport {
    /// Creates a new `Viewport` with the given `device` and `cache`.
    pub fn new(device: &Device, cache: &Cache) -> Self {
        let params = Params {
            screen_resolution: Resolution {
                width: 0,
                height: 0,
            },
            dpi_scale: 1.0,
            style_count: 0,
        };

        let params_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("glyphon params"),
            size: mem::size_of::<Params>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let empty = |label: &str| {
            device.create_buffer(&BufferDescriptor {
                label: Some(label),
                size: EMPTY_STYLE_BYTES,
                usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let styles_buffer = empty("glyphon text styles");
        let layers_buffer = empty("glyphon text layers");

        let bind_group = cache.create_uniforms_bind_group(
            device,
            &params_buffer,
            &styles_buffer,
            &layers_buffer,
        );

        Self {
            params,
            params_buffer,
            styles_buffer,
            layers_buffer,
            bind_group,
        }
    }

    /// 用新的样式表 / 图层表缓冲重建 bind group（缓冲增长或每帧换数据后调用）。
    pub(crate) fn rebind_style_buffers(
        &mut self,
        device: &Device,
        cache: &Cache,
        styles: Buffer,
        layers: Buffer,
    ) {
        self.styles_buffer = styles;
        self.layers_buffer = layers;
        self.bind_group = cache.create_uniforms_bind_group(
            device,
            &self.params_buffer,
            &self.styles_buffer,
            &self.layers_buffer,
        );
    }

    /// Updates the `Viewport` with the given `resolution` / dpi / 样式数量。
    pub fn update(&mut self, queue: &Queue, resolution: Resolution, dpi_scale: f32) {
        if self.params.screen_resolution != resolution || self.params.dpi_scale != dpi_scale {
            self.params.screen_resolution = resolution;
            self.params.dpi_scale = dpi_scale;

            queue.write_buffer(&self.params_buffer, 0, unsafe {
                slice::from_raw_parts(
                    &self.params as *const Params as *const u8,
                    mem::size_of::<Params>(),
                )
            });
        }
    }

    /// 直接覆盖样式数量（样式表上传后调用；同一帧内不重复写）。
    pub(crate) fn set_style_count(&mut self, queue: &Queue, count: u32) {
        if self.params.style_count == count {
            return;
        }
        self.params.style_count = count;
        queue.write_buffer(&self.params_buffer, 0, unsafe {
            slice::from_raw_parts(
                &self.params as *const Params as *const u8,
                mem::size_of::<Params>(),
            )
        });
    }

    /// 每帧整块重写样式表 / 图层表（空表时跳过 —— 着色器不会读）。
    pub(crate) fn write_style_buffers(&mut self, queue: &Queue, styles: &[u8], layers: &[u8]) {
        if !styles.is_empty() {
            queue.write_buffer(&self.styles_buffer, 0, styles);
        }
        if !layers.is_empty() {
            queue.write_buffer(&self.layers_buffer, 0, layers);
        }
    }

    /// Returns the current resolution of the `Viewport`.
    pub fn resolution(&self) -> Resolution {
        self.params.screen_resolution
    }
}
