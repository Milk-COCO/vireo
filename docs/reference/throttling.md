# 帧率、垂直同步与限流

> 面向 vireo 使用者。目标读者：想把帧率"放开"的人，以及放开以后被风扇教育过的人。
> 适用版本：0.2.0。

vireo 的哲学是 **"报状态、不做策略"**：它把「该不该画」和「有没有被 vsync 钳住」都告诉你，
但**不会**替你 sleep —— 除了你自己显式配置的 3 个上限。

所以"帧率"这件事，你得先知道它一共有几层。

---

## 1. 帧率的四个来源

| # | 层 | 默认值 | 它怎么限制你 | 关掉它 |
|---|---|---|---|---|
| 1 | **present vsync** | `PresentMode::AutoVsync` | `Present` 阻塞等 vblank → 卡在刷新率（60/120/144…） | `win.set_present_mode(PresentMode::AutoNoVsync)` |
| 2 | **窗口帧率上限** | `App` 默认 `Some(240)`，建窗时种给窗口 | 每次 draw 后按 `1/fps` 睡 | `win.set_max_fps(None)` |
| 3 | **Thread tick 上限** | `spawn` 时用 `App::max_fps` 做种子 | 整个 loop 一轮（tick）按 `1/tps` 睡 | `app.set_max_fps(None)`（**必须在 spawn/建窗之前**） |
| 4 | **拖动 resize 上限** | `drag_cap = true` | 拖动期间即使 `max_fps=None` 也压到刷新率 | `win.set_drag_cap(false)` / `app.set_drag_cap(false)` |

### 每层为什么存在

1. **vsync**：不撕裂。关掉后画面可能在扫描途中切换，出现水平撕裂线。
2. **窗口上限**：默认 240 是"防止无限空转"的保险丝，也是常见诉求。
3. **Thread tick 上限**：一个 Thread 可能驱动多个窗口/多个 `Loop`；它限制的是**循环次数**，不是单窗口帧数。
4. **拖动上限**：Windows 上拖动窗口时 `acquire` 会失去 vsync 节流，循环瞬间全速 → 拖动更卡、更烫。这层是故意在拖动期抢回控制权的。

---

## 2. 关闭节流

### 2.1 先选场景

| 场景 | 建议 |
|---|---|
| 只想**测吞吐**（benchmark / 压测） | 4 层全关，允许风扇起飞 |
| 想要**低延迟**（游戏、实时交互） | 关 vsync + `frame_latency(1)`，但保留一个较宽的 `max_fps`（如 500）兜底 |
| 想要**省电 60 帧** | 别关，直接 `set_max_fps(Some(60))` |
| 想要**自适应**（前台全速、后台省电） | 见 §4.3 |

### 2.2 代码

```rust
use vireo::prelude::*;

#[vireo::main]
async fn main() {
    // 3) Thread tick 上限：必须在 spawn 之前把 App 的种子清掉
    //    （Thread::max_tps = None 会回落到 App 的值，见 §5 误解 ③）
    app.set_max_fps(None);
    // 4) 拖动期不要压回刷新率
    app.set_drag_cap(false);

    let win_idx = app.window(
        WindowDesc::new("uncapped", 1280, 720)
            // 1) 关 vsync（AutoNoVsync 优先；Immediate 延迟最低但会撕裂）
            .present_mode(wgpu::PresentMode::AutoNoVsync)
            // 可选：减少在途帧，降低延迟（不是节流）
            .frame_latency(1),
        None::<fn()>,
    );

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&win_idx) {
            Ok(w) => w,
            Err(_) => return false, // 窗口还没建好 / 已关闭
        };
        // 2) 窗口级上限：建窗时已从 App 种下，但运行期改 App 不会传播给已存在的窗口，
        //    这里显式再清一次最保险。
        win.set_max_fps(None);

        let mut batch = DrawBatch::new();
        // … 你的绘制 …
        win.draw(BLACK, &[&batch]);
        true
    })
    .await
    .unwrap();
}
```

### 2.3 如何验证真的关掉了

三个读数**分开看**：

| 读数 | 含义 | 若它不符合预期 |
|---|---|---|
| `ctx.fps()` | **tick** 速率（每轮 loop 一次，跨窗口共享） | 不高 → 第 3 层（Thread tick 上限）没关 |
| `win.presented_fps()` | **真正 present 成功**的速率 | 卡在 60/120/144 → 第 1 层（vsync）或第 2 层（窗口上限）还在 |
| `win.is_vsync_throttled()` | present 有没有被 vblank 钳住（**实测**） | `true` = 仍被钳；`false` = vsync 已不兜底 |

判断经验：

- `ctx.fps()` 高、`presented_fps()` 卡在刷新率 → **vsync 没关**。
- 两者都高、`is_vsync_throttled() == false` → 真的全速了。
- 前 8 帧左右 `is_vsync_throttled()` 一直为 `true` 然后翻 `false` → 正常：那是**滞回**（见 §3.1）。

---

## 3. 关掉之后你要自己接管的事

关掉节流等于把保险丝换成"你自己"。以下每条都真实会咬人。

### 3.1 Windows 的"延迟解除"：DWM 遮挡检测

切到后台不会立刻掉帧，而是**过一会**才掉 —— 这不是 vireo 的问题：

1. `Present(1)` 阻塞等 vblank，这是 vsync 节流的来源；
2. 窗口被完全遮住 / 最小化 / 切虚拟桌面后，**DWM 停止为它合成**；
3. DXGI 把 swapchain 标记为 occluded → `Present` **立即返回**；
4. 从这一刻起 vsync 不再钳制，循环全速。

延迟来自 DWM 的判定是**周期性且保守**的（要"完全遮挡持续若干帧"才敢停合成，避免短暂遮挡时窗口闪断）。
微软未公开具体超时，实测数百毫秒到一两秒；最小化通常最快，"被别的窗口盖住"最慢。

**注意：失焦本身不会解除 vsync**。可见但无焦点的窗口照样被 vblank 钳住。
（0.1.x 的 `render_advice()` 曾用"是否聚焦"猜这件事，会误报；0.2.0 起改为实测。）

对策：

```rust
// 先看有没有 vsync 帮你踩刹车；没有再自己限
if !win.is_vsync_throttled() {
    std::thread::sleep(std::time::Duration::from_millis(16));
}
```

### 3.2 最小化 / 0×0 / 已关闭：别画

```rust
if matches!(win.render_advice(), RenderAdvice::Skip) {
    return true; // 跳过构建（注意：不要返回 false，那会结束这个 loop）
}
```

`Skip` 的含义是"构建出来也会被丢弃"（`ZeroSized` / `Occluded` / `Closing` / `SurfaceStale`）。
想真正停下来不空转，用 §4.4 的事件驱动写法。

### 3.3 拖窗口时会被压回刷新率

`drag_cap` 默认开启：即使你 `set_max_fps(None)`，**拖动 resize 期间**上限仍被压到显示器刷新率。
想连拖动也全速：`win.set_drag_cap(false)`。

### 3.4 高帧率会改变你的时间步进

`dt` 从 16.7ms 掉到 1ms 意味着：

- 用"每帧固定增量"的动画会**快 16 倍**；
- 固定步长物理若每帧只积分一次会**不稳定**；
- 鼠标采样率没变，高帧率下会**重复读到同一个位置**。

对策：所有时间逻辑用真实 `dt`，并做上限钳制（如 `dt.min(1.0 / 30.0)`），避免长时间挂起后"跳一大步"。

### 3.5 电池、风扇与降频

全速 = GPU/CPU 满载。笔记本上会触发降频，反而让**帧时间抖动更大**。
想要"高但稳"，通常 120~240 比"无上限"体验更好。

### 3.6 多窗口会互相抢

每个窗口有自己的上限，但 Thread 的 tick 是共享的：`ctx.fps()` 是所有窗口轮流的**总**速率。
多窗口时优先看各窗口的 `presented_fps()`。

---

## 4. 手动限流的四种配方

### 4.1 朴素 sleep（能用，但不推荐）

```rust
win.draw(BLACK, &[&batch]);
std::thread::sleep(std::time::Duration::from_millis(4)); // 看起来像 250fps
true
```

问题：Windows 定时器精度会让 `sleep(4ms)` 实际变成 4~15ms，帧率**不稳**；而且它让流水线多一次上下文切换。

### 4.2 相位锁（推荐；vireo 内部也是这么做的）

要点：**用绝对 deadline**，而不是"每帧睡固定时长"；落后不追赶。

```rust
let mut deadline: Option<std::time::Instant> = None;
const TARGET_FPS: f64 = 120.0;
let stride = std::time::Duration::from_secs_f64(1.0 / TARGET_FPS);

// 每帧：
let now = std::time::Instant::now();
match deadline {
    Some(d) if d > now => {
        std::thread::sleep(d - now);
        deadline = Some(d + stride);
    }
    _ => deadline = Some(now + stride), // 落后：重置，不追赶
}
```

### 4.3 自适应（全速 + 自动省电，推荐）

```rust
// 被 vsync 钳着就全速跑；一旦不钳了，自己踩刹车
if !win.is_vsync_throttled() {
    let target = 60.0;
    let need = 1.0 / target - last_frame_time;
    if need > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(need));
    }
}
```

配合 §3.2 的 `Skip` 检查，就是"前台满帧 / 后台 60 / 最小化不画"的三档策略。

### 4.4 事件驱动（最省电）

只有用户操作才需要重画的应用，根本不需要常驻循环：

```rust
app.run(move |ctx| {
    if !ctx.dirty() {   // 你自己维护的脏标记
        return false;   // 结束本 loop → 线程回到 condvar 等待
    }
    // …画…
    true
})
```

缺点是"常驻动画"场景不适用。

---

## 5. 常见误解

**① "失焦就会解除 vsync"** —— 错。可见但无焦点**仍被钳**。真正解除的是 Windows 判定窗口被遮挡（§3.1），且有延迟。

**② "`app.set_max_fps(None)` 会立刻让已存在的窗口不限制"** —— 错。App 的值只在**建窗那一刻**种给窗口，运行期改 App 不传播，需逐窗 `win.set_max_fps(None)`。

**③ "`Thread::with_max_tps(None)` 表示该线程不限速"** —— 错。实现是 `thread.max_tps.or(app.max_fps())`，`None` 会回落到 App 的值。想不限速请先 `app.set_max_fps(None)`。

**④ "`presented_fps()` 就是我的帧率"** —— 半对。它是真正 present 成功的速率；`ctx.fps()` 是 tick 速率，一个 loop 里可能画 0 帧或多帧。

**⑤ "`frame_latency(1)` 能提高帧率"** —— 不能。它只减少在途帧、降低延迟，不提高上限。

**⑥ "`PresentMode::Immediate` 是免费的低延迟"** —— 不免费，它会撕裂。`AutoNoVsync` 通常先在驱动层尝试"无 vsync 但不撕裂"的路径。

---

## 6. 已知限制

| # | 限制 | 说明 |
|---|---|---|
| 1 | `Thread::max_tps = None` 回落到 `App::max_fps` | 想真正不限速请先设 `app.set_max_fps(None)` |
| 2 | `App::set_max_fps` 不传播给已存在窗口 | 逐窗 `win.set_max_fps` |
| 3 | `drag_cap` 在 `max_fps=None` 时仍压到刷新率 | 拖动期防空调转的保险，可关 |
| 4 | 取不到显示器刷新率时按 60Hz 估算 | `is_vsync_throttled()` 可能偏保守 |
| 5 | 没有"空闲自动降频" | 引擎不做策略，用 `is_vsync_throttled()` 自己接（§4.3） |
| 6 | `ctx.fps()` 是 tick 速率、跨窗口共享 | 多窗口看 `presented_fps()` |

---

## 7. 完整模板：既全速又体面

```rust
use std::time::{Duration, Instant};

#[vireo::main]
async fn main() {
    app.set_max_fps(None);      // 不让 Thread 层限速
    app.set_drag_cap(false);    // 拖动也不压回刷新率

    let win_idx = app.window(
        WindowDesc::new("uncapped + idle throttle", 1280, 720)
            .present_mode(wgpu::PresentMode::AutoNoVsync)
            .frame_latency(1),
        None::<fn()>,
    );

    let mut idle_deadline: Option<Instant> = None;
    const IDLE_FPS: f64 = 60.0;

    app.run(move |ctx| {
        let Ok(win) = ctx.app().window_ref(&win_idx) else {
            return false;
        };
        if matches!(win.render_advice(), RenderAdvice::Skip) {
            return true; // 被遮挡/最小化：不构建，但别退出 loop
        }
        win.set_max_fps(None);

        // 只有"没被 vblank 钳住"时才自己踩刹车
        if win.is_vsync_throttled() {
            idle_deadline = None;
        } else {
            let now = Instant::now();
            let stride = Duration::from_secs_f64(1.0 / IDLE_FPS);
            match idle_deadline {
                Some(d) if d > now => {
                    std::thread::sleep(d - now);
                    idle_deadline = Some(d + stride);
                }
                _ => idle_deadline = Some(now + stride),
            }
        }

        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::new(40.0, 40.0), 200.0, 120.0, Some(SKYBLUE));
        win.draw(BLACK, &[&batch]);
        true
    })
    .await
    .unwrap();
}
```

---

## 8. 检查清单

- [ ] 清楚自己关的是哪几层、为什么
- [ ] `app.set_max_fps(None)` 在 spawn / 建窗**之前**
- [ ] 每个窗口显式 `set_max_fps(None)`（不依赖 App 传播）
- [ ] 想连拖动也全速 → `set_drag_cap(false)`
- [ ] 有"没被 vsync 钳住"时的兜底限流（`is_vsync_throttled()`）
- [ ] `RenderAdvice::Skip` 时跳过构建、**不**返回 `false`
- [ ] 动画/物理用真实 `dt`，并做上限钳制
- [ ] 在笔记本/电池场景实测过帧时间抖动
