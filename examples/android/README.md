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
    ├── assets/               # 随包只读资源（图片/字体/关卡）。AGP 默认打进 APK
    │   ├── logo.png
    │   └── notes/hello.txt
    ├── java/com/example/vireoandroid/MainActivity.java
    └── res/values/themes.xml
```

> **为什么入口是 `src/lib.rs` 而不是 `src/main.rs`**：这是 `crate-type = ["cdylib"]`
> 的 crate。Android 侧由 AGSK 按符号名查找 `android_main`，不需要 Rust 的 `fn main`，
> 有 bin target 反而多一份要交叉链接的桌面入口（`cargo ndk build` 漏 `--lib` 时的
> 报错就来自它）。

> `src/main.rs`（cargo）和 `src/main/`（gradle 的 java/res 根）**共用一个目录**，
> 这是 gradle Android 插件的默认布局，不是笔误。

### ⚠️ 资源读不到？先分清「三个世界」

安卓的「文件」有三个互不相通的地方，这是最常见的坑：

| 你要访问的东西 | 它在哪 | 怎么读 |
|---|---|---|
| 随包资源（图片/字体/关卡） | **不是路径**，是 APK 这个 zip 里的一项 | `app.assets().read("logo.png")` |
| 内部私有目录 | `/data/user/0/<包名>/files` | `app.data_dir()` |
| 外部 app 专属目录 | `/storage/emulated/0/Android/data/<包名>/files` | `app.external_data_dir()` |

```rust
// ✅ 随包资源：安卓读 APK 内的 assets/，桌面读 set_asset_root 的目录
app.set_asset_root("assets");            // 桌面上必需；安卓这行是 no-op
let tex = app.load_texture_asset("logo.png");

// ❌ 在安卓上必然失败：资源不在文件系统上
let tex = app.load_texture("logo.png");  // 等价于 std::fs::read("logo.png")
```

vireo **刻意不做**「相对路径自动回退到 assets」：那会让同一份代码在两个平台上资源
来源不同且不报错 —— 桌面调通了、安卓静默换一张图。所以上面那两行是**两个不同的 API**。

三个可写目录在安卓上**都免权限**，也都**随卸载删除**。要让用户自己选存储位置
（且卸载后文件还在），走 SAF —— **已实现**，见下。

> ⚠️ **`assets.list()` 在安卓上只列文件、不列子目录** —— 底层 `AAssetDir` 就是这个
> 语义，没有「有哪些子目录」的概念。真机实测：`assets/` 里放着 `logo.png` 和
> `notes/hello.txt`，`list("")` 返回 `["logo.png"]`，`notes` 不出现（尽管
> `read("notes/hello.txt")` 能读到）。**所以不能靠 `list` 遍历资源树**，
> 要遍历就自己维护一份资源清单。

#### SAF：让用户自己选位置（已实现）

```rust
// 三个入口全异步：立即返回，系统选择器出现，用户操作后调回调（渲染线程）
app.open_document_tree(move |res| { /* Ok(SafUri) / Err(UserCancelled) */ });
app.open_document(&["image/*"], move |res| { /* ... */ });
app.create_document("chart.json", move |res| { /* ... */ });

// SafUri：URI 字符串可持久化（自己存，下次 from_uri_string 复用）+
// open_reader/open_writer 开出真正 std::fs::File +
// 目录 list/resolve/create_*/delete + persisted/release
let mut buf = Vec::new();
std::io::Read::read_to_end(&mut uri.open_reader()?, &mut buf)?;
```

要点：

- SAF 返回的是 `content://` URI，**不是路径** —— 塞不进任何 `PathBuf` 参数，
  `std::fs` 也读不了它。必须经 `SafUri` 中转。
- 你的 `MainActivity.java` 必须含 SAF 桥（本模板已集成；独立可复制文件
  `examples/android/SAF_SNIPPET.java`，拷三块进自己的 Activity 即可）。
  缺了 launch 直接 `Err` 明说，不会静默无事发生。
- 收到 URI 当即自动持久化授权；`SafUri::persisted()` 列历史授权。
  卸载后授权清零（系统行为），但文件本身还在。
- 同一时刻只允许一个选择器在途；取消走 `Err(UserCancelled)`，不是故障。
- 本模板跑起来就有演示：HUD 上三按钮（`SAF:tree` / `SAF:open` / `SAF:save`），
  首跑还会自动弹一次目录选择（marker 落 cache，删掉可重演）。

#### 剪贴板（已实现）

```rust
// 免费函数（无 App 状态可注入，故不挂 App 上）
vireo::set_clipboard_text("关卡名")?;
let s: Option<String> = vireo::clipboard_text();  // None = 空剪贴板
vireo::set_clipboard_html("<p>hi</p>", "hi")?;
let h: Option<String> = vireo::clipboard_html();  // 有纯文本无 HTML 时为 None，不降级
```

- Android 侧纯 JNI：**无需 manifest 权限、无需改 Java**。API 33+ 读会弹系统
  "pasted" 提示（OS 行为）。
- 本模板 HUD 上两按钮（`CB:copy` 写文本+HTML / `CB:paste` 读回显示）。

#### 振动（已实现）

```rust
// 免费函数（同剪贴板，无状态可注入，不挂 App）
if vireo::has_vibrator() {
    vireo::vibrate_once(60, None)?;                       // 单次，默认强度
    vireo::vibrate_predefined(vireo::VibrateKind::Tick)?; // 系统预置触感
    vireo::vibrate_pattern(&[0, 80, 60, 80], Some(&[0, 200, 0, 200]), None)?;
}
vireo::cancel_vibration()?;  // 停掉当前振动
```

- 模板 manifest 已声明 `VIBRATE` 权限（普通权限，安装即授）。
- 本模板 HUD 上 `VB:test` 按钮跑全套记报告。MuMu 实测报有马达（全 Ok）；
  无马达的设备上预期 `has=false` + 三个 `ERR(no vibrator)` + `cancel ok`
  （验证不崩即可）；真震感只能物理手机。

#### APK 里 assets 的压缩存法（实测）

APK 是 zip，每一项要么 STORED（不压缩）要么 DEFLATE（压缩）。这决定能不能走
mmap 直读（`Asset::open_file_descriptor()`，压缩项会失败）。实测本模板的产物：

| APK 内条目 | 原始 | 压缩后 | 存法 |
|---|---|---|---|
| `assets/logo.png` | 40716 B | **40716 B** | **STORED（未压缩）** |
| `assets/notes/hello.txt` | 327 B | 272 B | DEFLATE（压缩） |

**AGP 默认把 `.png` / `.jpg` / `.wav` 等媒体类型存成未压缩**（为了 mmap 读取
性能），文本类才压。所以：

- **贴图**放 assets 里，`open_file_descriptor()` 零拷贝读取**本来就可用**，
  不需要配 `noCompress`。
- **字体 / 文本 / 自定义格式**会被压缩 —— 只能走 `read`，不能走 mmap。
- vireo 的 [`Assets::read`](../../src/asset.rs) 一律用 `read`（内部按需解压），
  **两种存法都能读**，所以对打包方式无要求。真要为大字体换 mmap 零拷贝，
  才需要在 `build.gradle` 加 `androidResources { noCompress += ['ttf'] }`。

验证方式（不需要真机）：

```powershell
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($apk)
$zip.Entries | ? { $_.FullName -like 'assets/*' } |
  % { "{0} {1} -> {2}" -f $_.FullName, $_.Length, $_.CompressedLength }
$zip.Dispose()
```

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
>
> ⚠️ **PowerShell 里 `-PndkVersion=` 必须加引号**：
>
> ```powershell
> .\gradlew.bat assembleDebug '-PndkVersion=28.2.13676358'   # ✅
> .\gradlew.bat assembleDebug -PndkVersion=28.2.13676358     # ❌ 被 cmd 截断
> ```
>
> 不加引号时 cmd 会在 `=` 后**首个 `.`** 处把参数切开，拆成
> `-PndkVersion=28` + 任务名 `.2.13676358`。症状**不是**「找不到 NDK」，而是
> gradle 在配置阶段报一堆看似无关的 AGP 错误
> （`Android Gradle Plugin has been applied at the root build file … specify compileSdk`），
> 跟 `build.gradle` 里的 `compileSdk = 35` 一点关系都没有 —— 很容易照着去改
> SDK 版本，改半天。bash / zsh 不受此影响，不用加引号。

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

## 4. 八个必须知道的坑

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

### 4.3 ⚠️ 横屏 extract mode：顶部全宽白条

**症状**：横屏弹出键盘时，屏幕顶部一条**全宽白条** + 一个按钮（「执行」或
「完成」）；`ime_insets.bottom` 恒为 0，`adjustResize` 形同虚设，
可见区只剩顶部约 128 物理像素。白条会在没有正在组合的拼音时自行消失。

**可观测特征**：那个按钮的文案是**框架字符串**
（`com.android.internal.R.string.ime_action_default`）—— 既不是 MIUI 画的，
也不是输入法画的。这是判断「白条属于 extract mode 而非输入法候选栏」的依据。

**原因**：输入法进入 AOSP **fullscreen extract mode**，把 IME 窗口设成
`MATCH_PARENT` 吃掉整屏。框架注释直言 *"In fullscreen mode, we never resize
the underlying window"*。

**正解**：vireo 内部固定发三个 flag：

```
IME_FLAG_NO_FULLSCREEN      | IME_FLAG_NO_EXTRACT_UI | IME_FLAG_NO_ACCESSORY_ACTION
0x0200_0000                | 0x1000_0000           | 0x2000_0000
```

⚠️ **`IME_FLAG_NO_EXTRACT_UI` 不是冗余**。`IME_FLAG_NO_FULLSCREEN` 的 AOSP 文档
明确写着「不是保证，某些输入法可能忽略」—— 实测 **HyperOS 搜狗横屏就是无视它**。
两者语义不等价：

| flag | 语义 |
|---|---|
| `NO_FULLSCREEN` | 「别进全屏模式」—— 被无视时毫无作用 |
| `NO_EXTRACT_UI` | 「别显示 extracted-text UI」—— 直接命中那块白条 |

**别自己设 `ImeOptions`**：AGSK 对 `imeOptions` 是**整体替换**而非按位或，
手写会把 vireo 固定下发的位抹掉。用 vireo 的
`set_input_type(InputType::Text)` 即可。

### 4.4 ⚠️ `targetSdk >= 35` 不是可选项

`windowLayoutInDisplayCutoutMode`（刘海/挖孔）在 `targetSdk < 35` 时被系统**忽略**，
内容被 letterbox 到安全区。真机对比：旧配置 2264×1080，新配置 2392×1080 满宽。

运行时调 `set_display_cutout_mode()` 在部分 ROM 上是**空操作**（返回的
`cutouts()` 恒为空），所以只能靠 manifest/style。

### 4.5 ⚠️ 首帧别急着弹键盘

刚 `onCreate` 就 `show_soft_input()` 在部分 ROM 上会被吞。每帧检查焦点、
需要时重复调（vireo 的模板示例就是这么做的）。

> **注意「每帧重复调」不等于「重复 post」**：vireo 内部按「请求值是否变化」
> 去重，同状态重复调用**不会**真的 post 到 Java 主线程。所以每帧调是安全的。
> 反过来，**引擎绝不会周期性重发** —— 输入法每次收到 `show` 都会重置组合态，
> 重复 post 会让候选词 / preedit 一闪即消、且用户按不掉收起键。
> 详见 [`src/platform/android_text_input.rs`](../../src/platform/android_text_input.rs)
> 的 `set_ime_wanted` 文档。

### 4.6 ⚠️ 键盘坐标换算被做了两次

**症状**：点输入框键盘不弹；`adb shell input tap` 打哪都没反应。

**原因**：`WindowEvent::Touch` 的坐标在 winit 层已按 scale 换算成逻辑像素，
若你自己再除一次 scale，点位会被缩掉 2.8 倍（HyperOS scale=2.8125），
所有命中判断都失效。

**正解**：用 vireo 提供的坐标，它们已统一：

| API | 坐标空间 |
|---|---|
| `mouse_pos()` | [`PixelPos`] 双表示（`.physical()` / `.logical()`） |
| `on_touch` 回调的 [`TouchEvent`] | 逻辑像素 `f32` |
| `touches()` / `touch_first()` 的 [`TouchPoint`] | [`PixelPos`] 双表示 |

[`TouchPoint`] 的 `pos` 由**逻辑**值现算 `.px`（`dp_of`，`dpi.rs`）—— 不是
`to_pixel_pos`。这两个方向相反，用错会再除一次 scale，是本项目反复出现的一类 bug
（见 `input.rs` 里 `touch_logical_to_mouse_pos` 的历史注释）。

### 4.6.1 ⚠️ 触摸**不会**模拟鼠标按键

`mouse_left()` / `mouse_down()` 在纯触摸设备上**恒为 `false`** —— Android 上点击
只走 `WindowEvent::Touch`，没有 `CursorMoved` / 鼠标按键事件；vireo 只把触摸**位置**
写进 `mouse_pos`，**不写按键状态**。判断「手指是否按下」只能用：

- **轮询**：`win.touch_count()` / `win.touches()` / `win.touch_first()`
- **回调**：`app.on_touch(idx, ..)`（在 winit 线程派发）

⚠️ **轮询是「电平」不是「边沿」**：手指按住期间每帧都在结果里。按钮这类
「按一下触发一次」必须自己按 `id` 差分，否则会每帧重复触发（本例按钮曾
true→false→true 乱跳）。见 `touches()` 的文档示例。

### 4.6.2 ⚠️ `on_*` 回调**可以在 `app.run(..)` 之前**注册

不需要挪进 run、也不需要标志位防重复注册。回调注册只依赖 `WindowIndex`（往
「handle 槽」追加闭包），与窗口对象何时创建无关。

早期版本这里必须挪进 run —— 因为 host drain `cb_rx` 时若槽还没建会 `get_mut`
返回 `None` 而**静默丢弃注册**。根因是 `App::window()` 走 `create_tx`、
`on_*` 走 `cb_tx`，**两个独立 channel 跨 channel 无顺序保证**，于是一轮
`about_to_wait` 里可能出现「create 请求还没到（槽 len=0）但 cb 已收到」的窗口。
Android 的跨线程入口（用户 main 与 winit 分属不同线程）下**必现**。现已改为
drain 时按 handle 提前建槽（`host::ensure_callback_slot`）。

### 4.6.3 ⚠️ `App::window()` 之后 `window_ref()` 立即调用返回 `Err`

`app.window(desc)` 只是把 `WindowDesc` 发给 winit 线程**异步**创建
（DX12 的 `surface.configure` 会等 present queue 排空，不能放在 winit 线程同步做）。
`VireoWindow` 对象要等建完才存在。所以：

```rust
let idx = app.window(desc, None);
// ❌ Err：窗口还在队列里
if let Ok(win) = app.window_ref(&idx) { /* ... */ }
// ✅ 在 app.run(..) 的闭包里取（首帧可能仍为 Err，用 return true 等下一帧）
```

### 4.7 ⚠️ PowerShell 调 `.bat` 会截断含 `.` 的参数

```powershell
.\gradlew.bat assembleRelease -PndkVersion=28.2.13676358   # ❌ 被拆成
                             #    -PndkVersion=28  +  任务名 .2.13676358
.\gradlew.bat assembleRelease '-PndkVersion=28.2.13676358'  # ✅ 加引号
```

不加引号时会报「找不到 NDK: `sdk/ndk/28`」—— 这个报错**极具误导性**，
真实原因是参数没传对。

### 4.8 ⚠️ `UnsatisfiedLinkError: cannot locate symbol "android_main"`

**症状**：APK 装得上、一点就崩，logcat 只有一行 JNI 错误。

**原因**：`lib<rustCrate>.so` 里没有 `android_main` 这个导出符号。它由
`#[vireo::main]` 展开产生，正常情况下一定存在。查：

```bash
"$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/windows-x86_64/bin/llvm-nm.exe" \
    -D --defined-only src/main/jniLibs/arm64-v8a/lib<rustCrate>.so | Select-String android_main
```

期望看到 `T android_main`。若只看到 `U android_main`（**未定义**），
说明宏没被编译进来 —— 检查 vireo 的 `Cargo.toml` 里 `vireo_macro` 是不是
写成了版本号（从 crates.io 拉，仓库里那份根本不参与构建）而不是 path 依赖。

**判据速查**：`llvm-nm` 输出里 `T` = 已定义的代码段，`U` = 未定义（引用别人）。
`U android_main` 说明 AGSK 胶水在找它，但 `.so` 里没有。

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
| `UnsatisfiedLinkError` | `rustCrate` / `[lib] name` / `loadLibrary` 三处名字不一致，**或**缺 `android_main` 导出（见 §4.8）|
| APK 里 `.so` 没被剥离（体积大） | NDK strip 工具缺失。功能正常，只是 APK 大 |
| 键盘不弹 | 回看 §4.1 / §4.2，再看 §4.6（坐标可能被换算了两次）|
| 内容被 letterbox | 回看 §4.4，检查 `targetSdk` |
| `warning: struct/function ... is never used` | **正常，忽略**。见下 |
| gradle 报 ``Android Gradle Plugin has been applied at the root build file ... specify `compileSdk` `` | ⚠️ **这条报错是误导性的，别照着查 `compileSdk`** —— `build.gradle` 里明明写了 `compileSdk = 35`。它是**级联次生错误**：真正的原因在它上面，被 gradle 截断了。往上翻找 `Build file ... line: NN` 那条，本模板最常见的是 `build.gradle:73` 的「传了 `-PndkVersion=` 但没有 `ANDROID_HOME` / `ANDROID_SDK_ROOT`」。那条中文消息在 GBK 控制台里会**显示成乱码**，认不出就查环境变量 |


### gradle 报 `compileSdk` 但 build.gradle 里明明写了 —— 怎么快速定位

这是本模板最容易浪费时间的一个坑：**次生报错指向了完全无关的方向**。

```text
Build file '.../build.gradle' line: 73          ← 真正的错误在这一行
* What went wrong:
> 传了 -PndkVersion=... 但没有 ANDROID_HOME / ANDROID_SDK_ROOT 环境变量 ...   ← 乱码
2: Task failed with an exception.
* What went wrong:
> Android Gradle Plugin has been applied at the root build file. ...
  To continue with the plugin in the resolved and applied state, specify `compileSdk`   ← 假线索
```

判据：**看第一条 `What went wrong`，不要看最后一条**。gradle 在根项目配置失败后，
会再跑一遍 AGP 的收尾检查并抛出它的通用提示，那条与真实原因无关。

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

- ~~**emoji 不保证能渲染**~~ ✅ **已修复**：Android 12+ 的 `NotoColorEmoji.ttf` 是
  **COLRv1**，而 swash 0.2.10 只会读 COLR **v0** 布局（swash 已是末版，上游 issue 五年
  零活动），于是 emoji 交回 `0×0` 被静默丢弃。vireo 内已用 `skrifa` + `zeno`（两者本来
  就在依赖树里，零新增 crate）补上 COLRv1 光栅化，`NotoColorEmoji` 也已进白名单。
  真机验证 `🦊🎵😀🇨🇳👨‍👩‍👧👍🏽` 全部渲染，保持 Android 原生风格。详见
  [`.opencode/android_colrv1_plan.md`](../../.opencode/android_colrv1_plan.md)。
  残留缺口：负半径径向渐变的零半径截断重插值（规范要求但无字体使用）、可变彩色
  字体未做真字体验证。
- **preedit（拼音候选）在部分 ROM 上拿不到**。搜狗小米版只 commit 不
  setComposingText，`text_input_state().compose` 恒为 `None`。能正常上屏汉字，
  只是拿不到候选串。
- **只处理了 `onPause` / `onResume` / `onStop`**。winit 的 Android 后端把 Activity 回调
  都注册了但**只发 `Resume`/`Start`**，所以 `onDestroy`、`onSaveInstanceState`、
  `onTrimMemory` 这些 vireo 都收不到 —— 它们只能由你的 `MainActivity.java` 转发，
  而 vireo 只为前三个绑了 native 方法（`onStop` 对应 `App::on_stopped`，见 §8）。
  需要更多就照 `nativeOnPause` 的写法自己加。
  （`onTrimMemory` 走的是另一条路：winit 把它转成了跨平台 `MemoryWarning`，
  对应 `App::on_memory_warning`，零 JNI。）
- **`on_insets_changed` 没做**：AndroidX 的 insets 变化通知不带值、且已被 winit 消费，
  自做 JNI 回调需要动态代理。Godot / Unity 也都是纯 poll，`safe_area()` /
  `padding()` / `ime_insets()` 逐帧读已经够用。
- **16 KB page size 静态合规但运行时未实测**（两台 MuMu 的 `PAGE_SIZE` 都是 4096，
  需要 16 KB 镜像或真机，见 §5）。
- **R8/混淆默认关**。`minifyEnabled false`，原因见 `proguard-rules.pro`。
- 只验证了 **arm64-v8a + x86_64 + Vulkan**。32 位 ABI 和 GLES 后端未测。
- **`cutouts()` 部分 ROM 恒空**：运行时 `set_display_cutout_mode` 是空操作，
  只能靠 manifest/style（见 §4.4）。

---

## 8. 生命周期：`onPause` / `onResume` / `onStop` 转发（别删）

winit 的 Android 后端**注册了** Activity 生命周期回调，但除 `Resume`/`Start` 外
**全部不发事件** —— `MainEvent::Pause` / `Stop` 在 Android 侧永远不会被构造。
所以 vireo 默认**收不到「我进后台了」**，切后台后继续全速渲染，直到被系统杀进程。

`MainActivity.java` 里那几个 `native` 方法就是补这个洞的：vireo 在运行时用
`RegisterNatives` 把它们绑到 Rust 侧，于是有了真正的暂停信号。

```java
private static native void nativeOnPause();
private static native void nativeOnResume();
private static native void nativeOnStop();      // 可选，见下
private static native void nativeOnDestroy();   // 可选，见下

@Override protected void onPause()  { nativeOnPause();  super.onPause(); }
@Override protected void onResume() { super.onResume(); nativeOnResume(); }
@Override protected void onStop()    { nativeOnStop();    super.onStop(); }
@Override protected void onDestroy() { nativeOnDestroy(); super.onDestroy(); }
```

Rust 侧能拿到的东西：

```rust
app.is_paused();                 // 任意线程可读；桌面恒 false
app.on_paused(|| save_progress());  // 渲染线程上、恢复之后触发
app.on_resumed(|| resume_audio());

app.is_stopped();                // activity 当前是否不可见；桌面恒 false
app.on_stopped(|| mark_really_left());  // 只在「本次暂停含 onStop」时触发

app.is_destroyed();              // 用户主动关闭（划掉最近任务）；桌面恒 false
app.on_destroyed(|| quit_now()); // 只在「用户主动关闭」时触发
```

**默认 `paused` 期间整条渲染 Thread 完全停摆** —— 不跑 `on_tick`、不 `draw`、
不 acquire/present。所以 `on_tick` 里的代码照常写，不用自己判 `is_paused()`。

### 想在后台继续跑逻辑：`set_auto_pause(false)`

默认 vireo 替你暂停。关掉后循环照常跑，**由你决定**做什么：

```rust
app.set_auto_pause(false);   // 默认 true，懒得写的人零改动
app.run(|ctx| {
    update_logic();                  // 后台也跑
    if !ctx.app().is_paused() {      // 不可见时不画
        win.draw(WHITE, &[&batch]);
    }
    if ctx.app().is_destroyed() {    // 用户划掉任务 → 退出
        return false;
    }
    true
});
```

关掉闸门后有两个必须知道的代价：

1. **后台全速空转**。「后台 CPU 占用高」正是 Android 杀进程的判定依据之一，
   结果是后台存活时间反而**更短**。上面判 `is_paused()` 不画是必须的。
2. **长时间后台需要前台服务**（Foreground Service + 常驻通知），否则系统迟早
   回收进程。vireo 管不了这一层。

### 「暂停直到」：自己选时机

```rust
// 等前台。会被窗口关闭/设备丢失/activity 销毁打断，不会挂死
app.wait_while_paused();

// 通用条件。平台生命周期变化会自动唤醒；你自己的条件变了要 notify
if !app.wait_until(Some(Duration::from_secs(5)), || flag.load(Relaxed)) {
    log::warn!("timeout");
}
app.notify_waiters();   // 自定义条件变化后叫醒等待者
```

> ⚠️ `wait_until` 的唯一坑：**你自己的条件变了却忘了 `notify_waiters()`**，
> 等待方只能等到超时。平台状态（切后台/恢复/销毁）不需要你 notify。

### `onDestroy`：唯一能回答「用户是切走了还是把我关了」

| 场景 | `is_paused` | `is_stopped` | `is_destroyed` |
|---|---|---|---|
| 回桌面 / 切 app | ✅ | ✅ | ❌ |
| 锁屏 / 息屏 | ✅ | ✅ | ❌ |
| 半透明弹窗 / 权限对话框 | ✅ | ❌ | ❌ |
| **划掉最近任务 / `finish()`** | ✅ | ✅ | **✅** |
| 系统回收内存杀进程 | ✅ | ✅ | ❌ |

⚠️ **进程被杀时不触发**（Android 措辞是 "might be called"）。所以
`is_destroyed()` **不能**用来做资源清理或存档 —— 那要在 `on_paused` 之类更早的
时机做。它只回答「用户是不是明确要把你关掉」。

这也是 `set_auto_pause(false)` 之后**唯一的退出信号**：`paused` / `stopped` 在
划掉任务时都会正常来（应用只是不可见了），只有 `destroyed` 告诉你「不用再跑了」。

### `onStop` 用来区分「短暂中断」与「彻底离开」

`onPause` 覆盖大量**短暂**中断，`onStop` 才对应「不可见」：

| 场景 | `onPause` | `onStop` |
|---|---|---|
| 半透明弹窗 / 权限对话框盖在上面 | ✅ | ❌ |
| 回桌面 / 切别的 app | ✅ | ✅ |
| 锁屏 | ✅ | ✅ |

> ⚠️ **别拿 `is_stopped()` 判断「刚才那次暂停算不算彻底离开」** —— 它是**实时**
> 查询，恢复后已经翻回 `false`（`onStart` 紧跟 `onResume` 跑完，渲染线程才被唤醒）。
> 要判断就用 `on_stopped` 回调。

`nativeOnStop` / `nativeOnDestroy` 是**可选**的：不声明 / 不调用只是对应的
`on_stopped` / `on_destroyed` 与 `is_destroyed()` 恒为 `false`，其它生命周期行为
不受影响。

两点注意：

1. **回调在「恢复时」才触发**。暂停信号来自 UI 线程，而回调要能碰 `App` /
   `DrawBatch`，所以不能直接在 JNI 回调里跑；但 Android 的 `onStart` 紧跟
   `onResume`，都在渲染线程被唤醒之前跑完 —— 等到能安全跑用户代码时，
   「这次暂停算不算彻底离开」才刚被确定。所以本回调必然是恢复后补发。
2. **没照抄这段也能跑**，只是退化成旧行为（后台仍全速）+ logcat 一条 warn。
3. ⚠️ **它不能替代 `onSaveInstanceState`**：正因为回调在恢复后才发，
   **进程在后台被系统杀掉时，vireo 侧一个回调都不会触发**。要保游戏状态，
   在你自己的 `MainActivity.java` 里 override `onSaveInstanceState(Bundle)`
   存起来（见下）。

### 实机自测

```bash
adb shell input keyevent 3        # HOME 回桌面
adb shell am start -n com.example.vireoandroid/.MainActivity
adb logcat -d | findstr lifecycle # 应有 paused 与 stopped 各一条
```

`destroyed` 需要**手动从最近任务划掉卡片**（回桌面不触发它）—— adb 没有直接的
「划掉卡片」命令，`am force-stop` 也不是 `onDestroy` 路径（它直接杀进程）。

模板 HUD 上的几行用于肉眼核对：

```
life  paused=1 stopped=1 destroyed=0
gate  auto=off bg=1245 auto_frames=0
wait  paused_wait=1 until_ok=1
```

- `bg` 在 HOME 后台期间持续增长 → 闸门确实关掉了。切到 `auto=ON` 再按 HOME，
  **`bg` 应立刻停止增长**（默认 `auto_pause=true` 时恒 0）
- `auto_frames` 是 `auto_pause=true` 时的帧数，用来确认默认路径没回归
- `paused` 与 `stopped` 同步递增 → 每轮暂停都含 `onStop`（锁屏/回桌面都算）
- `destroyed` 只在划掉任务后 +1
- `until_ok` 是 `wait_until` 的往返成功次数

> ⚠️ **切换按钮用点击而非按键**：示例常驻 `InputType::Text`（软键盘），Android 会把
> 硬件按键喂给 `InputConnection`（logcat 里是 `gti.InputConnection: onKey`），
> winit 的 `KeyboardInput` 收不到 —— 按键在真机上不可靠。
>
> **MuMu 上按钮交互可以自动验证**，但要用 `input motionevent` 而不是 `input tap`：
>
> ```powershell
> adb shell input motionevent DOWN <x> <y>   # ⚠️ input tap 注入的坐标是 (0,0)
> adb shell input motionevent UP   <x> <y>   #    → 命中判断必然落空
> ```
>
> `input tap` 在 MuMu 上**事件能到达**（`TouchPhase::Started` 会触发），只是坐标
> 丢失成 `(0,0)` —— 早期我误判成「注入事件到不了应用」，实际是坐标问题。
> 用 `motionevent` 传真实坐标即可（屏幕 3840x2160、app 逻辑 1138x640、
> scale 3.375；按钮在逻辑 `x 40..340 / y (h-120)..(h-120+64)`）。

### 想在 logcat 里看生命周期事件

`init_logger()` 走 env_logger，**默认 level 是 error**，而 vireo 的诊断日志都在
info/warn 级 —— 不设 `RUST_LOG` 的话 logcat 里一片空白。模板里演示了：

```rust
std::env::set_var("RUST_LOG", "info");   // 必须在 init_logger() 之前
vireo::init_logger();
```

（`env_logger` 初始化时只读一次。正式项目建议按 `BuildConfig.DEBUG` 开关，
别把 info 留在 release。）

### 保状态：只能用户自己在 Java 侧做

vireo 是**渲染库不是引擎** —— 你的谱面进度、分数、设置根本不在 vireo 里，
它手上只有 `Arc<wgpu::Texture>` / `Surface` / `Device` 这类句柄（存不了）和
每帧重建的 `DrawBatch`（存了也没意义）。所以：

```java
@Override
protected void onSaveInstanceState(Bundle outState) {
    nativeSaveGameState();              // 你自己的 native，照 nativeOnPause 的注册思路
    // 或者纯 Java 侧存：SharedPreferences / outState.putXxx(...)
    super.onSaveInstanceState(outState);
}
```

顺带说明：winit 其实**注册了** `onSaveInstanceState`，android-activity 也提供了
`StateSaver::store(&[u8])` 这个纯字节接口（**不需要** `Bundle` JNI 绑定），
但 winit 在 `platform_impl/android/mod.rs:276-280` 把事件 `warn!` 掉了 ——
所以 vireo 拿不到，这条路在当前依赖版本下走不通。

---

## 9. vireo 自身的已知 Android 缺口

模板之外，vireo 库还有这些（完整清单见 [`docs/android-audit.md`](../../docs/android-audit.md)）：

- ~~**emoji 不保证渲染**~~ ✅ **已修复**：Android 12+ 的 `NotoColorEmoji.ttf` 是
  **COLRv1**，而 swash 0.2.10 只会读 COLR **v0** 布局（swash 已是末版，上游 issue 五年
  零活动），于是 emoji 交回 `0×0` 被静默丢弃。vireo 内已用 `skrifa` + `zeno`（两者本来
  就在依赖树里，零新增 crate）补上 COLRv1 光栅化。详见
  [`.opencode/android_colrv1_plan.md`](../../.opencode/android_colrv1_plan.md)。
  注意 emoji 字体需要在 Android 字体白名单里（`src/text/android_fonts.rs` 已含
  `NotoColorEmoji`），否则 cosmic-text 根本排不到它。
- **`cutouts()` 部分 ROM 恒空**：运行时 `set_display_cutout_mode` 是空操作，
  只能靠 manifest/style（见 §4.4）。
  - **16 KB**：静态检查已过（`check-16kb.ps1`，双 ABI 全绿，NDK 28.2，
    见 §5）。设备实锤待定：手头两台模拟器都是 4K 页（`getconf PAGE_SIZE`），
    需 16K 物理设备或 Cuttlefish 才能验证运行时不炸。
  - ~~**SAF（系统文件选择器）未实现**~~ ✅ **已实现**：`App::open_document_tree` /
    `open_document` / `create_document` + `SafUri`（URI / fd 流式 API，见 §2
    的 SAF 节与 `TODO.md`）。模板 HUD 有演示按钮，`SAF_SNIPPET.java` 可抄走。

---

## 9.5 自定义系统字体扫描（`#[vireo::main(fonts = ..)]`）

vireo 在 Android 上按**文件名白名单**加载系统字体（只加载正文字体，不加载
`ComingSoon.ttf` / `AndroidClock.ttf` 那类装饰体 —— 挑中它们会导致「所有文字全大写、
数字消失」）。内置表覆盖 AOSP/Pixel、小米、华为、OPPO、通用 Noto/思源。

**没听说过的 ROM** 若正文字体命名不落在内置表里，用户原本只能自己 `adb push` 字体
再 `GpuContext::load_font_file` 手工加载，而那条路**钉不死 generic 家族**，救不了
那个症状。现在三张表都能在运行期追加：

```rust
#[vireo::main(fonts = |c| {
    // ① 额外扫描目录（app 私有目录 / /sdcard 上的用户字体）
    c.extra_dirs.push("/data/data/com.example.app/fonts".into());
    // ② 额外文件名前缀（与内置表取并集；扩展名门槛照旧）
    c.extra_prefixes.push("GameFont".to_owned());
    // ③ 额外优先家族名 —— **必须显式钉**，否则 generic 仍指向 fontdb 默认的
    //    "Arial"，Android 上无人声明它 → 回退到「排序后第一个 face」
    c.extra_families.push("Game Font".to_owned());
})]
async fn main(app: App) { /* .. */ }
```

| 字段 | 修什么 | 不设会怎样 |
|---|---|---|
| `extra_prefixes` | 字体**文件**不被扫描 | 字体根本没加载 |
| `extra_dirs` | 字体不在 `/system/fonts`、`/product/fonts` | 同上 |
| `extra_families` | 字体加载了但**家族名**不在优先表 | **症状照旧**：全大写 + 数字消失 |

> ⚠️ **`fonts = ..` 必须在 `App` 构造前求值**，宏已经保证了这点。
> 原因：`App::new()` 内部就建好了 `TextContext` 并跑完字体 install，而**用户 main
> 函数体是在那之后才被执行的** —— 所以宏属性是唯一时机。
> 手写 `App::new(...)`（不用宏）时，在它之前调
> `vireo::text::android_fonts::configure(|c| { .. })`；调用晚了**静默无效**。

> 家族名排在**内置表之前**：Roboto 在绝大多数设备上都存在，追加的规则永远轮不到。
> 代价是填错成装饰体家族名会抢走默认位置 —— 但那是显式要求，与
> `load_font_file` 后指定家族名同性质。

本模板的 `src/lib.rs` 里带了一段实测可跑的 `configure`（把 MuMu 上真实存在、
但不在内置表里的 `SourceSansPro` 设成默认字体）。看 HUD 底行的 `fonts` 行：

```
配置前： files=193 faces=201 failed=0 default=Roboto
配置后： files=199 faces=207 failed=0 default=Source Sans Pro
```

生效情况也会打进 `log::info!`，但**默认 log level 是 `error`**，`logcat` 里看不到 ——
排查字体问题看 HUD 的 `fonts` 行，那才是可靠通道。

---

## 10. 相关文档

- `vireo::platform::android` 的 API doc（`src/platform/android.rs` 顶部）
- `vireo::platform::android_lifecycle` 的模块文档（生命周期桥的设计取舍）
- [`docs/android-audit.md`](../../docs/android-audit.md) —— Android 适配完整性审计
- `.opencode/android_ime_verification.md` —— IME 真机验证全过程与 logcat 证据