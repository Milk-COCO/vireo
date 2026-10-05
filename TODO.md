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
### ~~`onStop` / `onDestroy`~~ ✅ `onStop` **已完成**（`App::on_stopped` + `is_stopped`）

`onDestroy` **仍不做**：`VireoWindow::close()` 走的是 vireo 自己那条路，Activity 真正
结束时收不到通知，影响很小。

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
- `cargo test --lib` **293 passed**（+4：锁存跨 resume 仍可读且只报一次、
  短暂中断不置锁存、连续多次 onStop 只报一次、桌面恒 `false`）
- `cargo ndk -t arm64-v8a` / `-t x86_64` `check --lib` 通过
- `gradlew assembleDebug '-PndkVersion=28.2.13676358'` BUILD SUCCESSFUL
- ⚠️ **真机 Home 键往返未验证**：验证进行到一半，两台 MuMu 的 adb 连接开始无响应
  （`adb shell echo` 都超时；`screencap` 早一步就已失灵，而它在本次会话早些时候是好的）。
  判定为模拟器侧故障，与本次改动无关（`force-stop` 应用后仍挂）。**JNI 绑定与 Java
  override 这一段尚无真机证据**，恢复后需补：`adb shell input keyevent 3` 回桌面
  → 重新启动 → `adb logcat -d | findstr lifecycle`，应看到 paused 与 stopped 各一条。

#### 踩坑（三处只有 Android target 才编到，桌面 `cargo test` 全绿）
1. `jni_str!` 宏**要求字面量**，不能传 `const` 标识符 → 绑定表只能写字面量，与常量
   脱钩；加 `JAVA_NAMES_MATCH_LITERALS` 编译期断言钉住（`PartialEq` 在 const 里还没
   稳定，自己写了 `const fn bytes_eq` 逐字节比）。
2. `AtomicBool::store` **不接受** `Acquire` / `AcqRel`（只有 `swap` 接受）。
3. `LifecycleCbs` 从 2 元组变 3 元组后 `cbs.0/.1/.2` 只能靠记忆区分 → 改具名字段
   结构体（`pub(crate)`，无 API 影响）。

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

### `onSaveInstanceState`：**不做**（已定论，勿再议）

三条理由：

1. **vireo 没有可序列化的状态**。`App.textures` / `offscreens` / `windows` /
   `gpu` 全是 `Arc<...>` 包着 `wgpu::Texture` / `Surface` / `Device` 句柄。
   `DrawBatch` 技术上可序列化但**毫无意义** —— 它是每帧从零重建的命令缓冲。
2. **用户游戏状态不在 vireo 里**。这是引擎层职责，vireo 是渲染库，在这一层之下。
3. **技术上被 winit 堵死**。`SaveState` 被 `warn!("TODO")`、
   `StateLoader` 被直接丢弃（`mod.rs:272-280`），绕过需绕开 winit 整个事件循环。

> **纠正一个旧误判**：此前写「`onSaveInstanceState` 需要 `android_os_Bundle` JNI
> 绑定 + 自定义 `Parcelable`」—— **Bundle 那部分完全错了**。
> `StateSaver::store(&[u8])`（`android-activity-0.6.1/src/game_activity/mod.rs:50-88`）
> 是**纯字节接口**，AGSK 的 recallback 自己写 Bundle。真实阻塞**只**是 winit 吞事件。

**用户该怎么做**：在**自己的** `MainActivity.java` 里 override
`onSaveInstanceState(Bundle)`，调自己的 native 方法存游戏状态（照
`nativeOnPause` 的 `RegisterNatives` 思路，与 vireo 无关）。恢复同理在 Java 侧做。

⚠️ **必须知道的现有限制**：`App::on_paused` 是**恢复后才补发**的，
所以**进程在后台被系统杀掉时，vireo 侧零回调** ——
`on_paused` **不能替代** `onSaveInstanceState`。

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

## 非 Android

（暂无）

---

## 已完成 / 已否决

见 [`docs/android-audit.md`](docs/android-audit.md) 的「P0 修复记录」与
「P3 修复记录」两节 —— 含被否决的方案及**否决理由**（避免以后有人重提）。