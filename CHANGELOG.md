# Changelog / 更新日志

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
