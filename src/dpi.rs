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
/// 与 [`crate::window::WindowDesc`] 对应 builder 均接受 `impl ToPx`，由调用点声明该值是物理
/// 还是逻辑像素，消除 `WindowDesc` 尺寸族「随 `dpi_override` 翻转语义」的问题。
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

/// [`crate::window::WindowDesc`] 尺寸/位置字段的**意图**存储（`Dp` = vireo 逻辑像素 /
/// `Px` = 物理像素），在 `create_attrs` 时才结合最终 `dpi_override` 换算——
/// 避免 builder 调用顺序（如 `.size(...)` 先于/晚于 `.dpi_override(...)`）
/// 影响结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DescDim {
    /// vireo 逻辑像素（用户坐标系，一对 f64）。
    Dp(f64, f64),
    /// winit/物理像素（一对 f64）。
    Px(f64, f64),
}

/// 把两个 `impl ToPx` 编码成 [`DescDim`]（用于 builder / setter 的统一换算）。
pub(crate) fn desc_dim_from<W: ToPx, H: ToPx>(w: W, h: H) -> DescDim {
    if w.is_px() {
        DescDim::Px(w.to_px(1.0).0, h.to_px(1.0).0)
    } else {
        DescDim::Dp(w.to_px(1.0).0, h.to_px(1.0).0)
    }
}

/// `DescDim` → winit `Size`：`Px` 意图直接物理；`Dp` 意图按 `dpi_override`：
/// `Some(d)` → 物理 = 逻辑 × d（vireo 全自持像素），`None` → winit 逻辑。
pub(crate) fn dim_to_winit_size(d: DescDim, dpi_override: Option<f64>) -> Size {
    let phys = |w: f64, h: f64| {
        Size::Physical(PhysicalSize::new(
            (w).round().max(1.0) as u32,
            (h).round().max(1.0) as u32,
        ))
    };
    match d {
        DescDim::Px(w, h) => phys(w, h),
        DescDim::Dp(w, h) => match dpi_override {
            Some(d) if d > 0.0 => phys(w * d, h * d),
            _ => Size::Logical(LogicalSize::new(w, h)),
        },
    }
}

/// `DescDim` → winit `Position`：同 [`dim_to_winit_size`] 的 dpi 语义。
pub(crate) fn dim_to_winit_position(d: DescDim, dpi_override: Option<f64>) -> Position {
    let phys = |x: f64, y: f64| {
        Position::Physical(PhysicalPosition::new(x.round() as i32, y.round() as i32))
    };
    match d {
        DescDim::Px(x, y) => phys(x, y),
        DescDim::Dp(x, y) => match dpi_override {
            Some(d) if d > 0.0 => phys(x * d, y * d),
            _ => Position::Logical(LogicalPosition::new(x, y)),
        },
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

/// 物理尺寸 → 逻辑尺寸。有效缩放 = `dpi_override.unwrap_or(os_scale)`：
/// `Some(v)` 覆盖下 vireo 逻辑为源真相（物理 = 逻辑 × v → 逻辑 = 物理 / v）；
/// `None` 用 OS 系统 DPI（逻辑 = 物理 / OS 缩放）。
pub(crate) fn logical_size(
    width: u32,
    height: u32,
    dpi_override: Option<f64>,
    os_scale: f64,
) -> (u32, u32) {
    let scale = dpi_override.unwrap_or(os_scale);
    if scale <= 0.0 {
        (width, height)
    } else {
        (
            (width as f64 / scale) as u32,
            (height as f64 / scale) as u32,
        )
    }
}