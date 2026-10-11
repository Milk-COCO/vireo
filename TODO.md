# TODO

按「阻塞程度」排。每条都写了**为什么还没做**和**做之前需要什么**，避免变成
无信息量的愿望清单。

---

## 环境

### WSA（Windows Subsystem for Android）**不能**替代真机测 vireo

2026-10-06 逐项排查后的定论。**已放弃这条路**，记在这里是为了别再走一遍。

**表面症状**：`adb install` 成功后启动即崩，logcat 里

```
vireo gpu: no adapter found
E/platform: DRM_IOCTL_VIRTGPU_CONTEXT_INIT failed with Invalid argument
E/platform: virtgpu backend not enabling VIRTGPU_PARAM_CREATE_GUEST_HANDLE
```

**根因**：宿主没把可用 GPU 透传给 guest。`/dev/dri/card0` + `renderD128` 设备节点
存在但打不开，`ro.hardware.egl=emulation`（EGL 在软件模式）。失败发生在
**内核层**，在 wgpu 之下。

**这不是能靠 vireo 配置绕过的**。已试过且全部同样结果：

| 尝试 | 结果 |
|---|---|
| 默认协商（`App::new()`） | `no adapter found` |
| `Backends::GL \| VULKAN`（走 ANGLE） | `no adapter found` |
| `Backends::ALL` + `InstanceFlags::DEBUG` | `no adapter found` |

**排查过程中被证伪的四个假设**（都曾以为是原因）：

1. ❌「机器不支持虚拟化，WSA 起不来」—— `VirtualizationFirmwareEnabled=False`
   在**任何客户机**上都读 False，不能当证据。WSA 服务一直在跑
   （`vmmemWSA` / `WsaService` / `WsaClient`）。
2. ❌「APK 装不上（ABI 不匹配）」—— `abilist` 含 `arm64-v8a`，实测装得上。
   （但 x86_64 仍值得带，见下）
3. ❌「GPU 后端没选对」—— 三个后端全试过。
4. ❌「Vulkan ICD 缺失」—— `C:\Users\obe\Documents\WSA\vulkan_dzn.dll`（5.63 MB）
   在，D3D12→Vulkan 转译层是有的，是 virtgpu 挂了。

**要重新启用这条路**：换一台**裸机或有 GPU 直通**的 Windows。WSA 在真实
NVIDIA/AMD/Intel GPU 上是能跑 wgpu 应用的。

### 顺带修掉的真问题：`INSTALL_FAILED_VERSION_DOWNGRADE`

旧 xbuild 版本（`versionCode=16777472`，xbuild 用时间戳推导）的 APK 还装在设备上，
而我们换 gradle 后是 `versionCode=1` → **所有 `adb install -r` 都被拒**，
报错 `Update version code 1 is older than current 16777472`。

- 绕过：`adb uninstall <pkg>` 后重装
- 模板默认 `versionCode = 1` 对新应用没问题，但如果用户是从 xbuild 迁移过来的，
  第一次装必须先卸载

### 多 ABI：模板已支持，但仍建议带 x86_64

`examples/android/build.gradle` 现在是 ABI→triple 映射（`arm64-v8a` + `x86_64`）。
加 ABI 只要加一行。虽然本机 WSA 用不上，但**换一台有 GPU 的机器就能立刻测**，
不必重新改构建配置。

---

## Android

### 16 KB page size：已修，两处真问题（2026-10-06）

这一条翻来覆去改了三次，最终结论如下 —— **之前两版都是错的**。

#### 真问题 1：判据有两条，官方工具只查一条

Google 要求两条：

| # | 判据 | 不满足后果 |
|---|---|---|
| 1 | 每个 `PT_LOAD` 的 `p_align >= 0x4000` | 加载失败 |
| 2 | **`PT_GNU_RELRO` 的 `(VirtAddr + MemSiz) % 0x4000 == 0`** | **运行时 segfault** |

**官方 `check_elf_alignment.sh` 只查第 1 条**（`grep LOAD | awk '{print $NF}' | head -1`
—— 连第二个 LOAD 都不看）。RELRO 那条是文档里独立的手工步骤。**所以官方工具会给假绿灯。**

#### 真问题 2：RELRO 由 `common-page-size` 控制，而 driver 从不注入它

| 开关 | 控制 | driver 默认（r26 / r28） |
|---|---|---|
| `-z max-page-size` | `PT_LOAD` 的 `p_align` | 4096 / **16384** |
| `-z common-page-size` | **RELRO 段末边界** | **两个都不注入**（lld 默认 4096） |

RELRO 起点随 LOAD 按 16384 对齐、终点只按 4096 对齐 ⇒
`(起点 + RELRO 大小) % 0x4000` **约 1/4 概率才是 0**。

**所以「NDK r28+ 就合规」是错的**，而且更隐蔽的是
**「cargo-ndk ≥ 4.0.0 会补 flag 所以 NDK 26 也合规」也是错的** ——
它只补 `max-page-size`（判据 1），不补 `common-page-size`（判据 2）。
NDK26 当时能过是**运气**：给 `.data.rel.ro` 加 4 KB 就立刻翻成 FAIL。

#### 已修

`examples/android/.cargo/config.toml`（新增）对两个 64 位 target 显式传**两个** flag。
代价：`.so` 体积**零增长**（padding 落在本就存在的零填充区）。

实测（同一 cdylib）：

| | LOAD | RELRO end | `mod 0x4000` |
|---|---|---|---|
| 修复前 NDK26 | `0x4000`×4 | `0xe0000` | `0x0` ✅ **运气** |
| 修复前 NDK28 | `0x4000`×4 | `0xe1000` | `0x1000` ❌ |
| **修复后 NDK26** | `0x4000`×4 | `0xe0000` | `0x0` ✅ |
| **修复后 NDK28** | `0x4000`×4 | `0xe4000` | `0x0` ✅ |
| **修复后 x86_64** | `0x4000`×4 | `0xf0000` | `0x0` ✅ |

`zipalign -c -P 16 -v 4`（release APK）→ `Verification succesful`。

新增 `examples/android/check-16kb.ps1`：**两条判据都查**，非零退出码拦出包。
（真机验证时它抓出 `jniLibs/` 里还是修复前的旧 `.so`，脚本本身有效。）

#### 仍然欠着的

1. **16 KB 环境实测** —— 静态全绿 ≠ 真机不炸。需要 Android 15 的
   "Google APIs Experimental 16 KB Page Size ARM64 v8a" 模拟器镜像或 16 KB 真机。
   ```bash
   adb shell getconf PAGE_SIZE     # 期望 16384
   ```
2. **AAB 验证** —— 模板只出 APK。改发 `.aab` 时 bundletool **不会**自动 zipalign：
   ```bash
   bundletool dump config --bundle=app-release.aab | Select-String alignment
   #   须 PAGE_ALIGNMENT_16K
   ```

**Play 截止日期是 2027-02-01**（不是之前写的 2025-11，那是旧表述）。

**相关**：`examples/android/README.md` §5（含完整原理与验证清单）

---
### ~~`onStop` / `onDestroy`~~ ✅ **两者都已完成**（`on_stopped` / `is_destroyed` + 闸门开关 + wait 家族）

原先写的「`onDestroy` 不做，影响很小」**是错的** —— 那个论证只对 vireo 内部成立
（`VireoWindow::close()` 确实走自己的路，不依赖 `onDestroy`），但对「区分用户是
**切走了**还是**主动关了我**」这个需求，`onPause` / `onStop` **都答不了**。

| 问题 | 靠什么 |
|---|---|
| 我在前台吗 | `is_paused()` |
| 我完全不可见吗 | `is_stopped()` |
| **用户是切走了还是主动关了我** | **`is_destroyed()`**（唯一） |
| 我的进程要死了吗 | **无解**（进程被杀时没有任何回调） |

且它是 `set_auto_pause(false)`（后台继续跑逻辑）之后**唯一的退出信号** ——
划掉任务时 `paused` / `stopped` 都会照常来（应用只是不可见），只有 `destroyed`
告诉你「不用再跑了」。

本轮一并做的（用户决定）：
1. `App::set_auto_pause(false)` —— 闸门可接管，**默认 `true`**（懒得写的用户零改动）
2. `App::wait_while_paused()` + `wait_until()` + `notify_waiters()` —— 「暂停直到」
3. `App::is_destroyed()` + `on_destroyed()` —— 纳入本次范围

#### 硬约束：等待必须可打断，否则 join 永久阻塞
supervisor 判定退出时调 `App::mark_shutdown()`（置标志 + `notify_all`），
**早于线程 join**。所以「等待方需要知道退出」这件事是引擎职责 ——
用户代码里挂着的 `wait_until` 不能因为引擎要退就把它挂死。

`LoopContext` 够不到自己线程的 `done`（只有 `app` / `tick_count` / `fps_stats`），
但**不需要** per-thread 粒度：「窗口全关」和「设备丢失」这两个全局条件已经覆盖了
「必须唤醒」的全部场景，所以用**进程级 `shutdown` 标志**即可，`LoopContext`
一行未改。

#### 发现的硬约束：回调**不可能**在暂停期间触发

原计划是「照 `nativeOnPause` 抄 4 步」。动手前先读了 `src/thread.rs` 的生命周期闸门，
发现渲染线程在暂停期间**整体阻塞**（不 `on_tick`、不 `draw`、不 acquire/present，
为的是后台 CPU+GPU 占用归零）。于是：

- 暂停期间**没有任何用户代码在跑** → 「离开时立刻保存进度」**做不到**
- 用户回调只能**恢复后补发**（`on_paused` 一直是这个语义）
- 恢复后再读 `is_paused()` 已经是 `false`，**信息已丢**

所以「只在彻底离开时保存进度」这个原始诉求**本身不成立**，必须把 API 定位成
「**区分两种暂停**」而不是「新的保存时机」。

#### 因此用锁存，不用实时标志

Android 回调顺序是 `onPause → onStop → … → onStart → onResume`，而 `onStart`
紧挨 `onResume`，两者都在渲染线程被唤醒**之前**跑完。所以渲染线程醒来时读
实时 `STOPPED` 读到的**已经是 false** —— 直接读会把「彻底离开」和「短暂中断」
混成同一件事，而这恰恰是要解决的核心问题。

改用**锁存** `SAW_STOP`：UI 线程在 `onStop` 置位，渲染线程恢复后**一次性消费**
（消费即清零，于是每个暂停周期恰好上报一次）。

#### 关键认知：渲染线程每个暂停周期只观察得到**一次**代号变化

闸门在暂停期间把渲染线程整个挡住，所以它只在**恢复后**醒来，`GEN` 变化
（pause / resume）它只见到 resume 那一次 —— 中间无论发生多少次 `onStop` 都看不见。
这既说明不需要独立的 `STOP_GEN`，也说明锁存是唯一的信息载体。

#### 早退顺序有坑

`fire_lifecycle_callbacks` 原本在「没注册任何回调」时早退。现在 `on_stopped` 一张表
可能是空的，若早退发生在消费锁存**之前**，`SAW_STOP` 会留在 true —— 下一个
「没有 stop 的暂停」就会**误报一次彻底离开**。锁存是一次性事件而不是状态，
必须先消费再考虑早退。

#### 验证
- `cargo test --lib` **294 passed**（+5：锁存跨 resume 仍可读且只报一次 /
  **每次 onStop 都置位（回归）** / 短暂中断不置锁存且 `is_stopped()` 为 false /
  连续多次 onStop 只报一次 / 桌面恒 `false`）
- `cargo ndk -t arm64-v8a` / `-t x86_64` `check --lib` 通过
- `gradlew assembleDebug '-PndkVersion=28.2.13676358'` BUILD SUCCESSFUL
- **MuMu 实机**（Android 15 / x86_64）：`adb shell input keyevent 3`（HOME）往返
  **3 轮**，logcat 恰好 6 条（每轮 paused + stopped 各一条），HUD `paused=3 stopped=3`
  与之一致。时间戳形态也印证设计：paused 在暂停生效时、stopped 在恢复时，
  两者相隔约等于 HOME 到回来的间隔。
- **闸门可接管 + `onDestroy` 也已实机验证**（`auto_pause=false` 时）：
  ```
  life  paused=1 stopped=1 destroyed=0
  gate  auto_pause=false bg_frames=1245 waited=1
  ```
  `bg_frames=1245` 是最硬的证据 —— HOME 后台 5 秒里循环仍在跑 1245 帧
  （`auto_pause=true` 时这里恒 0）。`waited=1` 证明 `wait_while_paused()`
  正常返回未挂死。手动划掉最近任务后 66ms 内三条回调依次触发：
  ```
  12:13:15.490  paused
  12:13:15.490  stopped
  12:13:15.556  destroyed = 用户主动关闭
  12:13:15.557  「用户主动关闭，退出循环」   ← is_destroyed() 分支生效
  ```
- 中途踩到一次 gradle 增量状态损坏（`mergeDebugJniLibFolders\merger.xml` 内容
  无法解析），是先前构建被中断留下的；删掉该目录即可，不必 `clean`。

#### 真机验证抓到的两个 bug（单测和桌面编译都抓不到）

1. **`App::on_paused` 在 Android 上从来就没触发过**（既有 bug，非本轮引入）。
   `thread.rs` 里生命周期闸门在**代号检查之前**：渲染线程进闸门后阻塞到恢复，
   醒来时 `is_paused()` 已是 `false`，`fire_lifecycle_callbacks` 于是永远走
   resumed 分支。**修法**：把代号检查移到闸门**之前** —— 先判代号、先发回调，
   再进闸门阻塞。那一刻 `is_paused()` 还是 `true`，`on_paused` 真的会在暂停生效时
   触发。文档里「恢复后补发」的说法是这个 bug 的副作用，不是有意设计，已一并改掉。
2. **`STOPPED` 实时标志只有 `onStop` 置位、没有任何地方重置**。于是 `on_stop` 里
   「值没变化就不重复置锁存」的守卫在第一轮之后就恒为假 —— 第二轮的 `onStop` 全被
   吞掉（实测 `paused=2` 但 `stopped=1`）。**修法**：删掉 `STOPPED`，
   `is_stopped()` 改为**派生量** `已暂停 && 锁存`。Android 保证「不可见 ⊆ 已暂停」，
   于是这个组合就是「不可见」，而且恢复时 `is_paused()` 变 false 就自动翻回 ——
   **根本不存在需要忘记的 reset 路径**。`on_stop` 也随之改为无条件置位（幂等，
   重复 `onStop` 无害）。

   教训：「只有 set、没有 clear」的标志是这类 bug 的温床。凡是能派生出所需语义的
   场合，就不要引入独立标志。

#### 踩坑
- **我的示例代码自己有个 `Arc` bug**：生命周期计数器用了裸 `AtomicU32` + 手写
  `Clone`（逐字段复制），回调自增的是**副本**，HUD 读原处恒为 0。这让我一度
  误判「回调从没触发」。真机验证时 **logcat 才可信**，HUD 计数在修好之前是假信号。
- 三处只有 Android target 才编到，桌面 `cargo test` 全绿：
  1. `jni_str!` 宏**要求字面量**，不能传 `const` 标识符 → 绑定表只能写字面量，与
     `JAVA_METHOD_*` 常量脱钩。加 `JAVA_NAMES_MATCH_LITERALS` 编译期断言钉住
     （`PartialEq` 在 const 里未稳定，自己写了 `const fn bytes_eq` 逐字节比）。
  2. `AtomicBool::store` **不接受** `Acquire` / `AcqRel`（只有 `swap` 接受）。
  3. `LifecycleCbs` 从 2 元组变 3 元组后 `cbs.0/.1/.2` 只能靠记忆区分 → 改具名
     字段结构体。
- 示例里 `move || atomic.fetch_add(1, ..)` 不满足 `Fn() -> ()`（fetch_add 返回
  u32）→ 必须写花括号语句块。模板是独立 package，`log` 也得自己加进 Cargo.toml。

---

### ~~`onTrimMemory`~~ ✅ **已完成**（`App::on_memory_warning`，零 JNI）

⚠️ **原判断错了，已纠正**：此前多处写「winit 把 `LowMemory` 也吞掉了」——
**不对**。winit 是**唯一**把它正常转发的：

```rust
// winit-0.30.13/src/platform_impl/android/mod.rs:265-267
MainEvent::LowMemory => {
    callback(event::Event::MemoryWarning, self.window_target());
},
```

`Runner` 只实现了 `resumed` / `about_to_wait` / `window_event`，
**没实现 `memory_warning`**，所以被 winit 的默认空实现吞掉。
**不需要任何 JNI / Java 改动。**

**做完能得到什么**：跨平台 `App::on_memory_warning` 回调（iOS 侧同样生效，
`ios/event_loop.rs:248-253` 也转 `MemoryWarning`）。

**做完不能承诺什么**：**这不是「防 OOM」**。vireo 能回收的只有 glyph atlas
（`TextContext.text_atlas`，`trim()` 已存在但从不调用，初始 256²、可 grow）；
**显存大头是用户贴图，而 vireo 结构上释放不了** —— `App.textures` 是
append-only 的 `Mutex<Vec<Arc<Texture>>>`，`load_texture` 只 push，
**全仓没有任何移除 API**。对贴图密集型游戏，单做这个回调不防 OOM。
wgpu 也没有内存压力 API（只有被动报 `DeviceError::OutOfMemory`）。

**若要真正防 OOM**，前置条件是先给 `App` 加贴图移除/驱逐 API，那是更大的改动，
且属于资源管理器范畴 —— `docs/android-audit.md` 已把「资源管理器」列进明确不做。

**另注**：走 winit 只能拿到**布尔**「吃紧了」，拿不到 level —— AGSK 的 C glue
（`android_native_app_glue.c:483-486`）只 `LOGV` 打印 level，然后写无 payload 的
`APP_CMD_LOW_MEMORY`。想要 level 必须回 Java override + JNI。**不推荐**：响应动作
在所有 level 上是同一个，level 缺失不构成阻塞。

---

### `onSaveInstanceState` / Bundle 机制：**暂不实现**（用户决定，2026-10-06）

**用户原话**：「那我们暂不考虑加上 SaveState 相关 api。」

> ⚠️ **本节此前有三处措辞错误，一并纠正**（都是我在讨论中越说越偏）：
>
> 1. **不是「Android 限制」，是 winit 的缺口。** Android **提供**了
>    `onSaveInstanceState(Bundle)` 这个平台能力。卡在 winit：
>    ```rust
>    // winit-0.30.13/src/platform_impl/android/mod.rs:276-280
>    MainEvent::SaveState { .. } => {
>        // XXX: how to forward this state to applications?
>        warn!("TODO: forward saveState notification to application");
>    }
>    ```
>    `has_saved_state` / `StateLoader` 在整个 winit android 目录里 grep **零命中** ——
>    恢复路径连占位都没有。
> 2. **「`on_paused` 是恢复后才补发的」现已过时**。2026-10-06 修了闸门顺序
>    （代号检查移到闸门之前），`on_paused` 现在在**暂停生效时**触发。
> 3. **说「`onPause` 保存不住状态」是误导**。用户在 `on_paused` 里
>    `std::fs::write(...)` **完全可行且是标准做法** —— 写文件落磁盘，进程被杀也不丢。
>    「保存不住」只指*仅存于内存*的状态。两者被混为一谈了。

**这两套机制的区别**（下次别再混）：

| | 干什么 | 存取方 | 适合 |
|---|---|---|---|
| 自己写文件 | 进程被杀也不丢 | 用户自己 read/write | 完整存档（主流做法） |
| `onSaveInstanceState(Bundle)` | Android **重建 Activity** 时（旋转、语言切换、从被杀进程返回）自动把状态还给你 | 系统保管 Bundle，重建时交还 | 小状态（第 N 关、分数、滚动位置） |

Bundle **不是「更可靠的保存」**，真正的可靠保存是写文件；它省掉的只是
「重建后自己判断并读文件」这一步。且整个 Bundle 受 Binder 事务 ~1MB 上限约束，
大状态本来就放不下。

**所以「进程被杀后进度不丢」这个需求，现在零新增 API 就能做**：

```rust
app.on_paused(|| {
    std::fs::write("save.dat", &serialize(my_state)).unwrap();
});
```

**将来若要做 Bundle 通道**（用户主动要求时再看）：
- 保存侧只能在 **Android UI 线程**上同步跑（Java 要立刻拿到字节），此时 vireo
  渲染线程**已阻塞在闸门里**，所以绝不能向渲染线程要数据 —— 否则死锁。
- 因此用户代码不该在 UI 线程上跑；可行形状是「用户自己推快照
  （`set_state_snapshot(&bytes)`，渲染线程任意时刻可调）→ Java 在
  `onSaveInstanceState` 里用 `nativeGetState() -> byte[]` 来取」。
- 恢复侧不受阻塞影响：`onCreate` 里 `getByteArray` → `nativeSetState(byte[])` 存
  `OnceLock`，等 `App` 建好在渲染线程上投递，保证在第一帧之前。
- 改动面：新模块 ~150 行 + `App` 两个方法 ~30 行 + `MainActivity.java` ~10 行。

**用户当前该怎么做**：用 `on_paused` + 写文件。不需要碰 Android 的 Bundle。

---

### ~~emoji 确认无法渲染~~ ✅ **已修复**（2026-10-05，COLRv1 自研光栅化）

**原实测结论（已作废）**：小米 25098RA98C / HyperOS / Android 16，`draw_text` 画
`🦊🎵😀👨‍👩‍👧` 渲染不出来。**注意原文写的「6 个豆腐块」是错的——实际是完全空白**，
不是 `.notdef` 方框。这个区别很重要：豆腐块说明 cmap 有映射但缺轮廓，空白说明连
glyph 都没被真正画出来。

**当时的根因判断也错了**（记录下来避免重犯）：

| 原判断 | 实际 |
|---|---|
| 白名单只收 `NotoSans` 前缀，`NotoColorEmoji` 不在其中 | 白名单**不是根因**。把 `NotoColorEmoji` 加进白名单后字体确实加载了（`201 faces`），但 emoji 仍然空白 |
| cosmic-text 需要显式加载 CBDT/CBLC 彩色位图 | 不需要。swash **本来就支持** CBDT/CBLC、sbix、COLRv0 —— 旗帜 🇨🇳 走 `NotoColorEmojiFlags.ttf` 的 CBDT 位图路径一直能画 |
| 「glyphon 的 glyph atlas 是单通道（R8？）还是 RGBA」是个未知数 | 早已确定：`mask_atlas` 是 `R8Unorm`，`color_atlas` 是 `Rgba8UnormSrgb`，两条通道都在 |

**真根因**：`swash 0.2.10` 按 **COLR v0 布局**读 COLR 表头（`scale/color.rs:26` 取
v0 的记录数），而 Android 12+ 的 `NotoColorEmoji.ttf` 是 **COLRv1**，该字段在 v1 布局里
是 `numBaseGlyphRecords` = 0 → 二分查找落空 → 回退到 `glyf` 轮廓 → 而这些 emoji 的
`glyf` 条目长度为 0 → 产出 `Some(Image{0×0})` → glyphon 判 `SkipRasterization`
**静默丢弃**。上游 issue [dfrg/swash#21](https://github.com/dfrg/swash/issues/21)
2021 年开、五年零活动，swash 已是末版。

**修法**：vireo 内用 `skrifa`（走 COLRv1 paint graph）+ `zeno`（光栅）自研一条路径，
只在 swash 交回 0×0 时二次尝试。两个 crate **都已在依赖树里**（swash 依赖它们），
**零新增 crate**。详见 `.opencode/android_colrv1_plan.md`。

**为什么不用打包一份 emoji 字体**（更省事，但被否）：那会把所有平台的 emoji 锁死成同一套
外观 —— 用户在 iOS 上看到 Noto 样式会觉得「不是本系统的 emoji」。路线选择与取舍见设计文档。

**验证**：MuMu（Android 15 / x86_64）实机 `🦊🎵😀🇨🇳👨‍👩‍👧👍🏽` 全部渲染，
保持 Android 原生 Noto Color Emoji 风格。回归测试在 `src/glyphon/colr.rs`
（含 8 个 emoji 的 contact sheet 与 8× 放大图写到 `target/`，供肉眼核对形状）。

---

### ~~Android 系统字体白名单：用户无法自定义~~ ✅ **已完成**（`configure` + 宏属性）

**原缺口**：三张表全是编译期固定的 —— `TEXT_FONT_PREFIXES` 是 `const` 不是
`static mut`（运行时改不了）、`install()` 是 `pub(crate)`（外部根本调不到）、
`FONT_DIRS` 私有（扫哪些目录也定死）。结果是：**某台 OEM ROM 的正文字体命名不落在
前缀表里 → 用户拿不到任何正文字体**，只能自己去 `adb push` 字体再
`GpuContext::load_font_file` 手工加载（这条逃生口是有的，但普通 app 用户不会也不能
这么做）。

#### 方案 A（`install_with(fs, extra_prefixes)`）**不可行** —— 已否决

原计划的 A 是「加 `pub fn install_with(font_system, extra_prefixes)`」。**实测行不通**：
调用链是 `App::new()` → `GpuContext::new()` → `TextContext::new()` → `install()`，
**全部急切**。用户既拿不到 `FontSystem` 句柄（它在 `TextContext` 内部），
也没有任何「`App::new()` 之前」的构造器钩子。公开 `install_with` 对用户毫无用处。

App 级 builder 同样做不到 —— 要它就得把 `GpuContext` 创建从 `App::new()` 推迟到 `run()`，
那是更大的重构。

→ 改为**进程级全局配置**，在 `App` 构造前设置（与既有的 `report()` 全局同源）。

#### 但宏路径连「构造前」都没有 —— 这是第二轮发现

`#[vireo::main]` 展开成 `App::new(|app| async move { ... })`：**用户函数体是在
`App::new()` 内部才被调用的**，而那时 `install()` 早已跑完。也就是说，
Android 模板（也是官方推荐的 Android 入口）**根本没有 `configure()` 的调用时机**。

→ 给宏加一个在 `App` 构造前求值的属性，与既有的 `descriptor = ...` 同构：

```rust
#[vireo::main(fonts = |c| {
    c.extra_dirs.push("/data/data/com.example.app/fonts".into());
    c.extra_prefixes.push("GameFont".to_owned());
    c.extra_families.push("Game Font".to_owned());
})]
async fn main(app: App) { .. }
```

手写 `App::new(...)`（不用宏）时可直接 `android_fonts::configure(..)`。

#### 三个字段都需要，缺一修不好症状

`extra_prefixes` / `extra_dirs` / `extra_families` 各修一个缺口。**只加前缀不够** ——
目标症状「所有文字全大写、数字消失」的成因**不是**字体文件没加载，而是
`PREFERRED_FAMILIES` 全 miss → `set_sans_serif_family` 没被调用 → fontdb 的
`family_sans_serif` 仍是默认 `"Arial"` → Android 上无人声明 `"Arial"` → `query`
返回 `None` → 回退到「排序后第一个 face」→ 可能挑中装饰体。
字体文件加载了但家族名没钉死，症状一模一样。

**家族名排在内置表之前**（而不是追加到最后）：追加只能服务「我的 ROM 内置表全 miss」
一种场景；而「我想把 app 自带字体设为默认字体」同样常见，此时 Roboto 几乎必然存在，
追加的规则永远轮不到。排在前面两种场景都成立。代价是填错成装饰体家族名会抢走
默认位置，但那是显式要求，与 `load_font_file` 后指定家族名同性质。

#### 验证
- `cargo test --lib` **289 passed**（+4：用户前缀只加不减、`configure` 三表写入、
  重复调用增量合并、空配置等价于内置行为）
- `cargo ndk -t arm64-v8a check --lib` / `-t x86_64` 通过
  （**桌面编译抓不到 Android-only 代码**，第一版就漏了个 `family` move 错误，
  是靠这条抓出来的）
- **MuMu 实机**：内置表外但真实存在的 `SourceSansPro-*.ttf`（6 个文件）经
  `extra_prefixes` + `extra_families` 加载后，HUD 从
  `files=193 faces=201 default=Roboto` 变成
  `files=199 faces=207 default=Source Sans Pro`；CJK 与 emoji 未受影响
- `gradlew assembleDebug '-PndkVersion=28.2.13676358'` BUILD SUCCESSFUL

#### 仍然不做
- **不把白名单放宽成「加载全部字体」**：那会把装饰体重新放进字体库，正是第 1 条要防的
  事故（挑中 `AndroidClock.ttf` → 全大写 + 数字消失）。用户前缀**只加不减**，
  有单测固化「装饰体仍然被拒」这一安全性质。
- **不加「加载失败的文件列表」到 `report()`**：默认 log level 是 `error`，
  `log::info!` 不输出，logcat 查不到 —— HUD 才是可靠诊断通道。用户配置的实际生效
  情况（几个前缀 / 几个目录 / 几个家族）打进 `log::info!`，`report()` 保持用户
  已确认过的形状不变。

---

### ~~触摸只能靠回调，无法轮询~~ ✅ **已完成**（`touches()` / `touch_count()` / `touch_first()` + `TouchPoint`）

原状态：`InputState.touches` 字段虽然是 `pub`（外部拿得到），但类型是裸
`HashMap<u64, (f32, f32, Option<f64>)>` —— 没有类型安全的读法、没有文档、没有排序
保证，触摸**实际只能用** `on_touch` 回调（在 winit 线程派发，要搬到渲染线程消费），
鼠标式命中判定做不了。

- `VireoWindow::touches() -> Vec<TouchPoint>`（按 `id` 升序）/ `touch_count()` /
  `touch_first()`（与 `touches().first()` 逐位一致）；`TouchPoint { id, pos: PixelPos, force }`
- 内核抽成 `input::touch_points_sorted` 纯函数（`VireoWindow::new` 需要 device，
  `cargo test --lib` 构造不出来）+ `dpi::dp_of` / `dp_to_pixel_pos`
- 单测 4 个：排序 / 逻辑值不被二次缩放 / 空集 / 非正 scale 不产 NaN

**明确不做：让触摸模拟鼠标按键**（用户决定）。`mouse_left()` 在纯触摸设备上恒
`false` —— 语义边界写进 `window/mod.rs` 的轮询 API 注释块。多指时「谁是鼠标」是主观
约定，且会与真实鼠标设备行为混淆。

**踩坑（实测）**：轮询是**电平**不是**边沿** —— 手指按住期间每帧都在结果里。按钮
直接 `if touch_count() > 0 { 动作() }` 会每帧重复触发（实测 true→false→true 乱跳）。
必须按 `id` 做帧间差分取「本帧新出现」的点。这条已写进 `touches()` 的文档示例和
`TouchPoint` 的 doc。

### ~~`on_*` 早于 `app.run()` 注册静默失效~~ ✅ **已完成**（host 侧按 handle 提前建槽）

**我犯的错**：先判定根因是「`cb_tx` 只在 `App::run` 里赋值，所以 run 前是 `None`」，
并据此写下「三个已发布示例键盘全失效」的结论 —— **两个都是错的**。

- `init_run_channels()` 的唯一调用点是 `run_entry`（`app/mod.rs:458`），它
  `spawn` 用户 main **之前**执行；`#[vireo::main]` 的每个示例都用这条路径，
  所以 `cb_tx` 在用户代码跑起来时**一直是 `Some`**（诊断日志实测
  `cb_tx=Some -> send`），桌面示例的键盘从来没失效过。
- 真正的根因是**跨线程竞态**：`App::window()` 走 `create_tx`、`on_*` 走 `cb_tx`，
  **两个独立 channel 跨 channel 没有顺序保证**。一轮 `about_to_wait` 里可以先
  drain `create_rx` 时建窗请求还没进（`window_callbacks.len() == 0`），紧接着
  drain `cb_rx` 却已收到回调 → `get_mut(handle)` 返回 `None` → **整个注册被静默
  丢弃，零日志**。Android 入口下用户 main（tid 8347）与 winit（tid 8335）分属
  不同线程，**必现**。

修法不是 App 侧缓冲（那只是绕开），而是 host 侧按 handle 提前建槽 —— 回调注册本来
就只依赖 `WindowIndex`（往「handle 槽」追加闭包），与窗口对象何时创建无关。
`window_callbacks` 只增不减（窗口关闭也不截断），提前建槽安全。

- `ensure_callback_slot` 抽成可测纯函数 + 单测 5 个（含「无效索引不得撑爆内存」
  的 `MAX_CALLBACK_SLOTS` 上限）

---

### ~~随包资源读不到 / 没有可写目录~~ ✅ **已完成**（`Assets` + 三个目录，2026-10-06）

安卓的「文件」有三个互不相通的世界，此前 vireo 一条都没覆盖：

| 世界 | 路径形态 | 本轮 |
|---|---|---|
| APK 内 | 不是路径，是 zip 里一项 | `App::assets()` → `AssetManager` |
| 内部私有 | `/data/user/0/<包名>/files` | `App::data_dir()` |
| 外部 app 专属 | `/storage/emulated/0/Android/data/<包名>/files` | `App::external_data_dir()` |

- **读**：`App::assets().read/exists/list` + `load_texture_asset` / `load_font_asset` /
  `WindowDesc::icon_from_asset`。桌面用 `set_asset_root` 指根目录，**同一份代码**两端通用。
- **写**：`data_dir` / `cache_dir` / `external_data_dir`，桌面按 XDG / APPDATA 标准目录。
- 桌面 `WindowDesc::icon_from_path` / `load_texture` / `load_font_file` 语义**完全不变**
  （纯 `std::fs`），不做「相对路径自动回退 assets」—— 那会让同一份代码在两个平台上
  资源来源不同且不报错。
- 示例 `examples/resource_io`；assets 脚手架 `examples/assets/` 与
  `examples/android/src/main/assets/`（两份内容一致）。

### SAF（系统文件选择器）：**已实现**（2026-10-10）

- `App::open_document_tree` / `open_document` / `create_document`（全异步，结果走回调，
  回调跑渲染线程）+ `SafUri`（URI 字符串可持久化 + `open_reader` / `open_writer`
  开出真正 `std::fs::File` + 目录 `list` / `resolve` / `create_*` / `delete` +
  `persisted` / `release`）+ `saf_cancel_pending`。`src/saf.rs`，prelude 导出。
- Java 侧强制：`MainActivity.java` 必须含 SAF 桥（模板已集成，另有独立可复制文件
  `examples/android/SAF_SNIPPET.java`）。缺了 launch 直接 `Err` 明说。
- 关键查证（纠正 2026-10-06 的记录）：`GameActivity extends AppCompatActivity`
  （javap 反编译 AAR 确认），`registerForActivityResult` / `onActivityResult` 继承可用；
  但三个 `OpenDocument*` contract 生成的 Intent **不带任何 flag**（字节码确认），
  持久化做不成 —— 所以用 `StartActivityForResult` contract + 自己组 Intent。
- 前置修复（同轮）：SAF 选择器盖住时会拆 native surface，回来后拿旧对象
  `configure` 必 panic。修：winit `Suspended` 置脏旗（suspend 期跳过一切 surface
  调用）+ `Resumed` 后首帧重建（`DrawSkipReason::SurfaceStale`）。见下。
- 现实限制（写文档时必须说清）：
  - `data_dir` / `external_data_dir` **都随卸载删除**。
  - Android 11+ 分区存储不允许往 `Documents` 之外的共享位置写。要让用户指定位置，
    **只能**走 SAF。
  - `AAssetDir` 式的遍历限制同样适用于 SAF 目录之外 —— 无。

### resume 后 surface 重建（SAF 前置修复，2026-10-10）

- 现象：SAF 选择器关闭 → `onSurfaceCreated_native` → 下一帧 `configure` 打在
  已死的旧 surface 上 → wgpu fatal handler panic（`Surface::configure` /
  `queue family`）；有时逃逸成 SIGABRT（tombstone 三连，`panic_cannot_unwind`）。
- 修：`Runner::suspended()` 置 `android_lifecycle::SURFACE_STALE`；
  `draw_frame` 帧首见旗即 `Skipped(SurfaceStale)`（不碰 surface）；
  `Runner::resumed()` 清旗；per-window `was_stale` 闩让 resume 后首帧
  `recreate_surface` + `configure` + 跳过（复用 `Lost` 路径形状）。
  桌面 winit 永不发 `Suspended`，零行为变化。`render_advice` 视 stale 为 Skip。
- 真机验证（MuMu Android 15）：杀 picker → `surface recreated after resume` →
  零 panic → SurfaceFlinger 时间戳持续推进（渲染真在继续）。
- 追补（同轮，真机复现倒逼）：① `was_stale` 闩只覆盖「suspend 期画过帧」，
  闸门拦住时（`auto_pause=true` 默认）走不到重建，`Outdated` 路径 configure 死对象
  照样炸 —— 修：`configure_surface` 返回 bool，`surface.configure` 那行包
  `catch_unwind`（wgpu 只给 fatal-panic/全局 handler 两条路，被逼的），失败跳
  bookkeeping，调用方按 Lost 跳帧。设备上此后三次真实 pick 全走主路径恢复，
  backstop 没再触发过（好事：说明主路径盖住了；它是纯保险）。
  ② 同进程第二个 Activity（旧的死后点图标）会跑第二个 `android_main`，
  winit 拒绝第二个 loop → SIGABRT（tombstone 实锤）—— 修：
  `ANDROID_MAIN_ENTERED` 重入直接拒绝（黑屏但不崩；多 Activity 本来就不支持）。
  该 guard 代码审查通过，触发场景难复现（需恰好点两下），暂无设备观测。

### 剩余 Android 缺口（2026-10-06 复核，2026-10-11 更新）

- ~~`configChanges` 缺 `fontScale` / `locale` / `layoutDirection` / `keyboard`~~
  **已补**（2026-10-11）：`fontScale|locale|layoutDirection|keyboard` 四个全进
  manifest。真机验证（MuMu Android 15）：改系统字号、切系统语言再回来，
  pid 不变、无重建、无 panic，只有正常 paused/stopped 往返。
  （`uiMode` **已在** configChanges 里，深色模式切换不会重建。读当前值与变更通知
  **已实现**（2026-10-11）：`win.theme()` 在安卓读系统深浅色，
  `win.on_theme_changed` 在安卓经渲染线程 poll 派发 —— 不另起新 API，
  把原来只在桌面生效的一套在安卓填上。）
- ~~剪贴板（`ClipboardManager`）~~、振动（`Vibrator`，且 manifest 无 `uses-permission`）。
  **剪贴板已实现**（2026-10-11）：免费函数 `clipboard_text` / `set_clipboard_text` /
  `clipboard_html` / `set_clipboard_html`（`src/clipboard.rs`，prelude 导出，
  刻意不挂 `App` —— 无状态可注入）。Windows（Win32 + `Win32_System_Memory` /
  `Win32_System_DataExchange`，0.61 模块位置已搬家）+ Android（纯 JNI，无权限
  无 Java 改动）；macOS/Linux → `Unsupported`。真机验证：写双格式 → 读回逐字一致。
- ~~振动~~ **已实现**（2026-10-11）：免费函数 `has_vibrator` / `vibrate_once` /
  `vibrate_predefined(Tick/Click/HeavyClick/DoubleClick)` / `vibrate_pattern` /
  `cancel_vibration`（`src/vibrate.rs`，prelude 导出）。Android 纯 JNI，模板
  manifest 加 `VIBRATE` 权限行。真机验证（MuMu 报有马达）：has=true + once/tick/
  pattern/cancel 全 Ok，无崩溃。模拟器无马达时走 `Err(no vibrator)`，真震感需物理机。
  - 可写目录不覆盖「卸载后还在」与「写共享存储任意位置」—— 见上面的 SAF 条目。

### 待验证清单（2026-10-11 审计，需人工 / 特殊设备，代码已就绪）

| # | 项 | 现状 | 怎么验 |
|---|---|---|---|
| 1 | SAF 单文件 open/create 全流程 | ✅ 已验证（2026-10-11）：open 读回 6.4MB 文件，create 写成功，零 panic | 用户点 SAF:open / SAF:save 完成 |
| 2 | SAF 持久化跨启动复用 | ✅ 已验证（2026-10-11）：`persisted ok, 4 grant(s)` + 重启后 `restored tree list ok` | pick 后重进看自检行 |
| 3 | `layoutDirection` RTL | ✅ 已验证（2026-10-11）：语言列表加 `ar_JO` 置顶，方向切到 `ldrtl`（locale+方向双变），pid 不变、无崩溃，正常 suspend/resume + surface 恢复 | 用户进设置加阿拉伯语 |
| 4 | API 31/32 分档（返回键 legacy、disable 向后兼容分支） | 手头只有 API 35 模拟器，历史分支单测覆盖但无真机 | 需 API 31/32 设备 |
| 5 | 振动物理震感 | MuMu 报有马达且全 Ok（API 层通了），但模拟器不会真震 | 需物理手机 |
| 6 | 16K 页设备运行 | 静态检查双 ABI 全绿（NDK 28.2），两台模拟器都是 4K 页 | 需 16K 物理设备 |
| 7 | `on_memory_warning` 真机路径 | ✅ 已验证（2026-10-11）：`am send-trim-memory RUNNING_CRITICAL` → logcat 出回调，进程存活 |

---

## 非 Android

（暂无）

---

## 已完成 / 已否决

见 [`docs/android-audit.md`](docs/android-audit.md) 的「P0 修复记录」与
「P3 修复记录」两节 —— 含被否决的方案及**否决理由**（避免以后有人重提）。