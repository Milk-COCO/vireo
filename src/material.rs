use std::cell::RefCell;
use std::sync::Arc;

use rustc_hash::FxHashMap;

/// group 3 纹理槽数量（tex0..tex3）。
pub const MATERIAL_TEX_SLOTS: usize = 4;

/// group 3 `@binding(0)` storage uniform 字节上限。
pub const MATERIAL_UNIFORM_SIZE: usize = 1024;

/// 运行时自定义 shader 材质。
///
/// 由 [`GpuContext::create_material`](crate::gpu::GpuContext::create_material) 创建。
/// **同一 Material 可同时用于形状、文字**——引擎按用途自动选对应顶点布局与 pipeline。
///
/// **group 3 layout**：
/// - `@binding(0)` storage uniform（[`MATERIAL_UNIFORM_SIZE`] B，**VERTEX|FRAGMENT**）
/// - `@binding(1)` tex0 · `@binding(2)` samp0
/// - `@binding(3)` tex1 · `@binding(4)` samp1
/// - `@binding(5)` tex2 · `@binding(6)` samp2
/// - `@binding(7)` tex3 · `@binding(8)` samp3
///
/// 未设置的纹理槽默认白贴图；sampler 默认 filtering。
pub struct Material {
    pub(crate) bgl: wgpu::BindGroupLayout,
    pub(crate) uniform_buf: wgpu::Buffer,
    pub(crate) bind_group: RefCell<wgpu::BindGroup>,
    /// 统一 pipeline 缓存：key = (Target, sample_count, atc, ssaa, stencil_op?)
    pub(crate) pipelines: RefCell<FxHashMap<u64, Arc<wgpu::RenderPipeline>>>,
    pub(crate) source: String,
    pub(crate) shape_vertex_source: Option<String>,
    /// 4 个纹理 view（默认白贴图）
    tex_views: RefCell<[wgpu::TextureView; MATERIAL_TEX_SLOTS]>,
    /// 4 个独立 sampler
    samplers: RefCell<[wgpu::Sampler; MATERIAL_TEX_SLOTS]>,
}

impl Material {
    pub(crate) fn new(
        bgl: wgpu::BindGroupLayout,
        uniform_buf: wgpu::Buffer,
        bind_group: wgpu::BindGroup,
        pipelines: FxHashMap<u64, Arc<wgpu::RenderPipeline>>,
        source: String,
        shape_vertex_source: Option<String>,
        white_view: wgpu::TextureView,
        sampler: wgpu::Sampler,
    ) -> Self {
        Self {
            bgl,
            uniform_buf,
            bind_group: RefCell::new(bind_group),
            pipelines: RefCell::new(pipelines),
            source,
            shape_vertex_source,
            tex_views: RefCell::new([
                white_view.clone(),
                white_view.clone(),
                white_view.clone(),
                white_view,
            ]),
            samplers: RefCell::new([
                sampler.clone(),
                sampler.clone(),
                sampler.clone(),
                sampler,
            ]),
        }
    }

    /// 写入 uniform storage buffer。`data.len()` 必须 ≤ [`MATERIAL_UNIFORM_SIZE`]。
    pub fn set_uniform_bytes(&self, queue: &wgpu::Queue, data: &[u8]) {
        assert!(
            data.len() <= MATERIAL_UNIFORM_SIZE,
            "uniform {} bytes exceeds max {}",
            data.len(),
            MATERIAL_UNIFORM_SIZE
        );
        queue.write_buffer(&self.uniform_buf, 0, data);
    }

    /// 写入 `#[repr(C)]` Pod 结构体；`size_of::<T>()` 必须 ≤ [`MATERIAL_UNIFORM_SIZE`]。
    pub fn set_uniform<T: bytemuck::Pod>(&self, queue: &wgpu::Queue, data: &T) {
        self.set_uniform_bytes(queue, bytemuck::cast_slice(std::slice::from_ref(data)));
    }

    /// 设置 slot 0 纹理 + slot 0 sampler（单纹理兼容）。
    pub fn set_texture(
        &self,
        device: &wgpu::Device,
        tex_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) {
        self.tex_views.borrow_mut()[0] = tex_view.clone();
        self.samplers.borrow_mut()[0] = sampler.clone();
        self.rebuild_bind_group(device);
    }

    /// 设置指定纹理槽（0..3）。不改 sampler。
    pub fn set_texture_slot(
        &self,
        device: &wgpu::Device,
        slot: usize,
        tex_view: &wgpu::TextureView,
    ) {
        assert!(
            slot < MATERIAL_TEX_SLOTS,
            "texture slot {} out of range (0..{})",
            slot,
            MATERIAL_TEX_SLOTS
        );
        self.tex_views.borrow_mut()[slot] = tex_view.clone();
        self.rebuild_bind_group(device);
    }

    /// 一次设置多个槽：`views[i]` → slot i；`None` 保持原槽。
    pub fn set_texture_slots(
        &self,
        device: &wgpu::Device,
        views: &[Option<&wgpu::TextureView>],
    ) {
        {
            let mut slots = self.tex_views.borrow_mut();
            for (i, v) in views.iter().enumerate().take(MATERIAL_TEX_SLOTS) {
                if let Some(view) = v {
                    slots[i] = (*view).clone();
                }
            }
        }
        self.rebuild_bind_group(device);
    }

    /// 设置 slot 0 的 sampler（兼容旧「共享 sampler」调用）。
    pub fn set_sampler(&self, device: &wgpu::Device, sampler: &wgpu::Sampler) {
        self.set_sampler_slot(device, 0, sampler);
    }

    /// 设置指定槽的 filtering sampler。
    pub fn set_sampler_slot(&self, device: &wgpu::Device, slot: usize, sampler: &wgpu::Sampler) {
        assert!(
            slot < MATERIAL_TEX_SLOTS,
            "sampler slot {} out of range (0..{})",
            slot,
            MATERIAL_TEX_SLOTS
        );
        self.samplers.borrow_mut()[slot] = sampler.clone();
        self.rebuild_bind_group(device);
    }

    /// 一次设置多个 sampler；`None` 保持。
    pub fn set_sampler_slots(
        &self,
        device: &wgpu::Device,
        samplers: &[Option<&wgpu::Sampler>],
    ) {
        {
            let mut slots = self.samplers.borrow_mut();
            for (i, s) in samplers.iter().enumerate().take(MATERIAL_TEX_SLOTS) {
                if let Some(samp) = s {
                    slots[i] = (*samp).clone();
                }
            }
        }
        self.rebuild_bind_group(device);
    }

    fn rebuild_bind_group(&self, device: &wgpu::Device) {
        let views = self.tex_views.borrow();
        let samps = self.samplers.borrow();
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("custom material bind group"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&samps[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&samps[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&samps[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&views[3]),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::Sampler(&samps[3]),
                },
            ],
        });
        *self.bind_group.borrow_mut() = bg;
    }
}
