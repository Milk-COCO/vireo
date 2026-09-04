use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

use winit::window::WindowId;
use winit::window::Icon;

use crate::error::VireoError;
use crate::gpu::GpuContext;
use crate::offscreen::OffscreenCanvas;
use crate::texture::Texture;
use crate::window::{
    AntiAliasing, FrameStyle, OffscreenIndex, VireoWindow, WinitEvent, WindowDesc, WindowIndex,
};

/// 渲染线程 → winit 线程的运行期窗口创建请求（`App::window` 在 on_tick 里调用时）。
pub(crate) struct CreateWindowRequest {
    pub handle: u64,
    pub desc: WindowDesc,
    pub init_duration: f64,
    pub on_close: Option<Box<dyn FnOnce() + Send>>,
}

/// 入口期一次性创建的通道集合。
pub(crate) struct RunChannels {
    pub event_tx: mpsc::Sender<WinitEvent>,
    pub event_rx: mpsc::Receiver<WinitEvent>,
    pub cb_rx: mpsc::Receiver<(usize, crate::input::InputCallbacks)>,
    pub exit_rx: mpsc::Receiver<()>,
    pub exit_tx: mpsc::Sender<()>,
    pub frame_style_rx: mpsc::Receiver<(isize, FrameStyle)>,
    pub aspect_ratio_rx: mpsc::Receiver<(isize, Option<f64>)>,
    pub nc_rx: mpsc::Receiver<(isize, crate::platform::windows::NcUpdate)>,
    pub create_rx: mpsc::Receiver<CreateWindowRequest>,
    pub close_rx: mpsc::Receiver<usize>,
    pub supervisor_event_tx: mpsc::Sender<WinitEvent>,
    pub frame_style_tx: mpsc::Sender<(isize, FrameStyle)>,
    pub aspect_ratio_tx: mpsc::Sender<(isize, Option<f64>)>,
    pub nc_tx: mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    pub close_tx: mpsc::Sender<usize>,
}

pub struct AppInner {
    pub windows: Mutex<Vec<Option<Arc<VireoWindow>>>>,
    alive_window_count: AtomicUsize,
    created_window_count: AtomicUsize,
    pub gpu: Arc<GpuContext>,
    instance: Mutex<Option<wgpu::Instance>>,
    device_lost: Arc<AtomicBool>,
    handle_to_id: Mutex<FxHashMap<u64, WindowId>>,
    next_handle: Mutex<u64>,
    default_icon: Mutex<Option<Icon>>,
    textures: Mutex<Vec<Arc<Texture>>>,
    offscreens: Mutex<Vec<Arc<OffscreenCanvas>>>,
    pub init_duration: f64,
    max_fps: Mutex<Option<u32>>,
    drag_cap: Mutex<bool>,
    create_tx: Mutex<Option<mpsc::Sender<CreateWindowRequest>>>,
    cb_tx: Mutex<Option<mpsc::Sender<(usize, crate::input::InputCallbacks)>>>,
    pub(crate) loop_states: Mutex<Vec<Arc<crate::thread::LoopHandleState>>>,
    pub(crate) windows_ready: AtomicBool,
    pub(crate) pending_window_creates: AtomicUsize,
    pub(crate) loops_ever_requested: AtomicBool,
    pub(crate) main_done: AtomicBool,
    pub(crate) event_tx: Mutex<Option<mpsc::Sender<WinitEvent>>>,
    pub(crate) loop_wake: Arc<(std::sync::Mutex<()>, std::sync::Condvar)>,
    pub(crate) event_loop_proxy: OnceLock<winit::event_loop::EventLoopProxy<()>>,
}

pub struct App {
    pub(crate) inner: Arc<AppInner>,
}

impl Clone for App {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl std::ops::Deref for App {
    type Target = AppInner;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub struct DeferredTask {
    kind: DeferredTaskKind,
    pub(crate) f: Box<dyn FnOnce() + Send>,
}

#[doc(hidden)]
pub struct DeferredTaskGuard;

impl DeferredTask {
    pub(crate) fn new(kind: DeferredTaskKind, f: Box<dyn FnOnce() + Send>) -> Self {
        DeferredTask { kind, f }
    }

    pub(crate) fn is_ready(&self, tick_count: u64) -> bool {
        match &self.kind {
            DeferredTaskKind::AfterTicks(target) => tick_count >= *target,
            DeferredTaskKind::AfterSecs(wakeup) => std::time::Instant::now() >= *wakeup,
        }
    }
}

#[cfg(test)]
impl DeferredTask {
    pub(crate) fn for_frames(target: u64) -> Self {
        DeferredTask {
            kind: DeferredTaskKind::AfterTicks(target),
            f: Box::new(|| {}),
        }
    }
}

pub(crate) enum DeferredTaskKind {
    AfterTicks(u64),
    AfterSecs(std::time::Instant),
}

static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);

impl App {
    pub fn new<F, Fut>(main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        App::from_instance_descriptor(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
            .run_entry(main)
    }

    pub fn with_descriptor<F, Fut>(desc: wgpu::InstanceDescriptor, main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        App::from_instance_descriptor(desc).run_entry(main)
    }

    fn run_entry<F, Fut>(self, main: F)
    where
        F: FnOnce(App) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let channels = self.init_run_channels();
        main::spawn(self.clone(), main);
        if let Err(e) = host::run_blocking(self, channels) {
            eprintln!("[vireo] app exited with error: {:?}", e);
        }
    }

    fn init_run_channels(&self) -> RunChannels {
        let (event_tx, event_rx) = mpsc::channel();
        let supervisor_event_tx = event_tx.clone();
        let (cb_tx, cb_rx) = mpsc::channel();
        let (exit_tx, exit_rx) = mpsc::channel();
        let (frame_style_tx, frame_style_rx) = mpsc::channel();
        let (aspect_ratio_tx, aspect_ratio_rx) = mpsc::channel();
        let (nc_tx, nc_rx) = mpsc::channel();
        let (create_tx, create_rx) = mpsc::channel();
        let (close_tx, close_rx) = mpsc::channel();
        *self.create_tx.lock() = Some(create_tx);
        *self.cb_tx.lock() = Some(cb_tx);
        *self.event_tx.lock() = Some(event_tx.clone());
        RunChannels {
            event_tx,
            event_rx,
            cb_rx,
            exit_rx,
            exit_tx,
            frame_style_rx,
            aspect_ratio_rx,
            nc_rx,
            create_rx,
            close_rx,
            supervisor_event_tx,
            frame_style_tx,
            aspect_ratio_tx,
            nc_tx,
            close_tx,
        }
    }

    fn from_instance_descriptor(desc: wgpu::InstanceDescriptor) -> Self {
        let init_start = std::time::Instant::now();
        let instance = wgpu::Instance::new(desc);
        let gpu = Arc::new(GpuContext::new(&instance));
        let device_lost = gpu.device_lost();
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
        let app = Self {
            inner: Arc::new(AppInner {
                windows: Mutex::new(Vec::new()),
                alive_window_count: AtomicUsize::new(0),
                created_window_count: AtomicUsize::new(0),
                gpu,
                instance: Mutex::new(Some(instance)),
                device_lost,
                handle_to_id: Mutex::new(FxHashMap::default()),
                next_handle: Mutex::new(0),
                default_icon: Mutex::new(default_icon),
                textures: Mutex::new(Vec::new()),
                offscreens: Mutex::new(Vec::new()),
                init_duration,
                max_fps: Mutex::new(Some(240)),
                drag_cap: Mutex::new(true),
                create_tx: Mutex::new(None),
                cb_tx: Mutex::new(None),
                loop_states: Mutex::new(Vec::new()),
                windows_ready: AtomicBool::new(false),
                pending_window_creates: AtomicUsize::new(0),
                loops_ever_requested: AtomicBool::new(false),
                main_done: AtomicBool::new(false),
                event_tx: Mutex::new(None),
                loop_wake: Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new())),
                event_loop_proxy: OnceLock::new(),
            }),
        };
        app
    }

    pub fn offscreen(&self, width: u32, height: u32, aa: AntiAliasing) -> OffscreenIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(aa, self.gpu.supported_sample_counts());
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        let mut guard = self.offscreens.lock();
        let idx = guard.len();
        let mut offscreen = OffscreenCanvas::with_aa(&self.gpu, width, height, aa, init_duration);
        offscreen.index = OffscreenIndex(idx);
        guard.push(Arc::new(offscreen));
        OffscreenIndex(idx)
    }

    pub fn offscreen_ref(&self, idx: &OffscreenIndex) -> Result<Arc<OffscreenCanvas>, VireoError> {
        match self.offscreens.lock().get(idx.0).cloned() {
            Some(c) => Ok(c),
            None => Err(VireoError::OffscreenNotFound(idx.0)),
        }
    }

    pub fn load_texture(&self, path: impl AsRef<std::path::Path>) -> usize {
        let tex = Texture::from_file(path, &self.gpu);
        let mut guard = self.textures.lock();
        let idx = guard.len();
        guard.push(Arc::new(tex));
        idx
    }

    pub fn texture(&self, index: usize) -> Result<Arc<Texture>, VireoError> {
        match self.textures.lock().get(index).cloned() {
            Some(t) => Ok(t),
            None => Err(VireoError::TextureNotFound(index)),
        }
    }

    fn wake_event_loop(&self) {
        if let Some(proxy) = self.event_loop_proxy.get() {
            let _ = proxy.send_event(());
        }
    }

    pub fn window(&self, mut desc: WindowDesc, on_close: Option<impl FnOnce() + Send + 'static>) -> WindowIndex {
        let start = std::time::Instant::now();
        let aa = crate::window::clamp_aa(desc.anti_aliasing, self.gpu.supported_sample_counts());
        desc.anti_aliasing = aa;
        let sc = aa.sample_count();
        let atc = aa.alpha_to_coverage();
        let ssaa = aa.is_ssaa();
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, false);
        let _ = self.gpu.ensure_pipeline(sc, atc, ssaa, true);
        let init_duration = start.elapsed().as_secs_f64();
        let handle = *self.next_handle.lock();
        *self.next_handle.lock() = handle + 1;
        let on_close = on_close.map(|f| Box::new(f) as Box<dyn FnOnce() + Send>);
        match self.create_tx.lock().as_ref().cloned() {
            Some(tx) => {
                let _ = tx.send(CreateWindowRequest {
                    handle,
                    desc,
                    init_duration,
                    on_close,
                });
                self.pending_window_creates
                    .fetch_add(1, Ordering::AcqRel);
                self.wake_event_loop();
            }
            None => unreachable!(
                "vireo: create_tx is always Some after run_entry/init_run_channels; \
                 App::window is only reachable with create_tx set"
            ),
        }
        WindowIndex::new(handle)
    }

    crate::def_app_ons! {
        on_key_down: impl FnMut(&crate::input::KeyEvent) + 'static => on_key_down,
        on_key_up: impl FnMut(&crate::input::KeyEvent) + 'static => on_key_up,
        on_mouse_down: impl FnMut(&crate::input::MouseButtonEvent) + 'static => on_mouse_down,
        on_mouse_up: impl FnMut(&crate::input::MouseButtonEvent) + 'static => on_mouse_up,
        on_scroll: impl FnMut(&crate::input::MouseScrollEvent) + 'static => on_scroll,
        on_cursor_entered: impl FnOnce() + 'static => on_cursor_entered,
        on_cursor_left: impl FnOnce() + 'static => on_cursor_left,
        on_touch: impl FnMut(&crate::input::TouchEvent) + 'static => on_touch,
        on_focus_gained: impl FnOnce() + 'static => on_focus_gained,
        on_focus_lost: impl FnOnce() + 'static => on_focus_lost,
        on_modifiers_changed: impl FnMut(crate::input::Modifiers) + 'static => on_modifiers_changed,
        on_ime: impl FnMut(&crate::input::Ime) + 'static => on_ime,
        on_file_dropped: impl FnMut(&std::path::PathBuf) + 'static => on_file_dropped,
        on_file_hovered: impl FnMut(&std::path::PathBuf) + 'static => on_file_hovered,
        on_file_hover_cancelled: impl FnOnce() + 'static => on_file_hover_cancelled,
        on_moved: impl FnMut(winit::dpi::PhysicalPosition<i32>) + 'static => on_moved,
        on_theme_changed: impl FnMut(winit::window::Theme) + 'static => on_theme_changed,
        on_resized: impl FnMut(winit::dpi::PhysicalSize<u32>) + 'static => on_resized,
        #[cfg(target_os = "windows")]
        on_thumb_button: impl FnMut(u32) + 'static => on_thumb_button,
    }

    pub fn run<F: FnMut(&mut crate::thread::LoopContext) -> bool + Send + 'static>(
        &self,
        on_tick: F,
    ) -> crate::thread::ThreadHandle {
        self.spawn(crate::thread::Thread::new().with_loop(crate::thread::Loop::new(on_tick)))
    }

    pub fn spawn(&self, thread: crate::thread::Thread) -> crate::thread::ThreadHandle {
        let loops = thread.loops;
        let shared = Arc::new(Mutex::new(Vec::<crate::thread::Loop>::new()));
        let fps_stats = Arc::new(Mutex::new(crate::thread::FpsStats::new()));
        let state = Arc::new(crate::thread::LoopHandleState::new());
        self.loops_ever_requested
            .store(true, Ordering::Release);
        self.loop_states.lock().push(state.clone());
        let app = self.clone();
        let device_lost = self.device_lost.clone();
        let state_for_thread = state.clone();
        let shared_for_thread = shared.clone();
        let fps_for_thread = fps_stats.clone();
        let device_lost_for_thread = device_lost.clone();
        let thread_tps = thread.max_tps.or(app.max_fps());
        let thread_name = thread.name.clone().unwrap_or_else(|| {
            let n = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
            format!("vireo-thread-{n}")
        });
        let join = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                crate::thread::run_thread_loop(
                    app,
                    loops,
                    state_for_thread,
                    fps_for_thread,
                    shared_for_thread,
                    device_lost_for_thread,
                    thread_tps,
                );
            })
            .expect("failed to spawn vireo thread");
        crate::thread::ThreadHandle { state, thread: Some(join), shared, fps_stats }
    }

    pub fn loops(&self, loops: impl IntoIterator<Item = crate::thread::Loop>) -> crate::thread::ThreadHandle {
        self.spawn(crate::thread::Thread::new().with_loops(loops))
    }

    pub fn material(&self, source: &str) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material(source)
    }

    pub fn material_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_with_vertex_shader(source, vertex_source)
    }

    pub fn material_with_resources(
        &self,
        source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_with_resources(source, resources)
    }

    pub fn material_with_resources_and_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        resources: crate::material::MaterialResources<'_>,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_with_resources_and_vertex_shader(source, vertex_source, resources)
    }

    pub fn material_manual(
        &self,
        source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_manual(source, bgl)
    }

    pub fn material_manual_with_vertex_shader(
        &self,
        source: &str,
        vertex_source: &str,
        bgl: &wgpu::BindGroupLayout,
    ) -> Result<Arc<crate::material::Material>, String> {
        self.gpu.create_material_manual_with_vertex_shader(source, vertex_source, bgl)
    }

    pub fn window_ref(&self, idx: &WindowIndex) -> Result<Arc<VireoWindow>, VireoError> {
        match self.windows.lock().get(idx.0 as usize).and_then(|w| w.clone()) {
            Some(w) => Ok(w),
            None => Err(VireoError::WindowNotFound(idx.0)),
        }
    }

    pub fn window_count(&self) -> usize {
        self.alive_window_count
            .load(Ordering::Acquire)
    }

    pub(crate) fn created_window_count(&self) -> usize {
        self.created_window_count
            .load(Ordering::Acquire)
    }

    pub fn init_duration(&self) -> f64 {
        self.init_duration
    }

    pub fn set_max_fps(&self, fps: Option<u32>) {
        *self.max_fps.lock() = fps;
    }

    pub fn max_fps(&self) -> Option<u32> {
        *self.max_fps.lock()
    }

    pub fn set_drag_cap(&self, enabled: bool) {
        *self.drag_cap.lock() = enabled;
    }

    pub fn drag_cap(&self) -> bool {
        *self.drag_cap.lock()
    }

    pub fn windows(&self) -> Vec<Arc<VireoWindow>> {
        self.windows.lock().iter().filter_map(|w| w.clone()).collect()
    }

    pub fn window_indices(&self) -> Vec<WindowIndex> {
        self.windows.lock().iter().enumerate()
            .filter(|(_, w)| w.is_some())
            .map(|(i, _)| WindowIndex::new(i as u64))
            .collect()
    }
}

mod main;
mod host;
mod supervisor;

pub(crate) use crate::app::supervisor::{supervisor_loop, panic_payload_to_string};
