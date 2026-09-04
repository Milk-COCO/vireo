use std::sync::{Arc, mpsc};
use rustc_hash::FxHashMap;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Icon, WindowAttributes, WindowId},
};

use crate::App;
use crate::app::{CreateWindowRequest, RunChannels, supervisor_loop, panic_payload_to_string};
use crate::dpi::{dim_to_winit_position, dim_to_winit_size, to_pixel_size};
use crate::gpu::GpuContext;
use crate::platform::windows::win_hwnd;
use crate::render::Renderer;
use crate::window::{FrameStyle, WinitEvent, WindowDesc};

/// winit 事件循环宿主（Runner）：运行在 OS 主线程上，负责创建窗口和转发事件。
pub struct Runner {
    event_tx: mpsc::Sender<WinitEvent>,
    cb_rx: mpsc::Receiver<(usize, crate::input::InputCallbacks)>,
    exit_rx: mpsc::Receiver<()>,
    frame_style_rx: mpsc::Receiver<(isize, FrameStyle)>,
    aspect_ratio_rx: mpsc::Receiver<(isize, Option<f64>)>,
    nc_rx: mpsc::Receiver<(isize, crate::platform::windows::NcUpdate)>,
    create_rx: mpsc::Receiver<CreateWindowRequest>,
    close_rx: mpsc::Receiver<usize>,
    hwnds: Vec<isize>,
    id_to_handle: FxHashMap<WindowId, usize>,
    close_hooks: FxHashMap<u64, Option<Box<dyn FnOnce() + Send>>>,
    window_callbacks: Vec<crate::input::InputCallbacks>,
    default_icon: Option<Icon>,
    instance: wgpu::Instance,
    gpu: Arc<GpuContext>,
    created: bool,
    resumed_fired: bool,
}

impl Runner {
    fn handle_for(&self, window_id: WindowId) -> Option<usize> {
        self.id_to_handle.get(&window_id).copied()
    }

    fn send(&self, event: WinitEvent) {
        let _ = self.event_tx.send(event);
    }

    /// 完整关窗路径：close_hooks / NC 状态清理 / 发 `WinitEvent::CloseRequested`。
    fn request_close(&mut self, handle: usize) {
        if let Some(hook_opt) = self.close_hooks.get_mut(&(handle as u64)) {
            if let Some(h) = hook_opt.take() { h(); }
        }
        if let Some(&hwnd) = self.hwnds.get(handle) {
            if hwnd != 0 {
                crate::platform::windows::nc_remove(hwnd);
                crate::platform::windows::drop_thumbar_icons(hwnd);
                crate::platform::windows::drop_overlay_icons(hwnd);
                crate::platform::windows::clear_thumbar_callback(hwnd);
                crate::platform::windows::remove_window_icons_entry(hwnd);
            }
        }
        self.send(WinitEvent::CloseRequested { handle });
    }

    fn create_attrs(desc: &WindowDesc, default_icon: &Option<Icon>, os_scale: f64) -> WindowAttributes {
        let mut attrs = WindowAttributes::default()
            .with_title(&desc.title)
            .with_inner_size(dim_to_winit_size(desc.size.0, desc.size.1, desc.dpi_override, os_scale))
            .with_resizable(desc.resizable)
            .with_maximized(desc.maximized)
            .with_visible(desc.visible && !desc.preparable)
            .with_transparent(desc.transparent)
            .with_decorations(desc.frame_style.decorated())
            .with_window_level(desc.window_level)
            .with_content_protected(desc.content_protected)
            .with_active(desc.active)
            .with_blur(desc.blur)
            .with_cursor(desc.cursor.clone())
            .with_enabled_buttons(desc.enabled_buttons);
        if let Some(d) = desc.min_size {
            attrs = attrs.with_min_inner_size(dim_to_winit_size(d.0, d.1, desc.dpi_override, os_scale));
        }
        if let Some(d) = desc.max_size {
            attrs = attrs.with_max_inner_size(dim_to_winit_size(d.0, d.1, desc.dpi_override, os_scale));
        }
        if let Some(d) = desc.position {
            attrs = attrs.with_position(dim_to_winit_position(d.0, d.1, desc.dpi_override, os_scale));
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
        if let Some(d) = desc.resize_increments {
            attrs = attrs.with_resize_increments(dim_to_winit_size(d.0, d.1, desc.dpi_override, os_scale));
        }
        if let Some(ph) = desc.parent_window {
            attrs = unsafe { attrs.with_parent_window(Some(ph.0)) };
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            if desc.frame_style == FrameStyle::HiddenTitlebar {
                attrs = attrs
                    .with_title_hidden(true)
                    .with_titlebar_transparent(true)
                    .with_fullsize_content_view(true);
            }
        }
        attrs
    }

    fn create_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        handle: usize,
        desc: &WindowDesc,
        init_duration: f64,
        on_close: Option<Box<dyn FnOnce() + Send>>,
    ) {
        if self.window_callbacks.len() <= handle {
            self.window_callbacks.resize_with(handle + 1, crate::input::InputCallbacks::default);
        }
        let os_scale = event_loop
            .primary_monitor()
            .map(|m| m.scale_factor())
            .unwrap_or(1.0);
        let attrs = Self::create_attrs(desc, &self.default_icon, os_scale);
        let window = Arc::new(
            event_loop.create_window(attrs).unwrap(),
        );
        if let Some(hwnd) = win_hwnd(&window) {
            let fs = desc.frame_style;
            if !fs.has_titlebar() && fs.has_border() {
                crate::platform::windows::install(hwnd, false, true);
            }
            if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                for cb in std::mem::take(&mut cbs.on_thumb_button) {
                    crate::platform::windows::set_thumbar_callback(hwnd, cb);
                }
            }
            if self.hwnds.len() <= handle {
                self.hwnds.resize(handle + 1, 0);
            }
            self.hwnds[handle] = hwnd;
        }
        if let Some(h) = on_close {
            self.close_hooks.insert(handle as u64, Some(h));
        }
        let surface = self.instance.create_surface(window.clone()).unwrap();
        let window_id = window.id();

        let scale = desc.dpi_override.unwrap_or(window.scale_factor()) as f32;
        let dpi = window.scale_factor() as f32;
        let s = to_pixel_size(
            window.inner_size().width as f64,
            window.inner_size().height as f64,
            desc.dpi_override.unwrap_or(dpi as f64),
        );
        let (logical_w, logical_h) = (s.width.dp.0 as f32, s.height.dp.0 as f32);
        let renderer = Renderer::new(
            self.gpu.clone(),
            logical_w,
            logical_h,
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
        let fmt = if caps.formats.contains(&self.gpu.surface_format()) {
            self.gpu.surface_format()
        } else {
            caps.formats[0]
        };
        if self.gpu.surface_format() != fmt {
            self.gpu.set_surface_format(fmt);
            self.gpu.clear_pipelines();
            self.gpu
                .text_ctx
                .lock()
                .unwrap()
                .ensure_text_format(&self.gpu.device, fmt);
        }
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: fmt,
            width: window.inner_size().width.max(1),
            height: window.inner_size().height.max(1),
            present_mode: desc.present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: desc.frame_latency,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };

        self.id_to_handle.insert(window_id, handle);
        self.send(WinitEvent::WindowCreated {
            handle,
            window,
            surface,
            surface_config,
            renderer,
            dpi_scale: dpi,
            dpi_override: desc.dpi_override,
            init_duration,
            frame_style: desc.frame_style,
            pending_show: desc.visible && desc.preparable,
        });
    }
}

impl ApplicationHandler for Runner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.created { return; }
        self.created = true;
        self.resumed_fired = true;
        while let Ok(req) = self.create_rx.try_recv() {
            self.create_window(
                event_loop,
                req.handle as usize,
                &req.desc,
                req.init_duration,
                req.on_close,
            );
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let mut exit_requested = false;
        while self.exit_rx.try_recv().is_ok() {
            exit_requested = true;
        }
        if self.resumed_fired {
            while let Ok(req) = self.create_rx.try_recv() {
                self.create_window(
                    event_loop,
                    req.handle as usize,
                    &req.desc,
                    req.init_duration,
                    req.on_close,
                );
            }
        }
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
                cbs.on_ime.extend(reg.on_ime.drain(..));
                cbs.on_file_dropped.extend(reg.on_file_dropped.drain(..));
                cbs.on_file_hovered.extend(reg.on_file_hovered.drain(..));
                cbs.on_file_hover_cancelled.extend(reg.on_file_hover_cancelled.drain(..));
                cbs.on_moved.extend(reg.on_moved.drain(..));
                cbs.on_theme_changed.extend(reg.on_theme_changed.drain(..));
                cbs.on_resized.extend(reg.on_resized.drain(..));
                if (handle as usize) < self.hwnds.len() && self.hwnds[handle as usize] != 0 {
                    for cb in std::mem::take(&mut reg.on_thumb_button) {
                        crate::platform::windows::set_thumbar_callback(
                            self.hwnds[handle as usize],
                            cb,
                        );
                    }
                }
            }
        }
        while let Ok((hwnd, style)) = self.frame_style_rx.try_recv() {
            if !style.has_titlebar() && style.has_border() {
                crate::platform::windows::set_frame(hwnd, false, true);
            } else {
                crate::platform::windows::remove(hwnd);
            }
        }
        while let Ok((hwnd, ratio)) = self.aspect_ratio_rx.try_recv() {
            crate::platform::windows::set_aspect_ratio(hwnd, ratio);
        }
        while let Ok((hwnd, upd)) = self.nc_rx.try_recv() {
            crate::platform::windows::nc_apply(hwnd, upd);
        }
        while let Ok(handle) = self.close_rx.try_recv() {
            self.request_close(handle);
        }
        if exit_requested {
            event_loop.exit();
        } else {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(handle) = self.handle_for(window_id) else { return; };
        match event {
            WindowEvent::CloseRequested => { self.request_close(handle); }
            WindowEvent::Resized(size) => {
                if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                    for cb in &mut cbs.on_resized { cb(size); }
                }
                self.send(WinitEvent::Resized { handle, width: size.width, height: size.height });
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.send(WinitEvent::ScaleFactorChanged { handle, scale: scale_factor });
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.send(WinitEvent::CursorMoved { handle, x: position.x, y: position.y });
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
            WindowEvent::Ime(ime) => {
                if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                    for cb in &mut cbs.on_ime { cb(&ime); }
                }
            }
            WindowEvent::DroppedFile(path) => {
                if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                    for cb in &mut cbs.on_file_dropped { cb(&path); }
                }
            }
            WindowEvent::HoveredFile(path) => {
                if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                    for cb in &mut cbs.on_file_hovered { cb(&path); }
                }
            }
            WindowEvent::HoveredFileCancelled => {
                if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                    for c in cbs.on_file_hover_cancelled.drain(..) { c(); }
                }
            }
            WindowEvent::Moved(position) => {
                if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                    for cb in &mut cbs.on_moved { cb(position); }
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                if let Some(cbs) = self.window_callbacks.get_mut(handle) {
                    for cb in &mut cbs.on_theme_changed { cb(theme); }
                }
            }
            WindowEvent::RedrawRequested => {}
            _ => {}
        }
    }
}

/// 启动 winit 事件循环（OS 主线程）。由 [`App::run_entry`] 调用。
pub(crate) fn run_blocking(
    app: App,
    channels: RunChannels,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let event_loop = EventLoop::new().unwrap();
    let _ = app.event_loop_proxy.set(event_loop.create_proxy());

    let default_icon = app.default_icon.lock().take();
    let instance = app.instance.lock().clone().expect("instance already taken");
    app.handle_to_id.lock().clear();
    app.windows.lock().clear();

    let gpu_for_runner = app.gpu.clone();
    let device_lost = app.device_lost.clone();
    let device_lost_super = device_lost.clone();

    let supervisor = std::thread::Builder::new()
        .name("vireo-supervisor".into())
        .spawn(move || {
            supervisor_loop(
                app,
                channels.event_rx,
                channels.supervisor_event_tx,
                channels.frame_style_tx,
                channels.aspect_ratio_tx,
                channels.nc_tx,
                channels.close_tx,
                channels.exit_tx,
                0usize,
                device_lost_super,
            )
        })
        .expect("failed to spawn supervisor thread");

    event_loop.run_app(&mut Runner {
        event_tx: channels.event_tx,
        cb_rx: channels.cb_rx,
        exit_rx: channels.exit_rx,
        frame_style_rx: channels.frame_style_rx,
        aspect_ratio_rx: channels.aspect_ratio_rx,
        nc_rx: channels.nc_rx,
        create_rx: channels.create_rx,
        close_rx: channels.close_rx,
        hwnds: Vec::new(),
        id_to_handle: FxHashMap::default(),
        close_hooks: FxHashMap::default(),
        window_callbacks: Vec::new(),
        default_icon,
        instance,
        gpu: gpu_for_runner,
        created: false,
        resumed_fired: false,
    }).unwrap();

    match supervisor.join() {
        Ok(()) => Ok(()),
        Err(payload) => Err(panic_payload_to_string(payload).into()),
    }
}
