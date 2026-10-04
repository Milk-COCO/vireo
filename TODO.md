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

### 16 KB page size 未保证（上架门槛，非优化）

**现状**：`examples/android/build.gradle` 已配
`packaging { jniLibs { useLegacyPackaging = false } }`，这是 16 KB 对齐的**必要
非充分**条件。缺的是 **NDK r28+**（r28 起默认 `-z max-page-size=16384`）。
真机验证时用的是 NDK **26.3**，所以**未保证**。

**为什么重要**：Google Play 从 2025-11 起要求所有新提交/更新的 APK 必须
16 KB 对齐。这不是「优化」，是**上架门槛** —— 不过就发不出去。

**为什么不能现在做**：升 NDK 后需要重新出一版 APK 并**真机验证**（`.so` 变大、
Vulkan 驱动差异都要实测）。属于独立的工程动作，不该插在 Android 适配收尾里。

**做之前需要**：
- NDK r28+（`sdkmanager "ndk;28.x.y"`，或装 Android Studio 的 SDK Manager）
- 一台真机
- 验证命令：
  ```bash
  # 升 NDK 后重编
  cargo ndk -t arm64-v8a build --release --lib
  ./gradlew assembleRelease --console=plain

  adb shell getconf PAGE_SIZE                     # 设备实际 page size
  zipalign -c -P 16 -v 4 build/outputs/apk/release/*.apk
  ```
- 若 `zipalign -c` 报未对齐，检查 `useLegacyPackaging = false` 是否生效、
  以及 AGP 是否 ≥ 8.5.1（模板用的 9.1.0，满足）

**相关**：`examples/android/README.md` §5

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