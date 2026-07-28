use std::sync::{Arc, mpsc};
use rustc_hash::FxHashMap;

use crate::context::Renderer;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{WindowId, WindowAttributes},
};

use crate::context::DrawBatch;
use crate::offscreen::OffscreenCanvas;
use crate::texture::Texture;
use crate::gpu::GpuContext;
use crate::input::InputState;

pub use winit::dpi::LogicalSize;
pub use winit::window::Fullscreen;
pub use winit::window::Icon;
pub use winit::window::WindowLevel;

use winit::window::Cursor;

/// 滑动窗口帧数：约 0.5s@60Hz，平滑 FPS，避免「满 1 秒整段重置」导致 55↔60 乱跳。
const FPS_SAMPLE_CAP: usize = 30;

/// 窗口描述符（用于配置待创建窗口）
/// 抗锯齿模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AntiAliasing {
    None,
    /// 多重采样：per-pixel 着色，硬件解析采样点覆盖。
    Msaa { samples: u32, alpha_to_coverage: bool },
    /// 超采样：per-sample 着色（`@interpolate(linear, sample)`），每个采样点独立计算 SDF。
    Ssaa { samples: u32, alpha_to_coverage: bool },
}

impl AntiAliasing {
    pub fn sample_count(&self) -> u32 {
        match self {
            AntiAliasing::None => 1,
            AntiAliasing::Msaa { samples, .. } | AntiAliasing::Ssaa { samples, .. } => *samples,
        }
    }

    pub fn alpha_to_coverage(&self) -> bool {
        match self {
            AntiAliasing::None => false,
            AntiAliasing::Msaa { alpha_to_coverage, .. } | AntiAliasing::Ssaa { alpha_to_coverage, .. } => *alpha_to_coverage,
        }
    }

    pub fn is_ssaa(&self) -> bool {
        matches!(self, AntiAliasing::Ssaa { .. })
    }
}

/// 把 AA 的 sample_count snap 到 `supported` 中 ≤ 请求的最大项。
/// 不可用 `min(req, max)`：列表可能是 `[1,4]`（无 2/8），硬截到 8 仍会在 pipeline 创建时 panic。
pub(crate) fn clamp_aa(aa: AntiAliasing, supported: &[u32]) -> AntiAliasing {
    let snap = |req: u32| -> u32 {
        let req = req.max(1);
        supported
            .iter()
            .copied()
            .filter(|&c| c <= req)
            .max()
            .unwrap_or(1)
    };
    match aa {
        AntiAliasing::None => AntiAliasing::None,
        AntiAliasing::Msaa { samples, alpha_to_coverage } => AntiAliasing::Msaa {
            samples: snap(samples),
            alpha_to_coverage,
        },
        AntiAliasing::Ssaa { samples, alpha_to_coverage } => AntiAliasing::Ssaa {
            samples: snap(samples),
            alpha_to_coverage,
        },
    }
}

pub struct WindowDesc {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub scale_factor_override: Option<f64>,
    pub min_size: Option<(u32, u32)>,
    pub max_size: Option<(u32, u32)>,
    pub position: Option<(i32, i32)>,
    pub resizable: bool,
    pub fullscreen: Option<Fullscreen>,
    pub maximized: bool,
    pub visible: bool,
    pub transparent: bool,
    pub decorations: bool,
    pub window_level: WindowLevel,
    pub window_icon: Option<Icon>,
    pub theme: Option<winit::window::Theme>,
    pub resize_increments: Option<(u32, u32)>,
    pub content_protected: bool,
    pub active: bool,
    pub cursor: Cursor,
    pub enabled_buttons: winit::window::WindowButtons,
    pub blur: bool,
    pub present_mode: wgpu::PresentMode,
    pub anti_aliasing: AntiAliasing,
}

impl WindowDesc {
    pub fn new(title: &str, width: u32, height: u32) -> Self {
        Self {
            title: title.to_string(),
            width,
            height,
            scale_factor_override: None,
            min_size: None,
            max_size: None,
            position: None,
            resizable: true,
            fullscreen: None,
            maximized: false,
            visible: true,
            transparent: false,
            decorations: true,
            window_level: WindowLevel::default(),
            window_icon: None,
            theme: None,
            resize_increments: None,
            content_protected: false,
            active: true,
            cursor: Cursor::default(),
            enabled_buttons: winit::window::WindowButtons::all(),
            blur: false,
            present_mode: wgpu::PresentMode::AutoVsync,
            anti_aliasing: AntiAliasing::None,
        }
    }

    /// 启用 high_dpi 模式：逻辑像素 = 物理像素（scale_factor = 1.0）
    pub fn high_dpi(mut self, enabled: bool) -> Self {
        self.scale_factor_override = if enabled { Some(1.0) } else { None };
        self
    }

    pub fn min_size(mut self, w: u32, h: u32) -> Self {
        self.min_size = Some((w, h));
        self
    }

    pub fn max_size(mut self, w: u32, h: u32) -> Self {
        self.max_size = Some((w, h));
        self
    }

    pub fn position(mut self, x: i32, y: i32) -> Self {
        self.position = Some((x, y));
        self
    }

    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    pub fn fullscreen(mut self, fullscreen: Fullscreen) -> Self {
        self.fullscreen = Some(fullscreen);
        self
    }

    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn transparent(mut self, transparent: bool) -> Self {
        self.transparent = transparent;
        self
    }

    pub fn decorations(mut self, decorations: bool) -> Self {
        self.decorations = decorations;
        self
    }

    pub fn window_level(mut self, level: WindowLevel) -> Self {
        self.window_level = level;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.window_icon = Some(icon);
        self
    }

    /// 从图片文件加载窗口图标（PNG/JPG/BMP）
    pub fn icon_from_path(mut self, path: impl AsRef<std::path::Path>) -> Self {
        if let Ok(data) = std::fs::read(path.as_ref()) {
            if let Ok(img) = image::load_from_memory(&data) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if let Ok(icon) = Icon::from_rgba(rgba.into_raw(), w, h) {
                    self.window_icon = Some(icon);
                }
            }
        }
        self
    }

    pub fn theme(mut self, theme: winit::window::Theme) -> Self {
        self.theme = Some(theme);
        self
    }

    pub fn resize_increments(mut self, w: u32, h: u32) -> Self {
        self.resize_increments = Some((w, h));
        self
    }

    pub fn content_protected(mut self, protected: bool) -> Self {
        self.content_protected = protected;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn cursor(mut self, cursor: Cursor) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn enabled_buttons(mut self, buttons: winit::window::WindowButtons) -> Self {
        self.enabled_buttons = buttons;
        self
    }

    pub fn blur(mut self, blur: bool) -> Self {
        self.blur = blur;
        self
    }

    pub fn present_mode(mut self, mode: wgpu::PresentMode) -> Self {
        self.present_mode = mode;
        self
    }

    pub fn anti_aliasing(mut self, aa: AntiAliasing) -> Self {
        self.anti_aliasing = aa;
        self
    }
}

/// 分段耗时（秒），由 render thread 回传。
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawTimings {
    /// `get_current_texture`：可能阻塞等 swapchain 空位 / 上一帧 present。
    pub acquire_secs: f64,
    /// 编码 + `queue.submit`（不含 present）。
    pub encode_secs: f64,
}

/// 从 winit 线程发往渲染线程的事件（全是 Send-safe 的自定义类型）。
enum WinitEvent {
    WindowCreated {
        handle: usize,
        window: Arc<winit::window::Window>,
        surface: wgpu::Surface<'static>,
        logical_width: u32,
        logical_height: u32,
        high_dpi: bool,
        transparent: bool,
        aa: AntiAliasing,
        present_mode: wgpu::PresentMode,
        init_duration: f64,
    },
    Resized { handle: usize, width: u32, height: u32 },
    ScaleFactorChanged { handle: usize, scale: f64 },
    CursorMoved { handle: usize, x: f64, y: f64 },
    KeyboardInput { handle: usize, event: crate::input::KeyEvent },
    MouseInput { handle: usize, button: winit::event::MouseButton, pressed: bool },
    MouseWheel { handle: usize, delta: crate::input::ScrollDelta },
    ModifiersChanged { handle: usize, modifiers: crate::input::Modifiers },
    Focused { handle: usize, focused: bool },
    CursorEntered { handle: usize },
    CursorLeft { handle: usize },
    Touch { handle: usize, event: crate::input::TouchEvent },
    CloseRequested { handle: usize },
    SetTitle { handle: usize, title: String },
    SetSize { handle: usize, width: u32, height: u32 },
    SetMinSize { handle: usize, width: Option<u32>, height: Option<u32> },
    SetMaxSize { handle: usize, width: Option<u32>, height: Option<u32> },
    SetFullscreen { handle: usize, fullscreen: Option<Fullscreen> },
    SetMaximized { handle: usize, maximized: bool },
    SetMinimized { handle: usize, minimized: bool },
    SetVisible { handle: usize, visible: bool },
    FocusWindow { handle: usize },
    SetWindowLevel { handle: usize, level: WindowLevel },
    SetDecorations { handle: usize, decorations: bool },
    SetIcon { handle: usize, icon: Icon },
    SetCursor { handle: usize, cursor: winit::window::Cursor },
    Exit,
}

/// 窗口实例 —— 渲染线程所有，持有 surface/renderer/input。
/// 所有公开 API 坐标系为逻辑像素（用户友好），GPU 内部使用物理像素。
pub struct VireoWindow {
    pub inner: Arc<winit::window::Window>,
    pub gpu: Arc<GpuContext>,
    pub mouse_pos: (f32, f32),
    pub logical_width: u32,
    pub logical_height: u32,
    pub high_dpi: bool,
    pub input: InputState,
    /// 该窗口初始化耗时（秒）：app.window() 内的 AA 管线预热。
    pub init_duration: f64,
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) surface_config: std::cell::RefCell<wgpu::SurfaceConfiguration>,
    pub(crate) renderer: std::cell::RefCell<Renderer>,
    frame_texture: std::cell::RefCell<Option<wgpu::SurfaceTexture>>,
    /// 用于向 winit 线程发送窗口操作事件
    event_tx: mpsc::Sender<WinitEvent>,
    /// 向 winit 线程注册输入回调
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    /// 待应用的 present mode（在 draw_timed 开头应用）
    pending_mode: std::cell::Cell<Option<wgpu::PresentMode>>,
    /// 待应用的 AA 模式（在 draw_timed 开头应用）
    pending_aa: std::cell::Cell<Option<AntiAliasing>>,
    /// 窗口 handle（在 App.windows 中的索引）
    handle: usize,
}

impl VireoWindow {
    fn new(
        inner: Arc<winit::window::Window>,
        gpu: Arc<GpuContext>,
        surface: wgpu::Surface<'static>,
        logical_width: u32,
        logical_height: u32,
        high_dpi: bool,
        transparent: bool,
        aa: AntiAliasing,
        present_mode: wgpu::PresentMode,
        init_duration: f64,
    event_tx: mpsc::Sender<WinitEvent>,
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    handle: usize,
    ) -> Self {
        let scale = if high_dpi { 1.0 } else { inner.scale_factor() as f32 };
        let dpi = inner.scale_factor() as f32;
        let renderer = Renderer::new(
            gpu.clone(),
            logical_width,
            logical_height,
            inner.inner_size().width,
            inner.inner_size().height,
            scale,
            aa,
            dpi,
        );

        let caps = surface.get_capabilities(&gpu.adapter);
        let alpha_mode = if transparent {
            if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PostMultiplied) {
                wgpu::CompositeAlphaMode::PostMultiplied
            } else if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                wgpu::CompositeAlphaMode::PreMultiplied
            } else {
                wgpu::CompositeAlphaMode::Auto
            }
        } else {
            wgpu::CompositeAlphaMode::Auto
        };
        let fmt = if caps.formats.contains(&gpu.surface_format) {
            gpu.surface_format
        } else {
            caps.formats[0]
        };
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: fmt,
            width: inner.inner_size().width.max(1),
            height: inner.inner_size().height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&gpu.device, &surface_config);

        Self {
            inner,
            gpu,
            mouse_pos: (-1.0, -1.0),
            logical_width,
            logical_height,
            high_dpi,
            input: InputState::default(),
            init_duration,
            surface,
            surface_config: std::cell::RefCell::new(surface_config),
            renderer: std::cell::RefCell::new(renderer),
            frame_texture: std::cell::RefCell::new(None),
            event_tx,
            cb_tx,
            pending_mode: std::cell::Cell::new(None),
            pending_aa: std::cell::Cell::new(None),
            handle,
        }
    }

    /// Acquire a surface texture, reconfiguring on Outdated/Lost.
    fn acquire_ft(&self) -> Option<wgpu::SurfaceTexture> {
        let mut ft = self.frame_texture.borrow_mut();
        if ft.is_some() {
            return ft.take();
        }
        let st = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(st) | wgpu::CurrentSurfaceTexture::Suboptimal(st) => Some(st),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.renderer.borrow().gpu.device, &self.surface_config.borrow());
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(st) | wgpu::CurrentSurfaceTexture::Suboptimal(st) => Some(st),
                    _ => None,
                }
            }
            _ => None,
        };
        *ft = st;
        ft.take()
    }

    /// 绘制一帧并 present。在当前线程（渲染线程）上同步执行。
    pub fn draw(&self, clear_color: Option<crate::color::Color>, batches: &[&DrawBatch]) {
        self.draw_timed(clear_color, batches);
    }

    /// 与 [`draw`] 相同，并返回分段耗时（秒），用于卡顿诊断。
    pub fn draw_timed(
        &self,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) -> DrawTimings {
        let t0 = std::time::Instant::now();
        // Apply pending present_mode change
        if let Some(mode) = self.pending_mode.take() {
            if let Some(st) = self.frame_texture.borrow_mut().take() {
                self.renderer.borrow().gpu.queue.present(st);
            }
            let mut config = self.surface_config.borrow().clone();
            let caps = self.surface.get_capabilities(&self.renderer.borrow().gpu.adapter);
            config.present_mode = mode;
            if !caps.present_modes.contains(&mode) {
                eprintln!("vireo: PresentMode {:?} not supported, falling back to AutoVsync", mode);
                config.present_mode = wgpu::PresentMode::AutoVsync;
            }
            self.surface.configure(&self.renderer.borrow().gpu.device, &config);
            *self.surface_config.borrow_mut() = config;
        }
        // Apply pending AA change
        if let Some(aa) = self.pending_aa.take() {
            let sc = aa.sample_count();
            let atc = aa.alpha_to_coverage();
            let ssaa = aa.is_ssaa();
            let _ = self.renderer.borrow().gpu.ensure_pipeline(sc, atc, ssaa, false);
            let _ = self.renderer.borrow().gpu.ensure_pipeline(sc, atc, ssaa, true);
            self.renderer.borrow_mut().update_aa(aa);
        }
        let Some(st) = self.acquire_ft() else {
            return DrawTimings::default();
        };
        let acquire_secs = t0.elapsed().as_secs_f64();
        let t1 = std::time::Instant::now();
        let view = st.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let target = crate::context::RenderTarget::from_texture_view(view);
        let batch_refs: Vec<&DrawBatch> = batches.iter().copied().collect();
        self.renderer.borrow_mut().draw(&target, clear_color, &batch_refs);
        let encode_secs = t1.elapsed().as_secs_f64();
        self.renderer.borrow().gpu.queue.present(st);
        DrawTimings { acquire_secs, encode_secs }
    }

    /// 强制 GPU 端 PSO 编译（DX12 懒编译需要）。
    pub fn preheat(&self, clear_color: crate::color::Color) {
        if let Some(st) = self.acquire_ft() {
            let view = st.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let target = crate::context::RenderTarget::from_texture_view(view);
            self.renderer.borrow_mut().preheat(&target, clear_color);
            self.renderer.borrow().gpu.queue.present(st);
        }
    }

    /// 调整窗口大小（size 为物理像素）
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 { return; }
        if self.high_dpi {
            self.logical_width = width;
            self.logical_height = height;
        } else {
            let sf = self.inner.scale_factor();
            self.logical_width = (width as f64 / sf) as u32;
            self.logical_height = (height as f64 / sf) as u32;
        }
        let scale = if self.high_dpi { 1.0 } else { self.inner.scale_factor() as f32 };
        let dpi_scale = self.inner.scale_factor() as f32;
        // Present any outstanding surface texture before reconfiguring.
        if let Some(st) = self.frame_texture.borrow_mut().take() {
            self.renderer.borrow_mut().gpu.queue.present(st);
        }
        self.surface_config.borrow_mut().width = width;
        self.surface_config.borrow_mut().height = height;
        self.surface.configure(&self.renderer.borrow().gpu.device, &self.surface_config.borrow());
        self.renderer.borrow_mut().resize(
            self.logical_width, self.logical_height,
            width, height, scale, dpi_scale,
        );
    }

    /// 获取当前鼠标位置（窗口用户坐标系，即 WindowDesc 传入的宽高范围）
    pub fn mouse_pos(&self) -> (f32, f32) {
        self.mouse_pos
    }

    /// 获取当前投影矩阵（逻辑像素）
    pub fn projection(&self) -> glam::Mat4 {
        glam::camera::rh::proj::opengl::orthographic(
            0.0,
            self.logical_width as f32,
            self.logical_height as f32,
            0.0,
            -1.0,
            1.0,
        )
    }

    /// 获取共享 GPU 上下文
    pub fn gpu(&self) -> &Arc<GpuContext> {
        &self.gpu
    }

    // ------ 输入状态轮询 API ------

    pub fn key_down(&self, key: crate::input::KeyCode) -> bool {
        self.input.keys_down.borrow().contains(&key)
    }

    pub fn any_key_down(&self) -> bool {
        !self.input.keys_down.borrow().is_empty()
    }

    pub fn mouse_down(&self, button: crate::input::MouseButton) -> bool {
        self.input.mouse_buttons_down.borrow().contains(&button)
    }

    pub fn mouse_left(&self) -> bool {
        self.mouse_down(crate::input::MouseButton::Left)
    }

    pub fn mouse_right(&self) -> bool {
        self.mouse_down(crate::input::MouseButton::Right)
    }

    pub fn modifiers(&self) -> crate::input::Modifiers {
        *self.input.modifiers.borrow()
    }

    pub fn ctrl_down(&self) -> bool {
        self.input.modifiers.borrow().ctrl()
    }

    pub fn shift_down(&self) -> bool {
        self.input.modifiers.borrow().shift()
    }

    pub fn alt_down(&self) -> bool {
        self.input.modifiers.borrow().alt()
    }

    pub fn take_scroll(&self) -> (f32, f32) {
        let mut delta = self.input.scroll_delta.borrow_mut();
        let result = delta.line;
        delta.line = (0.0, 0.0);
        result
    }

    pub fn take_scroll_pixel(&self) -> (f32, f32) {
        let mut delta = self.input.scroll_delta.borrow_mut();
        let result = delta.pixel;
        delta.pixel = (0.0, 0.0);
        result
    }

    pub fn focused(&self) -> bool {
        *self.input.focused.borrow()
    }

    pub fn cursor_inside(&self) -> bool {
        *self.input.cursor_inside.borrow()
    }

}

/// 应用管理器 —— 管理 GPU 上下文、窗口、纹理。
/// 构造后在 `run()` 之前配置窗口/纹理/离屏画布，`run()` 将 `self` 移动到渲染线程。
pub struct App {
    pub window_descs: Vec<WindowDesc>,
    /// `Vec<Option<VireoWindow>>`，以 handle 为索引。关闭的窗口为 `None`。
    pub windows: Vec<Option<VireoWindow>>,
    pub gpu: Arc<GpuContext>,
    instance: Option<wgpu::Instance>,
    /// 稳定 handle → winit WindowId。handle 由 `App::window()` 分配，
    /// 在 run() 中被取出给 winit 线程用。
    handle_to_id: FxHashMap<u64, WindowId>,
    /// 下一个待分配的 handle（单调递增；`App::window()` 自增）。
    next_handle: u64,
    close_hooks: FxHashMap<u64, Option<Box<dyn FnOnce() + Send>>>,
    /// 输入事件回调集合（按 handle 索引，run() 后迁移到 winit 线程）
    callbacks: FxHashMap<u64, crate::input::InputCallbacks>,
    default_icon: Option<Icon>,
    textures: Vec<Texture>,
    offscreens: Vec<OffscreenCanvas>,
    pub frame_count: u64,
    /// 上一帧间隔（秒），瞬时值。第一帧为 0（A 语义：没有「上一帧」）。
    pub frame_time: f64,
    /// 滑动窗口平均 FPS（比「每秒整段重置」更稳，少 55↔60 乱跳）。第一帧为 0。
    pub fps: f64,
    /// App::new 内部耗时（秒）：GPU 设备、shader 模块、bind group layout 构造。
    pub init_duration: f64,
    /// 各 `app.window()` 调用的 init_duration（秒），按调用顺序入队。
    /// run() 中按序出队给 winit 线程用。
    window_init_durations: Vec<f64>,
    /// 最近若干帧的间隔，用于平滑 FPS。
    fps_samples: Vec<f64>,
    last_frame: std::time::Instant,
}

impl App {
    /// 创建 App。构造时即初始化 GPU 设备，可在 run() 之前加载纹理等资源。
    pub fn new() -> Self {
        let init_start = std::time::Instant::now();
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        );
        let gpu = Arc::new(GpuContext::new(&instance));
        let default_icon = std::fs::read("logo.png")
            .ok()
            .and_then(|data| image::load_from_memory(&data).ok())
            .map(|img| {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Icon::from_rgba(rgba.into_raw(), w, h).ok()
            })
            .flatten();
        let init_duration = init_start.elapsed().as_secs_f64();
        Self {
            window_descs: Vec::new(),
            windows: Vec::new(),
            gpu,
            instance: Some(instance),
            handle_to_id: FxHashMap::default(),
            next_handle: 0,
            close_hooks: FxHashMap::default(),
            callbacks: FxHashMap::default(),
            default_icon,
            textures: Vec::new(),
            offscreens: Vec::new(),
            frame_count: 0,
            frame_time: 0.0,
            fps: 0.0,
            init_duration,
            window_init_durations: Vec::new(),
            fps_samples: Vec::with_capacity(FPS_SAMPLE_CAP),
            last_frame: std::time::Instant::now(),
        }
    }

    /// 创建离屏画布。与 window() 对称，可在 run() 之前调用。
    /// 同步预热 AA 对应的 SDF + geo 管线，构造耗时由 `OffscreenCanvas::init_duration()` 暴露。
    pub fn offscreen(&mut self, width: u32, height: u32, aa: AntiAliasing) -> OffscreenIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(aa, self.gpu.supported_sample_counts());
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        let idx = self.offscreens.len();
        let mut offscreen = OffscreenCanvas::with_aa(&self.gpu, width, height, aa, init_duration);
        offscreen.index = OffscreenIndex(idx);
        self.offscreens.push(offscreen);
        OffscreenIndex(idx)
    }

    /// 根据索引获取离屏画布引用
    pub fn offscreen_ref(&self, idx: &OffscreenIndex) -> Option<&OffscreenCanvas> {
        self.offscreens.get(idx.0)
    }

    /// 从文件加载纹理（存储在 App 中管理生命周期），返回纹理索引。
    /// 读取或解码失败时会打印错误并返回一个“missing”棋盘纹理（不返回 Err）。
    pub fn load_texture(&mut self, path: impl AsRef<std::path::Path>) -> usize {
        let tex = Texture::from_file(path, &self.gpu);
        let idx = self.textures.len();
        self.textures.push(tex);
        idx
    }

    /// 根据索引获取已加载的纹理
    pub fn texture(&self, index: usize) -> Option<&Texture> {
        self.textures.get(index)
    }

    /// 配置一个待创建的窗口。可选 on_close 钩子在窗口被关闭时调用。必须在 run() 之前调用。
    /// 同步预热窗口 AA 对应的 SDF + geo 管线，并把 AA clamp 到硬件上限（避免 wgpu panic）。
    /// 构造耗时在 `App::run` 创建窗口后由 `VireoWindow::init_duration()` 暴露。
    pub fn window(&mut self, mut desc: WindowDesc, on_close: Option<impl FnOnce() + Send + 'static>) -> WindowIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(desc.anti_aliasing, self.gpu.supported_sample_counts());
        desc.anti_aliasing = aa;
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        self.window_init_durations.push(init_duration);
        let handle = self.next_handle;
        self.next_handle += 1;
        self.window_descs.push(desc);
        self.close_hooks.insert(handle, on_close.map(|f| Box::new(f) as Box<dyn FnOnce() + Send>));
        WindowIndex::new(handle)
    }

    // ------ 输入事件回调注册（winit 线程 invoke，无需 +Send）------

    pub fn on_key_down(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_key_down.push(Box::new(callback));
        self
    }

    pub fn on_key_up(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_key_up.push(Box::new(callback));
        self
    }

    pub fn on_mouse_down(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_mouse_down.push(Box::new(callback));
        self
    }

    pub fn on_mouse_up(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_mouse_up.push(Box::new(callback));
        self
    }

    pub fn on_scroll(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::MouseScrollEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_scroll.push(Box::new(callback));
        self
    }

    pub fn on_cursor_entered(&mut self, handle: WindowIndex, callback: impl FnOnce() + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_cursor_entered.push(Box::new(callback));
        self
    }

    pub fn on_cursor_left(&mut self, handle: WindowIndex, callback: impl FnOnce() + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_cursor_left.push(Box::new(callback));
        self
    }

    pub fn on_touch(&mut self, handle: WindowIndex, callback: impl FnMut(&crate::input::TouchEvent) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_touch.push(Box::new(callback));
        self
    }

    pub fn on_focus_gained(&mut self, handle: WindowIndex, callback: impl FnOnce() + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_focus_gained.push(Box::new(callback));
        self
    }

    pub fn on_focus_lost(&mut self, handle: WindowIndex, callback: impl FnOnce() + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_focus_lost.push(Box::new(callback));
        self
    }

    pub fn on_modifiers_changed(&mut self, handle: WindowIndex, callback: impl FnMut(crate::input::Modifiers) + 'static) -> &mut Self {
        let h = handle.0;
        self.callbacks.entry(h).or_default().on_modifiers_changed.push(Box::new(callback));
        self
    }

    /// 启动事件循环 + 渲染线程。
    /// winit 线程只负责任何操作，渲染线程持有 `App` + `on_frame` 独立运行。
    /// 闭包签名: FnMut(&App) -> bool，返回 true 继续循环，false 退出。
    pub fn run<F: FnMut(&App) -> bool + Send + 'static>(mut self, on_frame: F) {
        let event_loop = EventLoop::new().unwrap();

        let window_init_durations = std::mem::take(&mut self.window_init_durations);
        let window_descs: Vec<_> = self.window_descs.drain(..).collect();
        let close_hooks = std::mem::take(&mut self.close_hooks);
        let default_icon = self.default_icon.take();
        let instance = self.instance.take().expect("instance already taken");
        self.handle_to_id.clear();
        self.windows.clear();

        // 提取回调集合：从 App 取出后移入 winit 线程 Runner。
        let window_callbacks: Vec<crate::input::InputCallbacks> = {
            let n = window_descs.len();
            let mut cbs: Vec<crate::input::InputCallbacks> = Vec::with_capacity(n);
            for i in 0..n {
                cbs.push(self.callbacks.remove(&(i as u64)).unwrap_or_default());
            }
            cbs
        };

        let (event_tx, event_rx) = mpsc::channel::<WinitEvent>();
        let render_event_tx = event_tx.clone();
        let (cb_tx, cb_rx) = mpsc::channel::<(usize, crate::input::InputCallbacks)>();

        // 渲染线程：持有 App + on_frame，处理事件 + 用户代码 + 渲染。
        let expected_windows = window_descs.len();
        let render_thread = std::thread::Builder::new()
            .name("vireo-render".into())
            .spawn(move || {
                render_on_frame(self, on_frame, render_event_tx, event_rx, cb_tx, expected_windows);
            })
            .expect("failed to spawn render thread");

        // Winit 线程：仅创建窗口和转发事件。
        struct Runner {
    event_tx: mpsc::Sender<WinitEvent>,
    /// 接收渲染线程发来的输入回调注册
    cb_rx: mpsc::Receiver<(usize, crate::input::InputCallbacks)>,
            window_descs: Vec<WindowDesc>,
            id_to_handle: FxHashMap<WindowId, usize>,
            close_hooks: FxHashMap<u64, Option<Box<dyn FnOnce() + Send>>>,
            window_callbacks: Vec<crate::input::InputCallbacks>,
            default_icon: Option<Icon>,
            window_init_durations: Vec<f64>,
            instance: wgpu::Instance,
            created: bool,
            alive_handles: usize,
        }

        // 辅助：从 Runner 获取 handle（panic-safe）
        impl Runner {
            fn handle_for(&self, window_id: WindowId) -> Option<usize> {
                self.id_to_handle.get(&window_id).copied()
            }

            fn send(&self, event: WinitEvent) {
                let _ = self.event_tx.send(event);
            }

            fn create_attrs(desc: &WindowDesc, default_icon: &Option<Icon>) -> WindowAttributes {
                let size: winit::dpi::Size = match desc.scale_factor_override {
                    Some(_) => winit::dpi::PhysicalSize::new(desc.width, desc.height).into(),
                    None => winit::dpi::LogicalSize::new(desc.width, desc.height).into(),
                };
                let mut attrs = WindowAttributes::default()
                    .with_title(&desc.title)
                    .with_inner_size(size)
                    .with_resizable(desc.resizable)
                    .with_maximized(desc.maximized)
                    .with_visible(desc.visible)
                    .with_transparent(desc.transparent)
                    .with_decorations(desc.decorations)
                    .with_window_level(desc.window_level)
                    .with_content_protected(desc.content_protected)
                    .with_active(desc.active)
                    .with_blur(desc.blur)
                    .with_cursor(desc.cursor.clone())
                    .with_enabled_buttons(desc.enabled_buttons);
                if let Some((w, h)) = desc.min_size {
                    attrs = attrs.with_min_inner_size(LogicalSize::new(w, h));
                }
                if let Some((w, h)) = desc.max_size {
                    attrs = attrs.with_max_inner_size(LogicalSize::new(w, h));
                }
                if let Some((x, y)) = desc.position {
                    attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
                }
                if let Some(ref fs) = desc.fullscreen {
                    attrs = attrs.with_fullscreen(Some(fs.clone()));
                }
                let icon = desc.window_icon.as_ref().or(default_icon.as_ref());
                if let Some(icon) = icon {
                    attrs = attrs.with_window_icon(Some(icon.clone()));
                }
                if let Some(theme) = desc.theme {
                    attrs = attrs.with_theme(Some(theme));
                }
                if let Some((w, h)) = desc.resize_increments {
                    attrs = attrs.with_resize_increments(LogicalSize::new(w, h));
                }
                attrs
            }
        }

        impl ApplicationHandler for Runner {
            fn resumed(&mut self, event_loop: &ActiveEventLoop) {
                if self.created {
                    return;
                }
                self.created = true;

                for (handle, desc) in self.window_descs.iter().enumerate() {
                    let attrs = Self::create_attrs(desc, &self.default_icon);
                    let window = Arc::new(
                        event_loop.create_window(attrs).unwrap(),
                    );
                    let surface = self.instance.create_surface(window.clone()).unwrap();
                    let window_id = window.id();

                    let init_duration = if handle < self.window_init_durations.len() {
                        self.window_init_durations[handle]
                    } else {
                        0.0
                    };

                    self.id_to_handle.insert(window_id, handle);
                    self.alive_handles += 1;

                    self.send(WinitEvent::WindowCreated {
                        handle,
                        window,
                        surface,
                        logical_width: desc.width,
                        logical_height: desc.height,
                        high_dpi: desc.scale_factor_override.is_some(),
                        transparent: desc.transparent,
                        aa: desc.anti_aliasing,
                        present_mode: desc.present_mode,
                        init_duration,
                    });
                }
            }

            fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
                // Drain callback registrations sent from render thread
                while let Ok((handle, mut reg)) = self.cb_rx.try_recv() {
                    if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                        cbs.on_key_down.extend(reg.on_key_down.drain(..));
                        cbs.on_key_up.extend(reg.on_key_up.drain(..));
                        cbs.on_mouse_down.extend(reg.on_mouse_down.drain(..));
                        cbs.on_mouse_up.extend(reg.on_mouse_up.drain(..));
                        cbs.on_scroll.extend(reg.on_scroll.drain(..));
                        cbs.on_cursor_entered.extend(reg.on_cursor_entered.drain(..));
                        cbs.on_cursor_left.extend(reg.on_cursor_left.drain(..));
                        cbs.on_touch.extend(reg.on_touch.drain(..));
                        cbs.on_focus_gained.extend(reg.on_focus_gained.drain(..));
                        cbs.on_focus_lost.extend(reg.on_focus_lost.drain(..));
                        cbs.on_modifiers_changed.extend(reg.on_modifiers_changed.drain(..));
                    }
                }
                event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
            }

            fn window_event(
                &mut self,
                event_loop: &ActiveEventLoop,
                window_id: WindowId,
                event: WindowEvent,
            ) {
                let Some(handle) = self.handle_for(window_id) else { return };

                match event {
                    WindowEvent::CloseRequested => {
                        if let Some(hook_opt) = self.close_hooks.get_mut(&(handle as u64)) {
                            if let Some(h) = hook_opt.take() { h(); }
                        }
                        self.send(WinitEvent::CloseRequested { handle });
                        self.alive_handles -= 1;
                        if self.alive_handles == 0 {
                            self.send(WinitEvent::Exit);
                            event_loop.exit();
                        }
                    }
                    WindowEvent::Resized(size) => {
                        self.send(WinitEvent::Resized {
                            handle,
                            width: size.width,
                            height: size.height,
                        });
                    }
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        self.send(WinitEvent::ScaleFactorChanged {
                            handle,
                            scale: scale_factor,
                        });
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        self.send(WinitEvent::CursorMoved {
                            handle,
                            x: position.x,
                            y: position.y,
                        });
                    }
                    WindowEvent::KeyboardInput { event: key_event, .. } => {
                        if let Some(mapped) = crate::input::map_key_event(&key_event) {
                            if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                                if mapped.state.is_pressed() {
                                    for cb in &mut cbs.on_key_down { cb(&mapped); }
                                } else {
                                    for cb in &mut cbs.on_key_up { cb(&mapped); }
                                }
                            }
                            self.send(WinitEvent::KeyboardInput { handle, event: mapped });
                        }
                    }
                    WindowEvent::MouseInput { state, button, .. } => {
                        let pressed = state == winit::event::ElementState::Pressed;
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            let evt = crate::input::MouseButtonEvent { button, state };
                            if pressed {
                                for cb in &mut cbs.on_mouse_down { cb(&evt); }
                            } else {
                                for cb in &mut cbs.on_mouse_up { cb(&evt); }
                            }
                        }
                        self.send(WinitEvent::MouseInput { handle, button, pressed });
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        let delta = crate::input::map_scroll_delta(delta);
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            let evt = crate::input::MouseScrollEvent { delta };
                            for cb in &mut cbs.on_scroll { cb(&evt); }
                        }
                        self.send(WinitEvent::MouseWheel { handle, delta });
                    }
                    WindowEvent::ModifiersChanged(state) => {
                        let modifiers = crate::input::map_modifiers(&state.state());
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_modifiers_changed { cb(modifiers); }
                        }
                        self.send(WinitEvent::ModifiersChanged { handle, modifiers });
                    }
                    WindowEvent::Focused(focused) => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            if focused {
                                for c in cbs.on_focus_gained.drain(..) { c(); }
                            } else {
                                for c in cbs.on_focus_lost.drain(..) { c(); }
                            }
                        }
                        self.send(WinitEvent::Focused { handle, focused });
                    }
                    WindowEvent::CursorEntered { .. } => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for c in cbs.on_cursor_entered.drain(..) { c(); }
                        }
                        self.send(WinitEvent::CursorEntered { handle });
                    }
                    WindowEvent::CursorLeft { .. } => {
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for c in cbs.on_cursor_left.drain(..) { c(); }
                        }
                        self.send(WinitEvent::CursorLeft { handle });
                    }
                    WindowEvent::Touch(touch) => {
                        let mapped = crate::input::map_touch_event(&touch, 1.0);
                        if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                            for cb in &mut cbs.on_touch { cb(&mapped); }
                        }
                        self.send(WinitEvent::Touch { handle, event: mapped });
                    }
                    _ => {}
                }
            }
        }

        event_loop.run_app(&mut Runner {
            event_tx,
            cb_rx,
            window_descs,
            id_to_handle: FxHashMap::default(),
            close_hooks,
            window_callbacks,
            default_icon,
            window_init_durations,
            instance,
            created: false,
            alive_handles: 0,
        }).unwrap();

        // Winit loop 结束后等待渲染线程退出。
        let _ = render_thread.join();
    }
}

/// 渲染线程主循环：处理 winit 事件 → 调用用户 on_frame → 重复。
fn render_on_frame<F>(
    mut app: App,
    mut on_frame: F,
    event_tx: mpsc::Sender<WinitEvent>,
    rx: mpsc::Receiver<WinitEvent>,
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    expected_windows: usize,
)
where F: FnMut(&App) -> bool + Send + 'static
{
    let mut created_windows = 0usize;
    loop {
        // 处理所有待处理事件
        loop {
            match rx.try_recv() {
                Ok(WinitEvent::WindowCreated {
                    handle, window, surface,
                    logical_width, logical_height, high_dpi, transparent, aa, present_mode, init_duration,
                }) => {
                    let vw = VireoWindow::new(
                        window, app.gpu.clone(), surface,
                        logical_width, logical_height, high_dpi, transparent, aa, present_mode,
                        init_duration, event_tx.clone(), cb_tx.clone(), handle,
                    );
                    while app.windows.len() <= handle {
                        app.windows.push(None);
                    }
                    app.windows[handle] = Some(vw);
                    if let Some(ref win) = app.windows[handle] {
                        win.preheat(crate::color::Color::new(0.0, 0.0, 0.0, 1.0));
                    }
                    created_windows += 1;
                }

                Ok(WinitEvent::Resized { handle, width, height }) => {
                    if let Some(Some(win)) = app.windows.get_mut(handle) {
                        win.resize(width, height);
                    }
                }

                Ok(WinitEvent::ScaleFactorChanged { handle, scale: _scale }) => {
                    if let Some(Some(win)) = app.windows.get_mut(handle) {
                        let size = win.inner.inner_size();
                        win.resize(size.width, size.height);
                    }
                }

                Ok(WinitEvent::CursorMoved { handle, x, y }) => {
                    if let Some(Some(win)) = app.windows.get_mut(handle) {
                        let sf = if win.high_dpi { 1.0_f64 } else { win.inner.scale_factor() };
                        win.mouse_pos = ((x / sf) as f32, (y / sf) as f32);
                    }
                }

                Ok(WinitEvent::KeyboardInput { handle, event }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let is_pressed = event.state.is_pressed();
                        let repeat = event.repeat;
                        if is_pressed && !repeat {
                            win.input.keys_down.borrow_mut().insert(event.key);
                        } else if !is_pressed {
                            win.input.keys_down.borrow_mut().remove(&event.key);
                        }
                    }
                }

                Ok(WinitEvent::MouseInput { handle, button, pressed }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        if pressed {
                            win.input.mouse_buttons_down.borrow_mut().insert(button);
                        } else {
                            win.input.mouse_buttons_down.borrow_mut().remove(&button);
                        }
                    }
                }

                Ok(WinitEvent::MouseWheel { handle, delta }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let mut acc = win.input.scroll_delta.borrow_mut();
                        match &delta {
                            crate::input::ScrollDelta::Line { x, y } => {
                                acc.line.0 += x; acc.line.1 += y;
                            }
                            crate::input::ScrollDelta::Pixel { x, y } => {
                                acc.pixel.0 += x; acc.pixel.1 += y;
                            }
                        }
                    }
                }

                Ok(WinitEvent::ModifiersChanged { handle, modifiers }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        *win.input.modifiers.borrow_mut() = modifiers;
                    }
                }

                Ok(WinitEvent::Focused { handle, focused }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let was_focused = std::mem::replace(&mut *win.input.focused.borrow_mut(), focused);
                        if !focused && was_focused {
                            win.input.keys_down.borrow_mut().clear();
                            win.input.mouse_buttons_down.borrow_mut().clear();
                        }
                    }
                }

                Ok(WinitEvent::CursorEntered { handle }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        *win.input.cursor_inside.borrow_mut() = true;
                    }
                }

                Ok(WinitEvent::CursorLeft { handle }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        *win.input.cursor_inside.borrow_mut() = false;
                    }
                }

                Ok(WinitEvent::Touch { handle, event }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let sf = if win.high_dpi { 1.0_f64 } else { win.inner.scale_factor() };
                        let tx = (event.x as f64 / sf) as f32;
                        let ty = (event.y as f64 / sf) as f32;
                        match event.phase {
                            crate::input::TouchPhase::Started | crate::input::TouchPhase::Moved => {
                                win.input.touches.borrow_mut().insert(event.id, (tx, ty, event.force));
                            }
                            _ => {
                                win.input.touches.borrow_mut().remove(&event.id);
                            }
                        }
                    }
                }

                Ok(WinitEvent::CloseRequested { handle, .. }) => {
                    if let Some(w) = app.windows.get_mut(handle) {
                        *w = None;
                    }
                }

                // Winit 窗口操作：转发到正确的窗口
                Ok(WinitEvent::SetTitle { handle, title }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_title(&title);
                    }
                }
                Ok(WinitEvent::SetSize { handle, width, height }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let _ = win.inner.request_inner_size(winit::dpi::LogicalSize::new(width, height));
                    }
                }
                Ok(WinitEvent::SetMinSize { handle, width, height }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let size = match (width, height) {
                            (Some(w), Some(h)) => Some(winit::dpi::LogicalSize::new(w, h)),
                            _ => None,
                        };
                        win.inner.set_min_inner_size(size);
                    }
                }
                Ok(WinitEvent::SetMaxSize { handle, width, height }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        let size = match (width, height) {
                            (Some(w), Some(h)) => Some(winit::dpi::LogicalSize::new(w, h)),
                            _ => None,
                        };
                        win.inner.set_max_inner_size(size);
                    }
                }
                Ok(WinitEvent::SetFullscreen { handle, fullscreen }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_fullscreen(fullscreen);
                    }
                }
                Ok(WinitEvent::SetMaximized { handle, maximized }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_maximized(maximized);
                    }
                }
                Ok(WinitEvent::SetMinimized { handle, minimized }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_minimized(minimized);
                    }
                }
                Ok(WinitEvent::SetVisible { handle, visible }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_visible(visible);
                    }
                }
                Ok(WinitEvent::FocusWindow { handle }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.focus_window();
                    }
                }
                Ok(WinitEvent::SetWindowLevel { handle, level }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_window_level(level);
                    }
                }
                Ok(WinitEvent::SetDecorations { handle, decorations }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_decorations(decorations);
                    }
                }
                Ok(WinitEvent::SetIcon { handle, icon }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_window_icon(Some(icon));
                    }
                }
                Ok(WinitEvent::SetCursor { handle, cursor }) => {
                    if let Some(Some(win)) = app.windows.get(handle) {
                        win.inner.set_cursor(cursor);
                    }
                }

                Ok(WinitEvent::Exit) => return,
                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }

        // FPS 统计
        let now = std::time::Instant::now();
        app.frame_count += 1;
        if app.frame_count == 1 {
            app.last_frame = now;
        } else {
            let dt = now.duration_since(app.last_frame).as_secs_f64();
            app.last_frame = now;
            if dt > 0.0 && dt < 0.5 {
                app.frame_time = dt;
                app.fps_samples.push(dt);
                if app.fps_samples.len() > FPS_SAMPLE_CAP {
                    app.fps_samples.remove(0);
                }
                let sum: f64 = app.fps_samples.iter().sum();
                if sum > 0.0 {
                    app.fps = app.fps_samples.len() as f64 / sum;
                }
            }
        }

        // 等所有窗口创建完才开始调用用户代码
        if created_windows >= expected_windows {
            if !(on_frame)(&app) {
                break;
            }
        } else {
            std::thread::yield_now();
        }
    }
}

/// 窗口索引 —— 用于在 run() 闭包中引用窗口。稳定 handle：关窗后该索引失效（`window_ref` 返回 None），
/// 不会因其他窗口关闭而重指向新窗口。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WindowIndex(pub(crate) u64);

impl WindowIndex {
    fn new(handle: u64) -> Self {
        Self(handle)
    }
}

impl App {
    /// 创建无 group 3 的材质（纯 shader，无额外 buffer/纹理绑定）。
    /// pipeline layout 仅含 groups 0–2。
    pub fn material(&self, source: &str) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material(source)
    }

    /// 创建无 group 3 的材质 + 自定义 shape 顶点着色器。
    pub fn material_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu
            .create_material_with_vertex_shader(source, vertex_source)
    }

    /// 带 group 3 资源的材质。引擎按描述符自动生成 BGL + 注入 WGSL + AutoDefaults。
    pub fn material_with_resources(
        &self,
        source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_with_resources(source, resources)
    }

    /// 带 group 3 资源的材质 + 自定义 shape 顶点着色器。
    pub fn material_with_resources_and_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_with_resources_and_vertex_shader(source, vertex_source, resources)
    }

    /// 自定义 BGL 材质。用户自建 BGL + buffer + 每帧 `set_bind_group_provider`。
    /// 见 `examples/custom_material_manual`。
    pub fn material_manual(
        &self,
        source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_manual(source, bgl)
    }

    /// 自定义 BGL 材质 + 自定义 shape 顶点着色器。
    pub fn material_manual_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_manual_with_vertex_shader(source, vertex_source, bgl)
    }

    /// 根据索引获取窗口引用。返回 None 表示窗口已关闭或索引无效。
    pub fn window_ref(&self, idx: &WindowIndex) -> Option<&VireoWindow> {
        self.windows.get(idx.0 as usize).and_then(|w| w.as_ref())
    }

    /// 存活窗口数量
    pub fn window_count(&self) -> usize {
        self.windows.iter().filter(|w| w.is_some()).count()
    }

    /// App::new 内部耗时（秒）：GPU 设备、shader 模块、bind group layout 构造。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    /// 所有存活窗口引用
    pub fn windows(&self) -> Vec<&VireoWindow> {
        self.windows.iter().filter_map(|w| w.as_ref()).collect()
    }

    /// 所有存活窗口索引（与 `window_ref` 配合使用）。
    /// handle 是稳定 id；同一 handle 跨关窗事件不变（关窗后 `window_ref` 返回 None）。
    pub fn window_indices(&self) -> Vec<WindowIndex> {
        self.windows.iter().enumerate()
            .filter(|(_, w)| w.is_some())
            .map(|(i, _)| WindowIndex::new(i as u64))
            .collect()
    }
}

impl VireoWindow {
    /// 该窗口初始化耗时（秒）：app.window() 内的 AA 管线预热。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    /// 获取窗口标题
    pub fn title(&self) -> String {
        self.inner.title()
    }

    /// 设置窗口标题（通过 winit 线程异步操作）
    pub fn set_title(&self, title: &str) {
        let _ = self.event_tx.send(WinitEvent::SetTitle {
            handle: self.handle(),
            title: title.to_string(),
        });
    }

    /// 设置窗口大小（逻辑像素，通过 winit 线程异步操作）
    pub fn set_size(&self, width: u32, height: u32) {
        let _ = self.event_tx.send(WinitEvent::SetSize {
            handle: self.handle(),
            width,
            height,
        });
    }

    /// 设置最小窗口大小（逻辑像素，通过 winit 线程异步操作）
    pub fn set_min_size(&self, width: Option<u32>, height: Option<u32>) {
        let _ = self.event_tx.send(WinitEvent::SetMinSize {
            handle: self.handle(),
            width,
            height,
        });
    }

    /// 设置最大窗口大小（逻辑像素，通过 winit 线程异步操作）
    pub fn set_max_size(&self, width: Option<u32>, height: Option<u32>) {
        let _ = self.event_tx.send(WinitEvent::SetMaxSize {
            handle: self.handle(),
            width,
            height,
        });
    }

    /// 切换全屏模式（通过 winit 线程异步操作）
    pub fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        let _ = self.event_tx.send(WinitEvent::SetFullscreen {
            handle: self.handle(),
            fullscreen,
        });
    }

    /// 最大化窗口（通过 winit 线程异步操作）
    pub fn set_maximized(&self, maximized: bool) {
        let _ = self.event_tx.send(WinitEvent::SetMaximized {
            handle: self.handle(),
            maximized,
        });
    }

    /// 最小化窗口（通过 winit 线程异步操作）
    pub fn set_minimized(&self, minimized: bool) {
        let _ = self.event_tx.send(WinitEvent::SetMinimized {
            handle: self.handle(),
            minimized,
        });
    }

    /// 显示/隐藏窗口（通过 winit 线程异步操作）
    pub fn set_visible(&self, visible: bool) {
        let _ = self.event_tx.send(WinitEvent::SetVisible {
            handle: self.handle(),
            visible,
        });
    }

    /// 获取焦点（通过 winit 线程异步操作）
    pub fn focus(&self) {
        let _ = self.event_tx.send(WinitEvent::FocusWindow {
            handle: self.handle(),
        });
    }

    /// 设置窗口层级（通过 winit 线程异步操作）
    pub fn set_window_level(&self, level: WindowLevel) {
        let _ = self.event_tx.send(WinitEvent::SetWindowLevel {
            handle: self.handle(),
            level,
        });
    }

    /// 设置窗口装饰（标题栏边框，通过 winit 线程异步操作）
    pub fn set_decorations(&self, decorations: bool) {
        let _ = self.event_tx.send(WinitEvent::SetDecorations {
            handle: self.handle(),
            decorations,
        });
    }

    // ------ 事件订阅 API（通过 cb_tx 异步发送到 winit 线程，无需 +Send）------

    pub fn on_key_down(&self, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_key_down.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_key_up(&self, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_key_up.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_mouse_down(&self, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_mouse_down.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_mouse_up(&self, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_mouse_up.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_scroll(&self, callback: impl FnMut(&crate::input::MouseScrollEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_scroll.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_cursor_entered(&self, callback: impl FnOnce() + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_cursor_entered.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_cursor_left(&self, callback: impl FnOnce() + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_cursor_left.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_touch(&self, callback: impl FnMut(&crate::input::TouchEvent) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_touch.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_focus_gained(&self, callback: impl FnOnce() + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_focus_gained.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_focus_lost(&self, callback: impl FnOnce() + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_focus_lost.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    pub fn on_modifiers_changed(&self, callback: impl FnMut(crate::input::Modifiers) + 'static) -> &Self {
        let mut cbs = crate::input::InputCallbacks::default();
        cbs.on_modifiers_changed.push(Box::new(callback));
        let _ = self.cb_tx.send((self.handle, cbs));
        self
    }

    /// 运行时切换 present mode（会 reconfigure surface）。
    /// 下次 `draw` 前应用。
    pub fn set_present_mode(&self, mode: wgpu::PresentMode) {
        self.pending_mode.set(Some(mode));
    }

    /// 当前 present mode（缓存的最后设置值）。
    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.surface_config.borrow().present_mode
    }

    /// 运行时切换抗锯齿（运行时重建 msaa 纹理）。
    /// 下次 `draw` 前应用。
    pub fn set_anti_aliasing(&self, aa: AntiAliasing) {
        let aa = crate::window::clamp_aa(aa, self.gpu.supported_sample_counts());
        self.pending_aa.set(Some(aa));
    }

    /// 窗口 handle（在 App.windows 中的索引）
    pub fn handle(&self) -> usize {
        self.handle
    }

    /// 返回 `WindowIndex`（与 `app.on_key_down` 等注册方法共用）
    pub fn index(&self) -> WindowIndex {
        WindowIndex(self.handle as u64)
    }

    /// 设置窗口图标（通过 winit 线程异步操作）
    pub fn set_icon(&self, icon: Icon) {
        let _ = self.event_tx.send(WinitEvent::SetIcon {
            handle: self.handle(),
            icon,
        });
    }

    /// 设置光标样式（通过 winit 线程异步操作）
    pub fn set_cursor(&self, cursor: winit::window::Cursor) {
        let _ = self.event_tx.send(WinitEvent::SetCursor {
            handle: self.handle(),
            cursor,
        });
    }
}


/// 离屏画布索引
/// 离屏画布索引。handle 是稳定 id（构造顺序，不受其他 canvas 关闭影响）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct OffscreenIndex(pub usize);
