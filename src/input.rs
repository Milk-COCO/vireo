use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;

// ------ Re-exports (winit types direct) ------

pub use winit::event::ElementState;
/// 输入法（IME）事件，1:1 对应 winit 的 [`Ime`]（winit 文档见
/// `winit::event::Ime`）。四个变体：`Enabled` / `Preedit(String, Option<(usize,usize)>)` /
/// `Commit(String)` / `Disabled`。
///
/// winit 行为要点：
/// - `Preedit` 的第二个参数是光标在组词串中的 **byte** 区间 `(begin, end)`；`None` = 隐藏光标；
///   空串 `""` = 清除组词（winit 合成事件）。
/// - 事件顺序：`Enabled → Preedit* → Commit → Disabled`。
/// - 仅当 [`VireoWindow::set_ime_allowed(true)`](crate::window::VireoWindow::set_ime_allowed)
///   且窗口聚焦时才会收到；IME 开启期间 preedit 阶段**收不到** `KeyboardInput` 事件。
pub use winit::event::Ime;
pub use winit::event::MouseButton;
pub use winit::keyboard::Key;
pub use winit::keyboard::KeyCode;

// ------ Vireo own types ------

/// 键盘事件（扁平化 winit 的 KeyEvent）
#[derive(Debug, Clone)]
pub struct KeyEvent {
    /// 物理键位（不随键盘布局变化）
    pub key: KeyCode,
    /// 逻辑键（受键盘布局影响，区分 NamedKey/Character/Dead/Unidentified）
    pub logical_key: Key,
    /// 按键状态
    pub state: ElementState,
    /// 按键关联的文本，无关联时为 None
    pub text: Option<String>,
    /// 是否由自动重复产生
    pub repeat: bool,
}

/// 鼠标按钮事件
#[derive(Debug, Clone, Copy)]
pub struct MouseButtonEvent {
    pub button: MouseButton,
    pub state: ElementState,
}

/// 滚轮滚动增量
#[derive(Debug, Clone, Copy)]
pub enum ScrollDelta {
    /// 按行滚动（大多数鼠标滚轮）
    Line { x: f32, y: f32 },
    /// 按像素滚动（触控板精确滚动）
    Pixel { x: f32, y: f32 },
}

/// 滚轮事件
#[derive(Debug, Clone, Copy)]
pub struct MouseScrollEvent {
    pub delta: ScrollDelta,
}

/// 修饰键位标志（位掩码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1 << 0);
    pub const CTRL: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);
    pub const SUPER: Self = Self(1 << 3);

    pub fn shift(&self) -> bool {
        self.0 & Self::SHIFT.0 != 0
    }
    pub fn ctrl(&self) -> bool {
        self.0 & Self::CTRL.0 != 0
    }
    pub fn alt(&self) -> bool {
        self.0 & Self::ALT.0 != 0
    }
    pub fn super_key(&self) -> bool {
        self.0 & Self::SUPER.0 != 0
    }
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for Modifiers {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// 触摸阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

/// 触摸事件
#[derive(Debug, Clone)]
pub struct TouchEvent {
    /// 触摸点的唯一标识符
    pub id: u64,
    /// 触摸阶段
    pub phase: TouchPhase,
    /// 触摸位置（逻辑坐标）
    pub x: f32,
    pub y: f32,
    /// 按压力度（0.0 ~ 1.0）
    pub force: Option<f64>,
}

/// **当前活跃**的触摸点（轮询用；对应回调见 [`TouchEvent`]）。
///
/// 由 [`crate::VireoWindow::touches`] / [`crate::VireoWindow::touch_first`] 返回。
/// 生命周期与 [`InputState::touches`] 一致：手指按下即出现，上抬 / 取消即消失。
///
/// # 与 [`TouchEvent`] 的分工
///
/// | | [`TouchEvent`] | [`TouchPoint`] |
/// |---|---|---|
/// | 来源 | `on_touch` 回调（winit 线程） | 轮询 getter（任意线程） |
/// | 含 phase | ✅（含 `Ended` / `Cancelled`） | ❌ **只含活跃点** |
/// | 用途 | 「这一帧手指抬起/取消了」 | 「此刻有几根手指、按在哪」 |
///
/// 终态 phase（抬起/取消）**只存在于回调** —— 轮询看到某点消失时无法区分它是
/// 正常抬起还是被系统取消；需要这个信息就用回调。
///
/// # ⚠️ 轮询是「电平」，不是「边沿」
///
/// 手指按住期间它**每帧都在** [`InputState::touches`] 里。所以「按一下触发一次」
/// 的交互必须自己在帧间对 `id` 做差分（或用回调的 `Started`），
/// 见 [`crate::VireoWindow::touches`] 的示例。
///
/// # 坐标空间
///
/// [`Self::pos`] 是 [`PixelPos`] 双表示，与 [`crate::VireoWindow::mouse_pos()`] 同构：
/// 绘制坐标系里用 `.logical()`，命中测试/屏幕定位用 `.physical()`。
/// （注意 [`Self::id`] 之外没有隐式单位 —— 和 vireo 其余位置 API 一致，
/// 坐标意图在调用点通过 `.physical()` / `.logical()` 显式声明。）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchPoint {
    /// 触摸点的唯一标识符（同一根手指在 Started→Moved→Ended 全程不变）。
    pub id: u64,
    /// 位置（物理 + 逻辑双表示）。
    pub pos: crate::dpi::PixelPos,
    /// 按压力度（0.0 ~ 1.0）；设备不报压力时为 `None`。
    pub force: Option<f64>,
}

/// [`InputState::touches`] 快照 → [`TouchPoint`] 列表（按 `id` 升序）。
///
/// 抽成独立函数是为了能**脱离 GPU 窗口**做单测 —— `VireoWindow::new` 需要
/// device/queue，`cargo test --lib` 里构造不出来。
///
/// 排序是刻意的：输入是 `HashMap`，迭代顺序随机 —— 不排序会让
/// 「取第一个点」类代码每帧拿到不同的手指。
pub(crate) fn touch_points_sorted(
    map: &HashMap<u64, (f32, f32, Option<f64>)>,
    scale: f64,
) -> Vec<TouchPoint> {
    let mut v: Vec<TouchPoint> = map
        .iter()
        .map(|(id, (x, y, force))| TouchPoint {
            id: *id,
            // map 里的坐标是 `map_touch_event` 产出的**逻辑**像素（那里已 ÷ scale）
            // → 必须走 dp→PixelPos 方向，用 `to_pixel_pos` 会再除一次 scale。
            pos: crate::dpi::dp_to_pixel_pos(*x as f64, *y as f64, scale),
            force: *force,
        })
        .collect();
    v.sort_unstable_by_key(|p| p.id);
    v
}

// ------ InputCallbacks ------

/// 输入事件回调集合（运行在 winit 线程，无需 +Send 约束）。
/// `unsafe impl Send`：`App.callbacks` 在移到渲染线程前已被抽空，
/// 不包含实际非 Send 数据。
#[derive(Default)]
pub struct InputCallbacks {
    pub on_key_down: Vec<Box<dyn FnMut(&KeyEvent)>>,
    pub on_key_up: Vec<Box<dyn FnMut(&KeyEvent)>>,
    pub on_mouse_down: Vec<Box<dyn FnMut(&MouseButtonEvent)>>,
    pub on_mouse_up: Vec<Box<dyn FnMut(&MouseButtonEvent)>>,
    pub on_scroll: Vec<Box<dyn FnMut(&MouseScrollEvent)>>,
    pub on_cursor_entered: Vec<Box<dyn FnOnce()>>,
    pub on_cursor_left: Vec<Box<dyn FnOnce()>>,
    pub on_touch: Vec<Box<dyn FnMut(&TouchEvent)>>,
    pub on_focus_gained: Vec<Box<dyn FnOnce()>>,
    pub on_focus_lost: Vec<Box<dyn FnOnce()>>,
    pub on_modifiers_changed: Vec<Box<dyn FnMut(Modifiers)>>,
    /// 输入法（IME）事件（见 [`Ime`]）。运行在 winit 线程。
    pub on_ime: Vec<Box<dyn FnMut(&Ime)>>,
    /// 文件被拖放进窗口。多文件逐个触发。运行在 winit 线程。
    pub on_file_dropped: Vec<Box<dyn FnMut(&std::path::PathBuf)>>,
    /// 文件正悬停在窗口上方。多文件逐个触发。运行在 winit 线程。
    pub on_file_hovered: Vec<Box<dyn FnMut(&std::path::PathBuf)>>,
    /// 悬停取消（文件移出窗口或取消）。无论多少文件悬停只触发一次。运行在 winit 线程。
    pub on_file_hover_cancelled: Vec<Box<dyn FnOnce()>>,
    /// 窗口位置（物理像素，含窗口边框外沿）变化。运行在 winit 线程。
    pub on_moved: Vec<Box<dyn FnMut(winit::dpi::PhysicalPosition<i32>)>>,
    /// 系统主题变化（`Theme`）。仅 Windows/macOS 上报。运行在 winit 线程。
    pub on_theme_changed: Vec<Box<dyn FnMut(winit::window::Theme)>>,
    /// 窗口尺寸（物理像素）变化。模态循环期间可能滞后（渲染线程逐帧轮询兜底）。
    /// 运行在 winit 线程。
    pub on_resized: Vec<Box<dyn FnMut(winit::dpi::PhysicalSize<u32>)>>,
    /// 任务栏缩略图按钮点击（参数 = 按钮 `id`）。仅 Windows 上报；依赖
    /// `set_thumbar_buttons` 安装的 `WM_COMMAND`/`THBN_CLICKED` 子类。运行在 winit 线程。
    pub on_thumb_button: Vec<Box<dyn FnMut(u32)>>,
}

// SAFETY: InputCallbacks 仅在 winit 线程使用。App 移入渲染线程前 self.callbacks 已被抽空。
unsafe impl Send for InputCallbacks {}

// ------ InputState ------

/// 持久的输入状态（VireoWindow 内部持有）
pub struct InputState {
    /// 当前按下的键集合（不包含 repeat 事件）
    pub keys_down: Mutex<HashSet<KeyCode>>,
    /// 当前按下的鼠标按钮集合
    pub mouse_buttons_down: Mutex<HashSet<MouseButton>>,
    /// 当前修饰键状态
    pub modifiers: Mutex<Modifiers>,
    /// 本帧滚轮增量累计（按单位分开，避免鼠标滚轮 vs 触控板的单位混淆）
    pub scroll_delta: Mutex<ScrollDeltaAccum>,
    /// 窗口是否有焦点
    pub focused: Mutex<bool>,
    /// 鼠标是否在窗口内
    pub cursor_inside: Mutex<bool>,
    /// 活跃的触摸点: id -> (x, y, force)
    pub touches: Mutex<HashMap<u64, (f32, f32, Option<f64>)>>,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            keys_down: Mutex::new(HashSet::new()),
            mouse_buttons_down: Mutex::new(HashSet::new()),
            modifiers: Mutex::new(Modifiers::NONE),
            scroll_delta: Mutex::new(ScrollDeltaAccum::default()),
            focused: Mutex::new(false),
            cursor_inside: Mutex::new(false),
            touches: Mutex::new(HashMap::new()),
        }
    }
}

/// 滚轮累计：按单位分桶。鼠标滚轮（Line）与触控板/高精度设备（Pixel）单位不同，
/// 直接相加会被触控板淹没。`take_scroll` 返回行单位，`take_scroll_pixel` 返回像素单位。
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct ScrollDeltaAccum {
    pub line: (f32, f32),
    pub pixel: (f32, f32),
}

// ------ Internal mapping functions ------

pub(crate) fn map_key_event(winit_event: &winit::event::KeyEvent) -> Option<KeyEvent> {
    let key = match winit_event.physical_key {
        winit::keyboard::PhysicalKey::Code(code) => code,
        winit::keyboard::PhysicalKey::Unidentified(_) => return None,
    };

    Some(KeyEvent {
        key,
        logical_key: winit_event.logical_key.clone(),
        state: winit_event.state,
        text: winit_event.text.as_ref().map(|s| s.to_string()),
        repeat: winit_event.repeat,
    })
}

pub(crate) fn map_scroll_delta(delta: winit::event::MouseScrollDelta) -> ScrollDelta {
    match delta {
        winit::event::MouseScrollDelta::LineDelta(x, y) => ScrollDelta::Line { x, y },
        winit::event::MouseScrollDelta::PixelDelta(pos) => ScrollDelta::Pixel {
            x: pos.x as f32,
            y: pos.y as f32,
        },
    }
}

pub(crate) fn map_modifiers(mods: &winit::keyboard::ModifiersState) -> Modifiers {
    let mut m = Modifiers::NONE;
    if mods.shift_key() {
        m = m | Modifiers::SHIFT;
    }
    if mods.control_key() {
        m = m | Modifiers::CTRL;
    }
    if mods.alt_key() {
        m = m | Modifiers::ALT;
    }
    if mods.super_key() {
        m = m | Modifiers::SUPER;
    }
    m
}

/// 逻辑像素触摸坐标 → `VireoWindow` 内部 `mouse_pos` 字段的**物理像素**。
///
/// `mouse_pos` 字段按约定存物理像素（`WindowEvent::CursorMoved` 给的就是物理），
/// 而 [`map_touch_event`] 的产物**已经是逻辑像素**。两条路径的坐标空间必须在这里
/// 对齐，否则 `mouse_pos()` 的 `.physical()` / `.logical()` 双表示会一边对一边错。
///
/// 历史上这里出过两次错：① 触摸完全不写 `mouse_pos`，Android 上恒 `(-1,-1)`；
/// ② 又除了第二遍 scale，`tap(844,551)` 在 scale 2.8125 下变成 `(300,196)` → `(37.9,24.8)`。
pub(crate) fn touch_logical_to_mouse_pos(lx: f32, ly: f32, scale_factor: f64) -> (f32, f32) {
    let sf = if scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    ((lx as f64 * sf) as f32, (ly as f64 * sf) as f32)
}

pub(crate) fn map_touch_event(touch: &winit::event::Touch, scale_factor: f64) -> TouchEvent {
    // 除数护栏：`scale_factor <= 0` 会产出 inf/NaN 坐标。
    // 实际路径上 `dpi_override` 非正值已被 `dpi.rs` 视为 None（走系统 scale），
    // 而 `Window::scale_factor()` 也不会返回 0 —— 但除法这里仍兜一层，
    // 免得将来新增调用点时静默产出 NaN 坐标（NaN 参与布局比较会静默失效）。
    let sf = if scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let phase = match touch.phase {
        winit::event::TouchPhase::Started => TouchPhase::Started,
        winit::event::TouchPhase::Moved => TouchPhase::Moved,
        winit::event::TouchPhase::Ended => TouchPhase::Ended,
        winit::event::TouchPhase::Cancelled => TouchPhase::Cancelled,
    };
    TouchEvent {
        id: touch.id,
        phase,
        x: (touch.location.x / sf) as f32,
        y: (touch.location.y / sf) as f32,
        force: touch.force.map(|f| match f {
            winit::event::Force::Calibrated {
                force,
                max_possible_force,
                ..
            } => {
                if max_possible_force > 0.0 {
                    force / max_possible_force
                } else {
                    force
                }
            }
            winit::event::Force::Normalized(v) => v,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Modifiers ----

    #[test]
    fn modifiers_default_is_none() {
        let m = Modifiers::default();
        assert_eq!(m, Modifiers::NONE);
        assert!(m.is_empty());
        assert!(!m.shift());
        assert!(!m.ctrl());
        assert!(!m.alt());
        assert!(!m.super_key());
    }

    #[test]
    fn modifiers_bitor_combines() {
        let m = Modifiers::SHIFT | Modifiers::CTRL;
        assert!(m.shift());
        assert!(m.ctrl());
        assert!(!m.alt());
    }

    #[test]
    fn modifiers_bitand_masks() {
        let m = (Modifiers::SHIFT | Modifiers::ALT) & Modifiers::SHIFT;
        assert_eq!(m, Modifiers::SHIFT);
    }

    #[test]
    fn modifiers_all_flags() {
        let m = Modifiers::SHIFT | Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER;
        assert!(m.shift());
        assert!(m.ctrl());
        assert!(m.alt());
        assert!(m.super_key());
        assert!(!m.is_empty());
    }

    // ---- map_scroll_delta ----

    #[test]
    fn map_scroll_line_delta() {
        let s = map_scroll_delta(winit::event::MouseScrollDelta::LineDelta(3.0, -2.0));
        if let ScrollDelta::Line { x, y } = s {
            assert_eq!(x, 3.0);
            assert_eq!(y, -2.0);
        } else {
            panic!("expected Line delta");
        }
    }

    #[test]
    fn map_scroll_pixel_delta() {
        use winit::dpi::PhysicalPosition;
        let s = map_scroll_delta(winit::event::MouseScrollDelta::PixelDelta(
            PhysicalPosition::new(120.0, -80.0),
        ));
        if let ScrollDelta::Pixel { x, y } = s {
            assert_eq!(x, 120.0);
            assert_eq!(y, -80.0);
        } else {
            panic!("expected Pixel delta");
        }
    }

    // ---- InputState ----

    #[test]
    fn input_state_key_tracking() {
        let state = InputState::default();
        state.keys_down.lock().insert(KeyCode::KeyW);
        assert!(state.keys_down.lock().contains(&KeyCode::KeyW));
        assert!(!state.keys_down.lock().contains(&KeyCode::KeyA));
    }

    #[test]
    fn input_state_scroll_accumulate() {
        let state = InputState::default();
        {
            let mut d = state.scroll_delta.lock();
            d.line.0 += 1.5;
            d.line.1 += -3.0;
            d.pixel.0 += 100.0;
            d.pixel.1 += -200.0;
        }
        let d = *state.scroll_delta.lock();
        assert_eq!(d.line, (1.5, -3.0));
        assert_eq!(d.pixel, (100.0, -200.0));
    }

    // ---- InputCallbacks ----

    // ---- 触摸坐标单位换算 ----

    fn touch_at(x: f64, y: f64) -> winit::event::Touch {
        winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase: winit::event::TouchPhase::Started,
            location: winit::dpi::PhysicalPosition::new(x, y),
            force: None,
            id: 0,
        }
    }

    #[test]
    fn map_touch_event_divides_by_scale_factor() {
        // 回归测试：`InputCallbacks::on_touch` 曾把 scale 硬编码成 1.0，
        // 导致所有 scale≠1 的平台（Windows 150% / Retina / Android 2.8x）
        // callback 路径给的是**物理像素**，与 `TouchEvent` 文档（逻辑坐标）
        // 以及 `InputState::touches` 轮询路径都不一致。
        let t = touch_at(300.0, 150.0);

        // scale = 1.0：恒等（这是桌面 100% 缩放，所以该 bug 长期未暴露）
        let e = map_touch_event(&t, 1.0);
        assert_eq!((e.x, e.y), (300.0, 150.0));

        // scale = 2.0：必须减半
        let e = map_touch_event(&t, 2.0);
        assert_eq!((e.x, e.y), (150.0, 75.0));

        // 真机 Android 值（HyperOS density 2.8125）
        let e = map_touch_event(&t, 2.8125);
        assert!((e.x - 300.0 / 2.8125).abs() < 1e-3);
        assert!((e.y - 150.0 / 2.8125).abs() < 1e-3);
    }

    #[test]
    fn map_touch_event_preserves_id_phase_force() {
        let mut t = touch_at(10.0, 20.0);
        t.id = 42;
        t.phase = winit::event::TouchPhase::Moved;
        t.force = Some(winit::event::Force::Normalized(0.75));
        let e = map_touch_event(&t, 2.0);
        assert_eq!(e.id, 42);
        assert_eq!(e.phase, TouchPhase::Moved);
        assert_eq!(e.force, Some(0.75));
    }

    #[test]
    fn map_touch_event_scale_zero_does_not_panic() {
        // 防御：scale 为 0 会产生 inf/NaN 坐标。虽不该发生（dpi_override 挡了非正值），
        // 但除法路径值得挡一下。
        let e = map_touch_event(&touch_at(300.0, 150.0), 0.0);
        assert!(e.x.is_finite() && e.y.is_finite(), "不能产生 inf/NaN");
    }

    #[test]
    fn touch_to_mouse_pos_round_trips_through_scale() {
        // `mouse_pos` 字段存物理，`TouchEvent` 存逻辑 —— 两者必须能互转，
        // 否则 `mouse_pos()` 的 `.physical()`/`.logical()` 双表示会一边对一边错。
        // 真机 tap 物理 (844,551)、scale 2.8125 → 逻辑 (300.1,195.9)。
        let sf = 2.8125;
        let logical = touch_logical_to_mouse_pos(300.1, 195.9, sf);
        assert!(
            (logical.0 - 844.0).abs() < 1.0 && (logical.1 - 551.0).abs() < 1.0,
            "逻辑 (300.1,195.9) 应还原成物理 (844,551)，实得 {logical:?}"
        );

        // 完整链路：winit 物理 touch → map_touch_event（逻辑）→ mouse_pos（物理）。
        let e = map_touch_event(&touch_at(844.0, 551.0), sf);
        let back = touch_logical_to_mouse_pos(e.x, e.y, sf);
        assert!(
            (back.0 - 844.0).abs() < 1.0 && (back.1 - 551.0).abs() < 1.0,
            "往返后应回到原始物理坐标，实得 {back:?}"
        );
        // 关键回归：逻辑值不能被二次除法缩小 2.8125 倍（历史 bug）。
        assert!(
            (e.x - 300.1).abs() < 1.0,
            "map_touch_event 之后必须已是逻辑坐标，实得 {}",
            e.x
        );
    }

    #[test]
    fn touch_to_mouse_pos_scale_zero_does_not_panic() {
        let p = touch_logical_to_mouse_pos(300.0, 150.0, 0.0);
        assert!(p.0.is_finite() && p.1.is_finite(), "不能产生 inf/NaN");
    }

    // ---- touch_points_sorted（VireoWindow::touches 的可测内核）----

    #[test]
    fn touch_points_sorted_orders_by_id() {
        let mut map = HashMap::new();
        // 故意乱序插入：HashMap 迭代顺序随机，排序必须把它整回来。
        map.insert(30u64, (3.0f32, 3.0f32, None));
        map.insert(10, (1.0, 1.0, None));
        map.insert(20, (2.0, 2.0, Some(0.5)));

        let pts = touch_points_sorted(&map, 1.0);
        assert_eq!(pts.len(), 3);
        assert_eq!(
            pts.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![10, 20, 30],
            "必须按 id 升序，否则「取第一个」每帧拿到不同手指"
        );
        assert_eq!(pts[1].force, Some(0.5));
        assert_eq!(pts[0].force, None);
    }

    #[test]
    fn touch_points_sorted_keeps_logical_and_derives_physical() {
        // map 里的值是**逻辑**像素（map_touch_event 已 ÷ scale）。
        let mut map = HashMap::new();
        map.insert(7u64, (300.0f32, 150.0f32, None));

        let pts = touch_points_sorted(&map, 2.8125);
        let (lx, ly) = pts[0].pos.logical();
        let (px, py) = pts[0].pos.physical();
        assert!((lx - 300.0).abs() < 1e-9, "逻辑值必须原样保留，实得 {lx}");
        assert!((ly - 150.0).abs() < 1e-9, "逻辑值必须原样保留，实得 {ly}");
        assert!((px - 843.75).abs() < 1e-6, "物理 = 逻辑 × scale，实得 {px}");
        assert!(
            (py - 421.875).abs() < 1e-6,
            "物理 = 逻辑 × scale，实得 {py}"
        );
    }

    #[test]
    fn touch_points_sorted_empty_map_yields_empty() {
        let map = HashMap::new();
        assert!(touch_points_sorted(&map, 2.0).is_empty());
    }

    #[test]
    fn touch_points_sorted_scale_non_positive_does_not_panic() {
        let mut map = HashMap::new();
        map.insert(1u64, (10.0f32, 20.0f32, None));
        for s in [0.0f64, -1.0] {
            let pts = touch_points_sorted(&map, s);
            assert!(
                pts[0].pos.physical().0.is_finite() && pts[0].pos.physical().1.is_finite(),
                "scale={s} 不能产生 inf/NaN"
            );
            // 兜底语义与 pixel_of 一致：非正 scale 时 px 直接取输入值。
            assert!((pts[0].pos.logical().0 - 10.0).abs() < 1e-9);
        }
    }
}
