//! # Vireo — 2D 渲染库
//!
//! ```rust
//! use vireo::prelude::*;
//! let mut app = App::new();
//! let win = app.window(WindowDesc::new("Hello", 400, 300), None::<fn()>);
//! app.run(move |app| {
//!     let mut batch = DrawBatch::new();
//!     draw_rectangle(&mut batch, Pos::new(10.0, 10.0), 100.0, 80.0, Some(RED));
//!     draw_text(&mut batch.texts, "Hello!", Pos::new(20.0, 20.0),
//!         TextDef::default().font_size(16.0), TextOverride::from_color(WHITE));
//!     app.window_ref(&win).unwrap().draw(Some(BLACK), &[&batch]);
//!     true
//! });
//! ```

pub mod area;
pub mod color;
pub mod context;
pub mod material;
pub mod glyphon;
pub mod gpu;
pub mod input;
pub mod offscreen;
pub mod shapes;
pub mod text;
pub mod texture;
pub mod window;

/// 一次导入所有常用类型。
pub mod prelude {
    pub use crate::area::Area;
    pub use crate::area::AreaGeom;
    pub use crate::color::colors::*;
    pub use crate::color::Color;
    pub use crate::color::{hsl_to_rgb, rgb_to_hsl};
    pub use crate::color_u8;
    pub use crate::context::DrawBatch;
    pub use crate::context::InheritFromParent;
    pub use crate::context::Pos;
    pub use crate::context::Rect;
    pub use crate::context::RenderTarget;
    pub use crate::context::Renderer;
    pub use crate::context::ShapeStats;
    pub use crate::context::Transform;
    pub use crate::context::UvRect;
    pub use crate::texture::Texture;
    pub use crate::material::Material;
    pub use crate::material::MATERIAL_TEX_SLOTS;
    pub use crate::material::MATERIAL_UNIFORM_SIZE;
    pub use crate::material::MaterialResource;
    pub use crate::material::MaterialResourceKind;
    pub use crate::material::MaterialResources;
    pub use crate::material::TexKind;
    pub use crate::material::TexSample;
    pub use crate::material::SampKind;
    pub use crate::material::CachePolicy;
    pub use crate::material::wgsl_snippets;
    pub use crate::material::expand_includes;
    pub use crate::gpu::GpuContext;
    pub use crate::gpu::Vertex;
    pub use crate::gpu::ShapeInstance;
    pub use crate::gpu::VIREO_TARGET_SHAPE;
    pub use crate::gpu::VIREO_TARGET_TEXT;
    pub use crate::shapes::*;
    pub use crate::text::Attrs;
    pub use crate::text::AttrsOwned;
    pub use crate::text::ColorMode;
    pub use crate::text::Family;
    pub use crate::text::FeatureTag;
    pub use crate::text::Style;
    pub use crate::text::TextAlign;
    pub use crate::text::TextDef;
    pub use crate::text::TextOverride;
    pub use crate::text::Weight;
    pub use crate::text::TextEntry;
    pub use crate::text::TextEntryList;
    pub use crate::text::TextTextureState;
    pub use crate::text::HudLine;
    pub use crate::text::StableText;
    pub use crate::text::TextPart;
    pub use crate::text::draw_hud_line;
    pub use crate::text::draw_text;
    pub use crate::text::draw_text_hud;
    pub use crate::text::draw_text_parts;
    pub use crate::text::split_hud;
    pub use crate::draw_text_hud;
    pub use crate::hud_format;
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

    pub use crate::window::ResizeRefreshPolicy;
    pub use crate::window::FollowAmount;
    pub use crate::window::FollowFramesOrTime;
    pub use crate::window::AntiAliasing;
    pub use crate::window::App;
    pub use crate::window::DrawOutcome;
    pub use crate::window::DrawReport;
    pub use crate::window::DrawSkipReason;
    pub use crate::window::DrawTimings;
    pub use crate::window::Fullscreen;
    pub use crate::window::Icon;
    pub use crate::window::LogicalSize;
    pub use crate::window::WindowDesc;
    pub use crate::window::WindowIndex;
    pub use crate::window::WindowLevel;
    pub use crate::window::WindowMetrics;
    pub use crate::offscreen::OffscreenCanvas;
    pub use crate::window::OffscreenIndex;
    pub use wgpu::PresentMode;
}

