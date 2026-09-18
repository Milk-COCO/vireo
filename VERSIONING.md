# Vireo 版本政策 / Versioning Policy

1. `0.x.y`：`y` = 纯修复（向后兼容），`x` = 新 API 或 breaking。
   Patch releases fix bugs only; any new API or breaking change bumps `x`.
2. 新 API 即使向后兼容也升 `x`（如 0.1.x → 0.2.0），不进 `y`（保守策略，下游显式跟进）。
   New APIs always bump `x`, never `y`.
3. breaking 含静默渲染语义变更，不止签名；必须升 `x` 并在 CHANGELOG 写迁移说明。
   Breaking includes silent rendering-semantics changes; bump `x` with migration notes.
4. yank 只用于缺陷版本（如刷屏日志的 0.1.0），不用于"撤回"好版本；yank 前须已有兼容替代版（Cargo Book 要求，否则浮动依赖新解析失败）。
   Yank defective releases only; a compatible replacement must exist first.
5. 发版流程：CHANGELOG 条目 → test/check/clippy/fmt 全绿 → commit → tag（`v0.x.y`，0.1.1 起补）→ `cargo publish --dry-run` → publish。
   Release flow: CHANGELOG → all green → commit → tag → dry-run → publish.
