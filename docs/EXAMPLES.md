# 示例清单

> 面向 vireo 使用者。**本文件是示例清单的唯一来源**；README 只留 6 行推荐表。
> 全部是扁平的 `examples/*.rs`，文件名即 `cargo run --example <名字>`。
>
> 加/删/改名示例时请同步本文件 —— `cargo check --examples` 不会发现这里过期。

## 入门

| example | 看什么 |
|---|---|
| `hello` | 最小窗口：一个环 + logo 贴图 + 两行文字（`load_texture` / 窗口图标） |
| `ez` | **官方 quickstart**（README 里贴的就是这个文件）：一帧画矩形 / 圆 / 线 / 一行中文 |
| `deferred` | 延迟任务：`after_ticks(0)` vs `after_ticks(1)` 与 `after_secs` 的区别 |
| `multi_loop` | `App::spawn(vec![loop_a, loop_b])`：两个独立 Loop 各跑一条线程 |

## 文字

| example | 看什么 |
|---|---|
| `text_attrs` | `Attrs` 链式 builder：字体族 / 字重 / 斜体 / 字号 |
| `text_hud` | HUD 分段：Normal / Dynamic / Glyphs + 自动切分（分数、计时等） |
| `text_measure` | 文本测量：画 X 验证 bounding box 准确 |
| `text_transform` | 文字跟随变换：旧 API（不吃变换）vs `batch.text(..)` |
| `text_font` | 自定义字体：`load_font_file` / `load_font` + `Family::Name` |
| `text_clip` | 文字裁切与对齐：`TextOverride::clip` + Justified / End / max_width |
| `text_batch_clip` | 文字 + batch 裁切（单层，无嵌套） |
| `text_shape_cache` | 文字 shape 缓存压力 + 动态 transform |
| `text_profile` | 文字路径剖析：无头跑固定帧后退出，便于 `cargo flamegraph` |
| `text_stress` | 文本极端情况压测（超长、空串、混合脚本等） |
| `text_style` | 文本样式画廊：描边 / 多层描边 / 软投影 / 发光 / 内描边 / 浮雕（`1..7` 切组，`←→` 改宽度） |

## 形状与变换

| example | 看什么 |
|---|---|
| `shapes` | 16 种内置形状全览（填充 + 描边） |
| `shapes_lines` | `draw_line` / `draw_line_chain`；按 `S` 切换 SDF 柔边 vs 纯几何 |
| `shapes_rotate` | 绕 pivot 旋转 + 平移 |
| `shapes_cover_text` | 图形覆盖文字：验证 batch 的 shapes → texts → 下一个 batch 的顺序 |
| `transform_stack` | 叠加变换：`translate` / `rotate_*` / `scale_by` / `apply_matrix` |
| `line_gradient` | 折线沿线渐变：逐点色 vs 旧「切 4 档单色」对照（`D` 键切换） |
| `gradient` | 渐变能力画廊：停点（平台/硬边/越界/多色）、角度、各形状、贴图 × 渐变、预乘、圆角、单色退化 |

## 纹理

| example | 看什么 |
|---|---|
| `texture` | `App::load_texture`：run 前加载、run 内直接用 |
| `texture_rgba` | 程序化纹理：`Texture::from_rgba` / `from_bytes`（不需要图片文件） |
| `texture_region` | 纹理子区域：`set_uv()` + `draw_rectangle` |
| `texture_repeat` | 左 Repeat 平铺、右 ClampToEdge 钳制 |
| `texture_sdf_geo` | 同一贴图在几何路径（上）与 SDF 路径（下）的差别 |
| `texture_fun` | logo 的花哨玩法 |

## Batch / 裁切

| example | 看什么 |
|---|---|
| `batch_inherit` | 多子 batch：`InheritFromParent` + 部分 `unclipped` + 动态裁切 |
| `batch_child_clip` | 父裁子（stencil）：圆形遮罩 vs 不裁切对比 |
| `batch_nest_clip` | 嵌套 stencil 裁切 + transform |
| `batch_area_clip` | `Area` 布尔裁切（include / exclude / ∪ ∩ ∖），与 `clips_children` 正交 |
| `batch_scissor_clip` | 矩形 scissor 裁剪 |
| `batch_override` | `Shape` / `ShapeOverride` / 画笔状态机 |
| `batch_view` | `DrawBatch.view`：整批统一视图变换（属性，不是画笔状态） |

## 渲染目标 / 计算

| example | 看什么 |
|---|---|
| `depth_stencil` | 深度预通道 + 着色通道 |
| `mrt` | MRT 基础：一次 pass 写两张图 |
| `mrt_glass` | MRT 用途：用掩码通道做一块磨砂玻璃面板 |
| `mrt_glow` | MRT 用途：用第二路通道做光晕（少了它就做不出来） |
| `mrt_light` | MRT 用途：albedo + 法线两路数据，把法线当样式场用 |
| `mrt_liquid_glass` | MRT 用途：液态玻璃风格面板 |
| `compute` | GPU compute：`compute_pipeline` + `dispatch`，结果对后续 render pass 可见 |
| `offscreen` | 离屏渲染：与窗口对称的 `OffscreenCanvas` |
| `offscreen_aa` | 离屏抗锯齿对比：per-sample / MSAA / SDF |

## 粒子

| example | 看什么 |
|---|---|
| `particle_burst` | `ParticlePool` 常驻 + `draw_particles` 每帧一次推送（VS 解析积分、图集序列、颜色/尺寸端点） |

## 自定义材质

| example | 看什么 |
|---|---|
| `custom_material` | 最小 `material_main`：4 个矩形、时间驱动脉动颜色 |
| `custom_material_stencil` | 材质 + stencil（`clips_children`） |
| `custom_material_textures` | 材质里同时用多张贴图 |
| `custom_material_vertex` | 自定义顶点着色器：实例化契约下的两种绘制路径 |
| `custom_material_text` | 同一份材质作用于 shape 与 text，且 text 能读 batch 贴图 |
| `custom_material_post` | 后处理：先画到 `OffscreenCanvas`，再当普通贴图贴回窗口 |
| `custom_material_dynamic` | Dynamic storage offset：每帧换 uniform 偏移 |
| `custom_material_manual` | 自建 BGL + buffer + bind group，自管 `@group(3)` |
| `custom_material_self_managed` | 完全自管：用户提供 BGL 与 group 编号，引擎不插入 group 0/1/2 |

## 窗口 / 输入 / 生命周期

| example | 看什么 |
|---|---|
| `window_create` | `WindowDesc` 全部 builder + 多窗口 + 运行期建窗（`B` 键弹 popup） |
| `window_api` | 窗口控制 API：1:1 封装 winit 的状态命令/查询（HUD 显示当前状态） |
| `window_multi` | 多窗口并行绘制 + 逐窗口回调 |
| `window_resize` | resize 刷新策略 + 去抖 + 布局跟随（拖动观察） |
| `window_present` | PresentMode 全模式：AutoVsync / Fifo / Mailbox / Immediate |
| `window_aa` | AA 对比：per-sample / MSAA / SDF / None |
| `on_demand` | 按需出帧：没变化就不画（空闲 0 present/s） |
| `layout_follow` | 布局跟随的平滑模式（`set_layout_follow_smoothing`） |
| `exit_guard` | 关窗否决 `on_close_requested` + `on_closing` / `on_exit` / `on_exit_render` |
| `input` | 输入两种读法：状态轮询 + 事件订阅 |
| `input_touch` | 触摸两种读法：`on_touch` 事件流 + `win.touches()` 轮询 |
| `input_ime` | IME 输入法 + 文件拖放 |
| `resource_io` | 文件三件套：随包资源（只读）+ 存档 / 缓存 / 用户可见目录 |

## 压力 / 诊断

| example | 看什么 |
|---|---|
| `bench` | 多维度压测：SDF / 几何 / 文字 / 变换 / 多边形 + draw call 统计 |
| `frame_stats` | 帧统计 + spike 诊断 |
| `frame_stats_multi` | 多窗口各自的帧统计 |
| `frame_time_probe` | 帧间隔探针（跑 12 帧后自动退出） |
| `msaa_clamp` | 请求 MSAA 8x：是 panic 还是被自动 clamp（会打日志） |
| `loop_panic` | loop 线程 panic 后的收尾：进程应干净退出（退出码 1），不需 taskkill |
| `color_palette` | 颜色工具：`color_u8!` / `from_hex` / `to_hex` / `lerp` / `with_alpha` |

## 平台工程

| 目录 | 看什么 |
|---|---|
| `examples/android/` | Android 工程模板（`cargo ndk` + gradle + GameActivity），详见该目录 README |
