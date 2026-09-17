//! loop panic 收尾验证：on_tick 里故意 panic，进程应干净退出。
//!
//! 跑法：`cargo run --example loop_panic`（需显示器）。
//! 预期（0.1.2+）：约 120 帧后 on_tick panic，主线程 `.await.unwrap()` 跟着 panic，
//! 控制台打出两行 `[vireo]` 日志后窗口关闭、进程退出（退出码 1），无需 taskkill。
//! 0.1.2 之前：窗口冻住、进程不退、exe 被锁，只能 taskkill。

use vireo::prelude::*;

#[vireo::main]
async fn main() {
    let idx = app.window(WindowDesc::new("loop-panic", 640, 420), None::<fn()>);

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return false,
        };
        if ctx.tick_count() >= 120 {
            panic!("intentional panic: testing shutdown-after-panic");
        }

        let mut batch = DrawBatch::new();
        draw_rectangle(
            &mut batch,
            Pos::new(270.0, 170.0),
            100.0,
            80.0,
            Some(ORANGE),
        );
        win.draw(BLACK, &[&batch]);

        true
    })
    .await
    .unwrap();
}
