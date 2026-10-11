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

- **SAF（系统文件选择器，安卓独有）**：`App::open_document_tree` /
  `open_document` / `create_document`（全异步，结果走回调，回调跑渲染线程）+
  `SafUri`（URI 字符串可持久化 + `open_reader` / `open_writer` 开出真正
  `std::fs::File` + 目录 `list` / `resolve` / `create_*` / `delete` +
  `persisted` / `release`）+ `saf_cancel_pending`。
  - Java 侧强制：`MainActivity.java` 必须含 SAF 桥（模板已集成，另有独立可复制文件
    `examples/android/SAF_SNIPPET.java`）；缺了 launch 直接 `Err` 明说。
  - 不用 `OpenDocument*` 三个 contract（其 Intent 不带 flag，持久化做不成），
    改用 `StartActivityForResult` + 自己组 Intent（READ/WRITE/PERSISTABLE 全要）。
  - 新增错误变体 `VireoError::UserCancelled`（用户取消，不是故障）/
    `Unsupported`（该能力只在特定平台有）。

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
- **修复** `App::on_*(WindowIndex, ..)` 注册**静默失效**（零日志）：当回调先于
  窗口创建到达时，host drain `cb_rx` 时 `window_callbacks.get_mut(handle)` 返回
  `None`，整个注册被丢弃。根因是 `App::window()` 走 `create_tx`、`on_*` 走
  `cb_tx`，**两个独立 channel 跨 channel 无顺序保证** —— 一轮 `about_to_wait` 里
  可能「建窗请求还没进（槽 len=0）但回调已到」。Android 的跨线程入口下**必现**
  （实测 pre-run 的 `app.on_touch(idx, ..)` 全部丢失；此前误判为「`cb_tx` 未就绪」，
  实际它一直是 `Some`）。现在 drain 时按 handle 提前建槽（回调注册本就只依赖
  `WindowIndex`，与窗口对象何时创建无关），并对超出 `MAX_CALLBACK_SLOTS` 的
  无效索引告警丢弃。**`on_*` 现在可以在 `app.run(..)` 之前正常注册。**
- Android 系统字体扫描可定制：`#[vireo::main(fonts = |c| { .. })]` 宏属性 +
  `vireo::text::android_fonts::configure`（进程级，须在 `App` 构造前调用），
  可追加 `extra_prefixes`（文件名前缀）/ `extra_dirs`（扫描目录）/
  `extra_families`（优先家族名，排在内置表之前）。
  解决「未收录的 ROM 拿不到任何正文字体 → 所有文字全大写、数字消失」。
  三个字段缺一不可：只加前缀不钉家族名，症状照旧。
  Android font scanning is now user-configurable (whitelist was compile-time fixed).
- Android `onStop` 生命周期：`App::on_stopped` 回调 + `App::is_stopped()` /
  `android_lifecycle::is_stopped()`。只对「暂停期间 activity 变得不可见」的
  暂停触发，用来区分「半透明弹窗 / 权限对话框」这类短暂中断与「回桌面 / 锁屏」
  这类彻底离开（`onPause` 两者都触发）。
  `is_stopped()` 是「已暂停 && 本次暂停发生过 onStop」的派生量，恢复后自动翻回。
  Android `onStop` lifecycle: distinguish a brief interruption from a real departure.
- **修复** Android 上 `App::on_paused` **从不触发**：渲染线程的生命周期闸门排在
  代号检查之前，进闸门后阻塞到恢复，醒来时 `is_paused()` 已是 `false`，回调于是
  永远走 resumed 分支。现改为先判代号、先发回调、再进闸门 —— `on_paused` 在暂停
  生效时触发。
  Fixed `App::on_paused` never firing on Android (gate ran before the generation check).
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

### Added / 新增

- `App::on_memory_warning(cb)` —— 跨平台内存压力回调（winit `Event::MemoryWarning`，
  Android 来自 `onTrimMemory`、iOS 来自 memory warning notification）。在**渲染线程**
  上触发，单个回调 panic 不打断渲染循环。**零 JNI**：winit 是唯一把 Android 生命周期
  事件正常转发出来的那个（`MainEvent::LowMemory`，`platform_impl/android/mod.rs:265-267`），
  其余 `Stop` / `Destroy` / `SaveState` 都是 `warn!("TODO")`。
  拿不到 Android 的具体 level（AGSK 的 C glue 只 `LOGV` 打印后写无 payload 的
  `APP_CMD_LOW_MEMORY`），但响应动作在所有 level 上相同。
  ⚠️ 这**不是「vireo 帮你防 OOM」**：vireo 侧能回收的只有 glyph atlas，
  显存大头（用户贴图）因 `App.textures` 是 append-only 而**结构上释放不了**。

### Fixed / 修复

- **Android 12+ emoji 渲染为空白**（`🦊🎵😀🇨🇳👨‍👩‍👧👍🏽` 全部画不出来）。
  `swash` 0.2.10 —— `cosmic-text` 0.19 唯一的字形光栅器 —— 按 **COLR version 0** 的
  布局读表头，而 Android 12+ 的 `NotoColorEmoji.ttf` 是 **COLRv1**：该字段在 v1 布局里
  是 `numBaseGlyphRecords` = 0，记录查找落空 → 回退到 `glyf` 轮廓 → 而这些 emoji 的
  `glyf` 条目长度为 0 → 交回一个 `0×0` 位图 → 被当作「无需光栅化」**静默丢弃**（无 panic、
  无日志）。上游 issue 五年零活动且 swash 已是末版，因此 vireo 内补了一条 COLRv1 光栅化
  路径（`skrifa` 走 paint graph + `zeno` 光栅，两者**本来就在依赖树里，零新增依赖**），
  仅在 swash 交回空位图时二次尝试。
  修法选的是「读系统字体」而不是「打包一份 emoji 字体」：后者更省事，但会把所有平台的
  emoji 锁死成同一套外观，抹掉平台原生风格。
  COLRv1 rasterization for Android emoji: reads the platform's own colour font instead of
  shipping one, so each OS keeps its native emoji design.
- 顺带修掉 `glyphon` 图集扩容路径上一个会**静默吞字形**的隐患：
  `InnerAtlas::grow()` 重建纹理后重光栅化每个缓存字形，此前对重光栅化结果不做任何校验，
  失败时写入变成 no-op，字形指向空白区域且无任何日志。现在尺寸或内容类型与缓存记录不符
  即淘汰该字形并归还打包槽位，下一帧重新光栅化。
- Android 文档纠正：`MainEvent::LowMemory` 并非「在 Android 侧永远不会被构造」。
  winit 该区间里恰好有一个非 `warn!` 分支正常转发它，此前审计读了
  `mod.rs:245-297` 却漏看这一个 arm，把 `LowMemory` 连同其他一起归进「全部吞掉」，
  且该错误结论在仓库里复制了 5 处（`android_lifecycle.rs` 模块文档 / README /
  TODO / 审计报告）。同时纠正「`onSaveInstanceState` 需要 `android_os_Bundle` JNI
  绑定」—— `StateSaver::store(&[u8])` 是纯字节接口，Bundle 由 AGSK 处理，
  真实阻塞只是 winit 吞事件。
- 测试：`android_text_input` 的三个 `IME_STATE` 测试加了串行锁。此前它们共享
  进程级全局量却依赖 cargo 默认串行执行，并行下会互相污染（`cargo test --lib`
  偶发 FAILED，单跑必过）。

- Android：`vireo_macro` 依赖指向 crates.io 而非仓库内的 `vireo_macro/`，导致本地宏
  改动**完全不生效**（改源码后构建 0.65s 不重编、宏内 `eprintln!` 一次不打印）。
  模板 `cdylib` 因此缺 `android_main` 导出符号，真机 `dlopen failed: cannot locate
  symbol "android_main"` 直接崩。改为 `vireo_macro = { path = "vireo_macro", version = "0.1" }`。
- Android：帧回调里每帧调用 `show_soft_input` / `hide_soft_input` 会把 Java 主线程
  handler 淹掉，真机 **5 秒 ANR**（`Input dispatching timed out` /
  `Waited 5000ms for FocusEvent(hasFocus=false)`），系统随后 `send 9` 杀进程。
  根因是这两个 API 无条件 `post` 到主线程，且已有 `IME_WANTED` 状态却没用来去重。
  现在两者都是**纯跳变去重**（未聚焦时每帧调用**零** post，同状态重复调用永不重复
  post），均可安全每帧调用。实测未聚焦时 `ImeTracker` 会话数从数百个降到 3 个。
  「用户用系统按钮收起键盘」这一观测不到的状态不靠引擎周期性重发兜底（见下条），
  而是在文档里给出显式恢复方式：`hide_soft_input()` 再 `show_soft_input()`。
- Android：**重复 `show_soft_input` 会清掉输入法的组合态**。为兜底「键盘被收起后
  请求值陈旧」曾加 200ms 重发窗口，真机症状：① 输入字符时候选词与 preedit 一闪即消；
  ② 按输入法的收起键会在 200ms 内被重新拉起，只有点到 vireo 其他区域（`focused=false`
  走 hide）才停。组合态易碎，**任何重复 show 都是破坏**，故重发窗口已完全移除。
- Android：触摸坐标被**双重换算**。`WinitEvent::Touch` 里存的已是逻辑像素
  （`host.rs` 收到事件时按有效 scale 除过），`apply_input_event` 又除了一遍 scale ——
  HyperOS(scale 2.8125) 上 `tap(844,551)` 变成 `(37.9,24.8)`，任何命中判断都失效，
  表现为「点输入框键盘不弹」。另外触摸**完全不更新** `mouse_pos`，而 Android 上点击
  不产生 `CursorMoved`，该字段恒为 `(-1,-1)`。现在触摸点也写入 `mouse_pos`（按
  `CursorMoved` 的约定存物理像素），新增 `touch_logical_to_mouse_pos` 纯函数 +
  往返测试锁死两个坐标空间的契约。
- Android：输入法无视 `IME_FLAG_NO_FULLSCREEN`，横屏进入 AOSP **fullscreen extract
  mode**，顶部弹全宽白条 + 框架字符串按钮，IME 窗口 `MATCH_PARENT` 吃掉整屏 →
  `ime_insets.bottom` 恒为 0、`adjustResize` 形同虚设。该 flag 的 AOSP 文档本就写明
  「不是保证，某些输入法可能忽略」，而 `IME_FLAG_NO_EXTRACT_UI`（`0x10000000`）与它
  **语义不等价**（前者「别显示 extracted-text UI」，后者「别进全屏模式」），
  前者被无视时只有后者管用，故一并下发。白条随之消失。
- Android：`gradle -PndkVersion=` 被静默忽略。`project.findProperty('ndkVersion')`
  撞上 AGP 注入的同名约定属性（AGP 9.1 默认 NDK 恰好是 `28`），于是去找不存在的
  `sdk/ndk/28`。改用 `gradle.startParameter.projectProperties`（只含命令行 `-P`）。
  另注：PowerShell 调 `.bat` 会在 `=` 后首个 `.` 截断参数，需写成
  `'-PndkVersion=28.2.13676358'`。

### Changed / 变更

- **Android 平台限制文档债已清**：所有带 `## Platform-specific` 段的 `pub fn`
  现在都明确标注了 Android 行为（`window/mod.rs` + `window/desc.rs` 共 47/47）。
  本轮补齐 `set_resizable`（原本**完全没有**该段）/ `title` / `set_size` /
  `set_min_size` / `set_max_size` / `focus` / `set_icon` / `set_cursor` /
  `request_resize`，并在 `WindowDesc` 的字段 + builder 双份标注 `resizable` /
  `fullscreen` / `maximized` / `icon` / `icon_from_path`。
  这些 API 在 Android 上是**静默 no-op**（winit Android 后端不实现，
  无报错无日志）—— 现在文档写明了，附带哪些 Android 上**确实可用**的对照
  （如 `set_visible` 半生效、`primary_monitor` 可用）。顺带修正一处失效的
  文档引用（`set_cursor` 指向不存在的 `InputState::cursor_pressed`）。
  无行为变更。
- `vireo_macro` 生成的 Android 入口改为 `pub extern "C" fn android_main`
  （原为 Rust ABI 的 `pub fn`；`#[no_mangle]` 本就足够导出，此项是 ABI 正确性改进）。
- Android 模板补上缺失的 gradle wrapper（`gradlew` / `gradlew.bat` /
  `gradle-wrapper.jar`）——README 一直让用户跑 `./gradlew`，但脚本并不存在，
  模板此前并非真正「可复制」。

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

- Android：另一个 Activity 盖住后回来（SAF 选择器等），`configure` 打在已死的旧
  surface 上必 panic（`Surface::configure` / `queue family`，有时逃逸成 SIGABRT）。
  根因是 GameActivity 在 suspend 时拆掉 native surface，而 vireo 只在 acquire 路径
  处理 `Lost`，resume 后 0×0→正常尺寸的 drift-configure 仍用旧对象。
  修：`Runner::suspended()` 置脏旗（suspend 期跳过一切 surface 调用，
  `DrawSkipReason::SurfaceStale`），`resumed()` 清旗，per-window `was_stale` 闩让
  resume 后首帧 `recreate_surface` + `configure` + 跳过（复用 `Lost` 路径形状）。
  桌面 winit 永不发 `Suspended`，零行为变化。
  Fixed panic when resuming after another Activity (e.g. SAF picker) destroyed the
  native surface: the stale surface object was reused for configure. Now Suspended
  marks it stale (frames skip surface use), and the first frame after Resumed
  recreates it.
- Android 模板 `configChanges` 补齐 `fontScale|locale|layoutDirection|keyboard`：
  改系统字号或语言不再重建 Activity（此前会整套重建，Java 字段全丢）。
  真机验证通过（pid 不变、无重建、无 panic）。
  Added missing configChanges values so font-scale and locale changes no longer
  recreate the Activity.
- **剪贴板**：免费函数 `clipboard_text` / `set_clipboard_text` /
  `clipboard_html` / `set_clipboard_html`（prelude 导出，刻意不挂 `App` ——
  剪贴板是进程级全局状态，无状态可注入）。
  Windows（Win32，`CF_UNICODETEXT`/`CF_HTML`，短暂占用重试）+
  Android（纯 JNI，无权限无 Java 改动）；macOS/Linux → `Unsupported`。
  读空剪贴板返回 `None`（不是错误），读 HTML 严格不降级。
  Clipboard: plain text + HTML on Windows and Android; free functions.
- **振动**：免费函数 `has_vibrator` / `vibrate_once(ms, amplitude?)` /
  `vibrate_predefined(Tick/Click/HeavyClick/DoubleClick)` /
  `vibrate_pattern(timings, amplitudes?, repeat?)` / `cancel_vibration`
  （prelude 导出）。Android 纯 JNI（模板 manifest 加 `VIBRATE` 权限行）；
  其它平台 → `Unsupported`。无马达设备 `vibrate_*` 报 `Err`（逼调用方用
  `has_vibrator` 做门），`cancel` 无马达直接 `Ok`。
  Vibration: one-shot, predefined effects, custom waveforms on Android.

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
