# Changelog / 更新日志

## [0.2.0] — 未发布 / unreleased

### Added / 新增

- GPU 粒子路径：`Particle` / `ParticlePool` / `ParticleSlot` / `draw_particles`
  （`spawn` 参数烘焙＋VS 按 `camera.time` 积分位移与 fade 包络，存活期 CPU 零更新）。
  GPU particle path: baked spawn params + VS integration over `camera.time`.
- 粒子时钟注册表：`ClockIndex`（generational）＋`create_clock` / `destroy_clock` /
  `set_clock_scale`（0＝暂停，恢复无跳变）/ `set_clock_time` / `clock_time`；
  多独立时间线，0 号默认钟。`ParticlePool::sweep`（单遍清除过期粒子，无分配）。
  Particle clock registry: multiple independent timelines, default clock 0.
- `ShapeStats::particles` 诊断字段（`shape_vertex_count` 含粒子等效顶点）。
- `ParticlePool::sweep`（单遍清除过期粒子，无分配；示例每 30 帧调用一次）。
  `ParticlePool::sweep` (single-pass expiry without allocation).

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
