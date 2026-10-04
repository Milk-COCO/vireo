# Android 适配审计报告

> 日期：2026-10-05（审计）/ 2026-10-06（P0+P1 修复完成）
> 审计对象：`D:\MilkCOCO\vireo` HEAD `3514e07`（`feat(android): 文本输入（IME）`）
> 方式：subagent 全量读源码 + 交叉核实 `winit 0.30.13` / `android-activity 0.6.1` 上游源码
> **重要**：subagent 的两条「阻塞级」结论被我复核后**修正**（见 §2）

## 状态：P0 / P1 已全部修复

| 阶段 | 状态 | commit |
|---|---|---|
| P0（3 项阻塞级）| ✅ 完成 | `bc0129d` |
| P1（3 项）| ✅ 完成 | `a624cfb`（gradle 模板）/ `57e2907`（契约文档）|
| P2（4 项）| 🔄 进行中 | 见 §5 修复计划表 |
| P3（生命周期）| ✅ 完成 —— 用户否决折中方案后改走「Java 转发 + RegisterNatives」| 见下 |

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

### 4.1 生命周期 ⚠️ 最大缺口

| 能力 | 状态 |
|---|---|
| `on_paused` / `on_resumed` | ❌ 无。winit 只在 `InitWindow`/`TerminateWindow` 发 `Suspended`/`Resumed` |
| 后台节流 | ❌ 无。`AppInner.max_fps` 默认 `Some(240)`（`app/mod.rs:318`），渲染循环**自驱动** → **切后台仍 240fps 空转**，Android 判耗电。`render_advice()` 会因 `focused()==false` 返回 `Unthrottled`，但**引擎明确不自动限速** |
| `LowMemory` | ❌ `Runner` 未实现 `memory_warning` |
| 状态保存恢复 | ❌ winit `MainEvent::SaveState` 直接 `warn!("TODO")` |

**技术障碍（重要）**：winit Android 后端把
`MainEvent::Pause/Stop/Resume/Start/Destroy/SaveState/LowMemory/InsetsChanged/ContentRectChanged`
**全部吞掉**（`winit-0.30.13/src/platform_impl/android/mod.rs:245-297`，只有 `warn!("TODO: ...")`）。
**vireo 想拿也拿不到**，除非绕开 winit 的事件循环。
→ 建议先做折中版（Android 下降低默认 `max_fps`，或用 `focused()` 当暂停代理），真解另立专项。

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
| **P2-1** | 缺陷 14：`platform::android` 的纯逻辑抽到无条件编译模块，让 8 个单测能跑 | **M** | ☐ |
| **P2-2** | 缺陷 8：`WindowLevel` → **`VireoWindowLevel`** 解 E0659 冲突（breaking，用户已定名）| **S** | 🔄 |
| **P2-3** | 缺陷 11：`system_bars_visible()` → `effective_system_bars() -> SystemBars`（用户：改彻底，不并存）| **S** | 🔄 |
| **P2-4** | 缺陷 12：Android 支持 `descriptor =`（强制 Vulkan）| **M** | 🔄 |
| **P3** | 生命周期：`MainActivity.onPause/onResume` 经 JNI 转发 + 渲染 Thread 停摆 | **M** | ✅ 完成 |
| **P3** | 生命周期真解（绕开 winit 吞事件）| **L** | ☐ |
| **待验** | 缺陷 15：真机确认 emoji 是否能渲染（`NotoColorEmoji` 不在白名单）| **S** | ☐ |

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