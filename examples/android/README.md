# vireo Android 模板

一个可直接 `cargo ndk build` + `gradlew assembleDebug` 出 APK 的最小工程。

真机验证环境：小米 `25098RA98C`（HyperOS / Android 16，density 2.8125，2392×1080），
Vulkan 后端，`minSdk 31` / `targetSdk 35`。

---

## 0. 先读这段：为什么不能用 `xbuild` / `cargo-apk`

vireo 在 Android 上**必须**用 `AGDK GameActivity`，因为 `NativeActivity`
没有 `InputConnection` —— 拿不到输入法，中文完全打不出来。

而 `GameActivity extends AppCompatActivity`，所以：

- 必须有 gradle 工程（不是纯 Cargo 的 apk 工具链能表达的依赖）
- 必须依赖 `androidx.appcompat` + `androidx.games:games-activity` 两个 AAR
- manifest 的 theme 必须是 **AppCompat 子主题**

`xbuild` 的 `manifest.yaml` 那套是给 `NativeActivity` 设计的，**不适用**。
本目录里也不需要 `manifest.yaml`。

---

## 1. 前置条件

| 需要 | 版本 | 备注 |
|---|---|---|
| JDK | **17+** | AGP 9 的硬要求。报 `Unsupported class file major version` 就是还在用 11 |
| Android SDK Platform | **35** | `compileSdk = 35` |
| Android SDK Build-Tools | 35.x | |
| NDK | 26.x 可用，**建议 28+** | 见 §5。不是 16 KB 的硬性要求 |
| cargo-ndk | 4.x | `cargo install cargo-ndk` |
| Gradle | 9.x | 或直接用 `gradle` 命令 |

环境变量：

```bash
export ANDROID_HOME=/path/to/sdk          # 或 ANDROID_SDK_ROOT
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/28.2.13676358
```

> 没有 Android Studio 也能做，本模板就是为了纯命令行准备的。

---

## 2. 目录结构

```
android/
├── Cargo.toml              # crate-type = cdylib；[lib] name = vireoandroid
├── src/lib.rs              # 入口。#[vireo::main] 在 Android 上自动展开成 android_main
├── settings.gradle         # AGP 9.1.0 + 仓库
├── build.gradle            # cargo ndk 接线 + SDK 版本 + ABI + 依赖
├── gradle.properties       # AndroidX 开关
├── gradle/wrapper/         # Gradle 9.3.1 wrapper 配置
└── src/main/
    ├── AndroidManifest.xml
    ├── java/com/example/vireoandroid/MainActivity.java
    └── res/values/themes.xml
```

> **为什么入口是 `src/lib.rs` 而不是 `src/main.rs`**：这是 `crate-type = ["cdylib"]`
> 的 crate。Android 侧由 AGSK 按符号名查找 `android_main`，不需要 Rust 的 `fn main`，
> 有 bin target 反而多一份要交叉链接的桌面入口（`cargo ndk build` 漏 `--lib` 时的
> 报错就来自它）。

> `src/main.rs`（cargo）和 `src/main/`（gradle 的 java/res 根）**共用一个目录**，
> 这是 gradle Android 插件的默认布局，不是笔误。

---

## 3. 三步出 APK

```bash
# 0) 先确认 Rust 侧能过（比 gradle 快得多，错误信息更直接）
cargo ndk -t arm64-v8a check --lib

# 1) 只出 .so（必须 --lib，Android 不要 bin target）
cargo ndk -t arm64-v8a build --release --lib

# 2) 出 APK
./gradlew assembleDebug --console=plain
```

产物：`build/outputs/apk/debug/android-debug.apk`

安装：

```bash
adb install -r build/outputs/apk/debug/android-debug.apk
```

> `gradlew` 二进制 wrapper（`gradlew` / `gradle-wrapper.jar`）**没有**提交进仓库
> （jar 是二进制且与版本绑定）。用 `gradle wrapper` 生成一次，或直接用你 PATH 上的
> `gradle` 命令 —— 等价。
>
> ⚠️ **NDK 版本别靠环境变量撞对**：没设 `ANDROID_NDK_HOME` / `NDK_HOME` /
> `local.properties` 时，`cargo-ndk` 会**自动取 SDK 里版本号最高的 NDK**。
> 想可复现地锁定，用 `-PndkVersion=`（见 §5）或写进 `local.properties` 的 `ndk.dir`。

### 拷去自己的项目时要改的 4 处

| 文件 | 改什么 |
|---|---|
| `settings.gradle` | `rootProject.name` |
| `build.gradle` | `rustCrate`、`applicationId` |
| `Cargo.toml` | `[package] name`、`[lib] name`、`vireo` 依赖来源 |
| `MainActivity.java` | `package` 行 + `System.loadLibrary("...")` |

**`rustCrate` / `[lib] name` / `loadLibrary` 三处名字必须一致**，不一致是
`UnsatisfiedLinkError` 的头号原因。

---

## 4. 五个必须知道的坑

全部在真机上踩过、且都表现为「没有任何报错，只是行为不对」。

### 4.1 ⚠️ 不要 `android:windowFullscreen`

**症状**：logcat 刷 `InsetsControlTarget invalid!:Window{...MainActivity}`，
`show_soft_input()` 返回 `Ok`，但 `mInputShown=false` / `mImeWindowVis=0` —— **键盘永不弹**。

**原因**：MIUI / HyperOS 的 `MiuiMirrorInputMethodImpl` 拒绝「全屏 + 强制显示输入法」的组合。

**正解**：沉浸式用 Rust 侧的 `set_system_bars(SystemBars::ALL)`，那条路有效。

### 4.2 ⚠️ `set_ime_allowed` 在 Android 无效

**别用 `win.set_ime_allowed(true)`**。它转发 winit，而 winit 的 Android 路径是
`show_soft_input(true)` —— `SHOW_IMPLICIT` 的语义是「仅当输入法被**隐式**请求时才显示」。
游戏没有聚焦的 Android View，永远不会有隐式请求。

**正解**：`win.show_soft_input()` / `win.hide_soft_input()`（`platform::android`）。

### 4.3 ⚠️ 别自己设 `ImeOptions`

**症状**：输入框上方一条全宽白条 + 输入法工具条显示中文「执行」；
`ime_insets.bottom` 恒为 0，`adjustResize` 失效。

**原因**：手动设 `ImeOptions` 会抹掉 `IME_FLAG_NO_FULLSCREEN`，
AOSP 的 extract mode 接管，输入区变成系统自己的全屏编辑条。

**正解**：什么都不设。vireo 内部固定发
`IME_FLAG_NO_FULLSCREEN | IME_FLAG_NO_ACCESSORY_ACTION`（`0x22000000`）。

### 4.4 ⚠️ `targetSdk >= 35` 不是可选项

`windowLayoutInDisplayCutoutMode`（刘海/挖孔）在 `targetSdk < 35` 时被系统**忽略**，
内容被 letterbox 到安全区。真机对比：旧配置 2264×1080，新配置 2392×1080 满宽。

运行时调 `set_display_cutout_mode()` 在部分 ROM 上是**空操作**（返回的
`cutouts()` 恒为空），所以只能靠 manifest/style。

### 4.5 ⚠️ 首帧别急着弹键盘

刚 `onCreate` 就 `show_soft_input()` 在部分 ROM 上会被吞。每帧检查焦点、
需要时重复调（vireo 的模板示例就是这么做的）。

---

## 5. 16 KB page size（Android 15+）

Google Play 截止 **2027-02-01**：此后 app 更新不支持 16 KB page size 就无法发布。
官方指南：<https://developer.android.com/guide/practices/page-sizes>

### 判据有两条，而且官方工具只查其中一条

| # | 判据 | 不满足的后果 |
|---|---|---|
| 1 | 每个 `PT_LOAD` 的 `p_align >= 0x4000` | 加载失败 |
| 2 | **`PT_GNU_RELRO` 的 `(VirtAddr + MemSiz) % 0x4000 == 0`** | **运行时 segfault** |

⚠️ Google 官方的 `check_elf_alignment.sh` **只查第 1 条**
（`grep LOAD | awk '{print $NF}' | head -1` —— 连第二个 LOAD 都不看）。
**RELRO 那条在官方文档里是独立的手工步骤，脚本不给。** 所以它会给假绿灯。

### 这两条由两个不同的 linker 开关控制

这是整件事的坑点：

| 开关 | 控制 | Android clang driver 默认（r26 / r28） |
|---|---|---|
| `-z max-page-size` | `PT_LOAD` 的 `p_align` | 4096 / **16384** |
| `-z common-page-size` | **RELRO 段末边界** | **两个版本都不注入**（用 lld 默认 4096） |

后果：RELRO 的**起点**随 LOAD 按 16384 对齐，**终点**却只按 4096 对齐 ——
`(起点 + RELRO 大小) % 0x4000` 等于取决于 RELRO 内容大小，**约 1/4 概率才是 0**。

### 本模板的做法：两个 flag 显式传

`.cargo/config.toml`（本目录）：

```toml
[target.aarch64-linux-android]
rustflags = [
    "-C", "link-arg=-Wl,-z,max-page-size=16384",
    "-C", "link-arg=-Wl,-z,common-page-size=16384",
]
```

**`common-page-size` 那一行才是关键** —— driver 从不注入它，RELRO 合规全靠它。
`max-page-size` 也显式传，是为了不依赖「r28 driver 默认值」和
「cargo-ndk 只给 NDK ≤ 27 补 flag」这两个外部事实（NDK r29+ 或 cargo-ndk
改判定条件都不会打我们）。

**代价：`.so` 体积零增长**（padding 落在本就存在的零填充区），
RELRO `MemSiz` 最多多 12 KB 未映射 guard 区，运行时无性能影响。

### 为什么不能只靠「用 NDK r28+」

实测（同一个 cdylib，仅切 NDK）：

| NDK | 是否有 cargo-ndk 补 flag | RELRO end | `mod 0x4000` |
|---|---|---|---|
| 26.3（补 `max-page-size`） | 是 | `0xe0000` | `0x0` ✅ **运气** |
| 28.2（driver 默认） | 不需要 | `0xe1000` | `0x1000` ❌ |

而只要给 `.data.rel.ro` 加 4 KB，NDK 26 那个 ✅ 立刻翻成 ❌。**所以「NDK 26 能过」
是 1/4 的巧合，不是它更合规。** 加上本模板的 flag 后，两个 NDK 都稳定通过：

| | LOAD | RELRO end | `mod 0x4000` |
|---|---|---|---|
| NDK 26 + 本模板 flag | `0x4000` ×4 | `0xe0000` | `0x0` ✅ |
| NDK 28 + 本模板 flag | `0x4000` ×4 | `0xe4000` | `0x0` ✅ |
| x86_64 + 本模板 flag | `0x4000` ×4 | `0xf0000` | `0x0` ✅ |

与 NDK 版本**无关**了。

> ⚠️ **环境变量 `RUSTFLAGS` 会覆盖 `.cargo/config.toml` 全部内容**
> （cargo 语义：设了 `RUSTFLAGS` 就忽略 `target.*.rustflags`），且**没有任何提示**。
> 排查 16 KB 问题前先确认 `echo $RUSTFLAGS` 是空的。

### 一键检查

```powershell
.\gradlew assembleRelease -PndkVersion=28.2.13676358
pwsh check-16kb.ps1 -NdkVersion 28.2.13676358 `
     -Apk build\outputs\apk\release\<你的>.apk
```

它查**两条**判据（官方脚本只查一条），非零退出码表示有不合规项。

### 仍然欠着的

1. **16 KB 环境实测** —— 静态全绿 ≠ 真机不炸。需要 Android 15 的
   "Google APIs Experimental 16 KB Page Size ARM64 v8a" 模拟器镜像或 16 KB 真机：
   ```bash
   adb shell getconf PAGE_SIZE     # 期望 16384
   ```
2. **AAB** —— 本模板只出 APK。若改发 `.aab`，bundletool **不会**自动 zipalign：
   ```bash
   bundletool dump config --bundle=app-release.aab | Select-String alignment
   #   期望 PAGE_ALIGNMENT_16K；PAGE_ALIGNMENT_4K 会导致 Play 侧安装失败
   ```

`resources.arsc` / `classes.dex` **不需要** 16 KB 对齐（官方未要求，
`zipalign -P 16` 也只对 `.so` 做页对齐）。

### 顺带：NDK 版本本身仍建议 28+

与 16 KB 无关的三条理由：

1. **r26 及更低的 `libc++_shared.so` 是 4 KB 对齐的真实地雷** —— 一旦给项目加
   `cc`/`cxx` 依赖就会打进一个 4 KB 的 `.so`。本模板**没链** C++ 运行库
   （`NEEDED` 只有 `liblog/libdl/libandroid/libc`），侥幸避开。
2. r28 起不再定义 `PAGE_SIZE` 宏 —— 纯 Rust 天然免疫，加了 C 依赖会提前编译失败。
3. 官方推荐路径，future-proof。

---
## 6. 常见问题

| 现象 | 原因 / 解法 |
|---|---|
| `Unsupported class file major version` | JDK < 17，改 `JAVA_HOME` |
| `failed to find tool "aarch64-linux-android-clang++"` | 没用 `cargo ndk`。它会自己填 `CC`/`CXX`；手填 `CC` 而漏 `CXX` 就是这个报错 |
| 交叉链接失败 | 用了 `cargo ndk build` 而漏了 `--lib`，bin target 是桌面入口 |
| `INSTALL_FAILED_VERSION_DOWNGRADE` | 装过旧签名/更高 versionCode 的包，先 `adb uninstall` |
| `INSTALL_FAILED_UPDATE_INCOMPATIBLE` | 同上，签名变了 |
| gradle 卡在一个空白 UI 窗口 | gradle daemon 在下载依赖，不是失败。等，或 `--console=plain` 绕过 |
| `UnsatisfiedLinkError` | `rustCrate` / `[lib] name` / `loadLibrary` 三处名字不一致 |
| APK 里 `.so` 没被剥离（体积大） | NDK strip 工具缺失。功能正常，只是 APK 大 |
| 键盘不弹 | 回看 §4.1 / §4.2 |
| 内容被 letterbox | 回看 §4.4，检查 `targetSdk` |
| `warning: struct/function ... is never used` | **正常，忽略**。见下 |

### 关于 `dead_code` 警告

`src/lib.rs` 顶部有 `#![allow(dead_code)]`。不加的话 rustc 会把
`android_main` 里用到的每个私有项都报成「未使用」—— 无论 `cargo check`
还是 `cargo build` 都报。

原因：这个 crate 的唯一入口是 `#[no_mangle] pub fn android_main`，
AGDK 在运行时**按符号名字符串**查找它。rustc 的 dead_code 分析看不到
「JNI 按字符串找符号」这层间接引用，于是判定不可达。代码在真机上确实在跑。

对纯 JNI cdylib 来说整个 crate 都「只被 JNI 到达」，所以整 crate 放宽是对的。
你自己往里加业务代码时如果想保留 dead_code 检查，把这条 allow 换成
逐个 `#[allow(dead_code)]` 即可。

---

## 7. 已知限制

- **emoji 不保证能渲染**。Android 字体白名单只收正文字体，`NotoColorEmoji`
  不在其中。cosmic-text 需要显式加载彩色位图字体，vireo 暂未做。
- **preedit（拼音候选）在部分 ROM 上拿不到**。搜狗小米版只 commit 不
  setComposingText，`text_input_state().compose` 恒为 `None`。能正常上屏汉字，
  只是拿不到候选串。
- **只处理了 `onPause` / `onResume`**。winit 的 Android 后端把 Activity 回调
  都注册了但**只发 `Resume`/`Start`**，所以 `onStop`、`onDestroy`、`onSaveInstanceState`、
  `onTrimMemory` 这些 vireo 都收不到 —— 它们只能由你的 `MainActivity.java` 转发，
  而 vireo 目前只为前两个绑了 native 方法。需要更多就照 `nativeOnPause` 的写法自己加。
- **16 KB page size 未保证**（NDK 版本限制，见 §5）。
- **R8/混淆默认关**。`minifyEnabled false`，原因见 `proguard-rules.pro`。
- 只验证了 **arm64-v8a + Vulkan**。32 位 ABI 和 GLES 后端未测。

---

## 8. 生命周期：`onPause` / `onResume` 转发（别删）

winit 的 Android 后端**注册了** Activity 生命周期回调，但除 `Resume`/`Start` 外
**全部不发事件** —— `MainEvent::Pause` / `Stop` 在 Android 侧永远不会被构造。
所以 vireo 默认**收不到「我进后台了」**，切后台后继续全速渲染，直到被系统杀进程。

`MainActivity.java` 里那两个 `native` 方法就是补这个洞的：vireo 在运行时用
`RegisterNatives` 把它们绑到 Rust 侧，于是有了真正的暂停信号。

```java
private static native void nativeOnPause();
private static native void nativeOnResume();

@Override protected void onPause()  { nativeOnPause();  super.onPause(); }
@Override protected void onResume() { super.onResume(); nativeOnResume(); }
```

Rust 侧能拿到的东西：

```rust
app.is_paused();                 // 任意线程可读；桌面恒 false
app.on_paused(|| save_progress());  // 渲染线程上、恢复之后触发
app.on_resumed(|| resume_audio());
```

**`paused` 期间整条渲染 Thread 完全停摆** —— 不跑 `on_tick`、不 `draw`、
不 acquire/present。所以 `on_tick` 里的代码照常写，不用自己判 `is_paused()`。

三点注意：

1. **回调在「恢复时」才触发**，因为回调要能碰 `App`，不能在 UI 线程的 JNI 回调里跑。
   所以「暂停瞬间要做的紧急收尾」请写在 `on_tick` 里判 `is_paused()`。
2. **`onPause` 对短暂中断也触发**（锁屏、部分弹窗、分屏切换）—— 和 Unity
   `OnApplicationPause` 同样的语义。
3. **没照抄这段也能跑**，只是退化成旧行为（后台仍全速）+ logcat 一条 warn。

---

## 9. vireo 自身的已知 Android 缺口

模板之外，vireo 库还有这些（完整清单见 [`docs/android-audit.md`](../../docs/android-audit.md)）：

- **emoji 不保证渲染**：Android 字体白名单只收正文字体，`NotoColorEmoji` 不在其中。
  cosmic-text 需要显式加载彩色位图字体，vireo 暂未做。
- **`cutouts()` 部分 ROM 恒空**：运行时 `set_display_cutout_mode` 是空操作，
  只能靠 manifest/style（见 §4.4）。
- **16 KB 未保证**：见 §5。

## 10. 相关文档

- `vireo::platform::android` 的 API doc（`src/platform/android.rs` 顶部）
- `vireo::platform::android_lifecycle` 的模块文档（生命周期桥的设计取舍）
- [`docs/android-audit.md`](../../docs/android-audit.md) —— Android 适配完整性审计
- `.opencode/android_ime_verification.md` —— IME 真机验证全过程与 logcat 证据