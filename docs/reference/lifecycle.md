# 关窗拦截与退出收尾

> 面向 vireo 使用者。目标读者：要「有未保存改动就别关窗」的人，和「退出前想存盘 / flush 日志 /
> 释放显存」的人。适用版本：0.2.0。

vireo 有 **一个否决钩子**和 **两个收尾钩子**，分工严格：

| 钩子 | 注册在 | 跑在哪个线程 | 跑几次 | 能否决吗 |
|---|---|---|---|---|
| `on_close_requested(handle, FnMut() -> bool)` | `App` / `VireoWindow` | **winit 线程**（同步） | 每次关窗尝试问一次 | ✅ 返回 `false` 就拦下 |
| `on_exit(FnOnce(ExitReason))` | `App` | **winit 线程** | 每进程**恰好一次** | ❌ 已经决定退了 |
| `on_exit_render(Fn())` | `App` | **渲染线程**（各自的 loop 线程） | 每个 loop 线程一次 | ❌ 同上 |

三条都是**新增**，不影响既有 `App::window(desc, on_close)` —— 那个是 `FnOnce` 的**事后通知**
（关窗已不可撤销），仍在「许可通过之后、真正关窗之前」触发。

---

## 1. 关窗否决（`on_close_requested`）

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vireo::prelude::*;

#[vireo::main]
async fn main() {
    let win = app.window(WindowDesc::new("editor", 800, 600), None::<fn()>);

    let dirty = Arc::new(AtomicBool::new(false));
    let allow = Arc::new(AtomicBool::new(false));
    let ask = Arc::new(AtomicBool::new(false));

    let (d, a, q) = (dirty.clone(), allow.clone(), ask.clone());
    app.on_close_requested(win, move || {
        if a.load(Ordering::SeqCst) || !d.load(Ordering::SeqCst) {
            return true;                     // 放行
        }
        q.store(true, Ordering::SeqCst);      // 让 loop 下一帧弹确认框
        false                                 // 否决这次关闭
    });

    app.run(move |ctx| {
        let w = match ctx.app().window_ref(&win) { Ok(v) => v, Err(_) => return false };
        // ……确认框绘制 + 用户点「退出」时：
        //    allow.store(true); w.close();
        true
    }).await.unwrap();
}
```

### 1.1 语义

- **返回 `true` = 允许关闭**，`false` = 取消本次关闭。窗口保持原样：不隐藏、不销毁、
  `close_hooks` 不会被消耗、NC 状态不动、事件不发。
- **可多次注册**（`App::on_close_requested` 与 `VireoWindow::on_close_requested` 都行）：
  按注册顺序跑，全部返回 `true` 才关，**遇 `false` 立刻短路**（后面的钩子不再执行）。
- 覆盖 **X 按钮 / Alt+F4 / 自绘标题栏关闭按钮 / `win.close()`**。
  无边框窗口的自绘按钮最终也走 `WM_SYSCOMMAND(SC_CLOSE)` → `WM_CLOSE` → winit
  `CloseRequested`，和 X 是同一条路，所以一样会被拦。
- 单条钩子 **panic 按「放行」处理**（fail-open）+ `log::error!`。
  这是刻意的：fail-closed 会让一个 bug 变成「窗口永远关不掉、只能 taskkill」，
  比「少拦一次」更坏。

### 1.2 什么时候**不会**问你

| 路径 | 会不会问 | 为什么 |
|---|---|---|
| `main` 返回后的批量收尾（关窗由 supervisor 直接做） | ❌ | main 已死、没人能驱动 UI，问了只会把进程锁死 |
| GPU 设备丢失 | ❌ | GPU 已不可用，确认框也画不出来 |
| Android Activity Destroy | ❌ | 系统在回收你，没有「不同意」这个选项（用 `App::on_destroyed`） |
| OS 强行销毁窗口 | ❌ | 没有可拦的回调点 |

### 1.3 线程约束（最容易踩）

否决钩子**同步跑在 winit 线程**，和所有 `on_*` 回调一致。因此：

- **不许阻塞**（`std::thread::sleep`、等通道、`MessageBoxW` 都会把整个消息泵卡住 ——
  Windows 会把窗口判成「未响应」并画成幽灵窗口）；
- **不许画帧**（这里没有 `DrawBatch`，也不该有）。「弹确认框」的正确姿势就是上面那段：
  **置原子旗 → 返回 `false` → 让正在绘制的 loop 下一帧把确认框画出来**；
- 需要「点两次 X 才真关」这种状态，用 `FnMut` 捕获自己的计数器即可。

### 1.4 强制退出的配方

程序内部想离开（菜单「退出」、确认框里的「仍然退出」）时，**不要**绕过钩子 ——
没有绕过 API。先置「允许」旗再关：

```rust
allow.store(true, Ordering::SeqCst);
for w in app.windows() { w.close(); }   // 多窗口：逐个关；关完最后一个进程才退
```

---

## 2. `on_exit`：进程级收尾（每进程一次）

```rust
app.on_exit(|reason| {
    log::info!("exiting: {reason:?}");
    save_now();              // 纯 CPU 收尾
});
```

- 时机：supervisor 判定退出 → winit 侧收到退出信号（携带原因）→ **钩子全部跑完** →
  才回 ack、才 `event_loop.exit()`。所以钩子不完成就不会退。
- 跑在 **winit 线程**，只做纯 CPU 的事：落盘、flush 日志、发信号、drop `Arc<Texture>` 释放显存。
- **不要**在这里画帧、建窗口、调 `win.draw`（event loop 马上退，没有下一帧）。
- 可注册多个，按注册顺序；单条 panic 被捕获 + `log::error!`，不影响后面几条，也不影响退出。
- `ExitReason` 四个变体，优先级 `DeviceLost > MainPanicked > MainReturned > AllWindowsClosed`：

| 变体 | 含义 |
|---|---|
| `MainReturned` | `#[vireo::main]` 的 main 体正常返回（**最常见**；包含 main 返回后自动关掉残留窗口那一步） |
| `MainPanicked` | main 体 panic（随后退出码 1） |
| `AllWindowsClosed` | 最后一个窗口被关掉，而 main 还在跑 |
| `DeviceLost` | GPU 设备丢失 |

---

## 3. `on_exit_render`：渲染线程收尾（每个 loop 线程一次）

```rust
let app_for_hook = app.clone();
app.on_exit_render(move || {
    // 跑在渲染线程：能碰 App / GPU
    drop(my_big_texture.clone());          // 显存
    let _ = app_for_hook.gpu().is_device_lost();
});
```

- 跑在**各自的 loop 线程**上，就在该线程退出前。所以：`app.run(..)` = 一次；
  `app.run(..)` + `app.spawn(..)` = 两次。需要「只做一次」就自己一个 `AtomicBool` 闸住。
- **能碰 `App` / `VireoWindow` / GPU**（提交一次 compute、读回 buffer、drop 纹理）。
- ⚠️ **不保证还能画帧**：走到这里时窗口 surface 多半已经被 supervisor drop 了
  （关窗握手比线程退出早）。要「最后一帧」就在自己的 `on_tick` 返回 `false` **之前**画。
- 时机排在 `ThreadHandle` 完成通知与 supervisor 的 `all_loops_done` **之前**
  （`state.done` 在钩子之后才置位），所以 main 返回型退出里「渲染侧收尾先于 `on_exit`」
  有保证；**末窗关闭 / 设备丢失**这两条不依赖 loop 结束，**没有顺序保证**。
- panic 路径也会跑到（崩溃后还想 flush 日志是合理需求）。
- 单条 panic 被捕获 + `log::error!`，不影响其余钩子。

---

## 4. 四条退出路径

| 触发 | 谁会跑 |
|---|---|
| main 返回（还开着窗） | `on_exit_render`（loop 退出时）→ 批量关窗 → `on_exit(MainReturned)` |
| 最后一个窗口关闭 | 你的循环看到 `window_ref` 报错后返回 → `on_exit_render` → `on_exit(AllWindowsClosed)` |
| main panic | 同上，`on_exit(MainPanicked)`，退出码 1 |
| GPU 设备丢失 | loop 退出 → `on_exit_render` → `on_exit(DeviceLost)` |

**两个钩子都不跑的例外**：进程被信号杀掉（Ctrl+C / `SIGKILL`）、`panic = "abort"`、
winit 连 event loop 都建不出来（`run_blocking` 返回 `Err`）。这些路径上没有 Rust 代码有机会执行。

---

## 5. Android

Android 没有 X，`on_close_requested` 基本用不上（返回键走 `App::on_back` / `consume_back`，
不经过它）。Activity 被销毁时 vireo 走的是既有生命周期 `App::on_destroyed`（渲染线程、
可以画帧），而不是 `on_exit` —— 那条路径上 `about_to_wait` 是直接 ack + exit 的。
`android_main` 里禁止 `process::exit`，这也是「Android 有自己一套收尾」的原因。

---

## 6. 调试

`VIREO_EXIT_TRACE=1` 打开退出链路打点（stderr），能看到：

```
[vireo-exit] request_close: vetoed by on_close_requested (handle=0)
[vireo-exit] supervisor: Exit (MainReturned, open=0 pending=0 created=1) -> exit_tx + wake, waiting for ack
[vireo-exit] about_to_wait: exit_rx hits=1 reason=MainReturned → ack + event_loop.exit() + self-wake
```

默认关闭，零开销（`OnceLock` 缓存开关）。

---

## 7. 常见误解

- **「`on_close` 就能拦」** —— 不能。它是 `FnOnce`、没有返回值，触发时木已成舟。
  拦要用 `on_close_requested`。
- **「否决钩子里能弹自己的 UI」** —— 不能，它在 winit 线程上同步跑。置旗 + 让 loop 画。
- **「`on_exit_render` 能再画最后一帧」** —— 不能指望。surface 可能已经没了。
- **「不注册就什么都不会变」** —— 是。三个钩子都有空表早退，不注册时零额外开销，
  退出行为与 0.1.x 完全一致。
- **「关不掉的时候进程是不是僵了」** —— 不是。否决只拦「用户/程序主动关某个窗口」；
  main 返回、设备丢失这些收尾路径不问否决，所以永远不会因为一个返回 `false` 的钩子
  变成关不掉的僵尸进程。
