# Changelog / 更新日志

## [0.2.0] — 未发布 / unreleased

### Added / 新增

- **文件读写 API**：安卓的「文件」有三个互不相通的世界，此前 vireo 一条都没覆盖
  ——随包资源在 APK 内部（`std::fs` 读不到），且没有任何可写目录概念。
  - **读（随包资源，只读）**：`App::assets() -> Assets`，含
    `read` / `exists` / `list`；配套 `App::load_texture_asset` /
    `App::load_font_asset` / `WindowDesc::icon_from_asset`。
    安卓读 APK 内 `src/main/assets/`（经 `AssetManager`），桌面读
    `App::set_asset_root` 指定的目录（默认流程 `./assets`）—— **同一份代码两端通用**。
  - **写（可写目录）**：`App::data_dir()`（持久私有）/
    `App::cache_dir()`（可再生）/ `App::external_data_dir()`（用户可见，可为 `None`）。
    安卓 = `internal_data_path` / `Activity.getCacheDir()` / `external_data_path`，
    全部免权限；桌面按平台标准目录（`%APPDATA%` / `%LOCALAPPDATA%\Cache` /
    `$XDG_DATA_HOME` / `$XDG_CACHE_HOME` / `$XDG_DOCUMENTS`）。
    末级名字默认取可执行文件名，`App::set_storage_name` 可改。
  - **刻意不做**「相对路径自动回退到 assets」。那会让同一份代码在两个平台上
    **资源来源不同且不报错** —— 桌面调通了、安卓静默换一张图，排查成本极高。
    `load_texture` / `load_font_file` / `icon_from_path` 语义**完全不变**（纯 `std::fs`）。
  - 资产名归一化接受 `\` 与前导 `/`（同一字符串两端都能用），**拒绝 `..`**
    （它会参与路径拼接，能跳出根目录）。
  - 新增错误变体 `VireoError::AssetNotFound` / `NoAssetRoot`，
    区分「资源不存在」与「桌面没设根目录」。
  - 读取走 `Asset::read`，压缩与未压缩资产都能读，**不依赖** `noCompress` 配置。
  - 示例 `examples/resource_io`；assets 脚手架 `examples/assets/` 与
    `examples/android/src/main/assets/`。

- **触摸状态轮询 API**：`VireoWindow::touches() -> Vec<TouchPoint>` /
  `touch_count()` / `touch_first()`，以及公开类型 `TouchPoint`
  （`{ id, pos: PixelPos, force }`）。此前触摸没有类型安全的读法 ——
  `InputState.touches` 字段虽为 `pub`，但类型是裸
  `HashMap<u64, (f32, f32, Option<f64>)>`（无排序保证、无文档），实际只能靠
  `on_touch` 回调，鼠标式命中判定做不了。
  - `touches()` 按 `id` 升序（底层 `HashMap` 迭代顺序随机，不排序会让「取第一个点」
    每帧拿到不同手指）；`touch_first()` 与 `touches().first()` 逐位一致。
  - `pos` 用 `PixelPos` 双视图，与 `mouse_pos()` 同构；由逻辑值现算 `.px`
    （`dpi::dp_of`，**不是** `to_pixel_pos` —— 方向相反，用错会再除一次 scale）。
  - ⚠️ 轮询是**电平**语义：手指按住期间每帧都在结果里。「按一下触发一次」必须
    自己按 `id` 差分，或改用 `on_touch` 的 `Started` 边沿。文档内有示例。
  - 触摸**仍不**模拟鼠标按键：`mouse_left()` 在纯触摸设备上恒为 `false`。
- Android `onStop` 生命周期：`App::on_stopped` 回调 + `App::is_stopped()` /
  `android_lifecycle::is_stopped()`。只对「暂停期间 activity 变得不可见」的
  暂停触发，用来区分「半透明弹窗 / 权限对话框」这类短暂中断与「回桌面 / 锁屏」
  这类彻底离开（`onPause` 两者都触发）。
  `is_stopped()` 是「已暂停 && 本次暂停发生过 onStop」的派生量，恢复后自动翻回。
  Android `onStop` lifecycle: distinguish a brief interruption from a real departure.
- **生命周期暂停闸门可接管**：`App::set_auto_pause(false)`（默认 `true`，老代码零改动）。
  关掉后切后台不再阻塞渲染循环，由用户自己决定跑什么 / 画不画 —— 引擎不替用户
  决定功耗策略。「策略」（`auto_pause()`）与「状态」（`is_paused()`）刻意用不同词。
- **「暂停直到」交给用户**：`App::wait_while_paused()`（等前台，可打断）、
  `App::wait_until(timeout, cond)` + `App::notify_waiters()`（通用条件）。
  会被窗口关闭 / 设备丢失 / activity 销毁打断，不会挂死；平台生命周期变化自动唤醒，
  自己的条件变化需自己 `notify_waiters`。
- Android `onDestroy` 生命周期：`App::is_destroyed()` + `App::on_destroyed()`。
  唯一能回答「用户是切走了还是主动关了我」的信号（`is_paused` / `is_stopped`
  在划掉任务时都会照常触发），也是 `set_auto_pause(false)` 之后唯一的退出条件。
  ⚠️ **进程被系统杀掉时不触发**，所以不能用于资源清理或存档。
  Lifecycle gate is now user-controllable; wait primitives and `onDestroy` exposed.

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
- Android 平台支持（0.2.0 起全新提供；此前版本无任何安卓适配）：
  - 入口与构建：`#[vireo::main]` 生成 GameActivity 胶水（`InputConnection` 正常，
    中文输入可用）＋`cargo ndk` / gradle 模板（`examples/android`，
    `xbuild` / `cargo-apk` 不再适用）＋ targetSdk 35（挖孔全宽）＋ 16KB 页合规。
  - 窗口与系统：`WindowExtAndroid`（系统栏 / 安全区三值 / 挖孔 / 朝向 / 常亮 /
    亮度 / 层级）＋真实刷新率＋深浅色读与通知（复用 `theme()`，不新增 API）＋
    `configChanges`（字号 / 语言不重建）。
  - 输入：`TextInputState`（UTF-16/UTF-8 下标换算）＋返回键接管
    （`App::on_back` / `consume_back`）＋剪贴板（文本 / HTML，免费函数）＋
    振动（once / predefined / pattern / cancel，免费函数）。
  - 文件：SAF（tree / open / create＋`SafUri` 持久化跨启动复用）——资产与可写目录
    见上「文件读写 API」条。
  - 生命周期（安卓侧）：resume surface 重建＋返回键退出防僵尸（跨平台闸门见上相关条）。
  - 文字与弱 GPU：系统字体白名单加载（可定制扫描）＋ COLRv1 emoji＋
    noflat GLES 回退＋离屏格式漂移自愈。

- **GPU Compute 入口（P5）**：`App::compute_pipeline(wgsl, entry, &[(group, &bgl)])` /
  `GpuContext::create_compute_pipeline` 创建 compute 管线（用户自带 BGL 与任意 group 编号，
  复用 P3 的自管 group 模型；WGSL/布局错误返回 `Err` 而不是 panic）；
  `App::dispatch(&kernel, &[(group, &bind_group)], [x, y, z])` / `GpuContext::dispatch`
  提交一次 dispatch（独立 command buffer，提交边界自动 barrier —— 写入对后续 render pass
  可见）。新增 `App::gpu()` 暴露共享 `GpuContext` 以便自建纹理 / buffer / BGL。
  示例 `compute`（compute shader 每帧写粒子场到 storage texture 再贴回窗口）。
- `App::on_memory_warning(cb)` —— 跨平台内存压力回调（winit `Event::MemoryWarning`，
  Android 来自 `onTrimMemory`、iOS 来自 memory warning notification）。在**渲染线程**
  上触发，单个回调 panic 不打断渲染循环。**零 JNI**：winit 是唯一把 Android 生命周期
  事件正常转发出来的那个（`MainEvent::LowMemory`，`platform_impl/android/mod.rs:265-267`），
  其余 `Stop` / `Destroy` / `SaveState` 都是 `warn!("TODO")`。
  拿不到 Android 的具体 level（AGSK 的 C glue 只 `LOGV` 打印后写无 payload 的
  `APP_CMD_LOW_MEMORY`），但响应动作在所有 level 上相同。
  ⚠️ 这**不是「vireo 帮你防 OOM」**：vireo 侧能回收的只有 glyph atlas，
  显存大头（用户贴图）因 `App.textures` 是 append-only 而**结构上释放不了**。
- `DrawBatch::clip_self`（默认 `false`）：让本 batch 自身内容也受自己的矩形
  `scissor` 裁剪。默认仍只裁子 batch（旧行为不变）；`clips_children` /
  `scissor` / `text_clip` 的作用域与坐标空间已在字段文档写清（不再依赖行内注释）。
  `DrawBatch::clip_self`: the batch's own content now opts into its scissor window.
- `Area::rect(Rect)`：由逻辑像素矩形直接构建 Area（SDF 掩码），免去手搓
  `AreaGeom`；`Area::geom` 在 debug 构建校验索引（整三角形、不越界），
  SDF 路径却没有 `sdf_type != 0` 顶点时首次 `warn!`（掩码会退化成外接矩形）。
  `Area::rect`: build a rectangular stencil mask from a logical rect.
- `DrawBatch::clip_path() -> ClipPath`：查询本 batch 实际走的裁剪路径
  （`None` / `AreaStencil` / `Scissor { rect, automatic, clips_self }` / `Stencil`），
  纯查询、不推进状态。矩形裁剪请优先 `scissor`（见 `area` 模块文档）。
- `VireoWindow::draw_with_passes` / `OffscreenCanvas::draw_with_passes`：在引擎 pass
  **之后**用同一个 `CommandEncoder` 追加自定义 pass（后处理链、额外 compute），
  整帧只 submit 一次；引擎 pass 的 DS 不延续，需要时由回调自建。
  `draw_with_passes`: append custom passes to the same encoder after the engine pass.

### Changed / 变更

- **渲染能力三项落地（P1 / P3 / P4）**：
  - **深度/模板细粒度控制**：新增 `DepthStencilAttachment` / `DepthStencilOps` /
    `DepthStencilTarget`（引擎 DS 或用户自带 view）；`RenderTarget::set_depth_stencil`
    （链式 `with_depth_stencil`）；`Material::set_depth_write` / `set_depth_compare`；
    `VireoWindow::draw_with_depth_stencil` / `OffscreenCanvas::draw_with_depth_stencil`。
    缺失 aspect 自动补齐并 warn-once。支持深度预通道＋着色通道跨 draw 保留深度。
  - **MRT 多目标**：`ColorTargetDesc { view, resolve_target, blend }` +
    `RenderTarget::from_color_attachments`；`VireoWindow::draw_mrt` /
    `OffscreenCanvas::draw_mrt`；`Renderer::draw` / `RenderTarget::draw` 改收
    `clear_colors: &[Option<Color>]`（与颜色附件等长）。custom material 返回
    `MaterialOutput`（第 0 个字段须名为 `color`，引擎在其上应用 alpha/SDF）即可写多目标；
    pipeline key 含目标数量与逐目标 blend，双目标不同 blend 一次提交。引擎形状/文字与
    引擎 MSAA 不参与 MRT（单颜色输出，跳过并 warn-once）。示例 `mrt`。
  - **Bind Group 去硬编码**：`App::material_self_managed(source, &[(group, &bgl)])` /
    `GpuContext::create_material_self_managed`——用户自带 BGL 与 group 编号，任意
    `@group(n)` 布局；自管材质不再绑定引擎 group 0/1/2。默认材质仍在 group 3
    （`MaterialState::B` 的 `group_index` 构造期固定）。示例 `custom_material_self_managed`。
- `OffscreenCanvas.texture` 改为 `Mutex<Texture>`（`draw` 内按当前
    `surface_format` 自愈重建，见下）；`view()` / `bind_group()` 改返回
    owned 克隆（原返回内部引用，与锁共存不了）。`Texture` 新增 `format` 字段。
    受影响的只有三处示例（`offscreen` / `offscreen_aa` /
    `custom_material_post`，`&canvas.texture` 改为持锁守卫）。
- SDF 实例路径改为 storage vertex-pulling：实例记录集中到 `@group(2) @binding(2)`
  storage buffer（`InstanceData`，112B，为 custom VS 提供可追加的稳定 ABI），
  内置 VS 用 `@builtin(instance_index)` 读取。公开类型 `ShapeInstance` 移除
  （`DrawBatch` 内部字段随之改名 `instance_data`）；渲染行为与 draw call 合并不变。
  SDF instancing now pulls `InstanceData` from a storage buffer (stable,
  append-only ABI for custom vertex shaders).
- **破坏性**：自定义顶点着色器（custom VS）不再回退 mesh，改为按绘制路径的契约参与实例化
  —— `sdf_feather = Some(..)` 走 SDF storage 契约（顶点输入仅 `QuadVertex` @0，
  实例经 `@builtin(instance_index)` 读 `InstanceData`）；`sdf_feather = None` 走 geo
  模板契约（`TemplateVertex` @0/@1 + `GeoInstanceInput` @2/@3）。创建期按
  Mesh / SdfInstance / GeoInstance 逐布局验证，全败才报错；形状所需布局不被支持时
  该形状被跳过 + 警告日志（不静默画错）。插值限定须用普通 `@interpolate(linear)`
  （`sample` 限定在 sample_count=1 验证失败）。示例与 GPU 回归已更新。
  Breaking: custom vertex shaders now plug into the instance paths instead of
  falling back to mesh; the shader must match the contract of its draw path
  (SDF storage contract or geo template contract).
- `preserve_order = false` 的段排序键纳入 material / blend / blend constant /
  geo 模板身份：同类段按状态聚拢，减少管线与绑定切换（**不改变**可合并条件——
  仍要求状态相等且底层范围连续，因此 draw call 数不变）。
  Reordered segments now sort by full draw state, reducing pipeline/bind switches.

### Fixed / 修复

- `App::on_*(WindowIndex, ..)` 注册**静默失效**（零日志）：当回调先于窗口创建到达时，
  host drain `cb_rx` 时 `window_callbacks.get_mut(handle)` 返回 `None`，整个注册被丢弃。
  根因是 `App::window()` 走 `create_tx`、`on_*` 走 `cb_tx`，**两个独立 channel
  跨 channel 无顺序保证** —— 一轮 `about_to_wait` 里可能「建窗请求还没进（槽 len=0）
  但回调已到」。现在 drain 时按 handle 提前建槽（回调注册本就只依赖 `WindowIndex`，
  与窗口对象何时创建无关），并对超出 `MAX_CALLBACK_SLOTS` 的无效索引告警丢弃。
  **`on_*` 现在可以在 `app.run(..)` 之前正常注册。**
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
