use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use crate::app::App;
use crate::platform::windows;
use crate::window::FrameStyle;
use crate::window::WinitEvent;

/// 将 `catch_unwind` 捕获的 panic payload 转成可读字符串。
pub(crate) fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// 应用单个 winit 事件到 App 状态（free function，供 supervisor 线程调用）。
/// 这是 supervisor 线程事件处理分支，语义与原渲染帧循环逐位一致，
/// 仅把 `app` 与各 channel 改为参数传入。
pub(crate) fn apply_winit_event_one(
    app: &App,
    event: WinitEvent,
    event_tx: &mpsc::Sender<WinitEvent>,
    frame_style_tx: &mpsc::Sender<(isize, FrameStyle)>,
    aspect_ratio_tx: &mpsc::Sender<(isize, Option<f64>)>,
    nc_tx: &mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    close_tx: &mpsc::Sender<usize>,
) {
    match event {
        WinitEvent::WindowCreated {
            handle,
            window,
            surface,
            surface_config,
            renderer,
            dpi_scale,
            dpi_override,
            init_duration,
            frame_style,
            pending_show,
        } => {
            let vw = VireoWindow::new(
                window,
                app.gpu.clone(),
                surface,
                app.instance.lock().clone().expect("instance available"),
                surface_config,
                renderer,
                dpi_scale,
                dpi_override,
                init_duration,
                frame_style,
                pending_show,
                event_tx.clone(),
                app.inner
                    .cb_tx
                    .lock()
                    .clone()
                    .unwrap_or_else(|| {
                        unreachable!(
                            "vireo: cb_tx is always Some after run_entry/init_run_channels; \
                             App::window is only reachable with cb_tx set"
                        )
                    }),
                nc_tx.clone(),
                close_tx.clone(),
                app.event_loop_proxy.get().cloned(),
                handle,
            );
            vw.set_max_fps(app.max_fps());
            vw.set_drag_cap(app.drag_cap());
            while app.windows.lock().len() <= handle {
                app.windows.lock().push(None);
            }
            app.windows.lock()[handle] = Some(Arc::new(vw));
            app.alive_window_count
                .fetch_add(1, Ordering::AcqRel);
            app.created_window_count
                .fetch_add(1, Ordering::AcqRel);
        }

        WinitEvent::Resized { handle, width, height } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.resize(width, height);
            }
        }

        WinitEvent::ScaleFactorChanged { handle, scale: _scale } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                let size = win.inner.inner_size();
                win.resize(size.width, size.height);
            }
        }

        WinitEvent::CursorMoved { handle, x, y } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::CursorMoved { handle, x, y });
            }
        }

        WinitEvent::KeyboardInput { handle, event } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::KeyboardInput { handle, event });
            }
        }

        WinitEvent::MouseInput { handle, button, pressed } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::MouseInput { handle, button, pressed });
            }
        }

        WinitEvent::MouseWheel { handle, delta } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::MouseWheel { handle, delta });
            }
        }

        WinitEvent::ModifiersChanged { handle, modifiers } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::ModifiersChanged { handle, modifiers });
            }
        }

        WinitEvent::Focused { handle, focused } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::Focused { handle, focused });
            }
        }

        WinitEvent::CursorEntered { handle } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::CursorEntered { handle });
            }
        }

        WinitEvent::CursorLeft { handle } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::CursorLeft { handle });
            }
        }

        WinitEvent::Touch { handle, event } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input
                    .lock()
                    .push(WinitEvent::Touch { handle, event });
            }
        }

        WinitEvent::CloseRequested { handle, .. } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                *win.closing.lock() = true;
                {
                    let cv = &win.draw_idle_cv;
                    let mut guard = cv.0.lock().unwrap();
                    while !win.draw_idle.load(Ordering::Acquire) {
                        guard = cv.1.wait(guard).unwrap();
                    }
                }
            }
            if let Some(w) = app.windows.lock().get_mut(handle) {
                *w = None;
                app.alive_window_count
                    .fetch_sub(1, Ordering::AcqRel);
            }
        }

        WinitEvent::SetTitle { handle, title } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_title(&title);
            }
        }
        WinitEvent::SetSize { handle, size } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                let _ = win.inner.request_inner_size(size);
            }
        }
        WinitEvent::SetMinSize { handle, size } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_min_inner_size(size);
            }
        }
        WinitEvent::SetMaxSize { handle, size } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_max_inner_size(size);
            }
        }
        WinitEvent::SetFullscreen { handle, fullscreen } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_fullscreen(fullscreen);
            }
        }
        WinitEvent::SetMaximized { handle, maximized } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_maximized(maximized);
            }
        }
        WinitEvent::SetMinimized { handle, minimized } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_minimized(minimized);
            }
        }
        WinitEvent::SetVisible { handle, visible } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_visible(visible);
            }
        }
        WinitEvent::FocusWindow { handle } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.focus_window();
            }
        }
        WinitEvent::SetWindowLevel { handle, level } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_window_level(level);
            }
        }
        WinitEvent::SetFrameStyle { handle, style } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                *win.frame_style.lock() = style;
                win.inner.set_decorations(style.decorated());
                if let Some(hwnd) = windows::win_hwnd(&win.inner) {
                    let _ = frame_style_tx.send((hwnd, style));
                    app.wake_event_loop();
                }
            }
        }
        WinitEvent::SetAspectRatio { handle, ratio } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                if let Some(hwnd) = windows::win_hwnd(&win.inner) {
                    let _ = aspect_ratio_tx.send((hwnd, ratio));
                    app.wake_event_loop();
                }
            }
        }
        WinitEvent::SetIcon { handle, icon } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_window_icon(Some(icon));
            }
        }
        WinitEvent::SetCursor { handle, cursor } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.inner.set_cursor(cursor);
            }
        }
        WinitEvent::Wake => {}
    }
}

/// supervisor 线程主循环：集中应用事件 + 判定退出。
/// 与 winit 线程解耦——winit 只发 `WinitEvent`，本线程 drain 并应用到 `App` 状态；
/// 当所有 `loop_states` 结束（或设备丢失）时通知 winit 线程退出事件循环。
pub(crate) fn supervisor_loop(
    app: App,
    rx: mpsc::Receiver<WinitEvent>,
    event_tx: mpsc::Sender<WinitEvent>,
    frame_style_tx: mpsc::Sender<(isize, FrameStyle)>,
    aspect_ratio_tx: mpsc::Sender<(isize, Option<f64>)>,
    nc_tx: mpsc::Sender<(isize, crate::platform::windows::NcUpdate)>,
    close_tx: mpsc::Sender<usize>,
    exit_tx: mpsc::Sender<()>,
    expected_windows: usize,
    device_lost: Arc<AtomicBool>,
) {
    let mut created_windows = 0usize;
    loop {
        let mut processed = false;
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    let is_created = matches!(ev, WinitEvent::WindowCreated { .. });
                    apply_winit_event_one(
                        &app, ev, &event_tx, &frame_style_tx, &aspect_ratio_tx, &nc_tx, &close_tx,
                    );
                    if is_created {
                        created_windows += 1;
                        app.pending_window_creates
                            .fetch_sub(1, Ordering::AcqRel);
                        app.inner.loop_wake.1.notify_all();
                    }
                    processed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if created_windows >= expected_windows {
            app.windows_ready
                .store(true, Ordering::Release);
            app.inner.loop_wake.1.notify_all();
        }
        let all_loops_done = {
            let loops_requested =
                app.loops_ever_requested.load(Ordering::Acquire);
            if loops_requested {
                let states = app.loop_states.lock();
                !states.is_empty()
                    && states
                        .iter()
                        .all(|s| s.done.load(Ordering::Acquire))
            } else {
                true
            }
        };
        let windows_settled = created_windows >= expected_windows
            && app.window_count() == 0
            && app.pending_window_creates.load(Ordering::Acquire) == 0;
        let all_windows_closed =
            created_windows > 0 && app.window_count() == 0 && app.pending_window_creates.load(Ordering::Acquire) == 0;
        if (app.inner
            .main_done
            .load(Ordering::Acquire)
            && all_loops_done
            && windows_settled)
            || all_windows_closed
            || device_lost.load(Ordering::Acquire)
        {
            let _ = exit_tx.send(());
            app.wake_event_loop();
            return;
        }
        if !processed {
            match rx.recv() {
                Ok(ev) => {
                    let is_created = matches!(ev, WinitEvent::WindowCreated { .. });
                    apply_winit_event_one(
                        &app, ev, &event_tx, &frame_style_tx, &aspect_ratio_tx, &nc_tx, &close_tx,
                    );
                    if is_created {
                        created_windows += 1;
                        app.pending_window_creates
                            .fetch_sub(1, Ordering::AcqRel);
                        app.inner.loop_wake.1.notify_all();
                    }
                }
                Err(_) => return,
            }
        }
    }
}

/// Winit 窗口引用类型（供 supervisor 使用）。
use crate::window::VireoWindow;
