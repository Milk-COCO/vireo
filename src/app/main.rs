use std::panic;
use std::sync::atomic::Ordering;
use std::thread;

use crate::app::App;
use crate::window::WinitEvent;

/// vireo-main 线程：运行用户 `main` future。
/// 由 [`App::run_entry`] 调用，spawn 一条独立 OS 线程执行 `pollster::block_on(main(app))`。
pub(crate) fn spawn<F, Fut>(app: App, main: F)
where
    F: FnOnce(App) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let app_inner_for_flag = app.inner.clone();
    thread::Builder::new()
        .name("vireo-main".into())
        .spawn(move || {
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                pollster::block_on(main(app));
            }));
            if let Err(payload) = result {
                let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "<non-string panic payload>".to_string()
                };
                eprintln!("[vireo] user main closure panicked: {}", msg);
            }
            app_inner_for_flag
                .main_done
                .store(true, Ordering::Release);
            if let Some(tx) = app_inner_for_flag.event_tx.lock().clone() {
                let _ = tx.send(WinitEvent::Wake);
            }
        })
        .expect("failed to spawn vireo-main thread");
}
