//! 按需出帧（脏驱动）—— 不画的时候真的不出帧
//!
//! 与其它示例相反：本示例**不**在每个 tick 调用 `win.draw`。"这一帧要不要画"由应用决定，
//! 只在三种情况出帧：
//!
//! 1. **脏信号**：输入到达（`refresh_input`）、尺寸/DPI 变化（`resize_pending`）、
//!    焦点变化 —— 判据全是现成的公开 API；
//! 2. **一次性脏标记**：按 `R` 显式请求重画一帧；
//! 3. **主动持续更新**：按 `A` 持续出帧（旋转扇形）；左键/空格触发一段 0.9s 脉冲动画，
//!    动画期间每个 tick 都出帧，走完那一帧补画后自动停。
//!
//! 其余时候一次都不调 `draw` —— 不 acquire、不 encode、不 present，屏幕保留上一张已
//! presented 的图（Windows flip model 下由 DWM 持有），不会闪白也不会残留半帧。
//!
//! 抄这个模式时别漏两个细节：
//! - 跳过出帧时**仍要**调 `refresh_input()`：它既是脏判据（返回值 = 这一 tick 有没有
//!   输入被应用），也是唯一把 `pending_input` 队列结算掉的地方，不调会一直堆积；
//! - 动画收尾要补画一帧（下面用 `one_shot` 实现），否则屏上停住的是"差一点没走完"
//!   的中间状态。
//!
//! 交互：鼠标移动 = 光斑跟随 | 左键 / 空格 = 0.9s 脉冲 | A = 持续动画
//!       R = 请求一帧 | Esc = 退出
//!
//! 观察点：
//! - 窗口标题里的 present/s 是**实时**的（标题更新不需要出帧，是本示例唯一的
//!   "实时外部通道"；引擎的 `presented_fps()` 是提交节拍的滑动平均，空闲时不会衰减，
//!   所以这里自己数过去 1s 的出帧数）。
//! - 窗口内 HUD 是**最后一次出帧时**的快照：空闲时它完全不动 —— 那就是"没在渲染"。

use std::time::{Duration, Instant};

use vireo::prelude::*;

/// 脉冲动画时长（秒）。
const PULSE_SECS: f32 = 0.9;
/// 状态汇报（标题 + stderr）间隔。
const REPORT_EVERY: Duration = Duration::from_secs(1);

const BG: Color = Color::new(0.06, 0.06, 0.09, 1.0);
const CARD: Color = Color::new(0.13, 0.14, 0.19, 1.0);
const ACCENT: Color = Color::new(0.34, 0.66, 1.0, 1.0);
const TEXT: Color = Color::new(0.92, 0.94, 1.0, 1.0);
const DIM: Color = Color::new(0.55, 0.58, 0.68, 1.0);

fn ease_out_cubic(u: f32) -> f32 {
    let v = 1.0 - u.clamp(0.0, 1.0);
    1.0 - v * v * v
}

#[vireo::main]
async fn main() {
    let idx = app.window(WindowDesc::new("Vireo 按需出帧", 720, 460));

    // ---- 动画状态 ----
    let mut spin = false; // A：持续动画开关
    let mut spin_t0 = Instant::now();
    let mut pulse_t0: Option<Instant> = None; // 脉冲起点
    let mut one_shot = true; // 首帧必须先画一张

    // ---- 输入边沿（轮询；要绝对不丢极短按键就用 app.on_key_down 注册回调）----
    let (mut prev_a, mut prev_r, mut prev_space, mut prev_esc, mut prev_left) =
        (false, false, false, false, false);

    // ---- 统计 ----
    let mut drawn = 0u64; // 应用主动出帧次数
    let mut skipped = 0u64; // 主动跳过次数
    let mut drawn_window = 0u64; // 过去一个汇报周期内的出帧数（= 实时 present/s）
    let mut last_report = Instant::now();
    let mut last_focus = true;
    let mut pointer = (0.0f32, 0.0f32);

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(w) => w,
            Err(_) => return false,
        };
        let now = Instant::now();

        // ── ① 脏信号：判据就这三行 ────────────────────────────────────────
        // `refresh_input()` 的返回值本身就是"这一 tick 有没有输入被应用"。
        let mut dirty = win.refresh_input();
        // 尺寸/DPI 与已配置 surface 不一致 → 必须重画，否则会看到被拉伸的旧图。
        dirty |= win.resize_pending();
        // 焦点变化只改 HUD 文字，但也得画一帧才看得见。
        let focused = win.focused();
        if focused != last_focus {
            last_focus = focused;
            dirty = true;
        }

        // ── ② 输入边沿 → 状态机 ──────────────────────────────────────────
        let (k_a, k_r, k_space, k_esc, k_left) = (
            win.key_down(KeyCode::KeyA),
            win.key_down(KeyCode::KeyR),
            win.key_down(KeyCode::Space),
            win.key_down(KeyCode::Escape),
            win.mouse_left(),
        );
        if k_a && !prev_a {
            spin = !spin;
            spin_t0 = now;
            dirty = true;
        }
        if k_r && !prev_r {
            one_shot = true;
        }
        if (k_space && !prev_space) || (k_left && !prev_left) {
            pulse_t0 = Some(now);
            dirty = true;
        }
        if k_esc && !prev_esc {
            win.close();
        }
        prev_a = k_a;
        prev_r = k_r;
        prev_space = k_space;
        prev_esc = k_esc;
        prev_left = k_left;

        if win.cursor_inside() {
            let (mx, my) = win.mouse_pos().logical();
            pointer = (mx as f32, my as f32);
        }

        // ── ③ 应用自己的脏：动画在跑就每个 tick 都要画 ────────────────────
        let mut animating = spin;
        if let Some(t0) = pulse_t0 {
            if now.duration_since(t0).as_secs_f32() >= PULSE_SECS {
                pulse_t0 = None;
                one_shot = true; // 补最后一帧
            } else {
                animating = true;
            }
        }
        if std::mem::take(&mut one_shot) {
            dirty = true;
        }
        dirty |= animating;

        // ── ④ 唯一的出帧点 ───────────────────────────────────────────────
        // `refresh_metrics()` 在 0×0（最小化）时返回 false，顺手当守卫用。
        if dirty && win.refresh_metrics() {
            let (card_x, card_y, card_w, card_h) = (40.0f32, 96.0f32, 640.0f32, 268.0f32);
            let (card_cx, card_cy) = (card_x + card_w * 0.5, card_y + card_h * 0.5);
            // 光斑位置钳在卡片内
            let spot = (
                pointer.0.clamp(card_x + 28.0, card_x + card_w - 28.0),
                pointer.1.clamp(card_y + 28.0, card_y + card_h - 28.0),
            );

            let mut batch = DrawBatch::new();
            draw_rounded_rect(
                &mut batch,
                Pos::new(card_x, card_y),
                card_w,
                card_h,
                18.0,
                Some(CARD),
            );

            // 光斑跟随光标：只有鼠标事件到来才会出帧，所以手一停画面立刻静止。
            if win.cursor_inside() {
                draw_circle(
                    &mut batch,
                    Pos::new(spot.0, spot.1),
                    22.0,
                    Some(Color::new(0.30, 0.62, 0.95, 0.85)),
                );
            }

            // 脉冲：0.9s 扩散 + 淡出。按 u 直接求值，不存补间状态。
            if let Some(t0) = pulse_t0 {
                let u = (now.duration_since(t0).as_secs_f32() / PULSE_SECS).min(1.0);
                let r = 26.0 + 96.0 * ease_out_cubic(u);
                draw_circle_outline(
                    &mut batch,
                    Pos::new(spot.0, spot.1),
                    r,
                    3.0,
                    Some(Color::new(0.45, 0.78, 1.0, 1.0 - u)),
                    72,
                );
            }

            // 主动持续更新：卡片中心的旋转弧 + 弧端的光点。
            if spin {
                let ang = now.duration_since(spin_t0).as_secs_f32() * 2.6;
                draw_sector_outline(
                    &mut batch,
                    Pos::new(card_cx, card_cy),
                    62.0,
                    ang,
                    ang + 1.15,
                    5.0,
                    Some(ACCENT),
                    64,
                );
                draw_circle(
                    &mut batch,
                    Pos::new(
                        card_cx + 62.0 * ang.cos(),
                        card_cy + 62.0 * ang.sin(),
                    ),
                    6.0,
                    Some(TEXT),
                );
            }

            // ---- HUD：最后一次出帧时的快照 ----
            drawn += 1;
            drawn_window += 1;
            let def = TextDef::default().font_size(12.0);
            let mode = match (pulse_t0.is_some(), spin) {
                (true, _) => "脉冲（0.9s，每 tick 出帧）",
                (false, true) => "持续动画（每 tick 出帧）",
                (false, false) => "空闲（不出帧）",
            };

            draw_text(
                &mut batch.texts,
                "按需出帧（脏驱动）",
                Pos::new(40.0, 30.0),
                TextDef::default().font_size(17.0),
                TextOverride::from_color(TEXT),
            );
            draw_text(
                &mut batch.texts,
                "不调 win.draw = 不 acquire / 不 present；屏上保留上一张。",
                Pos::new(40.0, 56.0),
                def.clone(),
                TextOverride::from_color(DIM),
            );
            draw_text(
                &mut batch.texts,
                &format!("模式：{}", mode),
                Pos::new(64.0, 126.0),
                def.clone(),
                TextOverride::from_color(ACCENT),
            );
            draw_text(
                &mut batch.texts,
                &format!(
                    "presented {}  ·  应用出帧 {}  ·  跳过 {} tick  ·  焦点 {}",
                    win.presented_frames(),
                    drawn,
                    skipped,
                    if focused { "有" } else { "无" },
                ),
                Pos::new(64.0, 152.0),
                def.clone(),
                TextOverride::from_color(TEXT),
            );
            draw_text(
                &mut batch.texts,
                "HUD 只在出帧时更新：空闲时它停住不动，那就是没在渲染。",
                Pos::new(64.0, 178.0),
                def.clone(),
                TextOverride::from_color(DIM),
            );
            draw_text(
                &mut batch.texts,
                "鼠标移动 = 光斑跟随    左键 / 空格 = 0.9s 脉冲    A = 持续动画    R = 请求一帧    Esc = 退出",
                Pos::new(40.0, 400.0),
                def.clone(),
                TextOverride::from_color(DIM),
            );
            draw_text(
                &mut batch.texts,
                "实时 present/s 看窗口标题；引擎 presented_fps() 是提交节拍均值，空闲时不衰减。",
                Pos::new(40.0, 422.0),
                TextDef::default().font_size(11.0),
                TextOverride::from_color(DIM),
            );

            win.draw(BG, &[&batch]);
        } else {
            skipped += 1;
        }

        // ── ⑤ 不需要出帧的反馈通道：标题 + 日志 ──────────────────────────
        // 标题更新走 winit，与出帧无关，所以空闲时它照样能报出"现在 0 fps"。
        if now.duration_since(last_report) >= REPORT_EVERY {
            let busy = spin || pulse_t0.is_some();
            let rate = drawn_window as f64 / now.duration_since(last_report).as_secs_f64();
            drawn_window = 0;
            last_report = now;
            eprintln!(
                "[od] tick={} 出帧={} 跳过={} presented={} 实时present/s={:.1} {}",
                ctx.tick_count(),
                drawn,
                skipped,
                win.presented_frames(),
                rate,
                if busy { "动画中" } else { "空闲" },
            );
            win.set_title(&format!(
                "Vireo 按需出帧 — {:.1} present/s{}",
                rate,
                if busy {
                    "（主动持续更新中）"
                } else {
                    "（空闲：正在跳过所有帧）"
                }
            ));
        }

        true
    })
    .await
    .unwrap();
}
