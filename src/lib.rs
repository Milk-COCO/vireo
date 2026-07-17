pub mod color;
pub mod context;
pub mod gpu;
pub mod input;
pub mod offscreen;
pub mod shapes;
pub mod text;
pub mod texture;
pub mod window;

pub mod prelude {
    pub use crate::color::colors::*;
    pub use crate::color::Color;
    pub use crate::color_u8;
    pub use crate::context::DrawBatch;
    pub use crate::context::RenderTarget;
    pub use crate::context::Renderer;
    pub use crate::texture::RenderTexture;
    pub use crate::texture::Texture;
    pub use crate::gpu::GpuContext;
    pub use crate::gpu::Vertex;
    pub use crate::shapes::*;
    pub use crate::text::Attrs;
    pub use crate::text::AttrsOwned;
    pub use crate::text::ColorMode;
    pub use crate::text::Family;
    pub use crate::text::Style;
    pub use crate::text::TextAlign;
    pub use crate::text::Weight;
    pub use crate::text::TextEntry;
    pub use crate::text::TextEntryList;
    pub use crate::text::TextOptions;
    pub use crate::text::draw_text;

    // 输入系统
    pub use crate::input::ElementState;
    pub use crate::input::Key;
    pub use crate::input::KeyCode;
    pub use crate::input::KeyEvent;
    pub use crate::input::Modifiers;
    pub use crate::input::MouseButton;
    pub use crate::input::MouseButtonEvent;
    pub use crate::input::MouseScrollEvent;
    pub use crate::input::ScrollDelta;
    pub use crate::input::TouchEvent;
    pub use crate::input::TouchPhase;

    pub use crate::window::App;
    pub use crate::window::Fullscreen;
    pub use crate::window::Icon;
    pub use crate::window::LogicalSize;
    pub use crate::window::WindowDesc;
    pub use crate::window::WindowIndex;
    pub use crate::window::WindowLevel;
    pub use crate::offscreen::OffscreenCanvas;
    pub use crate::window::OffscreenIndex;
    pub use wgpu::PresentMode;
}
