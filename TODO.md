# TODO

按「阻塞程度」排。每条都写了**为什么还没做**和**做之前需要什么**，避免变成
无信息量的愿望清单。

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

### emoji 可能无法渲染（待真机确认）

**现状**：Android 正文字体白名单只收 `NotoSans` 前缀（+ `ttf/otf/ttc`），
`NotoColorEmoji` 不在其中。cosmic-text 需要**显式加载**彩色位图字体
（CBDT/CBLC 格式），vireo 没做。所以 emoji 大概率渲不出来（可能显示为空白/
豆腐块/单色替代）。

**为什么没做**：需要在真机上看到实际结果才能确定严重程度 —— 有些 ROM 会用
系统 fallback 链补上，有些不会。如果真机确认能渲染（ROM 自己 fallback 了），
那这条根本不是问题，直接删掉白名单里不需要动。

**做之前需要**：一台能连的 Android 真机。在 pong 工程里 `draw_text` 画一串
emoji（`🦊🎵😀👨‍👩‍👧‍👦`）看输出。

**如果确认不能渲染**：方案是给 Android 字体加载加一条彩色字体通道 ——
`fontdb` 能解析 CBDT 但 cosmic-text 的 rasterizer 不直接用，
可能需要走 `cosmic_text::Font` 的 `font_metrics` + 自定义 raster，
工程量不小。**先确认再评估。**

---

### `android.rs` 的 8 个单测在桌面上跑不到

**现状**：`src/platform/android.rs` 整个是 `#[cfg(target_os = "android")]`
（`platform/mod.rs` 里门控），所以 `cargo test --lib` 里那 8 个
`#[cfg(test)]` 测试**从未在开发机上执行过** —— 只有跑 Android target 的 test
才会跑。

**后果**：那些测试验证的是纯逻辑（Insets 计算、SystemBars bitmask、
Android 常量与 AOSP 对齐、`SAFETY_MASK` 组成等），完全不需要 JVM。
但它们跟着模块一起被门控掉了，等于**白写**。

**怎么修**：照 `android_text_input.rs` 的做法 —— 把纯逻辑部分抽到
`src/platform/android_pure.rs`（无条件编译），Android 相关代码引用它。
参考 `platform/mod.rs` 里 `android_text_input` 的注释写法。

**为什么还没做**：需要先分清 `android.rs` 1780 行里哪些是纯逻辑、哪些碰 JNI。
`Insets` / `SystemBars` / `DisplayCutoutMode` / `Orientation` / `VireoWindowLevel`
的 `to_android`/`from_android` 应该是纯逻辑；`read_insets_logical` /
`post_to_java_thread` / 所有 `JniErr` 是 JNI。是一次纯搬运，但要有回归保障
（搬完先确认 8 个测试真的跑起来了 —— 现在连「它们能过」都不知道）。

---

## 非 Android

（暂无）

---

## 已完成 / 已否决

见 [`docs/android-audit.md`](docs/android-audit.md) 的「P0 修复记录」与
「P3 修复记录」两节 —— 含被否决的方案及**否决理由**（避免以后有人重提）。