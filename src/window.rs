use std::cell::RefCell;
use std::sync::Arc;
use wgpu::util::DeviceExt;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{WindowId, WindowAttributes},
};

use crate::context::{DrawBatch, RenderTarget};
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

/// 把 AA 模式的 sample_count clamp 到硬件上限。wgpu 在 sample_count > max 时会 panic。
pub(crate) fn clamp_aa(aa: AntiAliasing, max_sample_count: u32) -> AntiAliasing {
    match aa {
        AntiAliasing::None => AntiAliasing::None,
        AntiAliasing::Msaa { samples, alpha_to_coverage } => {
            let s = samples.min(max_sample_count.max(1));
            AntiAliasing::Msaa { samples: s, alpha_to_coverage }
        }
        AntiAliasing::Ssaa { samples, alpha_to_coverage } => {
            let s = samples.min(max_sample_count.max(1));
            AntiAliasing::Ssaa { samples: s, alpha_to_coverage }
        }
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

/// [`VireoWindow::draw_timed`] 返回的分段耗时（秒）。
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawTimings {
    /// `get_current_texture`：可能阻塞等 swapchain 空位 / 上一帧 present。
    pub acquire_secs: f64,
    /// 编码 + `queue.submit`（不含 present）。
    pub encode_secs: f64,
}

/// 窗口实例（胖指针 —— 持有 surface、camera、gpu 引用）
/// 所有公开 API 坐标系为逻辑像素（用户友好），GPU 内部使用物理像素。
pub struct VireoWindow {
    pub inner: Arc<winit::window::Window>,
    pub surface: wgpu::Surface<'static>,
    /// surface 配置（含 present_mode）。用 RefCell 以便 `set_present_mode` 在 `&self` 下更新。
    surface_config: RefCell<wgpu::SurfaceConfiguration>,
    pub gpu: Arc<GpuContext>,
    pub camera_bind_group: wgpu::BindGroup,
    pub mouse_pos: (f32, f32),
    pub logical_width: u32,
    pub logical_height: u32,
    pub high_dpi: bool,
    pub input: InputState,
    /// 该窗口初始化耗时（秒）：app.window() 内的 AA 管线预热。
    /// 窗口在 App::run 中创建后即可读取，固定不变。
    pub init_duration: f64,
    renderer: RefCell<crate::context::Renderer>,
    frame_texture: RefCell<Option<wgpu::SurfaceTexture>>,
}

/// Drop 时排干 frame_texture 中的 SurfaceTexture 并 present，
/// 避免 swapchain drop 时 `Arc::into_inner` panic
/// ("Trying to destroy a SwapchainAcquireSemaphore that is still in use by a SurfaceTexture")。
impl Drop for VireoWindow {
    fn drop(&mut self) {
        if let Some(st) = self.frame_texture.borrow_mut().take() {
            self.gpu.queue.present(st);
        }
    }
}

impl VireoWindow {
    /// 登记绘制（不立即 present）。同一帧内多次调用共用同一张 surface texture。
    /// 首次调用时获取 texture，后续调用 Load 叠加。帧结束时由 App 统一 present。
    pub fn draw(&self, clear_color: Option<crate::color::Color>, batches: &[&DrawBatch]) {
        let _ = self.draw_timed(clear_color, batches);
    }

    /// 与 [`draw`] 相同，并返回分段耗时（秒），用于卡顿诊断。
    ///
    /// - `acquire`：`get_current_texture`（可能等 swapchain / 上一帧 present / vsync）
    /// - `encode`：合并上传 + render pass + `queue.submit`（不含 present）
    pub fn draw_timed(
        &self,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) -> DrawTimings {
        let t0 = std::time::Instant::now();
        let mut ft = self.frame_texture.borrow_mut();
        if ft.is_none() {
            *ft = match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(st) => Some(st),
                _ => {
                    return DrawTimings {
                        acquire_secs: t0.elapsed().as_secs_f64(),
                        encode_secs: 0.0,
                    };
                }
            };
        }
        let acquire_secs = t0.elapsed().as_secs_f64();
        let view = ft.as_ref().unwrap().texture.create_view(&wgpu::TextureViewDescriptor::default());
        let target = RenderTarget::from_texture_view(view);
        drop(ft);

        let t1 = std::time::Instant::now();
        let renderer = self.renderer.borrow();
        target.draw(&renderer, clear_color, batches);
        DrawTimings {
            acquire_secs,
            encode_secs: t1.elapsed().as_secs_f64(),
        }
    }

    /// 强制 GPU 端 PSO 编译（DX12 懒编译需要）。在 `App::run` 的 `resumed` 后调用，
    /// 让首帧的 SDF + geo 管线 PSO 预先生成，避免首帧 hitch。
    /// 副作用：会提交并 present 一帧（clear 颜色），用户可能看到一帧黑屏。下一帧
    /// `request_redraw` 触发时 frame_texture 已空，正常 acquire 新 texture。
    pub fn preheat(&self, clear_color: crate::color::Color) {
        let mut ft = self.frame_texture.borrow_mut();
        if ft.is_none() {
            *ft = match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(st) => Some(st),
                _ => return,
            };
        }
        let view = ft.as_ref().unwrap().texture.create_view(&wgpu::TextureViewDescriptor::default());
        let target = crate::context::RenderTarget::from_texture_view(view);
        drop(ft);

        let renderer = self.renderer.borrow();
        renderer.preheat(&target, clear_color);

        // 立即 present 释放 surface texture（否则 drop 时 wgpu 报 "must be dropped" 错）
        if let Some(st) = self.frame_texture.borrow_mut().take() {
            self.gpu.queue.present(st);
        }
    }

    /// 获取当前鼠标位置（窗口用户坐标系，即 WindowDesc 传入的宽高范围）
    pub fn mouse_pos(&self) -> (f32, f32) {
        self.mouse_pos
    }

    /// 更新鼠标位置（事件循环内部调用，position 为物理像素）
    pub fn update_mouse_pos(&mut self, x: f64, y: f64) {
        if self.high_dpi {
            self.mouse_pos = (x as f32, y as f32);
        } else {
            let sf = self.inner.scale_factor();
            self.mouse_pos = ((x / sf) as f32, (y / sf) as f32);
        }
    }

    /// 调整窗口大小（size 为物理像素，来自 WindowEvent::Resized 事件）
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        {
            let mut cfg = self.surface_config.borrow_mut();
            cfg.width = width;
            cfg.height = height;
        }
        if self.high_dpi {
            self.logical_width = width;
            self.logical_height = height;
        } else {
            let sf = self.inner.scale_factor();
            self.logical_width = (width as f64 / sf) as u32;
            self.logical_height = (height as f64 / sf) as u32;
        }
        let surface_config = self.surface_config.borrow().clone();
        self.surface.configure(&self.gpu.device, &surface_config);

        let scale = if self.high_dpi {
            1.0
        } else {
            self.inner.scale_factor() as f32
        };
        self.renderer.borrow_mut().resize(self.logical_width, self.logical_height, width, height, scale);
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

    /// 获取渲染目标

    /// 手动 present（由 App::run 帧结束时调用）
    fn present(&self) {
        let mut ft = self.frame_texture.borrow_mut();
        if let Some(st) = ft.take() {
            self.gpu.queue.present(st);
        }
    }

    // ------ 输入状态轮询 API ------

    /// 检查指定按键是否被按下
    pub fn key_down(&self, key: crate::input::KeyCode) -> bool {
        self.input.keys_down.borrow().contains(&key)
    }

    /// 检查是否有任意按键被按下
    pub fn any_key_down(&self) -> bool {
        !self.input.keys_down.borrow().is_empty()
    }

    /// 检查鼠标按钮是否被按下
    pub fn mouse_down(&self, button: crate::input::MouseButton) -> bool {
        self.input.mouse_buttons_down.borrow().contains(&button)
    }

    /// 鼠标左键是否被按下
    pub fn mouse_left(&self) -> bool {
        self.mouse_down(crate::input::MouseButton::Left)
    }

    /// 鼠标右键是否被按下
    pub fn mouse_right(&self) -> bool {
        self.mouse_down(crate::input::MouseButton::Right)
    }

    /// 获取当前修饰键状态
    pub fn modifiers(&self) -> crate::input::Modifiers {
        *self.input.modifiers.borrow()
    }

    /// Ctrl 是否被按下
    pub fn ctrl_down(&self) -> bool {
        self.input.modifiers.borrow().ctrl()
    }

    /// Shift 是否被按下
    pub fn shift_down(&self) -> bool {
        self.input.modifiers.borrow().shift()
    }

    /// Alt 是否被按下
    pub fn alt_down(&self) -> bool {
        self.input.modifiers.borrow().alt()
    }

    /// 获取并清零本帧滚轮增量
    pub fn take_scroll(&self) -> (f32, f32) {
        let mut delta = self.input.scroll_delta.borrow_mut();
        let result = *delta;
        delta.0 = 0.0;
        delta.1 = 0.0;
        result
    }

    /// 窗口是否拥有焦点
    pub fn focused(&self) -> bool {
        *self.input.focused.borrow()
    }

    /// 鼠标是否在窗口内
    pub fn cursor_inside(&self) -> bool {
        *self.input.cursor_inside.borrow()
    }

    // ------ 事件订阅 API ------

    /// 注册键盘按下回调
    pub fn on_key_down(&self, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_key_down.push(Box::new(callback));
        self
    }

    /// 注册键盘释放回调
    pub fn on_key_up(&self, callback: impl FnMut(&crate::input::KeyEvent) + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_key_up.push(Box::new(callback));
        self
    }

    /// 注册鼠标按下回调
    pub fn on_mouse_down(&self, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_mouse_down.push(Box::new(callback));
        self
    }

    /// 注册鼠标释放回调
    pub fn on_mouse_up(&self, callback: impl FnMut(&crate::input::MouseButtonEvent) + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_mouse_up.push(Box::new(callback));
        self
    }

    /// 注册滚轮回调
    pub fn on_scroll(&self, callback: impl FnMut(&crate::input::MouseScrollEvent) + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_scroll.push(Box::new(callback));
        self
    }

    /// 注册鼠标进入窗口回调（一次性）
    pub fn on_cursor_entered(&self, callback: impl FnOnce() + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_cursor_entered.push(Box::new(callback));
        self
    }

    /// 注册鼠标离开窗口回调（一次性）
    pub fn on_cursor_left(&self, callback: impl FnOnce() + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_cursor_left.push(Box::new(callback));
        self
    }

    /// 注册获得焦点回调（一次性）
    pub fn on_focus_gained(&self, callback: impl FnOnce() + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_focus_gained.push(Box::new(callback));
        self
    }

    /// 注册失去焦点回调（一次性）
    pub fn on_focus_lost(&self, callback: impl FnOnce() + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_focus_lost.push(Box::new(callback));
        self
    }

    /// 注册修饰键变化回调
    pub fn on_modifiers_changed(&self, callback: impl FnMut(crate::input::Modifiers) + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_modifiers_changed.push(Box::new(callback));
        self
    }

    /// 注册触摸回调
    pub fn on_touch(&self, callback: impl FnMut(&crate::input::TouchEvent) + 'static) -> &Self {
        self.input.callbacks.borrow_mut().on_touch.push(Box::new(callback));
        self
    }
}

/// 应用管理器 —— 管理窗口创建、事件循环和帧驱动
pub struct App {
    pub window_descs: Vec<WindowDesc>,
    pub windows: Vec<VireoWindow>,
    pub gpu: Arc<GpuContext>,
    instance: Option<wgpu::Instance>,
    window_ids: Vec<WindowId>,
    close_hooks: Vec<Option<Box<dyn FnOnce()>>>,
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
    /// `App::run` 创建窗口时按序出队传给 `VireoWindow::init_duration`。
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
            window_ids: Vec::new(),
            close_hooks: Vec::new(),
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
        let max_sc = self.gpu.max_sample_count();
        let aa = crate::window::clamp_aa(aa, max_sc);
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        let idx = self.offscreens.len();
        let offscreen = OffscreenCanvas::with_aa(&self.gpu, width, height, aa, init_duration);
        self.offscreens.push(offscreen);
        OffscreenIndex(idx)
    }

    /// 根据索引获取离屏画布引用
    pub fn offscreen_ref(&self, idx: &OffscreenIndex) -> Option<&OffscreenCanvas> {
        self.offscreens.get(idx.0)
    }

    /// 从文件加载纹理（存储在 App 中管理生命周期），返回纹理索引
    pub fn load_texture(&mut self, path: impl AsRef<std::path::Path>) -> Result<usize, String> {
        let tex = Texture::from_file(path, &self.gpu)?;
        let idx = self.textures.len();
        self.textures.push(tex);
        Ok(idx)
    }

    /// 根据索引获取已加载的纹理
    pub fn texture(&self, index: usize) -> Option<&Texture> {
        self.textures.get(index)
    }

    /// 配置一个待创建的窗口。可选 on_close 钩子在窗口被关闭时调用。必须在 run() 之前调用。
    /// 同步预热窗口 AA 对应的 SDF + geo 管线，并把 AA clamp 到硬件上限（避免 wgpu panic）。
    /// 构造耗时在 `App::run` 创建窗口后由 `VireoWindow::init_duration()` 暴露。
    pub fn window(&mut self, mut desc: WindowDesc, on_close: Option<impl FnOnce() + 'static>) -> WindowIndex {
        let start = std::time::Instant::now();
        let max_sc = self.gpu.max_sample_count();
        let aa = crate::window::clamp_aa(desc.anti_aliasing, max_sc);
        desc.anti_aliasing = aa;
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        self.window_init_durations.push(init_duration);
        let idx = self.window_descs.len();
        self.window_descs.push(desc);
        self.close_hooks.push(on_close.map(|f| Box::new(f) as Box<dyn FnOnce()>));
        WindowIndex(idx)
    }

    /// 启动事件循环。在 resumed 里创建所有窗口，然后每帧调用用户闭包。
    /// 闭包签名: FnMut(&App) -> bool，返回 true 继续循环，false 退出。
    pub fn run<F: FnMut(&App) -> bool + 'static>(mut self, on_frame: F) {
        let event_loop = EventLoop::new().unwrap();

        // 预先取出窗口描述
        let window_descs: Vec<_> = self.window_descs.drain(..).collect();
        let close_hooks: Vec<_> = self.close_hooks.drain(..).collect();

        // 启动期管线预热在 app.window() / app.offscreen() 同步完成，不再在 App::run 入口做。
        // 理由：用户调用 app.window() 时就完成管线编译，App::run 入口不应再有「运行时预热」。

        struct Runner<F: FnMut(&App) -> bool + 'static> {
            on_frame: F,
            app: App,
            window_descs: Vec<WindowDesc>,
            close_hooks: Vec<Option<Box<dyn FnOnce()>>>,
            created: bool,
        }

        impl<F: FnMut(&App) -> bool + 'static> ApplicationHandler for Runner<F> {
            fn resumed(&mut self, event_loop: &ActiveEventLoop) {
                if self.created {
                    return;
                }
                self.created = true;

                let instance = self.app.instance.as_ref().unwrap();

                for (i, desc) in self.window_descs.iter().enumerate() {
                    let size: winit::dpi::Size = match desc.scale_factor_override {
                        Some(_) => winit::dpi::PhysicalSize::new(desc.width, desc.height).into(),
                        None => winit::dpi::LogicalSize::new(desc.width, desc.height).into(),
                    };
                    let attrs = WindowAttributes::default()
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

                    let attrs = if let Some((w, h)) = desc.min_size {
                        attrs.with_min_inner_size(LogicalSize::new(w, h))
                    } else { attrs };
                    let attrs = if let Some((w, h)) = desc.max_size {
                        attrs.with_max_inner_size(LogicalSize::new(w, h))
                    } else { attrs };
                    let attrs = if let Some((x, y)) = desc.position {
                        attrs.with_position(winit::dpi::PhysicalPosition::new(x, y))
                    } else { attrs };
                    let attrs = if let Some(ref fs) = desc.fullscreen {
                        attrs.with_fullscreen(Some(fs.clone()))
                    } else { attrs };
                    let icon = desc.window_icon.as_ref().or(self.app.default_icon.as_ref());
                    let attrs = if let Some(icon) = icon {
                        attrs.with_window_icon(Some(icon.clone()))
                    } else { attrs };
                    let attrs = if let Some(theme) = desc.theme {
                        attrs.with_theme(Some(theme))
                    } else { attrs };
                    let attrs = if let Some((w, h)) = desc.resize_increments {
                        attrs.with_resize_increments(LogicalSize::new(w, h))
                    } else { attrs };

                    let window = Arc::new(
                        event_loop.create_window(attrs).unwrap(),
                    );
                    let surface = instance.create_surface(window.clone()).unwrap();

                    let gpu = self.app.gpu.clone();

                    let size = window.inner_size();
                    let surface_config = wgpu::SurfaceConfiguration {
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        format: gpu.surface_format,
                        width: size.width,
                        height: size.height,
                        present_mode: desc.present_mode,
                        alpha_mode: if desc.transparent {
                            wgpu::CompositeAlphaMode::PreMultiplied
                        } else {
                            wgpu::CompositeAlphaMode::Auto
                        },
                        view_formats: vec![],
                        desired_maximum_frame_latency: 2,
                        color_space: wgpu::SurfaceColorSpace::Auto,
                    };
                    surface.configure(&gpu.device, &surface_config);
                    self.app.window_ids.push(window.id());
                    let init_duration = if i < self.app.window_init_durations.len() {
                        self.app.window_init_durations[i]
                    } else {
                        0.0
                    };
                    self.app.windows.push(Self::build_window(
                        gpu, window, surface, surface_config, desc.width, desc.height,
                        desc.scale_factor_override.is_some(),
                        desc.anti_aliasing,
                        init_duration,
                    ));
                    self.app.close_hooks.push(self.close_hooks[i].take());
                }

                for w in &self.app.windows {
                    // 强制 GPU 端 PSO 编译（DX12 懒编译需要）。
                    w.preheat(crate::color::Color::new(0.0, 0.0, 0.0, 1.0));
                    w.inner.request_redraw();
                }
            }

            fn window_event(
                &mut self,
                event_loop: &ActiveEventLoop,
                window_id: WindowId,
                event: WindowEvent,
            ) {
        use winit::event::ElementState as WinitElementState;

        match event {
            WindowEvent::CloseRequested => {
                if let Some(pos) = self.app.window_ids.iter().position(|id| *id == window_id) {
                    if let Some(hook) = self.app.close_hooks[pos].take() {
                        hook();
                    }
                    self.app.windows.retain(|w| w.inner.id() != window_id);
                }
                if self.app.windows.is_empty() {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(win) = self.app.windows.iter_mut().find(|w| w.inner.id() == window_id) {
                    win.resize(size.width, size.height);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(win) = self.app.windows.iter_mut().find(|w| w.inner.id() == window_id) {
                    win.update_mouse_pos(position.x, position.y);
                }
            }
            WindowEvent::RedrawRequested => {
                let now = std::time::Instant::now();
                self.app.frame_count += 1;
                // 第一帧按 A 语义：没有「上一帧」，frame_time / fps 保持 0；
                // dt（含启动延迟）不入滑动平均。第二帧起才计算真实 dt。
                if self.app.frame_count == 1 {
                    self.app.last_frame = now;
                } else {
                    let dt = now.duration_since(self.app.last_frame).as_secs_f64();
                    self.app.last_frame = now;
                    if dt > 0.0 && dt < 0.5 {
                        self.app.frame_time = dt;
                        self.app.fps_samples.push(dt);
                        if self.app.fps_samples.len() > FPS_SAMPLE_CAP {
                            self.app.fps_samples.remove(0);
                        }
                        let sum: f64 = self.app.fps_samples.iter().sum();
                        if sum > 0.0 {
                            self.app.fps = self.app.fps_samples.len() as f64 / sum;
                        }
                    }
                }

                if (self.on_frame)(&self.app) {
                    for w in &self.app.windows {
                        w.present();
                        w.inner.request_redraw();
                    }
                } else {
                    // 退出前 present 掉所有未 present 的 surface texture，
                    // 避免 swapchain drop 时 Arc::into_inner 失败。
                    for w in &self.app.windows {
                        w.present();
                    }
                    event_loop.exit();
                }
            }

            // ------ 新增输入事件处理 ------

            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if let Some(win) = self.app.windows.iter().find(|w| w.inner.id() == window_id) {
                    let vireo_event = match crate::input::map_key_event(&key_event) {
                        Some(e) => e,
                        None => return,
                    };
                    // 更新状态（忽略 repeat）
                    let is_pressed = vireo_event.state.is_pressed();
                    if is_pressed {
                        if !vireo_event.repeat {
                            win.input.keys_down.borrow_mut().insert(vireo_event.key);
                        }
                    } else {
                        win.input.keys_down.borrow_mut().remove(&vireo_event.key);
                    }
                    // 调用回调
                    let mut callbacks = win.input.callbacks.borrow_mut();
                    if is_pressed {
                        for cb in &mut callbacks.on_key_down {
                            cb(&vireo_event);
                        }
                    } else {
                        for cb in &mut callbacks.on_key_up {
                            cb(&vireo_event);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(win) = self.app.windows.iter().find(|w| w.inner.id() == window_id) {
                    let is_pressed = state == WinitElementState::Pressed;
                    if is_pressed {
                        win.input.mouse_buttons_down.borrow_mut().insert(button);
                    } else {
                        win.input.mouse_buttons_down.borrow_mut().remove(&button);
                    }
                    let vireo_event = crate::input::MouseButtonEvent {
                        button,
                        state,
                    };
                    let mut callbacks = win.input.callbacks.borrow_mut();
                    if is_pressed {
                        for cb in &mut callbacks.on_mouse_down {
                            cb(&vireo_event);
                        }
                    } else {
                        for cb in &mut callbacks.on_mouse_up {
                            cb(&vireo_event);
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(win) = self.app.windows.iter().find(|w| w.inner.id() == window_id) {
                    let mapped = crate::input::map_scroll_delta(delta);
                    let (dx, dy) = match &mapped {
                        crate::input::ScrollDelta::Line { x, y } => (*x, *y),
                        crate::input::ScrollDelta::Pixel { x, y } => (*x, *y),
                    };
                    {
                        let mut acc = win.input.scroll_delta.borrow_mut();
                        acc.0 += dx;
                        acc.1 += dy;
                    }
                    let vireo_event = crate::input::MouseScrollEvent { delta: mapped };
                    for cb in &mut win.input.callbacks.borrow_mut().on_scroll {
                        cb(&vireo_event);
                    }
                }
            }
            WindowEvent::ModifiersChanged(state) => {
                if let Some(win) = self.app.windows.iter().find(|w| w.inner.id() == window_id) {
                    let m = crate::input::map_modifiers(&state.state());
                    *win.input.modifiers.borrow_mut() = m;
                    for cb in &mut win.input.callbacks.borrow_mut().on_modifiers_changed {
                        cb(m);
                    }
                }
            }
            WindowEvent::CursorEntered { .. } => {
                if let Some(win) = self.app.windows.iter().find(|w| w.inner.id() == window_id) {
                    *win.input.cursor_inside.borrow_mut() = true;
                    for cb in win.input.callbacks.borrow_mut().on_cursor_entered.drain(..) {
                        cb();
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(win) = self.app.windows.iter().find(|w| w.inner.id() == window_id) {
                    *win.input.cursor_inside.borrow_mut() = false;
                    for cb in win.input.callbacks.borrow_mut().on_cursor_left.drain(..) {
                        cb();
                    }
                }
            }
            WindowEvent::Focused(focused) => {
                if let Some(win) = self.app.windows.iter().find(|w| w.inner.id() == window_id) {
                    *win.input.focused.borrow_mut() = focused;
                    let mut cb = win.input.callbacks.borrow_mut();
                    if focused {
                        for c in cb.on_focus_gained.drain(..) {
                            c();
                        }
                    } else {
                        for c in cb.on_focus_lost.drain(..) {
                            c();
                        }
                    }
                }
            }
            WindowEvent::Touch(touch) => {
                if let Some(win) = self.app.windows.iter().find(|w| w.inner.id() == window_id) {
                    let sf = if win.high_dpi {
                        1.0
                    } else {
                        win.inner.scale_factor()
                    };
                    let vireo_event = crate::input::map_touch_event(&touch, sf);
                    match vireo_event.phase {
                        crate::input::TouchPhase::Started | crate::input::TouchPhase::Moved => {
                            win.input.touches.borrow_mut().insert(
                                touch.id,
                                (vireo_event.x, vireo_event.y, vireo_event.force),
                            );
                        }
                        _ => {
                            win.input.touches.borrow_mut().remove(&touch.id);
                        }
                    }
                    for cb in &mut win.input.callbacks.borrow_mut().on_touch {
                        cb(&vireo_event);
                    }
                }
            }
            _ => {}
        }
            }
        }

        impl<F: FnMut(&App) -> bool + 'static> Runner<F> {
            fn build_window(
                gpu: Arc<GpuContext>,
                window: Arc<winit::window::Window>,
                surface: wgpu::Surface<'static>,
                surface_config: wgpu::SurfaceConfiguration,
                logical_width: u32,
                logical_height: u32,
                high_dpi: bool,
                aa: AntiAliasing,
                init_duration: f64,
        ) -> VireoWindow {
                let render_scale = if high_dpi { 1.0 } else { window.scale_factor() as f32 };
                let dpi = window.scale_factor() as f32;
                let renderer = crate::context::Renderer::new(
                    gpu.clone(),
                    logical_width,
                    logical_height,
                    window.inner_size().width,
                    window.inner_size().height,
                    render_scale,
                    aa,
                    dpi,
                );

                VireoWindow {
                    inner: window,
                    surface,
                    surface_config: RefCell::new(surface_config),
                    gpu: gpu.clone(),
                    camera_bind_group: gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("camera bind group"),
                        layout: &gpu.camera_bind_group_layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("camera buffer"),
                                contents: bytemuck::cast_slice(&[[0.0f32; 4]; 4]),
                                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            }).as_entire_binding(),
                        }],
                    }),
                    mouse_pos: (-1.0, -1.0),
                    logical_width,
                    logical_height,
                    high_dpi,
                    input: InputState::default(),
                    init_duration,
                    renderer: RefCell::new(renderer),
                    frame_texture: RefCell::new(None),
                }
            }
        }

        event_loop
            .run_app(&mut Runner {
                on_frame,
                app: App {
                    window_descs: Vec::new(),
                    windows: Vec::new(),
                    gpu: self.gpu.clone(),
                    instance: self.instance.take(),
                    window_ids: Vec::new(),
                    close_hooks: Vec::new(),
                    default_icon: self.default_icon.take(),
                    textures: self.textures.drain(..).collect(),
                    offscreens: self.offscreens.drain(..).collect(),
                    frame_count: 0,
                    frame_time: 0.0,
                    fps: 0.0,
                    init_duration: self.init_duration,
                    window_init_durations: std::mem::take(&mut self.window_init_durations),
                    fps_samples: Vec::with_capacity(FPS_SAMPLE_CAP),
                    last_frame: std::time::Instant::now(),
                },
                window_descs,
                close_hooks,
                created: false,
            })
            .unwrap();
    }
}

/// 窗口索引 —— 用于在 run() 闭包中引用窗口
#[derive(Clone, Copy)]
pub struct WindowIndex(usize);

impl App {
    /// 根据索引获取窗口引用。返回 None 表示窗口已关闭。
    pub fn window_ref(&self, idx: &WindowIndex) -> Option<&VireoWindow> {
        let id = self.window_ids.get(idx.0)?;
        self.windows.iter().find(|w| w.inner.id() == *id)
    }

    /// 存活窗口数量
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// App::new 内部耗时（秒）：GPU 设备、shader 模块、bind group layout 构造。
    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    /// 所有存活窗口
    pub fn windows(&self) -> &[VireoWindow] {
        &self.windows
    }

    /// 所有存活窗口索引（与遍历 windows() 配合使用）
    pub fn window_indices(&self) -> Vec<WindowIndex> {
        (0..self.windows.len()).map(|i| WindowIndex(i)).collect()
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

    /// 设置窗口标题
    pub fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    /// 设置窗口大小（逻辑像素）
    pub fn set_size(&self, width: u32, height: u32) {
        let _ = self.inner.request_inner_size(winit::dpi::LogicalSize::new(width, height));
    }

    /// 设置最小窗口大小（逻辑像素）
    pub fn set_min_size(&self, width: Option<u32>, height: Option<u32>) {
        let size = match (width, height) {
            (Some(w), Some(h)) => Some(winit::dpi::LogicalSize::new(w, h)),
            _ => None,
        };
        self.inner.set_min_inner_size(size);
    }

    /// 设置最大窗口大小（逻辑像素）
    pub fn set_max_size(&self, width: Option<u32>, height: Option<u32>) {
        let size = match (width, height) {
            (Some(w), Some(h)) => Some(winit::dpi::LogicalSize::new(w, h)),
            _ => None,
        };
        self.inner.set_max_inner_size(size);
    }

    /// 切换全屏模式
    pub fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        self.inner.set_fullscreen(fullscreen);
    }

    /// 最大化窗口
    pub fn set_maximized(&self, maximized: bool) {
        self.inner.set_maximized(maximized);
    }

    /// 最小化窗口
    pub fn set_minimized(&self, minimized: bool) {
        self.inner.set_minimized(minimized);
    }

    /// 显示/隐藏窗口
    pub fn set_visible(&self, visible: bool) {
        self.inner.set_visible(visible);
    }

    /// 获取焦点
    pub fn focus(&self) {
        self.inner.focus_window();
    }

    /// 设置窗口层级
    pub fn set_window_level(&self, level: WindowLevel) {
        self.inner.set_window_level(level);
    }

    /// 设置窗口装饰（标题栏边框）
    pub fn set_decorations(&self, decorations: bool) {
        self.inner.set_decorations(decorations);
    }

    /// 运行时切换 present mode（会 reconfigure surface）。
    /// 调用前会 present 掉未提交的 surface texture，避免 "must be dropped before configure"。
    pub fn set_present_mode(&self, mode: wgpu::PresentMode) {
        if let Some(st) = self.frame_texture.borrow_mut().take() {
            self.gpu.queue.present(st);
        }
        let cfg = {
            let mut cfg = self.surface_config.borrow_mut();
            cfg.present_mode = mode;
            cfg.clone()
        };
        self.surface.configure(&self.gpu.device, &cfg);
    }

    /// 当前 present mode。
    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.surface_config.borrow().present_mode
    }

    /// 切换抗锯齿（运行时重建 msaa 纹理）。同时预热新 AA 对应的 SDF + geo 管线，
    /// 避免切换后首帧的 shader 编译 hitch。
    /// 若请求的 sample_count 超过硬件上限，会自动 clamp 到 `gpu.max_sample_count()`，
    /// 避免 wgpu `create_texture` / `create_render_pipeline` panic。
    /// 若请求的 sample_count 超过硬件上限，会自动 clamp 并 `eprintln!` 警告。
    pub fn set_anti_aliasing(&self, aa: AntiAliasing) {
        let max_sc = self.gpu.max_sample_count();
        let requested_sc = aa.sample_count();
        let aa = clamp_aa(aa, max_sc);
        if requested_sc > max_sc {
            eprintln!(
                "vireo: AA sample_count {}x > hardware max {}x, clamped to {}x",
                requested_sc, max_sc, max_sc
            );
        }
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        self.renderer.borrow_mut().update_aa(aa);
    }

    /// 设置窗口图标
    pub fn set_icon(&self, icon: Icon) {
        self.inner.set_window_icon(Some(icon));
    }

    /// 设置光标样式
    pub fn set_cursor(&self, cursor: winit::window::Cursor) {
        self.inner.set_cursor(cursor);
    }
}


/// 离屏画布索引
#[derive(Clone, Copy)]
pub struct OffscreenIndex(usize);
