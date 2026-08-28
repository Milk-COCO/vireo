//! 多循环示例：用 `App::spawn(vec![loop_a, loop_b])` 启动两个独立循环。
//!
//! 每个 [`Loop`] 拥有自己的 [`LoopContext`]（帧计数 / 延迟任务）。本例：
//! - 循环 A 持续绘制窗口 1（用 `ctx.tick_count()` 让方块左右移动）。
//! - 循环 B 绘制窗口 2，约 3 秒后返回 `false` 自行结束；当所有循环结束，应用自动退出。
//!
//! 与 [`App::run`] 一样，`spawn` 也返回可 `await` 的 [`ThreadHandle`]；区别在于 [`App::run`] 通常作为末语句直接丢弃句柄（析构时 join 渲染线程，阻塞到窗口关闭），而 `spawn` 把句柄交给你显式 `.await` 或与其他 future 组合。
//!
//! 线程昵称：本例通过 [`Thread::with_name`] 把这条 OS 线程命名为 `loop-demo`；若不设置，
//! `spawn` 会用默认名 `vireo-thread-{n}`（n 为进程内 spawn 创建顺序，从 0 开始）。

use vireo::prelude::*;

#[vireo::main]
async fn main() {

    let w1 = app.window(
        WindowDesc::new("loop A", 360, 240).position(120, 120),
        None::<fn()>,
    );
    let w2 = app.window(
        WindowDesc::new("loop B", 360, 240).position(500, 120),
        None::<fn()>,
    );

    // 演示 Thread 显式容器 + push/extend：单 Thread 包含两循环，顺序执行（同线程无竞争，占用锁保证同窗口不并发）
    let mut t = Thread::new().with_name("loop-demo");
    t.push(Loop::new({
        let w1 = w1;
        move |ctx| {
            let win = match ctx.app().window_ref(&w1) {
                Ok(w) => w,
                Err(_) => return false,
            };
            let fc = ctx.tick_count();
            let t = (fc as f64 % 240.0) / 240.0;
            let x = 40.0 + t * 280.0;
            let mut b = DrawBatch::new();
            b.rectangle(Pos::new(x as f32, 100.0), 40.0, 40.0, Some(Color::new(0.2, 0.6, 1.0, 1.0)));
            win.draw(Color::new(0.05, 0.06, 0.08, 1.0), &[&b]);
            true
        }
    }));
    // 用 extend 一次性推入第二循环（演示 extend），约 3 秒后自行结束
    t.extend(vec![Loop::new({
        let w2 = w2;
        move |ctx| {
            let win = match ctx.app().window_ref(&w2) {
                Ok(w) => w,
                Err(_) => return false,
            };
            let fc = ctx.tick_count();
            let t = (fc as f64 % 240.0) / 240.0;
            let mut b = DrawBatch::new();
            b.circle(Pos::new(180.0, 120.0), (30.0 + t * 30.0) as f32, Some(Color::new(1.0, 0.5, 0.2, 1.0)));
            win.draw(Color::new(0.08, 0.05, 0.05, 1.0), &[&b]);
            if fc < 180 {
                true
            } else {
                ctx.after_ticks(1, || {});
                false
            }
        }
    })]);

    // 单 Thread 一条 OS 线程，窗口占用锁保证同窗口不并发 draw；也可用 app.loops(vec![...]) 糖
    let h = app.spawn(t);

    // vireo 不依赖异步运行时：ThreadHandle 实现了 Future，用 std 轮询直到就绪。
    let _ = h.await;
}


