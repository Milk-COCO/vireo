# Changelog / 更新日志

## [0.2.0] — 未发布 / unreleased

### Added / 新增

- GPU 粒子路径：`Particle` / `ParticlePool` / `ParticleSlot` / `draw_particles`
  （`spawn` 参数烘焙＋VS 按 `camera.time` 积分位移与 fade 包络，存活期 CPU 零更新）。
  GPU particle path: baked spawn params + VS integration over `camera.time`.
- 粒子时钟注册表：`ClockIndex`（generational）＋`create_clock` / `destroy_clock` /
  `set_clock_scale`（0＝暂停，恢复无跳变）/ `set_clock_time` / `clock_time`；
  多独立时间线，0 号默认钟。
  Particle clock registry: multiple independent timelines, default clock 0.
- `DrawBatch::particle_clock`（batch 属性，默认 0 号钟）＋Renderer per-clock camera 组：
  同 batch 同钟，多批多钟同屏各走各的时间线（单 pass，零管线变动）。
  Per-batch particle clock + per-clock camera groups: independent timelines on screen.
- `ShapeStats::particles` 诊断字段（`shape_vertex_count` 含粒子等效顶点）。
- `Texture` address mode：`set_address_mode`（原地切换）/ `with_address_mode`
  （共享 image 的新对象，同帧多 mode 混用）＋prelude `AddressMode`
  （直接用 `wgpu::AddressMode`；默认 ClampToEdge，老行为不变）。
  Texture address modes: switch in place or share one image across modes.
- 混合全透传：`DrawBatch::set_blend_state` / `clear_blend_state` /
  `set_blend_constant`（形状＋文字，逐段冻结；语义 = wgpu 原样，用户自负；
  `Src1` 要 dual-source feature，vireo 不开）＋prelude `BlendState` 等。
  Raw blend-state passthrough: per-segment freeze for shapes and text.
- `ParticlePool::sweep`（单遍清除过期粒子，无分配；示例每 30 帧调用一次）。
  `ParticlePool::sweep` (single-pass expiry without allocation).
- 粒子 G1~G5：`rotation`（只转网格）＋`damping`/`gravity`（原始指数语义闭式积分，
  正=增长、衰减填负；`q=d*t` 分段 Taylor 保 f32 稳定）＋`size_mul_end`/`color_end`
  （alpha 仍只走包络，`end.a` 忽略）＋`AtlasSeq` 图集序列（行优先）＋
  `set_max_live`/`OverflowPolicy::{DropNewest, KillOldest}`（堆维护，摊销 O(log n)；
  cap 为 0 全拒；降 cap 不追溯）＋prelude `AtlasSeq`/`OverflowPolicy`。
  Particle G1-G5: rotation, damping/gravity closed-form, size/color endpoints,
  atlas sequences, pool cap with heap KillOldest.
- 粒子 `fade_mid` 包络中点（`Option<f32>`，默认 None = smoothstep；设值后 fade_out 段
  改线性三点 `1.0 → mid → 0.0`，与时长正交，`clamp 0..1`；`fade_in` 恒 smoothstep）。
  Particle fade-out midpoint shaping (linear 3-point vs smoothstep).
- Android 平台支持：`#[vireo::main]` 在安卓 target 下生成 GameActivity 胶水层
  按符号名查找的 `android_main` 入口，`AndroidApp` 存进进程级单例后转 `App::new`
  —— 与桌面同一构造器，公开 API 不区分平台；Android 上按文件名白名单加载系统
  正文字体，修复 `no default font found` panic；`requested_maximum_frame_latency` 可配。
  - **Android 后端从 `android-native-activity` 换成 `android-game-activity`**：
    前者没有 `InputConnection`，软键盘只能以物理按键事件送ASCII，**中文输入不可能**
    （android-activity 把 native_activity 的 `set_text_input_state` 写成 `// NOP`）。
    GameActivity 提供 `GameTextInput`（`TextInputState`：text / selection /
    compose_region），真机已确认输入法 `onStartInput` 正常连上。
  - **构建链随之改变**：`GameActivity extends AppCompatActivity`，theme 必须是
    AppCompat 子主题，且需要 `androidx.games:games-activity:4.4.0` 这个 gradle AAR
    —— `xbuild` / `cargo-apk` 没有 gradle 依赖支持，**不再适用**。改用
    `cargo ndk` 编 cdylib + gradle 打包。版本号对齐官方模板
    `rust-mobile/rust-android-examples` 的 `agdk-winit-wgpu`（AGP 9.1.0 / Gradle 9.3.1）。
  - 附带解决**内容进不去挖孔**：`xbuild` 内置 NDK 拒绝 `target_sdk_version ≥ 34`
    （`ndk doesn't support sdk version`），导致 targetSdk 偏低、
    `windowLayoutInDisplayCutoutMode` 被系统忽略。gradle 下 targetSdk 35 正常，
    真机实测窗口从 `2264×1080`（letterbox）恢复为 `2392×1080` 全宽。
  Android support: GameActivity entry point (CJK IME capable) + Android system fonts.
- `vireo::platform::android::WindowExtAndroid`（`#[cfg(target_os = "android")]`，
  不进 prelude）：系统 UI 显隐 / 滑动唤回行为、安全区三值模型、挖孔模式、
  运行期朝向、常亮、屏幕亮度、窗口层级。
  - 安全区三值：`safe_area`（物理最大遮挡，忽略可见性）/ `padding`（当前可见遮挡，
    键盘会吸收底部）/ `ime_insets`（键盘）＋`cutouts()`（摄像头物理矩形）。
    坐标系左上 y-down，单位 vireo 逻辑像素。设计参考 Flutter `MediaQuery` 三值模型
    （Unity 单值 `safeArea` 与 Godot 4.4→4.5 都在这个歧义上出过 bug）。
  - `SystemBars` 是 bitmask（可只隐藏状态栏或导航栏），`SystemBarBehavior` 只提供
    Android 31+ 真正支持的两档（不提供 API 30 已废弃的僵尸枚举）。
  - `WindowLevel`（`LayoutParams.type`）按 Android 语义重新表达为
    `Normal / Overlay / SystemAlert / AttachedDialog`，**不硬套** winit 四档
    （Android 没有「置底」）。`Overlay` 需 `SYSTEM_ALERT_WINDOW` 权限＋用户授权。
  - 平台强制行为已在 doc 注明：targetSdk 35+ 强制 edge-to-edge、API 36 无法退出；
    桌面窗口化模式 caption bar 永远可见。
  - setter 一律返回 `Result`（不静默 no-op），getter 打日志返回零值（每帧热路径）。
    Rust 生态此前无任何安全区 / 沉浸式 API（winit 0.30 的
    `WindowExtAndroid` 只有 2 个方法）。
  - 模块 doc 新增「建窗期属性」一节：`screenOrientation` / `configChanges` /
    `windowSoftInputMode` / 挖孔 style 全是 **APK 构建期 manifest 条目**，winit 的
    `WindowAttributes` 在 Android 上全部忽略，运行期改 `Window.setAttributes` 也不
    触发 WM 重算边框（实测挖孔模式无效）——附参考 manifest 片段。
  - 模块 doc 新增「三个刻意缺席的方法」：`set_ime_purpose` /
    `set_ime_cursor_area` / `fullscreen`+`Frameless` 在 Android 的语义差异与
    不提供的原因（前者须与文本输入后端一起做，后者 Android 无对应概念）
    —— 说明而非静默 no-op。
    Android system UI / safe-area / cutout / window-level control
    (first of its kind in Rust).

### Fixed / 修复

- Android 字体：全量加载 `/system/fonts`（真机 230 个文件）会把装饰体带进字体库，
  cosmic-text 默认家族 `"Arial"` 在 Android 命中不到 → 回退到「排序后第一个 face」
  → 实测**所有文字全大写、比分数字画不出来**。改为**文件名白名单**（正文字体前缀
  AND 字体扩展名双条件），只解析已知的正文字体文件，其余一概不碰；同时钉死 fontdb
  三个 generic 家族。顺带省掉 200+ 次字体解析的启动开销。桌面因恰好装了 Arial
  一直没暴露。
  Android fonts: full-dir scan pulled decorative faces in; whitelist text fonts instead.

- 无焦点时自动退出挂起：窗口已关闭但进程不退。根因是 winit 0.30 Windows 的
  `exit()` 只置标志不唤醒等待者，而 `run_app` 的退出检查点全在无限 park 返回之后，
  无焦点零事件时 park 永不返回。修为 `exit()` 后自发一次 wake（多余时无害）；
  附带退出握手收敛：Exit ack 确认＋re-wake、`draw_idle` 2s 超时放行、建窗 pending
  backstop 告警、全链路 `VIREO_EXIT_TRACE` 诊断。
  Fixed hang on auto-exit without window focus: window closed but process stayed alive.
  Root cause is winit 0.30 Windows `exit()` setting the flag without waking the waiter
  while `run_app` only checks it after an infinite park; fixed with a self-wake after
  `exit()`, plus exit handshake hardening (ack, timeouts, backstop, trace).

## [0.1.2]

### Fixed / 修复
- vireo-main 结束（正常返回或 panic）即关闭所有窗口并退出进程：
  loop/main panic 后不再留下冻住的窗口与僵尸进程（此前须 taskkill）。
  收尾走逐窗 close 路径（与 X 关闭一致）；手动 `App::run`/`spawn`
  自驱 main 的程序不受影响。
  When vireo-main ends (normally or via panic), all windows are closed and the
  process exits: no more frozen windows or zombie processes after loop/main
  panics (which previously required taskkill). Teardown reuses the per-window
  close path, identical to X-close. Programs driving main manually via
  `App::run`/`spawn` are unaffected.
- 并发双重关窗计数下溢：`alive_window_count` 只在槽位实际由 `Some` 变 `None`
  时递减，否则回绕后退出谓词永久失效。
  Fixed double-close underflow of `alive_window_count`: decrement only when the
  slot actually transitions from `Some` to `None`, otherwise the counter would
  wrap and permanently disable the exit predicate.
- 进程退出码只反映 main 自身的结局：main panic → 1，其余 0
  （loop panic 被 main 妥善处理后正常返回仍是 0）。
  The process exit code reflects only main's own fate: main panic → 1,
  otherwise 0 (a loop panic gracefully handled by main followed by a normal
  return still exits 0).
- 默认窗口图标编译进库：根目录 `logo.png` 经 `include_bytes!` baked，
  不再运行时读启动目录——此前在哪启动决定有没有图标，且失败全程静默。
  `icon_from_path` 任一步失败改 `log::warn!`（路径 + 阶段）。
  The default window icon is now baked into the library (`logo.png` via
  `include_bytes!`) instead of being read from the startup directory at
  runtime — previously icon presence depended on where the process was launched
  from, and all failures were silent. `icon_from_path` now `log::warn!`s
  (path + stage) on any failure.

## [0.1.1]

### Fixed / 修复
- 删除渲染循环内全部调试 `eprintln!` 输出（`[acq]` / `[draw]` / `[resize-trace]`），
  含 `VIREO_DRAW_TRACE` / `VIREO_RESIZE_TRACE` 基础设施；`Validation` 路径保留
  `log::warn!`。0.1.0 中部分 acquire 日志未进 env 门导致每帧刷屏，现已清除。
  Removed all debug `eprintln!` output from the render loop (`[acq]` / `[draw]` /
  `[resize-trace]`), including the `VIREO_DRAW_TRACE` / `VIREO_RESIZE_TRACE`
  machinery. In 0.1.0 some acquire logs bypassed the env gate and printed every
  frame; now fully removed. The `Validation` path keeps its `log::warn!`.

## [0.1.0] — 已 yank / yanked

首个发布版。因含上述调试刷屏问题，不建议使用，请用 0.1.1。
Initial release. Contains the debug-spam issue above; not recommended, use 0.1.1.
