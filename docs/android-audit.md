# Android 适配审计报告

> 日期：2026-10-05（审计）/ 2026-10-06（P0+P1 修复完成）
> 审计对象：`D:\MilkCOCO\vireo` HEAD `3514e07`（`feat(android): 文本输入（IME）`）
> 方式：subagent 全量读源码 + 交叉核实 `winit 0.30.13` / `android-activity 0.6.1` 上游源码
> **重要**：subagent 的两条「阻塞级」结论被我复核后**修正**（见 §2）

## 状态：P0 / P1 / P2 / P3 已全部修复

> **文档债已清（2026-10-06）**：第 2.2 节列的「Android 上静默 no-op 但无文档」
> 已全部补进 API 的 `## Platform-specific` 段 —— `src/window/mod.rs` + `src/window/desc.rs`
> 里**所有**带 `## Platform-specific` 段的 `pub fn` 现在都提到 Android
> （47/47，覆盖率校验过）。补齐的条目见下方「P3 文档债修复记录」。

| 阶段 | 状态 | commit |
|---|---|---|
| P0（3 项阻塞级）| ✅ 完成 | `bc0129d` |
| P1（3 项）| ✅ 完成 | `a624cfb`（gradle 模板）/ `57e2907`（契约文档）|
| P2（4 项）| ✅ 完成 | `74cfe98`（纯逻辑抽离）/ `703c6a8` |
| P3（生命周期）| ✅ 完成 —— 用户否决折中方案后改走「Java 转发 + RegisterNatives」| `c769302` |
| P4（真机暴露的连环 bug）| ✅ 完成 | `be01a82` / `c488a1d` |

> **审计方法论教训**：桌面单测全绿 ≠ Android 能跑。P0~P3 全部「完成」时，
> 模板装到真机上仍然 `dlopen failed` —— 四个 bug 里有三个**只靠读代码发现不了**
> （依赖指向 crates.io、触摸坐标双重换算、输入法无视 `IME_FLAG_NO_FULLSCREEN`），
> 必须真机跑起来才暴露。详见 §7。

**想直接上手 Android？** 看 [`examples/android/README.md`](../examples/android/README.md) ——
前置条件、三步出 APK、五个「静默失败」的坑。

---

## 1. 定位：vireo 是渲染库，不是引擎

大量「引擎级」能力（音频、资源管理、场景、UI 组件、传感器、手柄）在桌面端就不存在，
Android 上自然也没有。审计严格区分「**桌面已有但 Android 缺失**」（真缺口）
与「**全平台都没有**」（范围外）。

### 1.1 已具备且质量高（🟢 代码已验证）

| 能力 | 位置 | 评价 |
|---|---|---|
| 沉浸式 / 系统栏 | `android.rs:879-954` | API 30+ `WindowInsetsController` / legacy 双分支；`SystemBars` bitmask（可只隐藏状态栏）|
| 安全区三值模型 | `android.rs:983-1007` | `safe_area` / `padding` / `ime_insets` + `cutouts()`；`SAFETY_MASK` 显式 `or displayCutout()`（业界著名坑）|
| 朝向 | `android.rs:1393-1448` | 8 档 `ActivityInfo` 值；**明确区分「请求」与「实际」**（不重复 Godot #114840 的错）|
| 常亮 / 亮度 | `android.rs:1450-1507` | `None` = 跟随系统 |
| 窗口层级 | `android.rs:1509-1570` | 4 档按 AOSP `LayoutParams.type` 常量重新表达，不硬套 winit 四档 |
| 文本输入 | `android_text_input.rs` | `TextInputState` + UTF-16↔UTF-8 换算 + 20 单测；**本次审计最扎实的部分** |
| 字体白名单 | `android_fonts.rs:144-204` | 前缀 AND 扩展名双白名单 + 钉死 generic family + 5 个真跑单测 |
| 内容穿挖孔 | manifest `windowLayoutInDisplayCutoutMode` + targetSdk 35 | 真机 2392×1080 全宽 |

**对比结论（显示/窗口子集）**：vireo 达到甚至超过 Unity / Godot / Flutter ——
安全区三值、沉浸式可配、朝向不撒谎，这几条业界头部引擎都踩过坑。

---

## 2. 对 subagent 结论的两处修正（我逐行复核过）

### 修正 1：「触摸坐标错 2.8×，触控 UI 全废」→ **夸大一半**

subagent 只看了 `src/app/host.rs:498`：

```rust
WindowEvent::Touch(touch) => {
    let mapped = crate::input::map_touch_event(&touch, 1.0);   // ← 硬编码 1.0
```

**但漏看了轮询路径** `src/window/mod.rs:1198-1204`：

```rust
WinitEvent::Touch { event, .. } => {
    let sf = self.applied_dpi_override.lock().unwrap_or(self.inner.scale_factor());
    let tx = (event.x as f64 / sf) as f32;      // ← 正确除了 scale
    let ty = (event.y as f64 / sf) as f32;
```

| 路径 | 坐标 | 状态 |
|---|---|---|
| `InputState::touches`（轮询，**推荐**） | 逻辑像素 | ✅ **正确** |
| `InputCallbacks::on_touch`（回调） | 物理像素 | ❌ 与 `TouchEvent` 文档矛盾 |

**且这不是 Android 专属问题** —— 同一个 bug 影响 Windows 150% / macOS Retina。
`scale=1.0` 时两个坐标空间恰好相同，所以从未暴露。

**结论**：缺陷成立、该修，但严重度从「Android 触控全废」降为「HiDPI 回调路径错」，
范围从「Android」扩大到「所有 scale≠1 的平台」。**这反而更该修**（影响面更广）。

### 修正 2：`text_input_state()` 恒 `Some` 的判断不完整

subagent 说恒返回 `Some`。实际 `android.rs:1608-1622` 在无 AndroidApp 时返回 `None`
（`self.android_app().ok()?`），有 App 时恒 `Some`。
「无法区分 IME 未连接与空文本」这个结论本身**成立**，但表述可以更准确。

---

## 3. 确认的缺陷清单

| # | 严重度 | 位置 | 问题 |
|---|---|---|---|
| 1 | 🔴 P0 | `window/mod.rs:1662` | `set_ime_allowed` 文档写「iOS/**Android**：控制软键盘显示/隐藏」，实现直接转发 winit → Android 上是 `show_soft_input(true)` = `SHOW_IMPLICIT` → **键盘永不弹**。这正是本项目当天自己踩过的坑，文档却说它能用 |
| 2 | 🔴 P0 | `app/host.rs:498` | `map_touch_event(&touch, 1.0)` → callback 路径给物理坐标。影响**所有 HiDPI 平台**，不限 Android |
| 3 | 🟠 P1 | `platform/android.rs:1531` + `:485` | 文档承诺 `set_window_level` 未授权时抛 `BadTokenException`，实际被 `post_to_java_thread` 的 `log::warn!` 吞掉，**恒返回 `Ok`** |
| 4 | 🟠 P1 | `platform/android.rs:985-990` | `padding()` 文档称「键盘弹出时吸收底部安全区」，但 `SAFETY_MASK`（`:570-571`）**不含 ime**（有单测锁定）。而「音游 HUD 避让中文键盘」正是这个 API 的存在理由 |
| 5 | 🟠 P1 | `platform/android.rs:81-122` | 模块文档**仍在教 xbuild**（`:113-122` 直接给 `manifest.yaml` 示例），而构建链已换 gradle（`CHANGELOG.md:47-55`）。且把「targetSdk 35 进挖孔」写成「无法验证」——实际已真机验证有效（2392×1080 全宽）|
| 6 | 🟠 P1 | `lib.rs:14-17` + `:90` | crate 文档第一句「**目前仅在 Windows 上做过测试，其余平台未验证**」；模块总览只列 `windows`/`macos`；README 零 Android。**可发现性归零** |
| 7 | 🟠 P1 | 全仓 | **0 个 Android 示例、0 个 gradle 模板**（示例工程在仓库外 `D:\MilkCOCO\pong`）|
| 8 | 🟡 P2 | `lib.rs:238` vs `platform/android.rs:497` | `WindowLevel` 重名 → `use prelude::*` + `use platform::android::*` 后 `WindowLevel::Normal` **E0659 编译错** |
| 9 | 🟡 P2 | `window/` 多处 | 10 个 API 在 Android 静默 no-op（`set_title`/`set_minimized`/`set_maximized`/`set_visible`/`set_icon`/`set_cursor`/`set_fullscreen`/`set_frame_style`/`set_window_level`/`set_resizable`），其中 6 个**连 `## Platform-specific` 段都没有** |
| 10 | 🟡 P2 | `window/desc.rs:174-181` | `visible` / `preparable` 在 Android 全无效（winit Android 不引用 `with_visible`；`set_visible` 是空实现）。`preparable` 默认开 → 空转，文档未提 |
| 11 | 🟡 P2 | `platform/android.rs:1279-1295` | `system_bars_visible()` 忽略 caption、只返回单个 `bool`。设计文档原定的 `effective_system_bars() -> SystemBars` 未实现 |
| 12 | 🟡 P2 | `vireo_macro/src/lib.rs:116-126` | `descriptor = ...` 在 Android target 是 `compile_error!` → **无法强制 Vulkan / 排除 GLES**。而 `gpu/mod.rs:341-353` 的 GLES fallback 说明该路径存在但未验证 |
| 13 | 🟡 P2 | `window/mod.rs:2207-2216` | `drag_cap` 在 Android **恒失效**（winit Android 的 `refresh_rate_millihertz()` 恒 `None`）|
| 14 | 🟡 P2 | `platform/android.rs:1641-1780` | 8 个单测在桌面**永不执行**（模块整体 `cfg(target_os="android")`）。`android_text_input.rs` / `android_fonts.rs` 已用「无条件编译」修过，`android.rs` 本体没修 |
| 15 | 🔶 待验 | `text/android_fonts.rs:70-79` | 白名单不含 `NotoColorEmoji`（前缀 `NotoSans` 不匹配）→ **emoji 可能渲染不出**。未真机验证 |
| 16 | 🟡 P2 | `text/context.rs` | `fontScale` / `uiMode`(深色) / `locale` 零处理。`fontScale` 不跟随是游戏引擎惯例（可接受），深色模式跟随值得考虑 |

---

## 4. 结构性缺口（不是 bug，是「没做」）

### 4.1 生命周期

| 能力 | 状态 |
|---|---|
| `on_paused` / `on_resumed` | ✅ `c769302` —— Java 转发 + `RegisterNatives`，渲染线程真停摆 |
| 后台节流 | ✅ 同上（`thread.rs:450-455` 暂停闸门，实测后台 CPU 0.0%）|
| `LowMemory` | ⚠️ winit **已**转发为跨平台 `Event::MemoryWarning`，vireo 只差 `Runner::memory_warning` 一个方法（零 JNI）|
| `onStop` / `onDestroy` | ❌ winit `warn!("TODO")`，需照 `nativeOnPause` 加 JNI |
| 状态保存恢复 | ❌ 不做 —— 见下 |

**技术障碍（已修正）**：~~winit 把 `Pause/Stop/Resume/Start/Destroy/SaveState/
LowMemory/InsetsChanged/ContentRectChanged` 全部吞掉~~ —— **这句话是错的**，
而且是**行号级可证的错误**：它引用的
`winit-0.30.13/src/platform_impl/android/mod.rs:245-297` 区间里恰好包含唯一
那个能正常工作的分支：

```rust
MainEvent::LowMemory => {
    callback(event::Event::MemoryWarning, self.window_target());   // ← 唯一的例外
},
MainEvent::Pause  => { self.running = false; }   // 构造了，只是不转发
MainEvent::Stop | Destroy | SaveState => { warn!("TODO: ..."); }   // 确实被吞
```

审计读了那个区间但**漏看了这一个非 `warn!` 分支**，把 `LowMemory` 归进了
「全部吞掉」。这个错误结论当时在仓库里**复制了 5 处**。教训：断言「某上游
把 X 全部吞掉」之前，先把整个 match 逐个 arm 看完 —— 一个例外就足以推翻结论。

**状态保存恢复为什么不做**（`onSaveInstanceState`）：

1. **vireo 没有可序列化的状态**。`App.textures` / `offscreens` / `windows` /
   `gpu` 全是 `Arc<...>` 包着 `wgpu::Texture` / `Surface` / `Device` 句柄；
   `DrawBatch` 技术上可序列化但**毫无意义** —— 它是每帧从零重建的命令缓冲，
   存它等于存「上一帧画了什么」，恢复后第一帧就覆盖。
2. **用户游戏状态不在 vireo 里**。这是引擎层职责（Godot/Unity 在这层存
   `SceneTree`），vireo 是渲染库，在这一层之下。
3. **技术上被 winit 堵死**：`SaveState` 被 `warn!` 掉、`StateLoader` 被丢弃，
   绕过需绕开 winit 整个事件循环（架构级改造）。

> **纠正 TODO.md 的一个误判**：原写「`onSaveInstanceState` 需要 `android_os_Bundle`
> JNI 绑定 + 自定义 `Parcelable`，不是照抄能解决的」—— **Bundle 那部分完全错了**。
> `StateSaver::store(&[u8])`（`android-activity-0.6.1/src/game_activity/mod.rs:50-88`）
> 是**纯字节接口**，AGSK 的 recallback 自己负责写 Bundle。真实阻塞**只**是 winit 吞事件。

⚠️ **一条必须写进文档的现有限制**：`App::on_paused` 是**恢复后才补发**的
（`thread.rs:447-449`：回调需在渲染线程且要能碰 `App`）。所以
**进程在后台被系统杀掉时，vireo 侧零回调** —— `on_paused` 不能替代
`onSaveInstanceState`。要保状态只能由用户在 `MainActivity.java` 里 override。

### 4.2 分发 / 构建

| 能力 | 状态 |
|---|---|
| gradle 集成 | ❌ 仓库**零模板**。用户必须自己从零搭 `com.google.androidgamesdk.GameActivity` 项目 |
| 构建说明 | 📄 只在 CHANGELOG / 私有文档，**无可复制步骤** |
| minSdk / targetSdk / abiFilters | ❌ 全无记录 |
| **16KB page size** | ❌ **零提及**。这是 Android 15+ **Play 上架硬要求**（targetSdk 35 的新应用/更新必须支持）|
| R8 / ProGuard keep 规则 | ❌ 零提及 |
| `.so` strip / APK 体积 | ❌ `Unable to strip` 未解决，APK 内 14MB 未剥离 |
| 调试符号上传（native crash 符号化）| ❌ 零提及 |
| AAssetManager | ❌ 打进 APK `assets/` 的资源读不到（`App::load_texture` 走 `std::fs`，只能读沙箱内文件）|

### 4.3 明确不做（附代价）

| 项 | 代价 |
|---|---|
| 折叠屏 hinge / displayFeatures | 折叠展开时布局无提示，用户布局可能错位。可用 `android:resizeableActivity=false` 规避 |
| `set_ime_cursor_area`（候选窗跟随光标）| 中文输入时候选栏位置可能与光标脱节（可接受的 UX 降级）|
| `IME_FLAG_NO_FULLSCREEN` 之外的 extract 防御 | 该 flag 文档明确是 best-effort，某些输入法可能忽略 |
| `fontScale` 跟随 | 游戏引擎惯例（Godot/Unity 也不自动跟随）|
| GPU device 丢失后重建 | Android 上 wgpu device 极少真丢（进程被杀）|
| 自建音频 / UI 组件库 / 资源管理器 | 范围外。**但 README 应明确写一句「vireo 不提供 X/Y/Z」**，避免用户误以为有 |

---

## 5. 易用性：学习曲线

**Android 部分的陡峭几乎全部来自文档缺失，而非 API 本身。**
一个 Bevy 用户需要自己搞懂 12 件事：

```
 1. 构建链换 gradle + cargo ndk（仓库零模板）
 2. theme 必须是 AppCompat 子主题（AGSK 硬要求）
 3. theme 里别放 windowFullscreen      ← 只存在于私有文档；MIUI 上 IME 10 秒超时
 4. screenOrientation/configChanges 写 manifest ← 公开文档有，但示例是废弃的 xbuild 格式
 5. 隐藏系统栏用 SystemBars::ALL 而不是 NONE
 6. 安全区用 padding() 且要自己并 ime_insets()   ← 见缺陷 4
 7. 文本输入是每帧轮询，不是事件驱动
 8. 必须 set_text_input_state() 回喂，否则光标移动后重复插入全文
 9. set_input_type 必须在 show_soft_input 之前
10. 首帧调 show_soft_input() 不生效，要延后约 30 帧
11. set_ime_allowed 不能用               ← 缺陷 1
12. on_touch 回调坐标是物理像素          ← 缺陷 2
```

**对比**：Unity / Godot 有 `OnApplicationPause`、`Screen.safeArea`、`Screen.sleepTimeout`、
`Screen.brightness`、`Display.rotation`、完整 lifecycle、Clipboard、传感器。
这些 vireo Android 没有的，**大部分是「一个 app 要上架必须处理」的标配** ——
不是 vireo 缺功能，是缺**平台适配层**。

---

## 6. 修复计划（按性价比排序）

| 优先级 | 项 | 规模 | 状态 |
|---|---|---|---|
| **P0-1** | 缺陷 1：`set_ime_allowed` 在 Android 转发到已验证的 `show_soft_input` | **S** | ✅ **已修** |
| **P0-2** | 缺陷 2：`on_touch` 传真实 scale（顺带修所有 HiDPI 平台）| **S** | ✅ **已修** |
| **P0-3** | 缺陷 6：`lib.rs` / README（中英）声明 Android 支持 + 修模块总览 | **S** | ✅ **已修** |
| **P1-1** | 缺陷 5+7：gradle 模板 + `AndroidManifest.xml` + `styles.xml`（**别放 windowFullscreen**）+ minSdk/targetSdk/AGP/NDK + **16KB page size** | **M** | ✅ `a624cfb` |
| **P1-2** | 缺陷 3、4：`set_window_level` / `padding()` 文档与实现对齐 | **S** | ✅ `57e2907` |
| **P1-3** | 缺陷 9、10：Android no-op API 补 `## Platform-specific` 段 | **S** | ✅ `57e2907` |
| **P2-1** | 缺陷 14：`platform::android` 的纯逻辑抽到无条件编译模块，让 8 个单测能跑 | **M** | ✅ `74cfe98`（后续又补 `FLAG_NO_EXTRACT_UI` 等，共 9 个）|
| **P2-2** | 缺陷 8：`WindowLevel` → **`VireoWindowLevel`** 解 E0659 冲突（breaking，用户已定名）| **S** | ✅ `703c6a8` |
| **P2-3** | 缺陷 11：`system_bars_visible()` → `effective_system_bars() -> SystemBars`（用户：改彻底，不并存）| **S** | ✅ `703c6a8` |
| **P2-4** | 缺陷 12：Android 支持 `descriptor =`（强制 Vulkan）| **M** | ✅ `703c6a8` |
| **P3** | 生命周期：`MainActivity.onPause/onResume` 经 JNI 转发 + 渲染 Thread 停摆 | **M** | ✅ `c769302` |
| **P3** | 生命周期真解（绕开 winit 吞事件）| **L** | ✅ 同上（Java 转发即真解）|
| **P4** | 真机暴露的 4 个连环 bug（宏依赖 / 触摸双重换算 / IME 重发 / extract 白条）| **M** | ✅ `be01a82` `c488a1d` |
| **待验** | 缺陷 15：真机确认 emoji 是否能渲染（`NotoColorEmoji` 不在白名单）| **S** | ✅ **确认无法渲染**，见 `TODO.md` |

### P0 修复记录

**P0-1 `set_ime_allowed`（`window/mod.rs`）**
Android 下不再转发 winit（那条路径带 `SHOW_IMPLICIT`，游戏场景永不生效），
改调 `platform::android::{show,hide}_soft_input`；失败走 `log::warn!`。
文档新增「## Android：这不是你该用的 API」段，说明 `SHOW_IMPLICIT` 陷阱并指向正确 API。

**P0-2 `on_touch` 缩放（`app/host.rs` + `input.rs`）**
`Runner` 新增 `window_scale: FxHashMap<usize, (f64, Option<f64>)>`（实时 scale_factor + dpi_override）
与 `effective_scale(handle)`：
- 建窗时写入（`host.rs` `create_window`）
- `ScaleFactorChanged` 只更新实时值，**`dpi_override` 设了之后有效值固定不动**
  （与 `Renderer::new` 的 `scale` 和轮询路径的
  `applied_dpi_override.unwrap_or(scale_factor)` 三者一致）
- `map_touch_event(&touch, self.effective_scale(handle))`

`map_touch_event` 另加**除数护栏**：`scale_factor <= 0` 时按 `1.0` 处理。
实际路径上 `dpi_override` 非正值已被 `dpi.rs` 视为 None、`scale_factor()` 也不会返回 0，
但除法值得兜一层 —— NaN 坐标参与布局比较会**静默失效**，比崩溃更难查。

新增 3 个单测（`input.rs::tests`）：scale=1.0 恒等 / scale=2.0 减半 /
真机值 2.8125 / scale=0 不产生非有限值 / id+phase+force 透传。

**P0-3 声明 Android（`lib.rs` + `README.md` + `README-EN.md`）**
`lib.rs`「支持平台」从一句话改成**四行状态表**（Windows ✅ / Linux 🟡 / macOS 🟡 /
**Android ✅ 已实测**），并列出 Android 独有能力 + 警告「构建链与桌面不同，
`xbuild`/`cargo-apk` 不适用」。模块总览补上 `android`。
README 两份同步加「平台支持」小节。

> 注意 `lib.rs` 的平台表引用了 `examples/android/`，该目录**由 P1-1 创建**——
> 在 P1-1 完成前这个链接是悬空的。

---

## 7. 本轮审计的方法论教训

1. **subagent 的结论必须逐行复核**。本次它报了 2 条「阻塞级」，
   其中 1 条因只看了一个调用点而**严重夸大**（漏看轮询路径是修好的），
   1 条表述不完整。**「读了一个调用点」不等于「读懂了数据流」。**
2. **判断严重度要看数据流全貌**，不能只看单个 `Edit`：
   同一条数据有 callback 和 polling 两条路，一条对一条错是最容易被漏的情况。
3. **文档与实现矛盾是独立缺陷**，且往往比实现 bug 更贵 —— 实现 bug 一次运行就暴露，
   文档 bug 会持续误导所有后来人（包括未来的作者自己）。
### P3 修复记录：生命周期（方案 B）

**被否决的折中方案**：Android 上把默认 `max_fps` 降到 30 + 用 `focused()` 当暂停代理。
用户否决，理由成立 —— 它**用错误的信号代替正确的信号**：

- 用户没切后台时也只跑 30fps，对音游是致命的判定精度损失
- 切后台时**仍然跑 30fps** —— 根本没解决问题，只是把功耗从 100% 降到 12.5%
- 等于「承认检测不到，但让所有人替没检测到的人买单」

**采纳的方案 B：`MainActivity.java` 转发 + `RegisterNatives`。**

三条路的对比：

| 方案 | 成本 | 问题 |
|---|---|---|
| 降默认 max_fps | 几行 | 没解决问题（如上） |
| Rust 侧注册 `ActivityLifecycleCallbacks` | 大 | 要用 `java.lang.reflect.Proxy` 动态实现接口，回调在 UI 线程 |
| **Java 侧转发** | 小 | `MainActivity` 本来就是用户代码 |

关键点：`MainActivity.java` 是 vireo 构建产物里的宿主类（`examples/android/`），
加两个 override 是零成本，换来的是**真正准确**的生命周期信号。

**为什么用 `RegisterNatives` 而不是 JNI 命名约定**：命名约定要求符号形如
`Java_<包名>_<类名>_<方法名>`，把 Java 包名编进 Rust 导出名 —— 用户改
`applicationId` 就得同步改 Rust 常量，忘了只在运行时找不到符号（无编译错）。
`RegisterNatives` 直接传**函数指针**，Java 侧只声明
`private static native void nativeOnPause();`（与包名无关），Rust 侧在运行时把
「方法名 + 签名 → 函数指针」交给 JVM。所以也不需要 `#[no_mangle]` 导出。

**绑定时机**：`android_main` 期间（`app::android_main_entry`）—— 那时
`MainActivity` 已被 AGSK 加载，而 `onResume`/`onPause` 必然在 `onCreate` 之后。

**取类的方式**：用 `env.get_object_class(activity)` 取 **Activity 实例的实际类**，
不写死包名 `findClass` —— 用户改 applicationId / 类名都不影响绑定。

**新增 API**（`src/app/mod.rs`）：

- `App::is_paused() -> bool` — 任意线程可读；桌面恒 `false`
- `App::on_paused(cb)` / `App::on_resumed(cb)` — 渲染线程上触发

**渲染侧**（`src/thread.rs`）：`paused` 期间整条 Thread **完全停摆** —— 不跑
`on_tick`、不 `draw`、不 acquire/present。省下的是**全部** CPU+GPU，而不只是降帧率。

**lost-wakeup 防护**：`set_paused` 必须**持 `WAKE` 的锁**再写标志。否则渲染线程
可能刚好在「检查到 paused=true」与「注册进 condvar 等待」之间被通知掉通知，
于是永久阻塞 —— 表现为「切回前台后画面再也不动」。

**多 Thread 去重**：每次真实状态变化 `GEN` +1，各 Thread 各自跟踪已处理的代号，
于是每次变更每个 loop 恰好触发一次回调。

**设计取舍（已写进 API 文档）**：
1. 回调在**恢复时**才触发（回调要能碰 `App`，不能在 UI 线程跑）。所以「暂停瞬间
   的紧急收尾」要写在 `on_tick` 里判 `is_paused()`。
2. `onPause` 对短暂中断也触发（锁屏、部分弹窗、分屏切换）—— 同 Unity
   `OnApplicationPause` 语义。
3. 绑定失败只 `log::warn!`（不 panic）：用户可能没照模板写 `MainActivity.java`，
   此时退化成旧行为（后台仍全速），warn 里写清怎么改。

**仍未覆盖**：`onStop` / `onDestroy` / `onSaveInstanceState` / `onTrimMemory`。
需要的话照 `nativeOnPause` 的写法自己加（Rust 侧只要在
`register_native_methods` 的表里多一行 + 一个 `extern "system" fn`）。
### P2-1 修复记录：9 个从未运行的测试

**背景**：`platform::android` 整个被 `#[cfg(target_os = "android")]` 门控，所以它内部
的 `#[cfg(test)] mod tests` 在桌面的 `cargo test --lib` 里**从未被编译过**，更别说执行。
那 9 个测试验证的全是纯计算（Insets 换算、SystemBars bitmask、AOSP 常量对齐、
`SAFETY_MASK` 组成、朝向编码），**根本不需要 JVM** —— 等于白写。

**做法**：新增 `src/platform/android_pure.rs`（无条件编译），搬入
`mod api`（AOSP 常量）/ `Insets` / `SystemBars` / `SystemBarBehavior` /
`DisplayCutoutMode` / `Orientation` / `DisplayOrientation` / `VireoWindowLevel` /
`SAFETY_MASK` / `to_logical` + 那 9 个测试。`android.rs` 用
`pub use super::android_pure::{...}` 回导，公开 API 路径不变。
参照同目录 `android_text_input`（同一个动机）。

**搬迁保真**：用 difflib 逐行对比「从 android.rs 删除的行」与「新文件的内容」，
归一化 4 处有意的可见性放宽（`fn` → `pub(crate) fn`，因为搬走后 JNI 那半边调不到
私有方法）后 —— **只有 3 处差异**：文件头文档、一处分隔注释、一处断言修正（见下）。
其余 499 行逐字节一致。

**搬完立刻发现：一个测试是坏的。**

```
test platform::android_pure::tests::safety_mask_includes_display_cutout ... FAILED
assertion `left != right` failed: IME 不应混进安全区，走独立 getter
```

`assert_ne!(SAFETY_MASK & api::IME, 0, "IME 不应混进安全区")` —— 断言的是
「IME **在**安全区里」，与它自己的提示语**正好相反**。正确写法是
`assert_eq!(SAFETY_MASK & api::IME, 0, ...)`。

生产代码一直是对的（`SAFETY_MASK = STATUS|NAVIGATION|CAPTION|CUTOUT`），
只有断言写反了。**因为这个测试从来没跑过，所以这个反向断言存活至今。**

这条正是「先搬出来跑一遍」的价值：不是「搬完多几个测试」，而是**搬完立刻暴露了
一个潜伏的测试缺陷**。若当初有 CI 跑 Android target 的 test，第一天就会发现。

同批修了另外两个搬迁副作用：
1. re-export 一开始被插到模块 `//!` 文档头**之前**，导致 Android target 编译报
   `expected outer doc comment`（桌面不编译该模块所以看不出来）→ 已移到文档头之后
2. 新模块里指向 `WindowExtAndroid::*` / `super::android` 的 intra-doc link 在桌面
   必然悬空（那些 item 被 cfg 门控）→ 改成代码 span

**验证**：test --lib 260 passed（9 个从「不跑」变成「跑且通过」）；
桌面 + Android target 均零 warning；cargo doc 零警告；模板 Android 检查通过
### 真机复验（2026-10-06，小米 25098RA98C / HyperOS / Android 16）

**P2-3 `effective_system_bars()`** — 真实数据：

```
bars=SystemBars(7)  visBars=Some(SystemBars(2))  ← 请求全隐藏，实际只剩导航栏可见
safe.top=45.51  pad.top=0.0  ime.bottom=0.0     ← 三值模型区分正确
cut_rects=[Rect { x:804.98, y:179.91, w:45.51, h:24.53 }]  ← 挖孔矩形，横屏时正确移到右上
orient=Landscape  scale=2.8125
```

这是旧 `system_bars_visible() -> Option<bool>` 拿不到的信息 —— 旧版只会说
「有栏可见」，新版能说出**只有导航栏**。

**P3 生命周期桥** — `onPause`/`onResume` 经 `RegisterNatives` 绑定成功
（logcat 无 `Failed to register native method`）。切后台实测：

| 状态 | CPU | 进程 |
|---|---|---|
| 前台 | 26.6% | 存活 |
| 后台 | **0.0%** | 存活（RSS 稳定 171M，状态 S） |
| 回前台 | 27.5% | 存活 |

后台 0% CPU 且进程未死，排除了两个混淆项：logcat 对后台 app 的限流、
Android 冻结后台进程。渲染线程是**自己**停的。

**emoji — 确认无法渲染**（详见 `TODO.md`）。`🦊🎵😀👨‍👩‍👧` 渲染成 6 个豆腐块。

> 教训：验证渲染**必须看像素**。logcat 只证明字符串提交了
> （`[emoji] draw "..."` 正常打印），完全看不出有没有字形 ——
> 中间差点据此得出「渲染正常」的错误结论。

---

## 8. 真机首装的四个连环 bug（`be01a82` / `c488a1d`）

P0~P3 全部标记完成、桌面 265 测试全绿之后，模板装到真机上**第一件事就是崩**。
这四个 bug 有三个**读代码发现不了**，是这次审计最值钱的产出。

### 8.1 依赖指向 crates.io，仓库里的宏从未被编译（`be01a82`）

**症状**：`UnsatisfiedLinkError: cannot locate symbol "android_main"`。

**根因**：`vireo/Cargo.toml` 写的是 `vireo_macro = "0.1"`，cargo 从 **registry**
解析。仓库里的 `vireo_macro/` 目录**一行都没编译过** —— 它是死代码。

**为什么审计没发现**：审计读的是「宏有没有生成 `android_main`」—— 生成了，
代码完全正确。问题在**依赖解析**，不在代码。

**定位手法（可复用）**：

| 手段 | 结果 |
|---|---|
| 改本地宏源码后构建 | **0.65s 一个 crate 都不重编** → cargo 压根不读它 |
| 往宏里塞 `eprintln!` 打印展开结果 | **一次都没打印** → 宏从未被调用 |
| 同文件手写同形状的探针函数 | 正常导出 → 排除「`no_mangle` 失效」等代码层猜测 |

最后一条是关键：它把「宏生成的」与「手写的」放在同一文件同一 crate 里对比，
一次就把问题锁死在「宏没被调用」，而不是在 ABI/属性写法上瞎猜。

### 8.2 触摸坐标双重换算（`c488a1d`）

**症状**：点输入框键盘不弹；`adb input tap` 打 10 个不同位置全部无效。

**根因（两层）**：

1. `WinitEvent::Touch.event` 存的**已经是逻辑像素**（`host.rs` 收到
   `WindowEvent::Touch` 时按有效 scale 除过了），但 `apply_input_event`
   **又除了一遍**。HyperOS（scale 2.8125）上 `tap(844,551)` → `(37.9,24.8)`。
2. 触摸**完全不更新** `mouse_pos`，而 Android 上点击**不产生 `CursorMoved`**
   （winit 在 `Window::Builder` 未启用 `with_cursor` 时不发），
   该字段恒为初始的 `(-1,-1)`。模板用 `mouse_pos()` 做命中判断 → 永远不成立。

**教训**：坐标系（物理 / 逻辑）在事件链上**跨了几个模块**，每个模块都可能
再换算一次。修法不只是改这一行，而是把契约**显式化**：新增纯函数
`touch_logical_to_mouse_pos` + 往返测试（`tap → map → mouse_pos` 必须回到原始
物理坐标），让两个坐标空间的换算有单一入口。

### 8.3 我自己引入的回归：200ms 重发窗口清掉组合态（`c488a1d`）

**症状**：① 输入字符时候选词与 preedit 一闪即消；② 按输入法收起键会在
200ms 内被重新拉起，只有点到 vireo 其他区域（`focused=false` 走 hide）才停。

**根因**：修 §8.1 之前的 ANR 时（每帧 `hide_soft_input` post 淹掉 Java 主线程，
5 秒 `Input dispatching timed out`）我加了跳变去重，但同时留了一个
**200ms 周期性重发**兜底「用户收起键盘后请求值陈旧」。而**输入法每次收到
`show` 都会重置组合态** —— 组合态是易碎的，任何重复 `show` 都是破坏。

**教训**：去重时容易只想到「不去重会 ANR」，没想到「重复调用会破坏对方的
内部状态」。这类「周期性兜底重试」在有内部状态的远端系统（输入法、IME、
网络请求）上要格外小心。

现在的语义：**纯跳变去重，永不周期性重发**；「键盘被收起」的恢复是文档化的
显式两步 `hide_soft_input()` → `show_soft_input()`（两次都是跳变，都会真下发）。
判断键盘是否还在用 `ime_insets().bottom`。

> 附带一个反直觉现象：**去掉 `set_input_type()` 反而让 extract 白条更严重**
> （按钮从「执行」变成更明显的「完成」）。这个对照实验一度让我误判成
> 「是模板抹了 flag」，实际根因完全不同 —— 见下。

### 8.4 输入法无视 `IME_FLAG_NO_FULLSCREEN`（`c488a1d`）

**症状**：横屏弹出键盘时，屏幕顶部一条**全宽白条** + 一个框架字符串按钮。
它会自行消失（没有正在组合的拼音时），所以看起来「像输入法自己的 UI」。

**我误判了两次**：

1. 第一次据 `dumpsys window` 里 `InputMethod` 窗口 frame 推断是「搜狗候选栏」——
   候选栏确实属于输入法窗口，但那个 frame 覆盖整个下半屏，解释不了「顶部白条」。
2. 第二次做对照实验（去掉 `set_input_type()`）发现白条**变严重**，于是误判成
   「模板自己设了 ImeOptions 抹掉 flag」。

**真相**：`android_text_input.rs` 的模块文档和 README §4.3 **早就写好了**这个
机制和它的**可观测特征**：

> 输入法在**横屏**下进入 AOSP 的 **fullscreen extract mode**，屏幕顶部弹出
> 一条全宽白条 `input_method_extract_view`；按钮上的「执行」是**框架字符串**
> `com.android.internal.R.string.ime_action_default` —— 属于 `com.android.internal`，
> 既不是 MIUI 也不是输入法画的。**这正是本条 flag 缺失的可观测特征。**

根因：HyperOS 搜狗横屏**无视** `IME_FLAG_NO_FULLSCREEN`。而该 flag 的 AOSP 文档
本就写明「不是保证，某些输入法可能忽略」，即它是 best-effort。
extract mode 下 IME 窗口被设成 `MATCH_PARENT` 吃掉整屏，框架注释直言
*"In fullscreen mode, we never resize the underlying window"* → `ime_insets.bottom`
恒为 0、manifest 里的 `adjustResize` 形同虚设（可见区只剩顶部 128 物理像素）。

**修法**：`IME_FLAG_NO_EXTRACT_UI`（`0x10000000`）与 `NO_FULLSCREEN`
**语义不等价** —— 前者是「别显示 extracted-text UI」，后者是「别进全屏模式」。
前者被无视时只有后者管用，故一并下发。白条随之消失。

**教训**：项目内文档里往往已经记着踩过的坑。**遇到疑似 bug 先 grep 自己的
文档**，比在代码里推演快得多也更准。我这次先猜了两轮才对，浪费了大量时间。

### 8.5 顺带修的构建链问题（`be01a82`）

- **`-PndkVersion=` 被静默忽略**：`project.findProperty('ndkVersion')` 撞上
  **AGP 注入的同名约定属性**（AGP 9.1 默认 NDK 恰好就是 `28`），于是去找不存在的
  `sdk/ndk/28`。改用 `gradle.startParameter.projectProperties`（只含命令行 `-P`）。
- **PowerShell 调 `.bat` 会在 `=` 后首个 `.` 截断参数**：
  `-PndkVersion=28.2.13676358` 被拆成 `-PndkVersion=28` + 任务名 `.2.13676358`。
  需写成 `'-PndkVersion=28.2.13676358'`。
- **模板缺 gradle wrapper**：README 一直让用户跑 `./gradlew`，但 `gradlew` /
  `gradlew.bat` / `gradle-wrapper.jar` **全都不存在**，模板并非真正「可复制」。

### 8.6 现在的真机基线（25098RA98C / HyperOS / Android 16）

| 项 | 结果 |
|---|---|
| 启动 | 正常，无 `UnsatisfiedLinkError` |
| 渲染 | **60 fps**，850x384 |
| JNI 注册 | 无 `Failed to register native method` |
| 生命周期 | 46.4% → **0.0%** → 46.4%，进程全程存活 |
| 中文 IME | 上下文字形正确，候选词与 preedit 稳定，按收起键不再被拉起 |
| IME 请求频率 | 静置 5s 增量 **0** |
| 16 KB 静态 | 两 ABI PASS（`PT_LOAD 0x4000`、`RELRO mod 0x0`）|

> 未验：**16 KB 运行时**（当前设备 `PAGE_SIZE=4096`，需 16 KB 模拟器或真机，
> 用户已决定搁置）、**emoji**（确认无法渲染，见 `TODO.md`）。

---

## 9. P3 文档债修复记录（2026-10-06）

第 2.2 节列的核心问题：`WindowDesc::fullscreen` / `set_fullscreen` /
`frame_style(Frameless)` / `set_window_level` 等在 Android 是**静默 no-op**
（winit Android 后端不实现，用户拿不到错误也没有文档提示）。

### 做法：不照抄过期清单，逐个核实当前代码

原清单只列了 4 个 API。实际扫了一遍「所有带 `## Platform-specific` 段的
`pub fn` 是否提到 Android」，发现两个问题：

1. **清单不完整** —— 另有 6 个 API 也没提（`set_resizable` / `set_icon` /
   `set_cursor` / `set_title` / `set_visible` / `set_maximized` / `set_minimized`）。
   其中 `set_resizable` 是**完全没有** `## Platform-specific` 段 —— 它是被
   清单漏掉的，不是漏了文案。
2. **清单已过期** —— 第 2.2 节说这些「无文档」，但实际 `set_title` /
   `set_fullscreen` / `set_maximized` / `set_minimized` / `set_visible` /
   `set_window_level` / `set_frame_style` / `set_icon` / `set_cursor` 的
   `## Platform-specific` 段**早已有** Android 说明（应该是后续轮次补的，
   但第 2.2 节没同步）。所以真正缺的是清单里没点到的那几个。

**教训与本项目此前的模式一致**：报「缺口」前先扫全量 + 读当前代码，
不要照抄旧清单或凭记忆。前面踩过同样的坑（winit 能力补全那轮，
凭记忆列了一批 winit 0.30 **不存在**的 API）。

### 本轮补齐的条目

**`VireoWindow`（src/window/mod.rs）**

| API | Android 行为 |
|---|---|
| `set_resizable` | no-op（原本**完全没有** `## Platform-specific` 段） |
| `title` | 返回 `WindowDesc::title` 原值，**不能**用来确认系统显示的标题 |
| `set_size` | no-op（并注明 `WindowDesc::size` / `dpi_override` 是建窗期的、**有效**，避免混淆） |
| `set_min_size` / `set_max_size` | no-op |
| `focus` | no-op（Android 焦点是 Activity / 输入法层面的概念） |
| `set_icon` | no-op（补：manifest `android:icon` 打包期定） |
| `set_cursor` | no-op（**修正一处错误引用**：原文写 `InputState::cursor_pressed`，该字段不存在，改为指向 `touch_count` / `touches` / `on_touch`，并说明触摸不模拟鼠标按键） |
| `set_opacity` / `set_cursor_hittest` / `reset_dead_keys` / `set_blur` / `set_content_protected` | 在「其余平台」里补上 Android（原本只说"其他平台"） |
| `request_resize` | no-op + **重要补充**：返回值恒 `None`，但这**不代表失败被检出**，只是「永远不生效」 |
| `primary_monitor` | 明确 Android **可用**（原文只列 Wayland/Web 例外，容易误读成 Android 也不行） |

**`WindowDesc`（src/window/desc.rs）** —— 结构体字段 + builder 双份：

| API | Android 行为 |
|---|---|
| `resizable` | winit 忽略，值记下即丢 |
| `fullscreen` / `maximized` | winit 忽略 |
| `icon` / `icon_from_path` | winit 忽略；并特别标注 `icon_from_path` 的**文件读取仍会执行**，所以路径错照样 warn，别误当成可用 API |
| `visible` | 半生效（指回字段文档的 `preparable` 说明） |

字段上也加了 `⚠️` 行内标注 —— 只在 builder 的 doc 里写，字段直接被读时看不到。

### 覆盖率校验

`src/window/mod.rs` + `src/window/desc.rs` + `src/input.rs` + `src/offscreen.rs` +
`src/texture.rs`：带 `## Platform-specific` 段的 `pub fn` 共 **47 个，
提到 Android 的 47 个（0 遗漏）**。

未加 `## Platform-specific` 段的 API 没动 —— 那是「本来就没有平台差异」
的 API，凭空加段反而稀释信号。

### 明确不做

- **不给这些 no-op 加 `log::warn!`**：它们是「平台本来就不支持」，加了每次调用
  都刷日志。且 `WindowDesc` 的字段被丢弃时甚至拿不到窗口上下文。真要诊断，
  用户查本文档的表格即可。
- **不把 no-op 改成 `Result`**：那要动签名（跨全仓 breaking），且语义上是
  「Android 无此概念」，不是「调用失败」。现有 `Result` 方法（如
  `set_cursor_hittest`）已经是 `NotSupported` 表达，风格一致。
