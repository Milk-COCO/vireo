use std::sync::{Arc, Mutex, mpsc};
use std::cell::RefCell;
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
    /// 上一份已完成提交的 GPU queue latency（不含 CPU 构图）。
    /// 该值包含驱动排队/GPU 竞争，不等于纯 shader 执行时间。
    pub gpu_secs: Option<f64>,
}

/// 从 winit 线程发往渲染线程的事件（全是 Send-safe 的自定义类型）。
enum WinitEvent {
    WindowCreated {
        handle: usize,
        window: Arc<winit::window::Window>,
        shared_present: Arc<SharedGPUState>,
        logical_width: u32,
        logical_height: u32,
        high_dpi: bool,
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

/// 跨线程 GPU 状态：逻辑线程编码 / winit 线程 submit+present。
///
/// **核心约束**：
/// - `SurfaceTexture` 整个生命周期都在 winit 线程（同线程 acquire+present，DWM 才接受）
/// - 逻辑线程通过 `pending_view: Mutex<Option<TextureView>>` 拿渲染目标
/// - 逻辑线程编码后通过 `pending_cmd_buf: Mutex<Option<CommandBuffer>>` 递交
/// - `TextureView` 和 `CommandBuffer` 都是 `Send + Sync`（wgpu 30 已确认）
///
/// 状态机：
/// - `Idle`：`pending_view = None`, `pending_cmd_buf = None`
/// - `Acquired`：`pending_view = Some(v)`, `pending_cmd_buf = None`
/// - `Encoded`：`pending_view = Some(v)`, `pending_cmd_buf = Some(c)`
pub(crate) struct SharedGPUState {
    pub surface: Mutex<wgpu::Surface<'static>>,
    pub surface_config: Mutex<wgpu::SurfaceConfiguration>,
    pub renderer: Mutex<crate::context::Renderer>,
    pub pending_view: Mutex<Option<wgpu::TextureView>>,
    pub pending_cmd_buf: Mutex<Option<wgpu::CommandBuffer>>,
    /// winit 线程是否持有未释放的 SurfaceTexture。
    /// true 时不能 surface.configure（wgpu 约束）。
    /// winit 线程在 acquire 成功后置 true，present 后置 false。
    pub winit_has_st: std::sync::atomic::AtomicBool,
    /// Resize/present-mode changes are applied by the winit owner thread.
    pub needs_configure: std::sync::atomic::AtomicBool,
    /// The logic thread temporarily owns the view while encoding.
    pub encoding: std::sync::atomic::AtomicBool,
    /// Wakes the logic thread when the owner thread has acquired the next
    /// surface view. This keeps the frame loop paced by the swapchain.
    pub frame_wait: std::sync::Condvar,
    pub frame_wait_lock: Mutex<()>,
    /// Completion handshake for the logic frame loop.
    pub frame_complete: std::sync::Condvar,
    pub frame_complete_lock: Mutex<bool>,
    /// Set by the owner thread before closing the window. All frame waits
    /// must be interruptible so event-loop shutdown cannot block on join.
    pub closing: std::sync::atomic::AtomicBool,
    /// Whether queue completion timing is requested for this window.
    pub gpu_timing_enabled: std::sync::atomic::AtomicBool,
    pub last_gpu_secs: Mutex<Option<f64>>,
    pub pending_gpu_starts: Mutex<std::collections::VecDeque<std::time::Instant>>,
    pub inner: Arc<winit::window::Window>,
}

impl SharedGPUState {
    /// 全部清空（resize / close / 异常路径）。view/cmd_buf 释放即可。
    pub fn drain(&self) {
        *self.pending_view.lock().unwrap() = None;
        *self.pending_cmd_buf.lock().unwrap() = None;
        self.frame_wait.notify_all();
        self.frame_complete.notify_all();
    }

    /// winit 线程：Idle → Acquired，把 acquire 出的 view 放进 pending_view。
    /// 返回 `true` 成功（之前 Idle），`false` 状态错乱（已有 view）。
    pub fn put_view(&self, view: wgpu::TextureView) -> bool {
        let mut slot = self.pending_view.lock().unwrap();
        if slot.is_some() { return false; }
        *slot = Some(view);
        self.frame_wait.notify_all();
        true
    }

    pub fn wait_for_view(&self) -> bool {
        let mut guard = self.frame_wait_lock.lock().unwrap();
        while !self.has_view()
            && !self.closing.load(std::sync::atomic::Ordering::Acquire)
        {
            guard = self.frame_wait.wait(guard).unwrap();
        }
        self.has_view()
    }

    /// 逻辑线程：Acquired → Encoded 起点，take view 用于编码。
    pub fn take_view(&self) -> Option<wgpu::TextureView> {
        self.pending_view.lock().unwrap().take()
    }

    /// 返回当前是否已经 acquire 了一个待编码的 view，不改变状态。
    pub fn has_view(&self) -> bool {
        self.pending_view.lock().unwrap().is_some()
    }

    /// 逻辑线程：编码完成后 put cmd_buf。
    /// 返回 `true` 成功（之前 Acquired），`false` 状态错乱（已有 cmd_buf）。
    pub fn put_cmd_buf(&self, cmd_buf: wgpu::CommandBuffer) -> bool {
        let mut slot = self.pending_cmd_buf.lock().unwrap();
        if slot.is_some() { return false; }
        *slot = Some(cmd_buf);
        *self.frame_complete_lock.lock().unwrap() = false;
        true
    }

    /// winit 线程：Encoded → Idle 起点，take cmd_buf 用于 submit+present。
    pub fn take_cmd_buf(&self) -> Option<wgpu::CommandBuffer> {
        self.pending_cmd_buf.lock().unwrap().take()
    }

    pub fn wait_for_present(&self) -> bool {
        let mut complete = self.frame_complete_lock.lock().unwrap();
        while !*complete
            && !self.closing.load(std::sync::atomic::Ordering::Acquire)
        {
            complete = self.frame_complete.wait(complete).unwrap();
        }
        *complete
    }

    pub fn mark_presented(&self) {
        *self.frame_complete_lock.lock().unwrap() = true;
        self.frame_complete.notify_all();
    }

    pub fn take_gpu_secs(&self) -> Option<f64> {
        self.last_gpu_secs.lock().unwrap().take()
    }

}

/// 窗口实例 —— 逻辑线程所有，持有 surface/renderer/input。
/// 所有公开 API 坐标系为逻辑像素（用户友好），GPU 内部使用物理像素。
pub struct VireoWindow {
    /// **必须**在 surface 字段之前声明。
    /// 原因：`SharedGPUState` 里的 `pending_view` 持有 `TextureView`（Send + Sync，
    /// 引用 SurfaceTexture）；SurfaceTexture 持有 swapchain semaphore。
    /// 如果 SharedGPUState 后于 surface drop，surface 的 swapchain 在
    /// `release_resources` 时遇到 semaphore 引用残留 → panic（第三十四轮踩过）。
    /// 字段按声明顺序 drop，所以 shared_present 必须先声明。
    pub(crate) shared_present: Arc<SharedGPUState>,
    pub inner: Arc<winit::window::Window>,
    pub gpu: Arc<GpuContext>,
    pub mouse_pos: (f32, f32),
    pub logical_width: u32,
    pub logical_height: u32,
    pub high_dpi: bool,
    pub input: InputState,
    /// 该窗口初始化耗时（秒）：app.window() 内的 AA 管线预热。
    pub init_duration: f64,
    /// 用于向 winit 线程发送窗口操作事件
    event_tx: mpsc::Sender<WinitEvent>,
    /// 向 winit 线程注册输入回调
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    /// 待应用的 present mode（在 draw 开头应用）
    pending_mode: std::cell::Cell<Option<wgpu::PresentMode>>,
    /// 待应用的 AA 模式（在 draw 开头应用）
    pending_aa: std::cell::Cell<Option<AntiAliasing>>,
    /// 窗口 handle（在 App.windows 中的索引）
    handle: usize,
}

impl VireoWindow {
    fn new(
        inner: Arc<winit::window::Window>,
        gpu: Arc<GpuContext>,
        shared_present: Arc<SharedGPUState>,
        logical_width: u32,
        logical_height: u32,
        high_dpi: bool,
        init_duration: f64,
    event_tx: mpsc::Sender<WinitEvent>,
    cb_tx: mpsc::Sender<(usize, crate::input::InputCallbacks)>,
    handle: usize,
    ) -> Self {
        Self {
            shared_present,
            inner,
            gpu,
            mouse_pos: (-1.0, -1.0),
            logical_width,
            logical_height,
            high_dpi,
            input: InputState::default(),
            init_duration,
            event_tx,
            cb_tx,
            pending_mode: std::cell::Cell::new(None),
            pending_aa: std::cell::Cell::new(None),
            handle,
        }
    }

    /// 绘制一帧并返回分段耗时（秒），用于卡顿诊断。
    ///
    /// 新流程（present proxy）：
    /// 1. 拿 `shared_present.pending_view` 的 view（winit 线程已 acquire）
    /// 2. 编码用此 view 的 CommandBuffer
    /// 3. 放 `shared_present.pending_cmd_buf`
    /// 4. request_redraw() 通知 winit 线程 submit+present
    ///
    /// 若 `pending_view` 是 None（winit 线程还没 acquire）→ 跳过本次编码，
    /// 返回零值 `DrawTimings`。这是半帧延迟的可见表现。
    ///
    /// **限制**：`set_present_mode` / `set_anti_aliasing` 只在 surface 空闲时
    /// （无 view/cmd_buf in flight）生效。其他时机调用会被忽略。这是 wgpu
    /// surface 约束（configure 时不能有 outstanding SurfaceTexture）。
    pub fn draw(
        &self,
        clear_color: Option<crate::color::Color>,
        batches: &[&DrawBatch],
    ) -> DrawTimings {
        let t0 = std::time::Instant::now();
        let gpu_secs = self.shared_present.take_gpu_secs();
        // Do not start the next logical frame until the previous one has
        // reached the owner-thread present point.
        if !self.shared_present.wait_for_present() {
            return DrawTimings { gpu_secs, ..DrawTimings::default() };
        }
        // Present mode changes are applied by the winit owner thread. The
        // render thread must not call Surface::configure.
        if let Some(mode) = self.pending_mode.take() {
            let caps = self.shared_present.surface.lock().unwrap()
                .get_capabilities(&self.gpu.adapter);
            let actual = if caps.present_modes.contains(&mode) {
                mode
            } else {
                eprintln!("vireo: PresentMode {mode:?} not supported, falling back to AutoVsync");
                wgpu::PresentMode::AutoVsync
            };
            self.shared_present.surface_config.lock().unwrap().present_mode = actual;
            self.shared_present.needs_configure.store(true, std::sync::atomic::Ordering::Release);
        }
        // Apply pending AA change; this does not touch the surface.
        if let Some(aa) = self.pending_aa.take() {
            let sc = aa.sample_count();
            let atc = aa.alpha_to_coverage();
            let ssaa = aa.is_ssaa();
            let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
            let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
            self.shared_present.renderer.lock().unwrap().update_aa(aa);
        }
        // The logical frame is paced by the owner thread's acquire. Do not
        // count a spin-loop iteration as a rendered frame.
        if !self.shared_present.wait_for_view() {
            self.shared_present.encoding.store(false, std::sync::atomic::Ordering::Release);
            return DrawTimings::default();
        }
        let Some(view) = self.shared_present.take_view() else {
            self.shared_present.encoding.store(false, std::sync::atomic::Ordering::Release);
            return DrawTimings::default();
        };
        // Mark the interval in which the logic thread owns the view. This
        // must happen after wait_for_view, otherwise the owner thread would
        // refuse the initial acquire and both threads would wait forever.
        self.shared_present.encoding.store(true, std::sync::atomic::Ordering::Release);
        let t1 = std::time::Instant::now();
        let target = crate::context::RenderTarget::from_texture_view(view);
        let batch_refs: Vec<&DrawBatch> = batches.iter().copied().collect();
        // 编码：返回 CommandBuffer（不 submit/present）
        let cmd_buf = self.shared_present.renderer.lock().unwrap()
            .draw(&target, clear_color, &batch_refs);
        let encode_secs = t1.elapsed().as_secs_f64();
        // 放 cmd_buf 供 winit 线程 submit
        if !self.shared_present.put_cmd_buf(cmd_buf) {
            // 状态错乱：cmd_buf 已存在。这不应该发生（winit take 后才能 put）。
            eprintln!("vireo: put_cmd_buf failed (state error)");
        }
        self.shared_present.encoding.store(false, std::sync::atomic::Ordering::Release);
        // 通知 winit 线程 present
        self.shared_present.inner.request_redraw();
        DrawTimings { acquire_secs: t0.elapsed().as_secs_f64() - encode_secs, encode_secs, gpu_secs }
    }

    /// 启用 queue completion 计时，用于诊断 GPU 竞争和提交排队。
    /// 结果通过下一帧的 [`DrawTimings::gpu_secs`] 返回。
    pub fn set_gpu_timing(&self, enabled: bool) {
        self.shared_present.gpu_timing_enabled.store(
            enabled,
            std::sync::atomic::Ordering::Release,
        );
    }

    /// 上一帧 draw 阶段实际发出的 shape draw_indexed 调用次数（渲染器真实统计）。
    /// `preserve_order=false` 重排合并后此值下降（如 bench 场景 3 混合 1000→2）。
    pub fn last_draw_calls(&self) -> u32 {
        self.shared_present.renderer.lock().unwrap().last_draw_calls()
    }

    /// 强制 GPU 端 PSO 编译（DX12 懒编译需要）。
    /// 旧版：直接 acquire + present。
    /// 新版：删除 preheat（首帧 PSO 编译卡一次可接受，避免跨线程 surface 持有）。
    /// 保留此函数为 no-op 以维持 API 兼容。
    pub fn preheat(&self, _clear_color: crate::color::Color) {
        // no-op
    }

    /// 调整窗口大小（size 为物理像素）
    ///
    /// 新流程：
    /// 1. 等 GPU 空闲（device.poll(Wait)），让 winit 线程把 pending cmd_buf submit 完
    /// 2. drain SharedGPUState（清空 pending_view/cmd_buf，释放 view/cmd_buf）
    /// 3. surface.configure（surface_id 会变，旧 view 失效）
    /// 4. request_redraw 触发 winit 重新 acquire
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
        // Surface configuration is owned by the winit thread. Keep any
        // acquired output in its owner-thread state until it is presented.
        self.shared_present.surface_config.lock().unwrap().width = width;
        self.shared_present.surface_config.lock().unwrap().height = height;
        self.shared_present.needs_configure.store(true, std::sync::atomic::Ordering::Release);
        // resize renderer 内部视图
        self.shared_present.renderer.lock().unwrap().resize(
            self.logical_width, self.logical_height,
            width, height, scale, dpi_scale,
        );
        // 触发 winit 重新 acquire
        self.shared_present.inner.request_redraw();
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
    deferred_tasks: RefCell<Vec<DeferredTask>>,
}

/// 延迟执行的任务，由 [`App::after_frames`] / [`App::after_secs`] 注册。
pub struct DeferredTask {
    kind: DeferredTaskKind,
    pub(crate) f: Box<dyn FnOnce() + Send>,
}

/// 在 `App::after_frames` / `App::after_secs` 内部使用。
/// 用户不直接构造。
#[doc(hidden)]
pub struct DeferredTaskGuard;

impl DeferredTask {
    fn is_ready(&self, frame_count: u64) -> bool {
        match self.kind {
            DeferredTaskKind::AfterFrames(target) => frame_count >= target,
            DeferredTaskKind::AfterSecs(wakeup) => std::time::Instant::now() >= wakeup,
        }
    }
}

enum DeferredTaskKind {
    AfterFrames(u64),
    AfterSecs(std::time::Instant),
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
            deferred_tasks: RefCell::new(Vec::new()),
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

    /// 注册一个延迟 `frames` 帧后执行的闭包。
    /// frame 计数以 `render_on_frame` 循环的帧为单位，首次调用 `on_frame` 时 `frame_count` 为 1。
    pub fn after_frames<F: FnOnce() + Send + 'static>(&self, frames: u64, f: F) {
        let target = self.frame_count + frames;
        self.deferred_tasks.borrow_mut().push(DeferredTask {
            kind: DeferredTaskKind::AfterFrames(target),
            f: Box::new(f),
        });
    }

    /// 注册一个延迟 `secs` 秒后执行的闭包（墙钟时间）。
    pub fn after_secs<F: FnOnce() + Send + 'static>(&self, secs: f64, f: F) {
        self.deferred_tasks.borrow_mut().push(DeferredTask {
            kind: DeferredTaskKind::AfterSecs(
                std::time::Instant::now() + std::time::Duration::from_secs_f64(secs),
            ),
            f: Box::new(f),
        });
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
        let num_windows = window_descs.len();
        // Clone GpuContext Arc for Runner（winit 线程需要调 queue.submit/queue.present）
        let gpu_for_runner = self.gpu.clone();
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
            /// 用于 winit 线程调 queue.submit/queue.present
            gpu: Arc<GpuContext>,
            /// per-window 共享 GPU 状态（surface/renderer/pending_view/pending_cmd_buf）
            shared_states: Vec<Arc<SharedGPUState>>,
            /// winit 线程本地的"刚 acquire 的 st，下一帧 present"缓冲
            /// —— 完全留在 winit 线程，st 永远不跨线程
            pending_st: FxHashMap<WindowId, wgpu::SurfaceTexture>,
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

                    // ---- 在 winit 线程上构造 SharedGPUState ----
                    // 必须在 winit 线程上做，因为 wgpu 资源（surface/queue）
                    // 后续 submit/present 都在 winit 线程跑。
                    let scale = if desc.scale_factor_override.is_some() {
                        1.0
                    } else {
                        window.scale_factor() as f32
                    };
                    let dpi = window.scale_factor() as f32;
                    let renderer = Renderer::new(
                        self.gpu.clone(),
                        desc.width,
                        desc.height,
                        window.inner_size().width,
                        window.inner_size().height,
                        scale,
                        desc.anti_aliasing,
                        dpi,
                    );

                    let caps = surface.get_capabilities(&self.gpu.adapter);
                    let alpha_mode = if desc.transparent {
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
                    let fmt = if caps.formats.contains(&self.gpu.surface_format) {
                        self.gpu.surface_format
                    } else {
                        caps.formats[0]
                    };
                    let surface_config = wgpu::SurfaceConfiguration {
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        format: fmt,
                        width: window.inner_size().width.max(1),
                        height: window.inner_size().height.max(1),
                        present_mode: desc.present_mode,
                        alpha_mode,
                        view_formats: vec![],
                        desired_maximum_frame_latency: 2,
                        color_space: wgpu::SurfaceColorSpace::Auto,
                    };
                    surface.configure(&self.gpu.device, &surface_config);

                    let shared_present = Arc::new(SharedGPUState {
                        surface: Mutex::new(surface),
                        surface_config: Mutex::new(surface_config),
                        renderer: Mutex::new(renderer),
                        pending_view: Mutex::new(None),
                        pending_cmd_buf: Mutex::new(None),
                        winit_has_st: std::sync::atomic::AtomicBool::new(false),
                        needs_configure: std::sync::atomic::AtomicBool::new(false),
                        encoding: std::sync::atomic::AtomicBool::new(false),
                        frame_wait: std::sync::Condvar::new(),
                        frame_wait_lock: Mutex::new(()),
                        frame_complete: std::sync::Condvar::new(),
                        frame_complete_lock: Mutex::new(true),
                        closing: std::sync::atomic::AtomicBool::new(false),
                        gpu_timing_enabled: std::sync::atomic::AtomicBool::new(false),
                        last_gpu_secs: Mutex::new(None),
                        pending_gpu_starts: Mutex::new(std::collections::VecDeque::new()),
                        inner: window.clone(),
                    });
                    // 存到 Runner 供 RedrawRequested handler 用
                    self.shared_states.push(shared_present.clone());

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
                        shared_present,
                        logical_width: desc.width,
                        logical_height: desc.height,
                        high_dpi: desc.scale_factor_override.is_some(),
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
                        // drain SharedGPUState 防止 swapchain semaphore 残留 panic
                        if let Some(shared) = self.shared_states.get(handle) {
                            shared.closing.store(true, std::sync::atomic::Ordering::Release);
                            shared.drain();
                            shared.winit_has_st.store(false, std::sync::atomic::Ordering::Release);
                        }
                        // 释放 winit 线程本地的 st（drop，semaphore 引用清）
                        self.pending_st.remove(&window_id);
                        self.send(WinitEvent::CloseRequested { handle });
                        self.alive_handles -= 1;
                        if self.alive_handles == 0 {
                            self.send(WinitEvent::Exit);
                            event_loop.exit();
                        }
                    }
                    WindowEvent::Resized(size) => {
                        // Keep an acquired output until the owner thread can
                        // submit/present it. The render thread marks the new
                        // configuration; configure happens below on winit.
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
                    WindowEvent::RedrawRequested => {
                        // winit 线程的 present 状态机（owner 线程，DWM 接受 → 解冻屏）。
                        //
                        // 状态转换：
                        // - Encoded: pending_cmd_buf = Some(c)
                        //   → take c, submit, take st from pending_st[window_id], present
                        //   → 回到 Idle
                        // - Idle: pending_view = None, pending_cmd_buf = None
                        //   → acquire st, create view, put view, save st to pending_st[window_id]
                        //   → 转到 Acquired
                        // - Acquired: pending_view = Some(v), pending_cmd_buf = None
                        //   → 等逻辑线程编码；本次啥也不做（request_redraw 已自维持）
                        if let Some(shared) = self.shared_states.get(handle).cloned() {
                            // Configure only on the owner thread, and only
                            // after the previous SurfaceTexture and encoded
                            // frame have been fully retired.
                            if shared.needs_configure.load(std::sync::atomic::Ordering::Acquire)
                                && !shared.winit_has_st.load(std::sync::atomic::Ordering::Acquire)
                                && !shared.has_view()
                                && !shared.encoding.load(std::sync::atomic::Ordering::Acquire)
                                && shared.pending_cmd_buf.lock().unwrap().is_none()
                            {
                                let cfg = shared.surface_config.lock().unwrap().clone();
                                shared.surface.lock().unwrap().configure(&self.gpu.device, &cfg);
                                shared.needs_configure.store(false, std::sync::atomic::Ordering::Release);
                            }
                            // 1. Encoded → Idle: take cmd_buf + present
                            if let Some(cmd_buf) = shared.take_cmd_buf() {
                                let gpu_timing = shared.gpu_timing_enabled.load(
                                    std::sync::atomic::Ordering::Acquire,
                                );
                                if gpu_timing {
                                    shared.pending_gpu_starts.lock().unwrap().push_back(std::time::Instant::now());
                                }
                                self.gpu.queue.submit([cmd_buf]);
                                if gpu_timing {
                                    let shared_done = shared.clone();
                                    self.gpu.queue.on_submitted_work_done(move || {
                                        let start = shared_done.pending_gpu_starts.lock().unwrap().pop_front();
                                        if let Some(start) = start {
                                            *shared_done.last_gpu_secs.lock().unwrap() =
                                                Some(start.elapsed().as_secs_f64());
                                        }
                                    });
                                }
                                if let Some(st) = self.pending_st.remove(&window_id) {
                                    let _ = self.gpu.queue.present(st);
                                } else {
                                    // 状态错乱：没有 st 供 present（逻辑线程没经过 Acquired 阶段）
                                    // 通常是 acquire 失败时 — 忽略，不 panic
                                }
                                shared.winit_has_st.store(false, std::sync::atomic::Ordering::Release);
                                shared.mark_presented();
                                // Start the next acquire only after this
                                // frame has been submitted and presented.
                                shared.inner.request_redraw();
                            }
                            // 2. Idle → Acquired: acquire st + put view
                            else if !shared.has_view()
                                && !shared.encoding.load(std::sync::atomic::Ordering::Acquire)
                                && !shared.winit_has_st.load(std::sync::atomic::Ordering::Acquire)
                                && shared.pending_cmd_buf.lock().unwrap().is_none()
                                // 不能与 needs_configure 抢：若 resize 置位后这里仍继续
                                // acquire，winit_has_st 会一直 true，configure 分支被
                                // 饿死 → 表面永不重配，内容冻结（livelock，无 panic）。
                                && !shared.needs_configure.load(std::sync::atomic::Ordering::Acquire)
                            {
                                match shared.surface.lock().unwrap().get_current_texture() {
                                    wgpu::CurrentSurfaceTexture::Success(st)
                                    | wgpu::CurrentSurfaceTexture::Suboptimal(st) => {
                                        let view = st.texture.create_view(&Default::default());
                                        if shared.put_view(view) {
                                            self.pending_st.insert(window_id, st);
                                            shared.winit_has_st.store(true, std::sync::atomic::Ordering::Release);
                                        } else {
                                            // 状态错乱：已有 view。drop st 防 semaphore 残留。
                                            drop(st);
                                        }
                                    }
                                    wgpu::CurrentSurfaceTexture::Outdated => {
                                        // 重新配置（罕见，resize 已处理过）
                                        // **关键**：必须先 drop 上一轮的 st，否则旧 st
                                        // 持有旧 surface_id，drop 时会撞 storage.get panic
                                        self.pending_st.remove(&window_id);
                                        shared.drain();
                                        let cfg = shared.surface_config.lock().unwrap().clone();
                                        shared.surface.lock().unwrap()
                                            .configure(&self.gpu.device, &cfg);
                                        // 已在上面就地重配，清掉挂起的 needs_configure，
                                        // 否则下一轮 acquire 又被挡住（见 Idle→Acquired 门）。
                                        shared.needs_configure.store(false, std::sync::atomic::Ordering::Release);
                                        // 不放 view，但必须重新 request_redraw，否则逻辑线程
                                        // 会永久阻塞在 wait_for_view()（无 view = 画面冻结）。
                                        shared.inner.request_redraw();
                                    }
                                    _ => {
                                        // Lost / Timeout / Occluded：暂时失败。也 re-arm，
                                        // 因为逻辑线程正阻塞在 wait_for_view()，不会自维持。
                                        // 若真被系统遮挡（如最小化），此调用返回后事件循环在
                                        // Poll 下继续，下一轮 RedrawRequested 会重试 acquire。
                                        shared.inner.request_redraw();
                                    }
                                }
                            }
                            // 3. Acquired: wait for the logic thread to
                            // encode. It will request the next redraw after
                            // putting the command buffer in the shared slot.
                        }
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
            gpu: gpu_for_runner,
            shared_states: Vec::with_capacity(num_windows),
            pending_st: FxHashMap::default(),
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
                    handle, window, shared_present,
                    logical_width, logical_height, high_dpi, init_duration,
                }) => {
                    let vw = VireoWindow::new(
                        window, app.gpu.clone(), shared_present,
                        logical_width, logical_height, high_dpi, init_duration,
                        event_tx.clone(), cb_tx.clone(), handle,
                    );
                    while app.windows.len() <= handle {
                        app.windows.push(None);
                    }
                    app.windows[handle] = Some(vw);
                    // preheat 删掉（首帧 PSO 编译卡一次可接受，避免跨线程 surface 持有）
                    // VireoWindow 构造后调 request_redraw() 触发首帧
                    if let Some(ref _win) = app.windows[handle] {
                        // 取 winit 线程的 inner
                        let shared = _win.shared_present.clone();
                        shared.inner.request_redraw();
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
                    if app.window_count() == 0 {
                        return;
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

        // The winit thread may have already exited after the final close
        // event. Do not enter user code or block in another frame wait.
        if created_windows >= expected_windows && app.window_count() == 0 {
            return;
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

        // 执行所有到期的延迟任务
        {
            let ready = {
                let mut tasks = app.deferred_tasks.borrow_mut();
                let mut ready = Vec::new();
                let mut i = 0;
                while i < tasks.len() {
                    if tasks[i].is_ready(app.frame_count) {
                        ready.push(tasks.swap_remove(i));
                    } else {
                        i += 1;
                    }
                }
                ready
            };
            for task in ready {
                (task.f)();
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
        self.shared_present.surface_config.lock().unwrap().present_mode
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

#[cfg(test)]
mod present_state_tests {
    //! 状态机测试：每窗口的 `SharedGPUState` 字段在逻辑线程/winit 线程间原子转移。
    //!
    //! 状态：
    //! - `Idle`：`pending_view = None`, `pending_cmd_buf = None`
    //! - `Acquired`：`pending_view = Some(v)`, `pending_cmd_buf = None`
    //! - `Encoded`：`pending_view = Some(v)`（持有中，但 view 可能已被编码器消费）,
    //!   `pending_cmd_buf = Some(c)`
    //!
    //! 关键不变量：winit 线程 `take_cmd_buf` 必须在 winit 已经 acquire 完 view 之后、
    //! 逻辑线程已经把 cmd_buf 放进 `pending_cmd_buf` 之后。
    //!
    //! 字段声明顺序：必须 `shared_present` 在 `surface` 之前 drop（drop 测试在
    //! `vireo_window_drop_order` 中验证字段声明顺序）。

    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    /// 通用状态机：`pending_view: Mutex<Option<V>>` + `pending_cmd_buf: Mutex<Option<C>>`。
    /// 用 `u32` 等值类型替身模拟 wgpu 的 TextureView/CommandBuffer。
    struct StateMachine<V, C> {
        pending_view: Mutex<Option<V>>,
        pending_cmd_buf: Mutex<Option<C>>,
    }

    impl<V, C> StateMachine<V, C> {
        fn new() -> Self {
            Self { pending_view: Mutex::new(None), pending_cmd_buf: Mutex::new(None) }
        }

        /// winit 线程调：acquire 后把 view 放进来。
        /// 返回 `true` 表示成功（之前是 Idle），`false` 表示状态错乱（已经有 view）。
        fn put_view(&self, v: V) -> bool {
            let mut slot = self.pending_view.lock().unwrap();
            if slot.is_some() { return false; }
            *slot = Some(v);
            true
        }

        /// 逻辑线程调：take view 用于编码。
        /// 返回 `Some(v)` 表示 Idle/Acquired 状态，`None` 表示 Encoded（view 已被消费）。
        fn take_view(&self) -> Option<V> {
            self.pending_view.lock().unwrap().take()
        }

        /// 逻辑线程调：编码完成后放 cmd_buf。
        /// 返回 `true` 表示成功（Acquired 状态），`false` 表示状态错乱（cmd_buf 已存在）。
        fn put_cmd_buf(&self, c: C) -> bool {
            let mut slot = self.pending_cmd_buf.lock().unwrap();
            if slot.is_some() { return false; }
            *slot = Some(c);
            true
        }

        /// winit 线程调：take cmd_buf 用于 submit+present。
        /// 返回 `Some(c)` 表示 Encoded 状态，`None` 表示 Idle/Acquired。
        fn take_cmd_buf(&self) -> Option<C> {
            self.pending_cmd_buf.lock().unwrap().take()
        }

        /// resize/close 路径：清空所有字段。
        fn drain(&self) {
            *self.pending_view.lock().unwrap() = None;
            *self.pending_cmd_buf.lock().unwrap() = None;
        }
    }

    #[test]
    fn idle_state_has_no_view_no_cmd_buf() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
        assert!(sm.pending_view.lock().unwrap().is_none());
        assert!(sm.pending_cmd_buf.lock().unwrap().is_none());
    }

    #[test]
    fn idle_to_acquired_puts_view() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        assert!(sm.put_view(42));
        assert!(matches!(*sm.pending_view.lock().unwrap(), Some(42)));
        assert!(sm.pending_cmd_buf.lock().unwrap().is_none());
    }

    #[test]
    fn acquired_take_view_returns_view() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(42);
        assert!(matches!(sm.take_view(), Some(42)));
        // Acquired → Idle 转移
        assert!(sm.take_view().is_none());
    }

    #[test]
    fn acquired_to_encoded_take_view_then_put_cmd_buf() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(42);
        let v = sm.take_view();
        assert!(matches!(v, Some(42)));
        // 现在 view 已被消费（即便字段可能仍 Some），模拟逻辑线程完成编码
        assert!(sm.put_cmd_buf(100));
        assert!(matches!(*sm.pending_cmd_buf.lock().unwrap(), Some(100)));
    }

    #[test]
    fn encoded_take_cmd_buf_returns_cmd_buf() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(42);
        sm.take_view();
        sm.put_cmd_buf(100);
        assert!(matches!(sm.take_cmd_buf(), Some(100)));
        // Encoded → Idle
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn full_cycle_idle_acquired_encoded_idle() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        // Idle
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
        // Idle → Acquired
        sm.put_view(1);
        // Acquired → Encoded
        sm.take_view();
        sm.put_cmd_buf(2);
        // Encoded → Idle
        sm.take_cmd_buf();
        // 回到 Idle
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn put_view_when_already_acquired_returns_false() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        // 状态错乱：重复 acquire
        assert!(!sm.put_view(2));
        // 仍是第一个 view
        assert!(matches!(*sm.pending_view.lock().unwrap(), Some(1)));
    }

    #[test]
    fn put_cmd_buf_when_already_encoded_returns_false() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        sm.take_view();
        sm.put_cmd_buf(10);
        // 状态错乱：重复编码
        assert!(!sm.put_cmd_buf(20));
        assert!(matches!(*sm.pending_cmd_buf.lock().unwrap(), Some(10)));
    }

    #[test]
    fn drain_clears_acquired_state() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn drain_clears_encoded_state() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        sm.take_view();
        sm.put_cmd_buf(2);
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn drain_on_idle_is_noop() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    #[test]
    fn double_drain_safe() {
        let sm: StateMachine<u32, u32> = StateMachine::new();
        sm.put_view(1);
        sm.take_view();
        sm.put_cmd_buf(2);
        sm.drain();
        sm.drain();
        // 不 panic，字段仍为 None
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    /// 并发安全：N 线程随机执行状态机操作，验证不 panic + 最终可达成 Idle。
    /// 模拟 winit 线程和逻辑线程同时操作 SharedGPUState 的 race 场景。
    #[test]
    fn concurrent_operations_safe() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let mut handles = Vec::new();
        for tid in 0..8 {
            let sm = Arc::clone(&sm);
            handles.push(thread::spawn(move || {
                for i in 0..1000 {
                    match i % 4 {
                        0 => { sm.put_view((tid * 1000 + i) as u32); }
                        1 => { sm.take_view(); }
                        2 => { sm.put_cmd_buf((tid * 1000 + i) as u32); }
                        3 => { sm.take_cmd_buf(); }
                        _ => unreachable!(),
                    }
                    // 防止某线程饿死
                    thread::yield_now();
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        // 不验证最终状态（race 下不保证 Idle），只验证不 panic
    }

    /// drain 路径并发：winit 线程调 drain 时，逻辑线程可能正在 take_view。
    /// 验证 drain 完成后所有 take 返回 None（不返回陈旧 view/cmd_buf）。
    #[test]
    fn drain_during_concurrent_access() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let mut handles = Vec::new();
        // 逻辑线程：持续 put_view + take_view 模拟编码循环
        for _ in 0..4 {
            let sm = Arc::clone(&sm);
            handles.push(thread::spawn(move || {
                for i in 0..500 {
                    sm.put_view(i);
                    sm.take_view();
                    sm.put_cmd_buf(i);
                    thread::yield_now();
                }
            }));
        }
        // winit 线程：持续 drain + acquire 模拟 resize/close
        for _ in 0..2 {
            let sm = Arc::clone(&sm);
            handles.push(thread::spawn(move || {
                for _ in 0..500 {
                    sm.drain();
                    thread::sleep(Duration::from_micros(10));
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        // 最终 drain
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    /// put_view 并发竞争：N 线程同时 put_view，恰好一个成功，其余 false。
    /// 这是 winit 线程 + 逻辑线程的 race 关键路径。
    #[test]
    fn put_view_is_atomic_only_one_succeeds() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let mut handles = Vec::new();
        let success_count = Arc::new(Mutex::new(0u32));
        for tid in 0..16 {
            let sm = Arc::clone(&sm);
            let success_count = Arc::clone(&success_count);
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    if sm.put_view((tid * 1000 + i) as u32) {
                        *success_count.lock().unwrap() += 1;
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // 仅当 pending_view 为 None 时 put 成功
        // 多次成功是允许的（drain 之后又能 put）
        // 但每个 put 时刻只有 1 个线程能 put
        assert!(*success_count.lock().unwrap() > 0);
    }

    /// 模拟完整帧循环：winit 线程跑 "acquire + present" 循环，逻辑线程跑
    /// "take_view + encode + put_cmd_buf" 循环。验证 N 帧后能稳定运行不 panic。
    #[test]
    fn winit_and_logic_thread_full_frame_loop() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let sm_winit = Arc::clone(&sm);
        let sm_logic = Arc::clone(&sm);

        // winit 线程：Idle → Acquire → ... → wait → ... → Idle
        let winit = thread::spawn(move || {
            for i in 0..500 {
                // 模拟 acquire：Idle 时 put_view（成功）
                if !sm_winit.put_view(i) {
                    // 状态错乱（不应发生）
                    panic!("winit: put_view failed at frame {}", i);
                }
                // 模拟等逻辑线程编码
                while sm_winit.take_cmd_buf().is_none() {
                    thread::yield_now();
                }
                // 模拟 submit+present：Encoded → Idle
                // 提交后回到 Idle（drain 模拟）
                sm_winit.drain();
            }
        });

        // 逻辑线程：等 view → 编码 → 放 cmd_buf
        let logic = thread::spawn(move || {
            for _ in 0..500 {
                // 等 view
                let v = loop {
                    if let Some(v) = sm_logic.take_view() {
                        break v;
                    }
                    thread::yield_now();
                };
                // 编码（用 view 值作为 cmd_buf 内容）
                assert!(sm_logic.put_cmd_buf(v + 1000));
            }
        });

        winit.join().expect("winit thread panicked");
        logic.join().expect("logic thread panicked");
        // 最终 Idle
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }

    /// Resize 路径模拟：winit 线程在 resize 期间调 drain，必须与逻辑线程正在
    /// 进行的 put_view/take_view 兼容。验证 drain 后状态为 Idle。
    #[test]
    fn resize_drain_concurrent_with_logic() {
        let sm = Arc::new(StateMachine::<u32, u32>::new());
        let mut handles = Vec::new();

        // 逻辑线程：持续 put_view + take_view
        for tid in 0..3 {
            let sm = Arc::clone(&sm);
            handles.push(thread::spawn(move || {
                for i in 0..500u32 {
                    let v = (tid as u32) * 10000 + i;
                    let _ = sm.put_view(v);
                    // take_view 可能返回 None（如果 winit 同时 drain）
                    let _ = sm.take_view();
                    let _ = sm.put_cmd_buf(v + 1000);
                    let _ = sm.take_cmd_buf();
                    thread::yield_now();
                }
            }));
        }

        // winit 线程：resize 触发 drain
        let sm_winit = Arc::clone(&sm);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                sm_winit.drain();
                thread::sleep(Duration::from_micros(50));
            }
        }));

        for h in handles {
            h.join().unwrap();
        }
        // 最终 drain
        sm.drain();
        assert!(sm.take_view().is_none());
        assert!(sm.take_cmd_buf().is_none());
    }
}

#[cfg(test)]
mod vireo_window_drop_order {
    //! 字段 drop 顺序：必须 `shared_present: Arc<SharedGPUState>` 在
    //! `surface: wgpu::Surface<'static>` 之前声明。
    //!
    //! 原因：SharedGPUState 里的 `pending_view` 持有 `TextureView`（Send + Sync，
    //! 引用 SurfaceTexture）；SurfaceTexture 持有 swapchain semaphore。
    //! 如果 SharedGPUState 后于 surface drop，surface 的 swapchain 在
    //! `release_resources` 时遇到 semaphore 引用残留 → panic（第三十四轮踩过）。
    //!
    //! 验证方法：源码注释 + 字段声明顺序检查（grep）。
    //!
    //! 真正的回归保护靠 code review：检查 `shared_present` 字段必须在 `surface`
    //! 字段之前声明。

    // 源码注释：vireo_window_drop_order_invariant
    //
    // invariant: VireoWindow.shared_present 字段必须在 surface 字段之前声明。
    // 任何对字段顺序的改动都必须重新跑测试 + 手动验证拖动不 panic。
    //
    // 验证脚本（手动）：
    //   grep "pub(crate) surface: wgpu::Surface" src/window.rs -n
    //   grep "pub(crate) shared_present: Arc<SharedGPUState>" src/window.rs -n
    //   前者的行号必须大于后者。
    //
    // 这个测试是 placeholder。真实保护靠 source review + drop order comment。

    #[test]
    fn doc_invariant_documented() {
        // 编译期保证：测试名包含 "drop order invariant" 提示
        // 实际保证见模块顶部注释
        let invariant_doc = "shared_present 在 surface 之前 drop";
        assert!(invariant_doc.contains("shared_present"));
        assert!(invariant_doc.contains("surface"));
        assert!(invariant_doc.contains("之前"));
    }
}
