//! on_* 回调宏 —— App 与 VireoWindow 的批量注册生成。
//!
//! 统计来源：`src/input.rs::InputCallbacks` 19 个 Vec 字段，映射 `on_*` 方法名 → 字段名 1:1。
//! - FnMut: on_key_down, on_key_up, on_mouse_down, on_mouse_up, on_scroll, on_touch,
//!         on_modifiers_changed, on_ime, on_file_dropped, on_file_hovered,
//!         on_moved, on_theme_changed, on_resized, on_thumb_button(windows)
//! - FnOnce: on_cursor_entered, on_cursor_left, on_focus_gained, on_focus_lost, on_file_hover_cancelled
//! 保持原方法签名（handle/泛型/callback 类型）与可见性不变，仅搬家为宏展开。

#[macro_export]
macro_rules! def_app_ons {
    (
        $(
            $(#[$meta:meta])*
            $fname:ident : $cb:ty => $field:ident
        ),* $(,)?
    ) => {
        $(
            $(#[$meta])*
            pub fn $fname(&mut self, handle: $crate::window::WindowIndex, callback: $cb) -> &mut Self {
                let h = handle.0;
                self.callbacks.entry(h).or_default().$field.push(Box::new(callback));
                self
            }
        )*
    };
}

#[macro_export]
macro_rules! def_window_ons {
    (
        $(
            $(#[$meta:meta])*
            $fname:ident : $cb:ty => $field:ident
        ),* $(,)?
    ) => {
        $(
            $(#[$meta])*
            pub fn $fname(&self, callback: $cb) -> &Self {
                let mut cbs = $crate::input::InputCallbacks::default();
                cbs.$field.push(Box::new(callback));
                let _ = self.cb_tx.send((self.handle, cbs));
                self
            }
        )*
    };
}
