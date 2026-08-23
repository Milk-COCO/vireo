/// 滑动窗口帧数：约 0.5s@60Hz，平滑 FPS，避免「满 1 秒整段重置」导致 55↔60 乱跳。
pub(crate) const FPS_SAMPLE_CAP: usize = 30;
/// presented rate 的滑动窗口大小（成功 `queue.present` 的间隔采样数）。
pub(crate) const PRESENT_SAMPLE_CAP: usize = 30;
/// resize 去抖默认值：尺寸**稳定**满此时间才 `surface.configure`。
/// 连续拖动时每帧尺寸都变、去抖永不触发 → 全程不 configure，按旧尺寸持续
/// present（DXGI SCALING_STRETCH 实时拉伸），帧流保持满速、无 27-68ms 卡顿
/// （wgpu-hal DX12 configure 每次都会 `wait_for_present_queue_idle` 等 present
/// queue 排空，DWM 停消费时无限等 → 旧实现拖动即冻屏）。
/// 用户可经 `VireoWindow::set_resize_debounce` 覆盖。
pub(crate) const DEFAULT_RESIZE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(100);

/// resize 尺寸漂移容差（物理像素，每轴）。
///
/// 快速拖动窗口边缘缩放松手后，Windows 可能把 `inner_size()` 短暂报成相邻像素
/// 的抖动（约 ±1~2px，持续约 1s）。若按精确比较：
/// - `moved` 每帧都为真 → `pending_resize_at` 永不过期 → debounce 永不触发 →
///   snap 永不执行；
/// - follow-layout 持续把抖动的尺寸写进 camera / 文字 viewport → 画面反复左右
///   拉伸（抽搐）。
///
/// 加此容差后，物理尺寸变化不超过 `RESIZE_DRIFT_EPSILON` 时视为「未移动」：
/// 去抖计时开始老化、松手即一次性 snap；snap 后的小抖动也不再重新触发
/// follow/configure。可调大以更抗抖动（代价：极小尺寸的真实 resize 不再立即
/// 重配，内容做 ≤ 容差的不可见拉伸）。
pub(crate) const RESIZE_DRIFT_EPSILON: u32 = 2;

/// 拖动窗口时的 resize 尺寸刷新策略（`VireoWindow::set_resize_refresh_policy`）。
/// 目标是把「拖动中是否实时跟踪尺寸」的选择权交给用户：每帧/周期刷新在 wgpu-hal
/// DX12 上每次 `surface.configure` 都要阻塞等 present queue 排空（实测 ~50-80ms），
/// 会明显掉帧，但能实时看到新布局；`OnRelease` 全程不卡但内容拉伸到松手。
/// 「松手 snap」的去抖时长由 `VireoWindow::set_resize_debounce` 配置（默认 100ms）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeRefreshPolicy {
    /// 拖动全程不更新（按旧尺寸拉伸 present，帧流满速），松手尺寸稳定满
    /// 去抖时长后一次性 `surface.configure`（snap）。默认。
    OnRelease,
    /// 拖动中**每帧**都 `surface.configure` 实时跟踪尺寸。每次 configure
    /// 阻塞 ~50-80ms，帧率骤降、一顿一顿——给需要拖动时看到真实布局的用户。
    EveryFrame,
    /// 拖动中每满 `interval` 强制 configure 一次（折中；每次同样 ~50ms 级停顿）。
    Periodic(std::time::Duration),
}

/// resize 刷新决策（`VireoWindow::draw` 尺寸同步用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResizeRefresh {
    /// 尺寸未变，或当前策略下不需要 configure。
    None,
    /// 尺寸已**稳定**满 debounce（松手 snap）。
    Stable,
    /// 拖动中按策略实时刷新（每帧或周期性）。
    Live,
}

/// 验证 `set_aspect_ratio` 入参：非正数视为清除（与 `None` 等价）。
pub(crate) fn validate_aspect_ratio(ratio: Option<f64>) -> Option<f64> {
    match ratio {
        Some(r) if r > 0.0 => Some(r),
        _ => None,
    }
}

/// 布局跟随（`layout_follow`）的平滑强度单位：既可按**帧数**也可按**真实时长**表达。
///
/// - `Time(d)`：以采样时间差判断（与刷新率无关）。
/// - `Frames(n)`：以**实际执行跟随**的帧数判断（`VireoWindow` 会按此计数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowFramesOrTime {
    /// 平滑窗长 / 保持时长按时间：`Duration::ZERO` = 每帧追（= `PerFrame`）。
    Time(std::time::Duration),
    /// 平滑窗宽 / 保持步幅按帧数：`0` = 每帧追（= `PerFrame`）。
    Frames(u32),
}

/// 布局跟随的平滑模式（`VireoWindow::set_layout_follow_smoothing`）。
///
/// `layout_follow` 打开时，窗口尺寸已变但 surface 未重配（拖动中），内容跟随窗口
/// 更新的节奏由这里决定。强度统一用 [`FollowFramesOrTime`] 表达——既可按真实时长
/// （`Time`，与刷新率无关）也可按帧数（`Frames`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowAmount {
    /// 每帧都追到最新尺寸（不平滑）。最跟手，但窗口动得快时画面容易「抖/闪一帧」。
    PerFrame,
    /// 平均窗：camera 目标 = 最近 `amt` 内观察到的窗口尺寸的**均值**。画面连续渐变
    /// 不跳格；`amt` 越小越跟手、越大越平滑（反应越慢）。`0` 退化为 `PerFrame`。
    Average(FollowFramesOrTime),
}

impl Default for FollowAmount {
    /// 默认 `Average(Time(16ms))`：小幅平滑，既不每帧硬追（免抖）也不明显滞后。
    fn default() -> Self {
        FollowAmount::Average(FollowFramesOrTime::Time(std::time::Duration::from_millis(16)))
    }
}

/// 尺寸是否需要在 draw 阶段 configure：
/// - 未变化 → `None`；
/// - 稳定满 `debounce` → `Stable`（拖动结束后的 snap，任何策略下都生效）；
/// - `EveryFrame` → 尺寸变化即 `Live`；`Periodic(iv)` → 距上次 configure 满 iv
///   → `Live`；`OnRelease` → 无实时刷新。
pub(crate) fn resize_refresh(
    size_changed: bool,
    stable_since: Option<std::time::Instant>,
    now: std::time::Instant,
    debounce: std::time::Duration,
    policy: ResizeRefreshPolicy,
    last_configure: std::time::Instant,
) -> ResizeRefresh {
    if !size_changed {
        return ResizeRefresh::None;
    }
    if let Some(t) = stable_since {
        if now.saturating_duration_since(t) >= debounce {
            return ResizeRefresh::Stable;
        }
    }
    match policy {
        ResizeRefreshPolicy::OnRelease => {}
        ResizeRefreshPolicy::EveryFrame => return ResizeRefresh::Live,
        ResizeRefreshPolicy::Periodic(iv) => {
            if now.saturating_duration_since(last_configure) >= iv {
                return ResizeRefresh::Live;
            }
        }
    }
    ResizeRefresh::None
}

/// 物理尺寸是否「显著」漂移（任一轴超出 `eps` 才算）。快速拖动松手后 Windows
/// 会短暂把 `inner_size()` 报成相邻像素抖动，精确比较会把 ±1~2px 的小抖动当成
/// 真漂移，导致去抖/snap 永不触发、follow 持续跟随抖动（画面抽搐）。
pub(crate) fn size_drifted_beyond(configured: (u32, u32), observed: (u32, u32), eps: u32) -> bool {
    configured.0.abs_diff(observed.0) > eps || configured.1.abs_diff(observed.1) > eps
}

/// 观测值相对锚点（上次显著变化位置）是否「移动」。
///
/// 物理尺寸变化 > `eps` 或 `scale` 变化才算移动（逻辑 = 物理/scale，scale ≥ 1 时
/// 物理在容差内 ⇒ 逻辑必在容差内，故同一 `eps` 即可约束；scale 变化单独比较）。
/// 锚点只在移动时推进（`draw` 里维护）——松手后尺寸在锚点 ±`eps` 内抖动时返回
/// `false`，使去抖计时能老化并触发一次性 snap。
#[allow(clippy::type_complexity)]
pub(crate) fn observed_moved(anchor: (u32, u32, f32), observed: (u32, u32, f32), eps: f32) -> bool {
    anchor.0.abs_diff(observed.0) as f32 > eps
        || anchor.1.abs_diff(observed.1) as f32 > eps
        || anchor.2 != observed.2
}

/// 物理像素尺寸 ÷ 有效缩放 → 逻辑尺寸 (f64)。`scale <= 0` 时原样返回物理值
///（无缩放语义兜底，与 `dpi.rs::pixel_of` 一致）。
#[inline]
pub(crate) fn phys_to_logical(phys: (u32, u32), scale: f64) -> (f64, f64) {
    if scale > 0.0 {
        (phys.0 as f64 / scale, phys.1 as f64 / scale)
    } else {
        (phys.0 as f64, phys.1 as f64)
    }
}

/// 分段耗时（秒），由 [`VireoWindow::draw`] 回传。
///
/// 新流程（render thread 独占 surface 帧循环）：
/// - `configure_secs`：本帧 `surface.configure`（未 configure 时为 0）
/// - `acquire_secs`：`get_current_texture`（不含此前的尺寸同步与 `surface.configure`）
/// - `encode_secs`：Renderer 编码 + `queue.submit`
/// - `present_secs`：`queue.present`
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawTimings {
    /// `surface.configure` 同步耗时；本帧未 configure 时为 0。
    pub configure_secs: f64,
    /// `get_current_texture`：可能阻塞等待 swapchain 空位；不含尺寸同步与
    /// `surface.configure`，后两者发生在此计时区间之前。
    pub acquire_secs: f64,
    /// 编码 + `queue.submit`（不含 present）。
    pub encode_secs: f64,
    /// `queue.present`（不含 GPU 执行）。
    pub present_secs: f64,
    /// 上一份已完成提交的 GPU queue latency（不含 CPU 构图）。
    /// 该值包含驱动排队/GPU 竞争，不等于纯 shader 执行时间。
    pub gpu_secs: Option<f64>,
}

/// 一次 [`VireoWindow::draw`] 的结果。
#[derive(Clone, Copy, Debug)]
pub struct DrawReport {
    /// 本帧结局（presented / skipped / failed）
    pub outcome: DrawOutcome,
    /// 分段耗时
    pub timings: DrawTimings,
}

/// 一次 [`VireoWindow::draw`] 的结局。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawOutcome {
    /// 已 acquire → submit → present 完成一帧。
    /// `suboptimal` 原样报告 wgpu 的 surface acquire 状态；它只表示当前 surface
    /// 对 swapchain 而言不是最优状态，不是规范的 resize/拉伸/拖动状态信号。
    Presented { suboptimal: bool },
    /// 本帧被跳过（未 acquire / 未 present）。
    Skipped(DrawSkipReason),
    /// GPU 设备丢失，应用应终止。
    Failed(DrawFailure),
}

/// 本帧被跳过的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawSkipReason {
    /// 窗口为 0×0（如最小化）——本次 draw 不 acquire。
    /// 这只跳过当前帧，不负责限制调用方渲染循环的 CPU 频率。
    ZeroSized,
    /// `get_current_texture` 超时，稍后重试。
    Timeout,
    /// 窗口被遮挡/最小化，重开后再画。
    Occluded,
    /// surface 过期（`Outdated`），已重配，本帧跳过。
    SurfaceReconfigured,
    /// 窗口正在关闭，不绘制。
    Closing,
}

/// 不可恢复的帧失败。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawFailure {
    /// GPU 设备丢失，surface 与全部 GPU 资源失效。
    DeviceLost,
}

/// 构造「本帧跳过」的 [`DrawReport`]（保留 gpu_secs）。
pub(crate) fn skip_report(gpu_secs: Option<f64>, reason: DrawSkipReason) -> DrawReport {
    DrawReport {
        outcome: DrawOutcome::Skipped(reason),
        timings: DrawTimings { gpu_secs, ..DrawTimings::default() },
    }
}

/// 滑动窗口平均频率（样本/总时长）。空窗口或总时长非正返回 0。
pub(crate) fn sliding_rate(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().sum();
    if sum > 0.0 {
        samples.len() as f64 / sum
    } else {
        0.0
    }
}

/// 空转抑制的纯决策（相位锁）。给定当前时刻 `now`、上一次算出的 `pacing_deadline`
/// 与上限 `max_fps`，返回 `(新的 pacing_deadline, 本次应 sleep 的时长)`。
/// 推进规则：
///   - 未设上限 / fps=0 → `(None, None)`（不限制）。
///   - 本帧已落后于 deadline（`deadline <= now`，说明 work 已经占满 stride）→
///     **不睡**（`None`），只把 deadline 重锚到 `now+stride`。「落后不追赶」。
///     关键：此刻 work 已耗尽目标间隔，再多睡一整格就是「work+stride」超帧，
///     实测 60fps→48、30fps→25。cap 只在「跑太快」时补觉，跑慢了不加倍等。
///   - 还有剩余时间（`deadline > now`）→ 睡 `deadline-now`，下次推到 `deadline+stride`。
///     `(Some(d+stride), Some(d-now))`。
pub(crate) fn pac_advance(
    now: std::time::Instant,
    deadline: Option<std::time::Instant>,
    max_fps: Option<u32>,
) -> (Option<std::time::Instant>, Option<std::time::Duration>) {
    let stride = match max_fps {
        Some(f) if f > 0 => std::time::Duration::from_secs_f64(1.0 / f as f64),
        _ => return (None, None),
    };
    match deadline {
        Some(d) if d > now => (Some(d + stride), Some(d - now)),
        _ => (Some(now + stride), None),
    }
}

/// resize 拖动期间把用户帧率上限压到显示器刷新率（acquire 失去 vsync 节流时
/// 防空转）。`user` 为 `Some(u)` → `min(u, hz)`；`None`（用户不限制）→ 也压到
/// `hz`（与 `set_max_fps` 解耦，见 [`drag_cap_effective`]）。`hz` 下限 1 防除零。
/// `drag_refresh_mhz` 是 winit 返回的 milli-Hz（120Hz → 120_000），必须转 Hz 再比，
/// 否则 `min(240, 120_000) = 240`，cap 永远压不下去。
pub(crate) fn drag_effective_cap(user: Option<u32>, drag_refresh_mhz: u32) -> Option<u32> {
    let hz = mhz_to_hz(drag_refresh_mhz).max(1);
    Some(match user {
        Some(u) => u.min(hz),
        None => hz,
    })
}

/// 拖动期帧率上限的最终决策：`drag_cap` 开启（默认）→ 压到显示器刷新率
/// （`user=None` 也压，与 `set_max_fps` 解耦）；关闭 → 原样返回 `user`。
pub(crate) fn drag_cap_effective(user: Option<u32>, drag_cap: bool, drag_refresh_mhz: u32) -> Option<u32> {
    if drag_cap {
        drag_effective_cap(user, drag_refresh_mhz)
    } else {
        user
    }
}

/// 把 winit `refresh_rate_millihertz()` 的 milli-Hz 转成 Hz（四舍五入）。
/// 单位不匹配会导致 `min(240, 120_000) = 240`，cap 永远压不下去。
pub(crate) fn mhz_to_hz(millihertz: u32) -> u32 {
    (millihertz + 500) / 1000
}

pub(crate) fn should_backoff_after_draws(
    outcomes: impl IntoIterator<Item = Option<DrawOutcome>>,
) -> bool {
    let mut any = false;
    for outcome in outcomes {
        any = true;
        if !matches!(
            outcome,
            Some(DrawOutcome::Skipped(
                DrawSkipReason::ZeroSized
                    | DrawSkipReason::Timeout
                    | DrawSkipReason::Occluded
                    | DrawSkipReason::Closing
            ))
        ) {
            return false;
        }
    }

    any
}
