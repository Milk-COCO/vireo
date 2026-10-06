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

/// supervisor 单轮决策（纯函数，可单测）。
///
/// - `Exit`：原退出条件（`main_done && all_loops_done && settled` / 全关 / 设备丢失）。
/// - `CloseAll`：vireo-main 已结束且无存活 loop，但仍有窗口开着——这些窗口永远不会
///   再被驱动（`ThreadHandle` 全在 main 手里，main 结束即全部 join；`mem::forget`
///   或把 handle move 到别的线程属于刻意行为，不予保证）。继续留着只会冻住，
///   故按 X 关闭同款路径逐个关窗，收尾由 `all_closed` 接管退出。
///   只在宏风格生效：`main_done` 仅由 `vireo-main` spawn 路径置位，手动
///   `App::run`/`spawn` 自驱 main 的程序（`main_done` 恒假）行为零变化。
/// - `Wait`：继续等事件。
pub(crate) enum SupervisorAction {
    Exit,
    CloseAll,
    Wait,
}

pub(crate) fn decide_supervisor_action(
    main_done: bool,
    all_loops_done: bool,
    windows_settled: bool,
    all_closed: bool,
    device_lost: bool,
    open_windows: usize,
    pending_creates: usize,
) -> SupervisorAction {
    if (main_done && all_loops_done && windows_settled) || all_closed || device_lost {
        SupervisorAction::Exit
    } else if main_done && all_loops_done && pending_creates == 0 && open_windows > 0 {
        SupervisorAction::CloseAll
    } else {
        SupervisorAction::Wait
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
                app.inner.cb_tx.lock().clone().unwrap_or_else(|| {
                    unreachable!(
                        "vireo: cb_tx is always Some after run_entry/init_run_channels; \
                             App::window is only reachable with cb_tx set"
                    )
                }),
                nc_tx.clone(),
                close_tx.clone(),
                app.event_loop_proxy.get().cloned(),
                handle,
                #[cfg(target_os = "android")]
                crate::app::android_app().cloned(),
            );
            vw.set_max_fps(app.max_fps());
            vw.set_drag_cap(app.drag_cap());
            while app.windows.lock().len() <= handle {
                app.windows.lock().push(None);
            }
            app.windows.lock()[handle] = Some(Arc::new(vw));
            app.alive_window_count.fetch_add(1, Ordering::AcqRel);
            app.created_window_count.fetch_add(1, Ordering::AcqRel);
        }

        WinitEvent::Resized {
            handle,
            width,
            height,
        } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.resize(width, height);
            }
        }

        WinitEvent::ScaleFactorChanged {
            handle,
            scale: _scale,
        } => {
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

        WinitEvent::MouseInput {
            handle,
            button,
            pressed,
        } => {
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone()) {
                win.pending_input.lock().push(WinitEvent::MouseInput {
                    handle,
                    button,
                    pressed,
                });
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
                    // 关窗握手：等渲染线程不在帧内再 take。加 2s 超时——帧在途正常是 ms 级，
                    // 超时说明渲染线程已死或卡住，无限等只会把退出拖成永久挂起，打 warn 放行。
                    let cv = &win.draw_idle_cv;
                    let mut guard = cv.0.lock().unwrap();
                    while !win.draw_idle.load(Ordering::Acquire) {
                        let (g, res) =
                            cv.1.wait_timeout(guard, std::time::Duration::from_secs(2))
                                .unwrap();
                        guard = g;
                        if res.timed_out() {
                            super::exit_log(&format!(
                                "supervisor: draw_idle not set after 2s (handle={}), forcing close",
                                handle
                            ));
                            break;
                        }
                    }
                    super::exit_log(&format!(
                        "supervisor: close handle={} (draw_idle ok)",
                        handle
                    ));
                }
            }
            if let Some(w) = app.windows.lock().get_mut(handle) {
                // take-guard：并发双重关窗（X + `close()` 竞争）时只计数一次，
                // 否则 `alive_window_count` 下溢回绕，退出谓词永久失效。
                if w.take().is_some() {
                    app.alive_window_count.fetch_sub(1, Ordering::AcqRel);
                }
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
            if let Some(win) = app.windows.lock().get(handle).and_then(|o| o.clone())
                && let Some(hwnd) = windows::win_hwnd(&win.inner)
            {
                let _ = aspect_ratio_tx.send((hwnd, ratio));
                app.wake_event_loop();
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
        // 内存压力不是某个窗口的事，走 App 级计数；渲染线程在旁边 fire 回调。
        WinitEvent::MemoryWarning => app.inner.note_memory_warning(),
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
    exit_ack_rx: mpsc::Receiver<()>,
    expected_windows: usize,
    device_lost: Arc<AtomicBool>,
) {
    let mut created_windows = 0usize;
    let mut pending_since: Option<std::time::Instant> = None;
    let mut pending_warned_at: Option<std::time::Instant> = None;
    loop {
        let mut processed = false;
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    let is_created = matches!(ev, WinitEvent::WindowCreated { .. });
                    apply_winit_event_one(
                        &app,
                        ev,
                        &event_tx,
                        &frame_style_tx,
                        &aspect_ratio_tx,
                        &nc_tx,
                        &close_tx,
                    );
                    if is_created {
                        created_windows += 1;
                        app.pending_window_creates.fetch_sub(1, Ordering::AcqRel);
                        app.inner.loop_wake.1.notify_all();
                    }
                    processed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if created_windows >= expected_windows {
            app.windows_ready.store(true, Ordering::Release);
            app.inner.loop_wake.1.notify_all();
        }
        let all_loops_done = {
            let loops_requested = app.loops_ever_requested.load(Ordering::Acquire);
            if loops_requested {
                let states = app.loop_states.lock();
                !states.is_empty() && states.iter().all(|s| s.done.load(Ordering::Acquire))
            } else {
                true
            }
        };
        let windows_settled = created_windows >= expected_windows
            && app.window_count() == 0
            && app.pending_window_creates.load(Ordering::Acquire) == 0;
        let all_windows_closed = created_windows > 0
            && app.window_count() == 0
            && app.pending_window_creates.load(Ordering::Acquire) == 0;
        // pending backstop：建窗在途超 10s 未落定（WindowCreated 丢了或 winit 侧没泵），
        // 退出谓词永假。只打 error（不强行归零，迟到的 WindowCreated 还会正常递减）。
        {
            let pending = app.pending_window_creates.load(Ordering::Acquire);
            let now = std::time::Instant::now();
            if pending == 0 {
                pending_since = None;
                pending_warned_at = None;
            } else {
                let since = *pending_since.get_or_insert(now);
                let overdue = now.saturating_duration_since(since);
                let warn_due = pending_warned_at
                    .map(|t| now.saturating_duration_since(t) >= std::time::Duration::from_secs(10))
                    .unwrap_or(true);
                if overdue >= std::time::Duration::from_secs(10) && warn_due {
                    super::exit_log(&format!(
                        "supervisor: ERROR pending_window_creates={} stuck for {:?} (WindowCreated never arrived)",
                        pending, overdue
                    ));
                    pending_warned_at = Some(now);
                }
            }
        }
        match decide_supervisor_action(
            app.inner.main_done.load(Ordering::Acquire),
            all_loops_done,
            windows_settled,
            all_windows_closed,
            device_lost.load(Ordering::Acquire),
            app.window_count(),
            app.pending_window_creates.load(Ordering::Acquire),
        ) {
            SupervisorAction::Exit => {
                // 置「正在退出」并唤醒所有 `App::wait_until` / `wait_while_paused`
                // 等待者 —— 否则用户代码里挂着的等待会让下面的 join 永久阻塞。
                // 必须在 `exit_tx.send` **之前**：先让等待者醒，再让线程退出。
                app.mark_shutdown();
                super::exit_log(&format!(
                    "supervisor: Exit (open={} pending={} created={}) -> exit_tx + wake, waiting for ack",
                    app.window_count(),
                    app.pending_window_creates.load(Ordering::Acquire),
                    created_windows
                ));
                let _ = exit_tx.send(());
                app.wake_event_loop();
                // 确认循环：winit 侧 about_to_wait 回 ack。无焦点 parked 时若某次 wake
                // 丢了，这里 500ms 一次 re-wake＋打日志，最多等 5s（不再永久卡死）。
                let start = std::time::Instant::now();
                loop {
                    match exit_ack_rx.recv_timeout(std::time::Duration::from_millis(500)) {
                        Ok(()) => {
                            super::exit_log("supervisor: exit ack ok, returning");
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            super::exit_log("supervisor: exit ack timed out -> re-wake");
                            let _ = exit_tx.send(());
                            app.wake_event_loop();
                            if start.elapsed() >= std::time::Duration::from_secs(5) {
                                super::exit_log(
                                    "supervisor: exit ack still missing after 5s, returning anyway (see event loop side logs)",
                                );
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            super::exit_log("supervisor: exit ack channel disconnected, returning");
                            break;
                        }
                    }
                }
                return;
            }
            SupervisorAction::CloseAll => {
                super::exit_log(&format!(
                    "supervisor: CloseAll (open={}), closing each window via its close path",
                    app.window_count()
                ));
                // vireo-main 已死、无存活 loop：快照 open 句柄后逐个走关窗路径
                //（close hooks / draw_idle / alive 计数与 X 关闭逐位一致），
                // 下一轮由 all_closed 退出。`continue` 而非阻塞等事件——
                // 关窗已同步完成，退出条件本轮即可满足。
                // 与并发 X 关闭撞车时上方的 take-guard 保证计数只减一次。
                let handles: Vec<usize> = {
                    app.windows
                        .lock()
                        .iter()
                        .enumerate()
                        .filter_map(|(i, w)| w.as_ref().map(|_| i))
                        .collect()
                };
                for handle in handles {
                    apply_winit_event_one(
                        &app,
                        WinitEvent::CloseRequested { handle },
                        &event_tx,
                        &frame_style_tx,
                        &aspect_ratio_tx,
                        &nc_tx,
                        &close_tx,
                    );
                }
                continue;
            }
            SupervisorAction::Wait => {}
        }
        if !processed {
            match rx.recv() {
                Ok(ev) => {
                    let is_created = matches!(ev, WinitEvent::WindowCreated { .. });
                    apply_winit_event_one(
                        &app,
                        ev,
                        &event_tx,
                        &frame_style_tx,
                        &aspect_ratio_tx,
                        &nc_tx,
                        &close_tx,
                    );
                    if is_created {
                        created_windows += 1;
                        app.pending_window_creates.fetch_sub(1, Ordering::AcqRel);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn is_exit(a: SupervisorAction) -> bool {
        matches!(a, SupervisorAction::Exit)
    }
    fn is_close_all(a: SupervisorAction) -> bool {
        matches!(a, SupervisorAction::CloseAll)
    }
    fn is_wait(a: SupervisorAction) -> bool {
        matches!(a, SupervisorAction::Wait)
    }

    #[test]
    fn exit_on_main_loops_settled() {
        assert!(is_exit(decide_supervisor_action(
            true, true, true, false, false, 0, 0
        )));
    }

    #[test]
    fn exit_on_all_closed_even_if_main_alive() {
        assert!(is_exit(decide_supervisor_action(
            false, false, false, true, false, 0, 0
        )));
    }

    #[test]
    fn exit_on_device_lost_beats_everything() {
        assert!(is_exit(decide_supervisor_action(
            false, false, false, false, true, 3, 0
        )));
    }

    #[test]
    fn close_all_when_main_done_loops_done_windows_open() {
        assert!(is_close_all(decide_supervisor_action(
            true, true, false, false, false, 2, 0
        )));
    }

    #[test]
    fn wait_when_main_alive() {
        assert!(is_wait(decide_supervisor_action(
            false, true, false, false, false, 2, 0
        )));
    }

    #[test]
    fn wait_when_loops_running() {
        assert!(is_wait(decide_supervisor_action(
            true, false, false, false, false, 2, 0
        )));
    }

    #[test]
    fn wait_when_creates_in_flight() {
        // 在途建窗：等 WindowCreated 落定再评估，保证收敛，不提前关窗。
        assert!(is_wait(decide_supervisor_action(
            true, true, false, false, false, 1, 1
        )));
    }

    #[test]
    fn wait_when_no_windows_and_not_settled() {
        assert!(is_wait(decide_supervisor_action(
            true, true, false, false, false, 0, 0
        )));
    }
}
