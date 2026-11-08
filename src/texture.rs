//! 纹理：可从文件加载，也可作为离屏渲染目标。

use crate::gpu::GpuContext;

/// 纹理——可从文件/RGBA 加载，也可创建为离屏渲染目标。
///
/// 贴图用 `draw_texture()`；离屏用 `target()` 获取 `RenderTarget`。
pub struct Texture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub bind_group: wgpu::BindGroup,
    pub width: u32,
    pub height: u32,
    /// 创建时格式。离屏画布据此检测 `GpuContext::surface_format` 漂移
    /// （GLES 首帧由默认 sRGB 翻为线性格式）并重建目标纹理。
    pub format: wgpu::TextureFormat,
    /// 这张图是不是 [`Texture::placeholder`] 生成的棋盘占位图。
    pub(crate) placeholder: bool,
}

impl Texture {
    // ---- 离屏渲染目标构造 ----

    /// 创建可渲染的纹理（离屏渲染目标）。
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        bg_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Self {
        assert!(
            width > 0 && height > 0,
            "Texture dimensions must be non-zero, got {}x{}",
            width,
            height
        );
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vireo texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("texture bind group"),
            layout: bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Self {
            texture,
            view,
            bind_group,
            width,
            height,
            format,
            placeholder: false,
        }
    }

    /// 用现成的 wgpu 对象包装一张纹理（compute / 自建 pass / 外部管线产出）。
    ///
    /// 宽高从 `texture.size()` 取；`bind_group` 需按 `gpu.texture_bind_group_layout`
    /// 建好（采样器槽在 binding 1）。这是外部构造 `Texture` 的**唯一**方式 ——
    /// 结构体字段仍可读，但不再允许字面量构造（避免漏掉内部标记）。
    pub fn wrap(
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        bind_group: wgpu::BindGroup,
        format: wgpu::TextureFormat,
    ) -> Self {
        let size = texture.size();
        Self {
            texture,
            view,
            bind_group,
            width: size.width,
            height: size.height,
            format,
            placeholder: false,
        }
    }

    /// 获取 RenderTarget 用于离屏渲染。
    pub fn target(&self) -> crate::render::RenderTarget {
        crate::render::RenderTarget::from_texture_view(self.view.clone())
    }

    /// 这张纹理是不是 [`Texture::placeholder`] 生成的棋盘占位图。
    ///
    /// `from_file` / `from_bytes` 失败会返回 `Err`（不再静默变占位图）；占位图只由
    /// 调用方**显式**要求（`Texture::placeholder` / `App::load_texture_or_placeholder`）
    /// 时产生。本方法用来在调试/断言里确认没有把占位图当成真图用。
    pub fn is_placeholder(&self) -> bool {
        self.placeholder
    }

    // ---- 从文件/RGBA 加载 ----

    /// 从文件加载纹理（自动识别 png/jpg/bmp）。
    ///
    /// 读文件或解码失败返回 [`VireoError`]；要"失败也能画"的行为用
    /// [`App::load_texture_or_placeholder`](crate::App::load_texture_or_placeholder)
    /// 或显式 [`Texture::placeholder`]。
    pub fn from_file(
        path: impl AsRef<std::path::Path>,
        gpu: &GpuContext,
    ) -> Result<Self, crate::error::VireoError> {
        let data = std::fs::read(path.as_ref()).map_err(|e| {
            crate::error::VireoError::Other(format!("read {}: {e}", path.as_ref().display()))
        })?;
        Self::from_bytes(&data, gpu)
    }

    /// 从内存字节加载纹理（自动识别 png/jpg/bmp）；解码失败返回 [`VireoError`]。
    pub fn from_bytes(data: &[u8], gpu: &GpuContext) -> Result<Self, crate::error::VireoError> {
        let img = image::load_from_memory(data).map_err(|e| {
            crate::error::VireoError::Other(format!("decode image ({} bytes): {e}", data.len()))
        })?;
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Self::from_rgba(width, height, &rgba, gpu))
    }

    /// 棋盘占位纹理（ffcc00/6699ff 8×8）。
    ///
    /// 加载失败但应用仍需继续画时**显式**用它 —— 不再有"静默变占位图"的路径。
    pub fn placeholder(gpu: &GpuContext) -> Self {
        create_missing_checker(gpu)
    }

    /// 从 RGBA 像素数据创建纹理。
    ///
    /// 宽高为 0 或数据不足属于**调用方编程错误**，直接 `assert!`（不是静默占位）。
    pub fn from_rgba(width: u32, height: u32, pixels: &[u8], gpu: &GpuContext) -> Self {
        assert!(
            width > 0 && height > 0,
            "Texture::from_rgba: width/height must be > 0 (got {width}x{height})"
        );
        let needed = (width as usize) * (height as usize) * 4;
        assert!(
            pixels.len() >= needed,
            "Texture::from_rgba: pixel buffer too short (need {needed} bytes for {width}x{height} RGBA, got {})",
            pixels.len()
        );
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vireo texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("texture bind group"),
            layout: &gpu.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gpu.default_sampler),
                },
            ],
        });
        Self {
            texture,
            view,
            bind_group,
            width,
            height,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            placeholder: false,
        }
    }

    /// 切换本纹理的 address mode（原地）：按 mode 重建 bind group。
    /// 只影响之后 `set_texture` 录的段（已录段拿的是旧 bind group 快照，不变）。
    /// 默认 ClampToEdge（`from_*` 构造即此，老行为不变）。
    ///
    /// 警告：Repeat/MirrorRepeat ＋ 拼 atlas＝wrap 渗色（采到邻区域甚至对边，
    /// 半 texel 内收救不了），要 gutter 或独占整图。单图平铺（tiling 背景）随便用。
    pub fn set_address_mode(&mut self, gpu: &GpuContext, mode: wgpu::AddressMode) {
        self.bind_group = make_bind_group(
            &gpu.device,
            &gpu.texture_bind_group_layout,
            &self.view,
            &gpu.sampler_for(mode),
        );
    }

    /// 同一张 image、另一种采样：共享 texture/view（句柄 clone，零重传），
    /// 只按 mode 建新 bind group。同一帧内同图多 mode 混用走这个；
    /// 只要单 mode 走 `set_address_mode`（不产生多余对象）。
    pub fn with_address_mode(&self, gpu: &GpuContext, mode: wgpu::AddressMode) -> Texture {
        Texture {
            texture: self.texture.clone(),
            view: self.view.clone(),
            bind_group: make_bind_group(
                &gpu.device,
                &gpu.texture_bind_group_layout,
                &self.view,
                &gpu.sampler_for(mode),
            ),
            width: self.width,
            height: self.height,
            format: self.format,
            placeholder: self.placeholder,
        }
    }

    /// 像素区域转换为归一化 UV 坐标 (u0, v0, u1, v1)。
    pub fn uv(&self, px: u32, py: u32, pw: u32, ph: u32) -> (f32, f32, f32, f32) {
        let w = self.width as f32;
        let h = self.height as f32;
        (
            px as f32 / w,
            py as f32 / h,
            (px + pw) as f32 / w,
            (py + ph) as f32 / h,
        )
    }
}

/// 以指定 view＋sampler 建纹理 bind group（from_*/set/with 共用形状）。
fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("texture bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// 生成 ffcc00 / 6699ff 棋盘 missing 纹理（8x8），并标记为占位图。
///
/// 只由 [`Texture::placeholder`] 调用；`from_*` 不会再静默产出它。
fn create_missing_checker(gpu: &GpuContext) -> Texture {
    const W: u32 = 8;
    const H: u32 = 8;
    let mut data = Vec::with_capacity((W * H * 4) as usize);
    let c0 = [0xffu8, 0xcc, 0x00, 0xff]; // #ffcc00
    let c1 = [0x66u8, 0x99, 0xff, 0xff]; // #6699ff
    for y in 0..H {
        for x in 0..W {
            let c = if ((x ^ y) & 1) == 0 { c0 } else { c1 };
            data.extend_from_slice(&c);
        }
    }
    let mut tex = Texture::from_rgba(W, H, &data, gpu);
    tex.placeholder = true;
    tex
}
