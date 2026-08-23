//! vireo 层的逻辑/物理像素意图类型与换算。
//!
//! 与 `winit::dpi`（`LogicalSize` / `PhysicalSize` / `Size` / `Position` 等）是两个抽象层：
//! - `winit::dpi` 是 winit 的窗口/平台输入输出尺寸；换算都 `assert!(validate_scale_factor)`。
//! - 本模块是 vireo 用户坐标系的像素语义：裸数值默认按逻辑像素（`Dp` 语义），`Px` 显式
//!   物理；由调用点声明意图，不随 `dpi_override` 翻转。

use winit::dpi::{
    LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, Position, Size,
};

/// 物理像素（一维标量，f64）。见 [`ToPx`]。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Px(pub f64);

/// 逻辑像素（vireo 用户坐标系，一维标量，f64）。见 [`ToPx`]。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dp(pub f64);

/// 从物理像素数值显式构造（模块级便捷函数，等价 `Px(v)`）。
pub const fn px(v: f64) -> Px {
    Px(v)
}

/// 从逻辑像素（vireo 坐标）数值显式构造（模块级便捷函数，等价 `Dp(v)`）。
pub const fn dp(v: f64) -> Dp {
    Dp(v)
}

impl From<f64> for Px {
    fn from(v: f64) -> Self {
        Px(v)
    }
}

impl From<f64> for Dp {
    fn from(v: f64) -> Self {
        Dp(v)
    }
}

/// 可转成物理像素的标量值。物理像素 = 逻辑像素 × dpi 换算因子。
///
/// 目前只有两种实现：
/// - [`Px`]：已经是物理像素（`to_px` 原样返回）。
/// - [`Dp`]：vireo 逻辑像素（`to_px(dpi)` = 值 × dpi）。
///
/// 所有位置/尺寸 setter（`set_size` / `set_min_size` / `set_max_size` /
/// `set_outer_position` / `set_cursor_position` / `set_resize_increments`）
/// 与 [`crate::window::WindowDesc`] 对应 builder 均接受 `impl Into<Pp>`，由调用点声明该值是物理
/// 还是逻辑像素（裸数值默认 = [`Dp`]），消除 `WindowDesc` 尺寸族「随 `dpi_override` 翻转语义」的问题。
pub trait ToPx {
    /// 在给定 `dpi`（逻辑→物理换算因子）下得到物理像素。
    fn to_px(self, dpi: f64) -> Px;

    /// 该值是否「已经是物理像素」（[`Px`] 为真，[`Dp`] 为假）。
    /// 用于 [`crate::window::WindowDesc`] 的意图存储与转换辅助，两种类型都实现。
    fn is_px(&self) -> bool;
}

impl ToPx for Px {
    #[inline]
    fn to_px(self, _dpi: f64) -> Px {
        self
    }
    #[inline]
    fn is_px(&self) -> bool {
        true
    }
}

impl ToPx for Dp {
    #[inline]
    fn to_px(self, dpi: f64) -> Px {
        Px(self.0 * dpi)
    }
    #[inline]
    fn is_px(&self) -> bool {
        false
    }
}

macro_rules! impl_to_px_primitive {
    ($($t:ty),*) => {
        $(
            impl ToPx for $t {
                #[inline]
                fn to_px(self, dpi: f64) -> Px {
                    Px(self as f64 * dpi)
                }
                #[inline]
                fn is_px(&self) -> bool {
                    false
                }
            }
        )*
    };
}
// 原始数值默认按 vireo 逻辑像素（[`Dp`]）处理；需要物理像素用 [`Px`]。
impl_to_px_primitive!(f64, f32, i32, u32, i64, u64, i16, u16, i8, u8, isize, usize);

/// 像素尺度意图：一个值要么是物理像素（[`Px`]），要么是 vireo 逻辑像素（[`Dp`]）。
///
/// `Pp` = "**P**ixel，取 **P**x 或 Dp 之一"，变体名即它的两义。
///
/// 所有位置/尺寸 setter 与 builder（[`crate::window::WindowDesc::size`] 等）接收
/// `impl Into<Pp>`：裸数值与 [`Dp`] 默认按逻辑像素（[`Pp::Dp`]），[`Px`] 显式物理。
///
/// 实现 [`ToPx`]：物理意图原样返回，逻辑意图按 dpi 换算。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pp {
    /// vireo 逻辑像素（用户坐标系）。
    Dp(Dp),
    /// winit/物理像素。
    Px(Px),
}

impl Default for Pp {
    fn default() -> Self {
        Pp::Dp(Dp(0.0))
    }
}

impl From<Px> for Pp {
    #[inline]
    fn from(v: Px) -> Self {
        Pp::Px(v)
    }
}

impl From<Dp> for Pp {
    #[inline]
    fn from(v: Dp) -> Self {
        Pp::Dp(v)
    }
}

impl From<f64> for Pp {
    #[inline]
    fn from(v: f64) -> Self {
        Pp::Dp(Dp(v))
    }
}

impl ToPx for Pp {
    #[inline]
    fn to_px(self, dpi: f64) -> Px {
        match self {
            Pp::Px(px) => px,
            Pp::Dp(dp) => dp.to_px(dpi),
        }
    }
    #[inline]
    fn is_px(&self) -> bool {
        matches!(self, Pp::Px(_))
    }
}

macro_rules! impl_from_primitive_for_pp {
    ($($t:ty),*) => {
        $(
            impl From<$t> for Pp {
                #[inline]
                fn from(v: $t) -> Self {
                    Pp::Dp(Dp(v as f64))
                }
            }
        )*
    };
}
// 原始数值默认按 vireo 逻辑像素（`Pp::Dp`）处理（`f64` 已有显式 `From<f64>`）；
// 需要物理像素用 [`Px`]。
impl_from_primitive_for_pp!(f32, i32, u32, i64, u64, i16, u16, i8, u8, isize, usize);

/// 一个数值的物理 + 逻辑双表示快照（由窗口 getter 返回）。
///
/// `.px`/`.dp` 都指向同一「位置或尺寸」，只是单位不同：
/// - `.px`：物理像素（`Px`）
/// - `.dp`：vireo 逻辑像素（`Dp`，= `.px` ÷ 当前 scale_factor）
///
/// 不实现 [`ToPx`]（已是快照，无换算语义）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pixel {
    /// 物理像素
    pub px: Px,
    /// vireo 逻辑像素（=.px ÷ scale_factor）
    pub dp: Dp,
}

/// 位置 getter（`inner_position` / `outer_position`）的返回值：两轴各带
/// 物理 + 逻辑双表示。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelPos {
    pub x: Pixel,
    pub y: Pixel,
}

impl PixelPos {
    /// 便捷：物理像素坐标 (px, py)。
    pub fn physical(&self) -> (f64, f64) {
        (self.x.px.0, self.y.px.0)
    }
    /// 便捷：逻辑像素坐标 (dp_x, dp_y)。
    pub fn logical(&self) -> (f64, f64) {
        (self.x.dp.0, self.y.dp.0)
    }
}

/// 尺寸 getter（`outer_size` / `resize_increments`）的返回值：宽/高各带物理 +
/// 逻辑双表示。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelSize {
    pub width: Pixel,
    pub height: Pixel,
}

impl PixelSize {
    /// 便捷：物理像素尺寸 (w_px, h_px)。
    pub fn physical(&self) -> (f64, f64) {
        (self.width.px.0, self.height.px.0)
    }
    /// 便捷：vireo 逻辑尺寸 (w_dp, h_dp)。
    pub fn logical(&self) -> (f64, f64) {
        (self.width.dp.0, self.height.dp.0)
    }
}

/// [`Pp`] → 物理像素值。有效缩放 = `dpi_override.unwrap_or(os_scale)`：
///
/// - [`Pp::Px`]：物理意图，原样返回。
/// - [`Pp::Dp`]：vireo 逻辑像素 → 物理 = 值 × 有效缩放。
///
/// 每轴独立换算，因此两轴（宽/高、x/y）意图可以混用（如宽用物理、高用逻辑）。
#[inline]
pub(crate) fn pp_to_phys(v: Pp, dpi_override: Option<f64>, os_scale: f64) -> f64 {
    let scale = dpi_override.unwrap_or(os_scale);
    match v {
        Pp::Px(px) => px.0,
        Pp::Dp(dp) => dp.0 * if scale > 0.0 { scale } else { 1.0 },
    }
}

/// [`Pp`] × 意图对 + 有效缩放 → winit `Size`。
///
/// - 两轴均无物理意图：`Some(d > 0)` → Physical（vireo 全自持像素）；
///   否则 → winit Logical（OS DPI 参与）。
/// - 任一轴为 [`Pp::Px`]：强制 winit **Physical**，各轴按 [`pp_to_phys`] 独立换算，
///   宽/高意图可以混用。
pub(crate) fn dim_to_winit_size(
    w: Pp,
    h: Pp,
    dpi_override: Option<f64>,
    os_scale: f64,
) -> Size {
    let phys = |w: f64, h: f64| {
        debug_assert!(w.is_finite() && h.is_finite(), "dim_to_winit_size: size must be finite");
        Size::Physical(PhysicalSize::new(
            (w).round().max(1.0) as u32,
            (h).round().max(1.0) as u32,
        ))
    };
    match (w, h) {
        (Pp::Dp(w), Pp::Dp(h)) => match dpi_override {
            Some(d) if d > 0.0 => phys(w.0 * d, h.0 * d),
            _ => Size::Logical(LogicalSize::new(w.0, h.0)),
        },
        _ => {
            let w = pp_to_phys(w, dpi_override, os_scale);
            let h = pp_to_phys(h, dpi_override, os_scale);
            phys(w, h)
        }
    }
}

/// [`Pp`] × 意图对 + 有效缩放 → winit `Position`：同 [`dim_to_winit_size`] 的语义。
pub(crate) fn dim_to_winit_position(
    x: Pp,
    y: Pp,
    dpi_override: Option<f64>,
    os_scale: f64,
) -> Position {
    let phys = |x: f64, y: f64| {
        Position::Physical(PhysicalPosition::new(x.round() as i32, y.round() as i32))
    };
    match (x, y) {
        (Pp::Dp(x), Pp::Dp(y)) => match dpi_override {
            Some(d) if d > 0.0 => phys(x.0 * d, y.0 * d),
            _ => Position::Logical(LogicalPosition::new(x.0, y.0)),
        },
        _ => {
            let x = pp_to_phys(x, dpi_override, os_scale);
            let y = pp_to_phys(y, dpi_override, os_scale);
            phys(x, y)
        }
    }
}

/// 物理像素值 + 当前缩放 → [`Pixel`] 双表示（逻辑 = 物理 ÷ scale）。
pub(crate) fn pixel_of(v: f64, scale: f64) -> Pixel {
    Pixel {
        px: Px(v),
        dp: Dp(if scale > 0.0 { v / scale } else { v }),
    }
}

/// 物理像素坐标 → [`PixelPos`] 双表示。
pub(crate) fn to_pixel_pos(x: f64, y: f64, scale: f64) -> PixelPos {
    PixelPos { x: pixel_of(x, scale), y: pixel_of(y, scale) }
}

/// 物理像素尺寸 → [`PixelSize`] 双表示。
pub(crate) fn to_pixel_size(w: f64, h: f64, scale: f64) -> PixelSize {
    PixelSize { width: pixel_of(w, scale), height: pixel_of(h, scale) }
}