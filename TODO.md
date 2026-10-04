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
### `onStop` / `onDestroy` / `onSaveInstanceState` / `onTrimMemory` 未转发

**现状**：vireo 只为 `onPause` / `onResume` 绑了 JNI（见
`src/platform/android_lifecycle.rs`）。winit 的 Android 后端把这四个也注册了但
不发事件，所以 vireo 收不到。

**影响**：
- `onStop` —— 「彻底离开」信号。现在只有 `onPause`，无法区分「短暂中断」
  （锁屏、弹窗）与「真的走了」。若要「只在彻底离开时保存进度」就做不了。
- `onSaveInstanceState` —— **进程被系统杀掉后状态无法恢复**。对游戏是硬伤：
  用户切后台被回收，回来后进度丢失。
- `onTrimMemory` —— 内存吃紧时无法主动释放资源（贴图、shape 缓存）。
- `onDestroy` —— Activity 真正结束时收不到通知（`VireoWindow::close()` 走的是
  自己那条路，所以影响较小）。

**做之前需要**：确认哪些真的需要。`onSaveInstanceState` 优先级最高
（数据丢失），`onTrimMemory` 次之（OOM 崩溃），`onStop` / `onDestroy` 较低。

**怎么加**（照 `nativeOnPause` 抄）：
1. `MainActivity.java` 加 `private static native void nativeOnStop();` + override
2. `android_lifecycle.rs` 的 `register_native_methods` 表里多一行
3. 一个 `extern "system" fn` 设对应的标志位
4. `App` 上加对应的 `on_stopped` 回调

`onSaveInstanceState` 特殊：参数是 `Bundle`，跨 JNI 传需要 `android_os_Bundle`，
比无参方法麻烦；且 `Bundle` 跨 JNI 需要序列化策略（`Bundle.putAll` + 一个
自定义 `Parcelable`）。**这条要单独设计，不是照抄能解决的。**

---

### emoji **确认无法渲染**（2026-10-06 真机实测）

**实测结论**：小米 25098RA98C / HyperOS / Android 16，`draw_text` 画
`🦊🎵😀👨‍👩‍👧`，**渲染成 6 个豆腐块**（带 X 的方框 = `.notdef` 字形）。
拉丁字母 `EMOJI:` 正常。所以**不是「可能」，是真的不行**。

**根因**：Android 正文字体白名单只收 `NotoSans` 前缀（+ `ttf/otf/ttc`），
`NotoColorEmoji` 不在其中。即使在白名单里也不够 —— cosmic-text 需要
**显式加载**彩色位图字体（CBDT/CBLC），vireo 的字体通道只走灰度 outline。

**要做的话**：给 Android 字体加载加一条彩色字体通道。已知难点：

- `fontdb` 能解析 CBDT/CBLC 元数据，但 cosmic-text 的 rasterizer 产出灰度位图，
  彩色位图得走它自己的 `swash`/纹理路径
- glyphon 的 glyph atlas 是单通道（`R8`？）还是 RGBA，决定了改造成本
- 要考虑 tint / 两色 emoji（`COLRv1`）—— Android 上是单色为主，优先级可低

**成本评估**：**不是小改动**。在确认「业务真的需要 emoji」之前不建议启动 ——
音游 / 工具类 UI 里 emoji 通常不是必需项。**若不需要，这条可以永远不做。**

**验证方法已就绪**（下次要复现直接照抄）：在应用里 `draw_text` 画
`"EMOJI: 🦊🎵😀👨‍👩‍👧"`，`adb shell screencap -p` 抓屏看是否豆腐块。
判据是**像素**，不是 logcat —— logcat 只会告诉你字符串提交了，
看不出有没有字形（这次就差点被 logcat 误导）。

---

## 非 Android

（暂无）

---

## 已完成 / 已否决

见 [`docs/android-audit.md`](docs/android-audit.md) 的「P0 修复记录」与
「P3 修复记录」两节 —— 含被否决的方案及**否决理由**（避免以后有人重提）。