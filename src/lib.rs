//! # Vireo
//!
//! 2D 渲染库，基于 [wgpu](https://crates.io/crates/wgpu) + [winit](https://crates.io/crates/winit)。
//!
//! ## 特性
//!
//! - SDF 与几何双路径形状绘制（16 种内置形状）
//! - 自定义 WGSL fragment 材质
//! - [cosmic-text](https://crates.io/crates/cosmic-text) shaping + [glyphon](https://crates.io/crates/glyphon) 光栅化；整段 shape 缓存，HUD 分段文字（`TextPart::Glyphs` 按需单字 shape，等宽步进对齐）
//! - 多窗口、多循环线程模型
//! - DPI 感知与像素意图类型（`Px` / `Dp`）
//! - Stencil 裁切（Area include / exclude / ∩ / ∪）
//!
//! ## 支持平台
//!
//! Windows（DX12 / Vulkan）、Linux（Vulkan）、macOS（Metal）。
//! 目前仅在 Windows 上做过测试，其余平台未验证。
//!
//! ## 快速上手
//!
//! ```no_run
//! use vireo::prelude::*;
//!
//! #[vireo::main]
//! fn main() {
//!     init_logger();
//!     let app = App::new();
//!     let win = app.window(
//!         WindowDesc::new("Hello Vireo", 800, 600),
//!         None,
//!     );
//!     app.run(move |cx: &mut LoopContext| {
//!         let mut batch = DrawBatch::new();
//!         draw_rectangle(&mut batch, Pos::new(100.0, 100.0), 200.0, 120.0, RED);
//!         draw_circle(&mut batch, Pos::new(400.0, 300.0), 60.0, BLUE);
//!         draw_text(
//!             &mut batch.texts,
//!             "Hello Vireo!",
//!             Pos::new(200.0, 400.0),
//!             TextDef::default().font_size(32.0),
//!             TextOverride::from_color(WHITE),
//!         );
//!         draw_text_parts(
//!             &mut batch.texts,
//!             &[
//!                 TextPart::normal("Score: "),
//!                 TextPart::glyphs("12345"),
//!             ],
//!             Pos::new(16.0, 16.0),
//!             TextDef::default().font_size(20.0),
//!             TextOverride::from_color(WHITE),
//!         );
//!         if let Ok(w) = cx.window_ref(&win) {
//!             w.draw(BLACK, &[&batch]);
//!         }
//!         true
//!     });
//! }
//! ```
//!
//! ## 从哪开始
//!
//! - `examples/hello.rs` — 最小窗口 + 一个矩形
//! - `examples/input.rs` — 键盘/鼠标/触摸输入
//! - `examples/batch_view.rs` — transform 与坐标系
//! - `examples/text_hud.rs` — 文字与 HUD 分段
//! - `examples/frame_stats.rs` — 帧率与性能诊断
//! - `examples/` 目录下还有 50+ 个示例，覆盖形状、材质、多窗口、resize 等场景
//!
//! ## 模块概览
//!
//! | 模块 | 说明 |
//! |------|------|
//! | [`app`] | 应用入口、窗口/材质/离屏管理 |
//! | [`render`] | `DrawBatch` + `Renderer`，顶点合并、transform table、stencil 裁切 |
//! | [`shapes`] | 16 种形状（SDF / 几何 / 实例三路径） |
//! | [`text`] | 文字绘制、shape 缓存、HUD 分段、StableText |
//! | [`material`] | 自定义 WGSL 材质、纹理/采样器/bind group |
//! | [`gpu`] | `GpuContext`、`Vertex`、pipeline 缓存 |
//! | [`window`] | `VireoWindow`、`WindowDesc`、resize 策略、present mode |
//! | [`thread`] | 多循环线程模型 |
//! | [`dpi`] | `Px` / `Dp` / `ToPx` 像素意图类型 |
//! | [`color`] | RGBA 颜色 + 预定义常量 |
//! | [`math`] | `Rect` / `Pos` / `Transform` / `UvRect` |
//! | [`input`] | 键盘/鼠标/触摸/IME 输入 |
//! | [`area`] | Area stencil 裁切（include / exclude / ∩ / ∪） |
//! | [`offscreen`] | 离屏渲染 canvas |
//! | [`texture`] | 贴图加载与管理 |
//! | [`nc`] | 非客户区 hit-test 类型 |
//! | [`platform`] | 平台特化 API（`windows` / `macos`） |
#![allow(clippy::too_many_arguments, clippy::type_complexity)]
pub mod area;

#[doc(hidden)]
pub use vireo_macro::main;

pub use crate::app::App;
pub mod app;
pub mod thread;

pub mod color;
pub mod dpi;
pub mod error;
pub mod glyphon;
pub mod gpu;
pub mod input;
pub mod material;
pub mod math;
pub mod nc;
pub mod offscreen;
pub mod particle;
pub mod platform;
pub mod render;
pub mod shapes;
pub mod text;
pub mod texture;
pub mod window;

/// 初始化日志（透传 `env_logger`，`RUST_LOG` 生效）。应用 `main` 首行调用一次。
pub fn init_logger() {
    let _ = env_logger::try_init();
}

/// 一次导入所有常用类型。
pub mod prelude {
    pub use crate::area::Area;
    pub use crate::area::AreaGeom;
    pub use crate::color::Color;
    pub use crate::color::colors::*;
    pub use crate::color::{hsl_to_rgb, rgb_to_hsl};
    pub use crate::color_u8;
    pub use crate::draw_text_hud;
    pub use crate::gpu::ClockIndex;
    pub use crate::gpu::GpuContext;
    pub use crate::gpu::ParticleInstance;
    pub use crate::gpu::ShapeInstance;
    pub use crate::gpu::VIREO_TARGET_SHAPE;
    pub use crate::gpu::VIREO_TARGET_TEXT;
    pub use crate::gpu::Vertex;
    pub use crate::hud_format;
    pub use crate::material::CachePolicy;
    pub use crate::material::MATERIAL_TEX_SLOTS;
    pub use crate::material::MATERIAL_UNIFORM_SIZE;
    pub use crate::material::Material;
    pub use crate::material::MaterialResource;
    pub use crate::material::MaterialResourceKind;
    pub use crate::material::MaterialResources;
    pub use crate::material::SampKind;
    pub use crate::material::TexKind;
    pub use crate::material::TexSample;
    pub use crate::material::expand_includes;
    pub use crate::material::wgsl_snippets;
    pub use crate::render::BatchOverride;
    pub use crate::render::DrawBatch;
    pub use crate::render::InheritFromParent;
    pub use crate::render::Pos;
    pub use crate::render::Rect;
    pub use crate::render::RenderTarget;
    pub use crate::render::Renderer;
    pub use crate::render::ShapeStats;
    pub use crate::render::Transform;
    pub use crate::render::UvRect;
    pub use crate::shapes::*;
    pub use crate::text::Attrs;
    pub use crate::text::AttrsOwned;
    pub use crate::text::ColorMode;
    pub use crate::text::Family;
    pub use crate::text::FeatureTag;
    pub use crate::text::HudLine;
    pub use crate::text::StableText;
    pub use crate::text::Style;
    pub use crate::text::TextAlign;
    pub use crate::text::TextDef;
    pub use crate::text::TextEntry;
    pub use crate::text::TextEntryList;
    pub use crate::text::TextOverride;
    pub use crate::text::TextPart;
    pub use crate::text::TextTextureState;
    pub use crate::text::Weight;
    pub use crate::text::draw_hud_line;
    pub use crate::text::draw_text;
    pub use crate::text::draw_text_hud;
    pub use crate::text::draw_text_parts;
    pub use crate::text::split_hud;
    pub use crate::texture::Texture;
    // 输入系统
    pub use crate::input::ElementState;
    pub use crate::input::Ime;
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

    pub use crate::app::App;
    pub use crate::window::AntiAliasing;
    pub use crate::window::CursorGrabMode;
    pub use crate::window::DrawOutcome;
    pub use crate::window::DrawReport;
    pub use crate::window::DrawSkipReason;
    pub use crate::window::DrawTimings;
    pub use crate::window::ExternalError;
    pub use crate::window::FollowAmount;
    pub use crate::window::FollowFramesOrTime;
    pub use crate::window::FrameStyle;
    pub use crate::window::Fullscreen;
    pub use crate::window::Icon;
    pub use crate::window::ImePurpose;
    pub use crate::window::LogicalPosition;
    pub use crate::window::LogicalSize;
    pub use crate::window::MonitorHandle;
    pub use crate::window::NotSupportedError;
    pub use crate::window::PhysicalPosition;
    pub use crate::window::PhysicalSize;
    pub use crate::window::Position;
    pub use crate::window::ResizeDirection;
    pub use crate::window::ResizeRefreshPolicy;
    pub use crate::window::Size;
    pub use crate::window::Theme;
    pub use crate::window::UserAttentionType;
    pub use crate::window::VideoModeHandle;
    pub use crate::window::WindowButtons;
    pub use crate::window::WindowDesc;
    pub use crate::window::WindowId;
    pub use crate::window::WindowIndex;
    pub use crate::window::WindowLevel;
    // 非客户区管理（§7.6）
    pub use crate::dpi::Dp;
    pub use crate::dpi::Pixel;
    pub use crate::dpi::PixelPos;
    pub use crate::dpi::PixelSize;
    pub use crate::dpi::Pp;
    pub use crate::dpi::Px;
    pub use crate::dpi::ToPx;
    pub use crate::dpi::dp;
    pub use crate::dpi::px;
    pub use crate::error::VireoError;
    pub use crate::nc::HitTestInput;
    pub use crate::nc::NonClientHit;
    pub use crate::nc::NonClientRegion;
    pub use crate::nc::WindowState;
    pub use crate::offscreen::OffscreenCanvas;
    pub use crate::particle::Particle;
    pub use crate::particle::ParticlePool;
    pub use crate::particle::ParticleSlot;
    pub use crate::particle::draw_particles;
    pub use crate::thread::Loop;
    pub use crate::thread::LoopContext;
    pub use crate::thread::Thread;
    pub use crate::thread::ThreadHandle;
    pub use crate::window::OffscreenIndex;
    pub use wgpu::AddressMode;
    pub use wgpu::BlendComponent;
    pub use wgpu::BlendFactor;
    pub use wgpu::BlendOperation;
    pub use wgpu::BlendState;
    pub use wgpu::PresentMode;
    // `#[vireo::main]` 过程宏：把 `async fn main` 改写为在主线程跑 winit 的入口。
    pub use vireo_macro::main;
}
