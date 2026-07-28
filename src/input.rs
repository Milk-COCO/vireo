use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

// ------ Re-exports (winit types direct) ------

pub use winit::event::ElementState;
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

/// 修饰键位标志
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

// ------ InputCallbacks ------

/// 输入事件回调集合（运行在 winit 线程，无需 +Send 约束）。
/// `unsafe impl Send`：`App.callbacks` 在移到渲染线程前已被抽空，
/// 不包含实际非 Send 数据。
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
}

// SAFETY: InputCallbacks 仅在 winit 线程使用。App 移入渲染线程前 self.callbacks 已被抽空。
unsafe impl Send for InputCallbacks {}

impl Default for InputCallbacks {
    fn default() -> Self {
        Self {
            on_key_down: Vec::new(),
            on_key_up: Vec::new(),
            on_mouse_down: Vec::new(),
            on_mouse_up: Vec::new(),
            on_scroll: Vec::new(),
            on_cursor_entered: Vec::new(),
            on_cursor_left: Vec::new(),
            on_touch: Vec::new(),
            on_focus_gained: Vec::new(),
            on_focus_lost: Vec::new(),
            on_modifiers_changed: Vec::new(),
        }
    }
}

// ------ InputState ------

/// 持久的输入状态（VireoWindow 内部持有）
pub struct InputState {
    /// 当前按下的键集合（不包含 repeat 事件）
    pub keys_down: RefCell<HashSet<KeyCode>>,
    /// 当前按下的鼠标按钮集合
    pub mouse_buttons_down: RefCell<HashSet<MouseButton>>,
    /// 当前修饰键状态
    pub modifiers: RefCell<Modifiers>,
    /// 本帧滚轮增量累计（按单位分开，避免鼠标滚轮 vs 触控板的单位混淆）
    pub scroll_delta: RefCell<ScrollDeltaAccum>,
    /// 窗口是否有焦点
    pub focused: RefCell<bool>,
    /// 鼠标是否在窗口内
    pub cursor_inside: RefCell<bool>,
    /// 活跃的触摸点: id -> (x, y, force)
    pub touches: RefCell<HashMap<u64, (f32, f32, Option<f64>)>>,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            keys_down: RefCell::new(HashSet::new()),
            mouse_buttons_down: RefCell::new(HashSet::new()),
            modifiers: RefCell::new(Modifiers::NONE),
            scroll_delta: RefCell::new(ScrollDeltaAccum::default()),
            focused: RefCell::new(false),
            cursor_inside: RefCell::new(false),
            touches: RefCell::new(HashMap::new()),
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
        winit::event::MouseScrollDelta::PixelDelta(pos) => {
            ScrollDelta::Pixel {
                x: pos.x as f32,
                y: pos.y as f32,
            }
        }
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

pub(crate) fn map_touch_event(
    touch: &winit::event::Touch,
    scale_factor: f64,
) -> TouchEvent {
    let phase = match touch.phase {
        winit::event::TouchPhase::Started => TouchPhase::Started,
        winit::event::TouchPhase::Moved => TouchPhase::Moved,
        winit::event::TouchPhase::Ended => TouchPhase::Ended,
        winit::event::TouchPhase::Cancelled => TouchPhase::Cancelled,
    };
    TouchEvent {
        id: touch.id,
        phase,
        x: (touch.location.x / scale_factor) as f32,
        y: (touch.location.y / scale_factor) as f32,
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
    fn modifiers_single_flag() {
        let m = Modifiers::SHIFT;
        assert!(m.shift());
        assert!(!m.ctrl());
        assert!(!m.alt());
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
    fn input_state_default() {
        let state = InputState::default();
        assert!(state.keys_down.borrow().is_empty());
        assert!(state.mouse_buttons_down.borrow().is_empty());
        assert_eq!(*state.modifiers.borrow(), Modifiers::NONE);
        assert_eq!(*state.scroll_delta.borrow(), ScrollDeltaAccum::default());
        assert!(!*state.focused.borrow());
        assert!(!*state.cursor_inside.borrow());
        assert!(state.touches.borrow().is_empty());
    }

    #[test]
    fn input_state_key_tracking() {
        let state = InputState::default();
        state.keys_down.borrow_mut().insert(KeyCode::KeyW);
        assert!(state.keys_down.borrow().contains(&KeyCode::KeyW));
        assert!(!state.keys_down.borrow().contains(&KeyCode::KeyA));
    }

    #[test]
    fn input_state_scroll_accumulate() {
        let state = InputState::default();
        {
            let mut d = state.scroll_delta.borrow_mut();
            d.line.0 += 1.5;
            d.line.1 += -3.0;
            d.pixel.0 += 100.0;
            d.pixel.1 += -200.0;
        }
        let d = *state.scroll_delta.borrow();
        assert_eq!(d.line, (1.5, -3.0));
        assert_eq!(d.pixel, (100.0, -200.0));
    }

    // ---- InputCallbacks ----

    #[test]
    fn input_callbacks_empty_by_default() {
        let cb = InputCallbacks::default();
        assert!(cb.on_key_down.is_empty());
        assert!(cb.on_key_up.is_empty());
        assert!(cb.on_mouse_down.is_empty());
        assert!(cb.on_mouse_up.is_empty());
        assert!(cb.on_scroll.is_empty());
        assert!(cb.on_cursor_entered.is_empty());
        assert!(cb.on_cursor_left.is_empty());
        assert!(cb.on_touch.is_empty());
        assert!(cb.on_focus_gained.is_empty());
        assert!(cb.on_focus_lost.is_empty());
        assert!(cb.on_modifiers_changed.is_empty());
    }

    #[test]
    fn input_callbacks_push() {
        let mut cb = InputCallbacks::default();
        cb.on_key_down.push(Box::new(|_| {}));
        cb.on_mouse_down.push(Box::new(|_| {}));
        assert_eq!(cb.on_key_down.len(), 1);
        assert_eq!(cb.on_mouse_down.len(), 1);
    }
}
