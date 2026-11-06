[中文](README.md) | [English](README-EN.md)

# Vireo

**V**ault's **I**nterface and **R**endering **E**ngine for **O**ptics —— 一只可爱的小鸟。

基于 `wgpu` + `winit` 的 **Rust 2D 渲染库**：单 pass 多 batch、SDF 形状与描边、自绘文本
（**Android 中文输入法可用**），Windows 与 Android 都实机跑过。

它**不是**游戏引擎，也**不是** UI 框架 —— 没有 ECS、没有控件、没有布局系统。
它给你的是「这一帧怎么画」这一层：窗口、事件循环、GPU 资源、批处理合并、文字光栅化。
UI 长什么样、布局怎么算，是你自己的事。

[![crates.io](https://img.shields.io/crates/v/vireo.svg)](https://crates.io/crates/vireo)
[![docs.rs](https://docs.rs/vireo/badge.svg)](https://docs.rs/vireo)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可证)

<img src="https://raw.githubusercontent.com/Milk-COCO/vireo/master/logo.png" width="120" alt="Vireo logo">

> ⚠️ 本项目代码主要由 AI 生成、按作者自己的用例验收，**不承诺生产可用** ——
> 详见文末[项目状态与风险](#项目状态与风险)。

## 快速开始

```toml
[dependencies]
vireo = "0.1"     # crates.io 上是 0.1.2
# 想用本文档描述的 API（0.2.0 开发中）：
# vireo = { git = "https://github.com/Milk-COCO/vireo" }
```

```rust
use vireo::prelude::*;

#[vireo::main]
async fn main(app: App) {
    let idx = app.window(WindowDesc::new("EZ Vireo", 800, 600));

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return true, // 窗口已经关了 → 结束循环
        };

        let mut batch = DrawBatch::new();
        draw_rectangle(&mut batch, Pos::new(100.0, 100.0), 200.0, 150.0, Some(RED));
        draw_circle(&mut batch, Pos::new(500.0, 300.0), 80.0, Some(BLUE));
        draw_line(&mut batch, 50.0, 50.0, 750.0, 550.0, 2.0, Some(GREEN));
        draw_text(
            &mut batch.texts,
            "你好，Vireo",
            Pos::new(100.0, 40.0),
            TextDef::default().font_size(32.0),
            TextOverride::from_color(WHITE),
        );

        win.draw(BLACK, &[&batch]);
        true
    })
    .await
    .unwrap();
}
```

上面就是 [`examples/ez.rs`](https://github.com/Milk-COCO/vireo/blob/master/examples/ez.rs)
的逐字副本，跑起来能看到黑底 + 红矩形 + 蓝圆 + 一条绿线 + 一行中文。接着看：

- 文字与 HUD 分段 → `cargo run --example text_hud`
- 自定义 WGSL 材质 → `cargo run --example custom_material`
- Android 工程 → [`examples/android/`](https://github.com/Milk-COCO/vireo/blob/master/examples/android/README.md)

## 平台

| 平台 | 状态 |
|---|---|
| **Windows**（DX12 / Vulkan）| ✅ 日常使用 + 全部 example 跑过 |
| **Android**（Vulkan，弱 GPU 回退 GLES）| ✅ 真机实测（HyperOS 16）；构建链与桌面不同，需 `cargo ndk` + gradle |
| Linux（Vulkan）/ macOS（Metal）| 🟡 仅保证编译通过，未实机验证 |
| Web / wasm | ❌ 不支持 |

细节、验证程度与已知问题（模拟器兼容、字体来源、工具链）见
[`docs/platforms.md`](https://github.com/Milk-COCO/vireo/blob/master/docs/platforms.md)。

## 核心概念

- **即时模式**：每帧新建 `DrawBatch`，往里塞形状/文字，`win.draw(清屏色, &[批次...])` 一次提交。
  批次树可嵌套（子节点、继承、裁切）。
- **单 pass 多批次**：一次 `draw()` 内的多个批次按顺序画进同一个 pass —— 不需要为每个批次
  建 render pass，也不强制你攒顶点。
- **坐标系**：左上角原点，x 向右、y 向下，单位是 vireo 逻辑像素。尺寸/位置 API 收
  `impl ToPx`：裸数值与 `Dp` 是逻辑像素，`Px` 是物理像素，语义由调用点声明。
  `dpi_override(Some(d))` 时 vireo 全自持像素（物理 = 逻辑 × d）；`None` 时逻辑即 winit
  逻辑（OS 缩放参与）。
- **三条形状路径**：SDF 实例（默认，`sdf_feather` 控制软边）、几何模板（`clear_sdf_feather()`
  之后）、mesh 顶点（自己 push）。选哪条决定抗锯齿效果与自由度。
- **裁切与剔除**：`clips_children`（子树 stencil，轴对齐矩形自动走 scissor）、
  `area_include` / `area_exclude`（`Area` 布尔遮罩）、`text_clip` / `TextOverride.clip`
  （文字逻辑像素裁切）、`bounds`（子树 AABB 剔除，默认自动）。
- **多循环线程**：`app.run(..)` / `app.spawn(Thread)` 每条 Loop 跑在独立 OS 线程上，
  `App` 是 `Clone`（共享同一个 GPU 设备/队列）。每个线程用自己的 `DrawBatch`，
  不要跨线程共享正在构建的批次。
- **文本**：cosmic-text shaping + glyphon 光栅化，默认用**系统字体**（中文一般开箱可用）。

## 能做什么

- **SDF 形状 + 描边**：16 种内置形状，`sdf_feather` 控制软边；`PerSample` 档做逐采样着色 ——
  不用自己写距离场，也不用为了抗锯齿去堆 MSAA 采样。
- **自定义 WGSL 材质**：一份 `material_main` 同时作用于 shape 与 text；改颜色、采样贴图、
  算屏空间效果都行 —— 不用 fork 引擎。
- **MRT**：一次提交写多张颜色附件（磨砂玻璃、光晕、法线当样式场）—— 不用多 pass 拼。
- **深度 / 模板**：深度预通道、跨批次深度保留、`Area` 布尔裁切 —— 阴影与遮罩不用自己搭。
- **GPU compute**：`compute_pipeline` + `dispatch`，结果对后续 render pass 可见。
- **多窗口 + 离屏**：`OffscreenCanvas` 与窗口对称，`read_pixels()` 可回读（截图 / 回归测试）。
- **粒子**：`ParticlePool` + 一次推送，位置在 VS 里解析积分 —— 万级粒子不用每帧回写。
- **贴图**：文件 / 字节 / RGBA 三种来源，`UvRect` 子区域、Repeat / Clamp 采样模式 ——
  图集、九宫格、程序化纹理都不用自己写 shader。
- **按需出帧与节流**：没变化就不画（空闲 0 present/s），四层节流可逐层关，见
  [帧率与限流](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/throttling.md)。
- **渐变**：矩形 / 圆角 / 圆 / 线段 / 折线 / 多边形……一个机制全覆盖（轴向或沿走向），
  仍是 1 个实例 1 次 draw，见
  [渐变](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/gradient.md)。
- **文字**：整段 shape 缓存、HUD 分段（等宽步进）、`StableText`、自定义字体与 attrs、
  COLRv1 彩色 emoji。

## Android：这里是它最特别的地方

桌面 2D 渲染库不缺一个，缺的是**能在 Android 上正常处理中文输入的**：

- **中文输入法开箱可用** —— 走 `AGDK GameActivity`（`InputConnection`），
  而不是 `NativeActivity`（后者**根本做不到**输入法）。
- **安全区三值**：`safe_area`（物理最大遮挡）/ `padding`（当前可见）/ `ime_insets`（键盘），
  外加 `cutouts()` 拿摄像头挖孔矩形。
- **挖孔 / 沉浸式 / 系统栏 bitmask / 朝向锁定 / 常亮 / 亮度**，都在 `platform::android`。
- **弱 GPU 自动回退 GLES**，不要求目标设备一定有 Vulkan。

可直接复制的工程模板（NDK、gradle、16KB page size 合规、JNI 接线全在里面）：
[`examples/android/`](https://github.com/Milk-COCO/vireo/blob/master/examples/android/README.md)。

## 示例

| example | 看什么 |
|---|---|
| `ez` | 官方 quickstart（上面那段代码） |
| `text_hud` | 中文 HUD / 分段文本 |
| `shapes` | 16 种形状与描边 |
| `custom_material` | 自定义 WGSL 材质 |
| `mrt` | 一次提交写多张颜色附件 |
| `bench` | 压测 + draw call 统计 |

完整清单（72 个，按主题分类）见
[`docs/EXAMPLES.md`](https://github.com/Milk-COCO/vireo/blob/master/docs/EXAMPLES.md)。

## 深入阅读

- [自定义材质（Material ABI）](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/material.md)
  —— `material_main` 契约、`MaterialInput` 字段、4-group 布局、MRT 多输出、自管 bind group。
- [帧率、垂直同步与限流](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/throttling.md)
  —— 四层节流分别是什么、怎么关、关掉之后必须自己接管什么、按需出帧与常见误解。
- [关窗拦截与退出收尾](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/lifecycle.md)
  —— `on_close_requested` 否决关窗，`on_exit` / `on_exit_render` 做收尾，四条退出路径各自覆盖到什么。
- [渐变](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/gradient.md)
  —— 矩形/圆/线段/折线的渐变、停点规则、强制 SDF 的取舍、降级与实测。
- [折线逐点色](https://github.com/Milk-COCO/vireo/blob/master/docs/reference/line-gradient.md)
  —— `draw_line_chain_colors`：沿弧长的逐点色与实测。
- [平台支持](https://github.com/Milk-COCO/vireo/blob/master/docs/platforms.md)
  —— 各平台验证到什么程度、Android 构建链、工具链与字体。

## 构建与测试

```bash
cargo check --lib --examples --tests   # 期望 0 warning
cargo test --lib                       # 只跑 lib 单测
cargo test --release --test gpu_bench -- --ignored --test-threads=1   # 无头 GPU 用例（需 GPU）
```

`cargo test`（不带 `--lib`）会跑到 crate 文档里的 doc-test，而 **doc-test 会真的开窗口**，
在无头环境会挂住 —— 这是已知缺陷，所以只跑 `cargo test --lib`。

## 项目状态与风险

**版本与文档的对应关系**：crates.io 上是 **0.1.2**；本文档描述的是**仓库 master**（0.2.0，
未发布），其中 `on_close_requested`、`on_exit`、`set_clock`、`draw_line_chain_colors` 等
**0.1.2 里还没有**。用 crates.io 版本时请对照
[`CHANGELOG.md`](https://github.com/Milk-COCO/vireo/blob/master/CHANGELOG.md) 的迁移说明。

**验证程度**：Windows 日常使用 + 全部 example 跑过；Android（HyperOS 16 真机 + API 35 模拟器）
实测；Linux / macOS 只保证编译通过。测试覆盖等于「作者用过的路径」，不等于全路径 ——
多窗口并发、极端 DPI、罕见 AA 组合随时可能出问题。

### 前言：别用 Vireo

此项目 95%（**甚至更多**）内容由 **AI生成**。而唯一开发者看过的内容只有 5%（**甚至更少**）。

即使代码经过了 AI 的反复审查，依旧存在随时爆炸的风险。

## 许可证

Vireo 采用双许可证，可任选以下任一许可证使用：

- [Apache License 2.0](https://github.com/Milk-COCO/vireo/blob/master/LICENSE-APACHE)
- [MIT License](https://github.com/Milk-COCO/vireo/blob/master/LICENSE-MIT)
