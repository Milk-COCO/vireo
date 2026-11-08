# 平台支持与验证程度

> 面向 vireo 使用者。**本文件是平台状态的唯一来源**；README 只留 4 行摘要。
> 平台状态变化时同步这里 + `README.md` 的摘要表。

## 1. 一览

| 平台 | 后端 | 验证到什么程度 |
|---|---|---|
| **Windows** | DX12 / Vulkan | ✅ **日常使用 + `examples/` 全量跑过**：窗口、resize/DPI、IME、多窗口、MRT、compute、深度/模板 |
| **Android** | Vulkan（弱 GPU 自动回退 GLES） | ✅ **真机实测**：HyperOS 16 + Vulkan；API 35 模拟器强制 GLES 跑到 60fps。构建链与桌面不同，见 §3 |
| **Linux** | Vulkan | 🟡 **仅保证编译通过**，没有实机验证 |
| **macOS** | Metal | 🟡 **仅保证编译通过**，没有实机验证 |
| **Web / wasm** | — | ❌ **不支持**（代码里没有任何 wasm 目标支持） |

## 2. 每个平台的具体情况

### Windows

首选平台。DX12 下 `surface.configure` 会等 present 队列排空（几十毫秒级），所以拖动 resize
走「松手再重配 + 去抖」；拖动期间 acquire 可能失去 vsync 节流，`drag_cap` 默认开就是为此。
这些已在库里处理，用户侧照常用。

### Android

- **架构**：`winit` 的 `android-game-activity` 后端 + `AGDK GameActivity`（`AppCompatActivity`
  子类）。**这正是中文输入法能用的原因** —— `NativeActivity` 没有 `InputConnection`。
- **平台能力**（`vireo::platform::android`，仅该平台编译）：沉浸式 / 系统栏 bitmask、
  安全区三值（`safe_area` / `padding` / `ime_insets`）、挖孔（`cutouts()`）、文本输入
  （UTF-16↔UTF-8 下标换算 + `InputType` 6 档）、朝向锁定 / 常亮 / 亮度 / 窗口层级。
- **生命周期要自己接**：`on_paused` / `on_resumed` / `on_destroyed` 需要 `MainActivity.java`
  转发 `onPause` / `onResume`（winit 的 Android 后端不发这些事件，暂停期间整条渲染线程会停摆）。
  模板里已经接好。
- **已知问题**：
  - **MuMu12 模拟器实例**建不出 EGL window surface（`surface` 创建失败）。判断为该实例的
    EGL/驱动问题，不是 vireo 缺陷；用 API 35 官方模拟器或真机。
  - API 35 模拟器上 Vulkan 不可用时会强制走 GLES，帧率与 Vulkan 路径有差距（实测 60fps 量级）。
- **字体**：Android 侧不无差别扫 `/system/fonts`，而是按白名单（AOSP/Pixel 的 Roboto、
  Google CJK 的 NotoSans / NotoSerif / DroidSansFallback 等）加载，避免字体数据库过大；
  需要别的字体用 `load_font_file` / 资产字体显式加载。

### Linux / macOS

能编译、能链接，但**没有实机跑过**。macOS 上 surface 格式常见为 `Bgra8UnormSrgb`，
库里按真实 surface 格式同步文字管线；`HiddenTitlebar` 走系统红绿灯（不自绘）。
遇到问题按 bug 报，不保证及时。

## 3. Android 构建链（与桌面不同）

```bash
# 需要：Android SDK + NDK、cargo-ndk、gradle
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/28.2.13676358
cargo ndk -t arm64-v8a build --release --lib     # 产出 libvireoandroid.so
./gradlew assembleDebug                          # 打包 APK
```

**`xbuild` / `cargo-apk` 不适用**；必须是 `cargo ndk` + gradle 工程。
可直接复制的模板见 [`examples/android/`](../examples/android/README.md)，里面写了
NDK 版本、16 KB page size 合规、`androidx.games:games-activity` AAR、JNI 接线与踩坑。

## 4. 工具链

- **edition 2024** ⇒ 需要 **Rust ≥ 1.85**；具体依赖版本以仓库的 `Cargo.lock` 为准
  （`Cargo.toml` 里没有钉 `rust-version`，也不承诺 MSRV）。
- 依赖：`wgpu 30` / `winit 0.30` / `cosmic-text 0.19`。升级 wgpu 主版本属于 breaking
  （见 `VERSIONING.md`）。
- **没有 CI**：仓库里没有 GitHub Actions。质量门禁靠本地命令：
  `cargo fmt --check` / `cargo check --lib --examples --tests`（0 warning）/
  `cargo test --lib` / `cargo test --release --test gpu_bench -- --ignored --test-threads=1`。
- `cargo test`（不带 `--lib`）会跑到 `lib.rs` 里的 doc-test，而 **doc-test 会真的开窗口**，
  在无头/CI 环境会挂住 —— 这是已知缺陷，所以本地只跑 `cargo test --lib`。

## 5. 字体与文本

- **默认字体来自系统**：`FontSystem::new()` + `load_system_fonts()`。Windows / macOS / Linux
  用系统字体（中文一般开箱可用），Android 按上面的白名单。
- 自定义字体：`app.load_font(Source::path(..))` / `app.load_font(Source::asset(名字))`
  / `app.load_font(Source::bytes(&b))`（来源显式指定），再按 `Family::Name` 指定族名。
- 文字是**灰度（或 COLRv1 彩色）图集**光栅化，不是 SDF 字形；缩放窗口时按新 scale 重新
  光栅化（图集有缓存与上限）。
