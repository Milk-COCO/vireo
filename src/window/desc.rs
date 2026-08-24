use winit::window::{Cursor, Fullscreen, Icon, WindowLevel};

use crate::dpi::{dp, Pp};

/// 抗锯齿模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AntiAliasing {
    None,
    /// 多重采样：per-pixel 着色，硬件解析采样点覆盖。
    Msaa { samples: u32, alpha_to_coverage: bool },
    /// 超采样：per-sample 着色（`@interpolate(linear, sample)`），每个采样点独立计算 SDF。
    Ssaa { samples: u32, alpha_to_coverage: bool },
}

impl AntiAliasing {
    pub fn sample_count(&self) -> u32 {
        match self {
            AntiAliasing::None => 1,
            AntiAliasing::Msaa { samples, .. } | AntiAliasing::Ssaa { samples, .. } => *samples,
        }
    }

    pub fn alpha_to_coverage(&self) -> bool {
        match self {
            AntiAliasing::None => false,
            AntiAliasing::Msaa { alpha_to_coverage, .. } | AntiAliasing::Ssaa { alpha_to_coverage, .. } => *alpha_to_coverage,
        }
    }

    pub fn is_ssaa(&self) -> bool {
        matches!(self, AntiAliasing::Ssaa { .. })
    }
}

/// 窗口边框样式（跨平台统一枚举；平台差异见各变体注释）。
///
/// 取代两个独立布尔（`titlebar`/`border`）——布尔自由组合会产生「合法但无效」
/// 的状态（如 `titlebar(true) + border(false)`），枚举让每个值都有明确语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FrameStyle {
    /// 完整系统标题栏 + 边框（默认）。
    #[default]
    Normal,
    /// 无标题栏、无边框：客户区 == 窗口矩形（旧 frameless）。
    /// ## Platform-specific
    /// - **Windows**：客户区 = 整个窗口矩形，无系统 resize 热区（可配合
    ///   自定义 `drag_window`/`drag_resize_window` 手势）。
    /// - 其它平台：等价 winit `set_decorations(false)`，标题栏与边框一起移除；
    ///   `resizable(true)` 仍保留系统缩放。
    Frameless,
    /// 无标题栏但保留系统 resize 边框（= Electron `titleBarStyle: 'hidden'`）。
    /// ## Platform-specific
    /// - **Windows**：唯一真正「只去标题栏」的模式——保留 `WS_SIZEBOX` +
    ///   `WM_NCCALCSIZE` 非客户区 insets（约 8px），系统默认边缘 hit-test
    ///   自动接管缩放热区。
    /// - **macOS**：隐藏标题栏文本 + 透明标题栏 + 内容区延伸到红绿灯下，
    ///   红绿灯保留并由系统自动接管（`with_title_hidden` +
    ///   `with_titlebar_transparent` + `with_fullsize_content_view` =
    ///   Electron `titleBarStyle: 'hidden'`）。
    ///   **注意**：winit 0.30 的 `with_titlebar_hidden` 实现为 `Borderless`
    ///   （红绿灯/边框全部消失），与 `Frameless` 等效，**不是**本变体想要的
    ///   语义——故此处用 `with_title_hidden`（只藏文本、保留红绿灯）。
    /// - **其它平台：与 [`FrameStyle::Frameless`] 行为相同**（winit 无法只去
    ///   标题栏，边框随装饰整体移除）。
    HiddenTitlebar,
}

impl FrameStyle {
    /// 是否保留系统装饰整体（winit `set_decorations` 参数）。
    /// - Windows / 其它平台：`Normal` 才保留；`Frameless`/`HiddenTitlebar`
    ///   都整体去装饰（winit 无法只去标题栏）。
    /// - **macOS**：`HiddenTitlebar` **保留**装饰（红绿灯按钮），配合原生
    ///   `with_title_hidden` + `with_titlebar_transparent` +
    ///   `with_fullsize_content_view` 实现 Electron `titleBarStyle: 'hidden'`
    ///   （`with_titlebar_hidden` 在 winit 0.30 实为 `Borderless`，不采用）。
    pub(crate) fn decorated(self) -> bool {
        #[cfg(target_os = "macos")]
        {
            matches!(self, FrameStyle::Normal | FrameStyle::HiddenTitlebar)
        }
        #[cfg(not(target_os = "macos"))]
        {
            matches!(self, FrameStyle::Normal)
        }
    }

    /// 是否绘制系统标题栏（仅 `Normal`）。
    pub(crate) fn has_titlebar(self) -> bool {
        matches!(self, FrameStyle::Normal)
    }

    /// 是否保留系统 resize 边框（除 `Frameless` 外均保留）。
    pub(crate) fn has_border(self) -> bool {
        !matches!(self, FrameStyle::Frameless)
    }
}

/// 把 AA 的 sample_count snap 到 `supported` 中 ≤ 请求的最大项。
/// 不可用 `min(req, max)`：列表可能是 `[1,4]`（无 2/8），硬截到 8 仍会在 pipeline 创建时 panic。
pub(crate) fn clamp_aa(aa: AntiAliasing, supported: &[u32]) -> AntiAliasing {
    let snap = |req: u32| -> u32 {
        let req = req.max(1);
        supported
            .iter()
            .copied()
            .filter(|&c| c <= req)
            .max()
            .unwrap_or(1)
    };
    match aa {
        AntiAliasing::None => AntiAliasing::None,
        AntiAliasing::Msaa { samples, alpha_to_coverage } => AntiAliasing::Msaa {
            samples: snap(samples),
            alpha_to_coverage,
        },
        AntiAliasing::Ssaa { samples, alpha_to_coverage } => AntiAliasing::Ssaa {
            samples: snap(samples),
            alpha_to_coverage,
        },
    }
}

/// `winit::raw_window_handle::RawWindowHandle` 的 Send+Sync 包装（`RawWindowHandle` 本身
/// 未实现 Send/Sync，但内部只是平台原生指针/整数句柄，跨线程 Move 安全——与 winit 的
/// `SendSyncRawWindowHandle` 相同策略）。
#[derive(Clone, Copy, Debug)]
pub struct SendRawWindowHandle(pub winit::raw_window_handle::RawWindowHandle);
unsafe impl Send for SendRawWindowHandle {}
unsafe impl Sync for SendRawWindowHandle {}

pub struct WindowDesc {
    pub title: String,
    /// 初始客户区尺寸（每轴意图 [`Pp`]：`Dp` = vireo 逻辑像素，`Px` = 物理像素）。
    /// 在 `create_attrs` 时才结合最终 `dpi_override` 换算为 winit `Size`。
    /// [`WindowDesc::new`] 与 [`WindowDesc::size`] 的裸数值按意图解释。
    pub size: (Pp, Pp),
    /// vireo 层自定义 dpi 覆盖：`Some(v)` = **vireo 全自持像素**（vireo 逻辑为源真相，
    /// 物理 = vireo 逻辑 × v，窗口对 OS 的系统 DPI 缩放被忽略）；`None`（默认）=
    /// vireo 逻辑即 winit 逻辑（OS 系统 DPI 正常参与）。见 [`WindowDesc::dpi_override`]。
    pub dpi_override: Option<f64>,
    pub min_size: Option<(Pp, Pp)>,
    pub max_size: Option<(Pp, Pp)>,
    pub position: Option<(Pp, Pp)>,
    /// 父窗口句柄（rwh_06，Windows/X11 子窗口）。`None` = 顶层窗口。
    pub parent_window: Option<SendRawWindowHandle>,
    pub resizable: bool,
    pub fullscreen: Option<Fullscreen>,
    pub maximized: bool,
    pub visible: bool,
    /// 创建时先隐藏，首帧渲染完成后再显示（`visible=true` 时）。默认 `true`——
    /// winit 建窗是先以默认尺寸+边框显示、再改尺寸/去边框，`preparable` 让窗口
    /// 第一次出现即「正确尺寸 + 无边框 + 已渲染内容」，消除 4 阶段闪烁。
    /// 设 `false` 回到旧行为（创建即显示，可能短暂闪烁）。
    pub preparable: bool,
    pub transparent: bool,
    /// 窗口边框样式（标题栏 + resize 边框的组合语义）。默认 [`FrameStyle::Normal`]。
    ///
    /// 见 [`FrameStyle`] 各变体文档；`HiddenTitlebar` 仅在 Windows 上与
    /// `Frameless` 有实际差别（其它平台都退化为整体去装饰）。
    pub frame_style: FrameStyle,
    pub window_level: WindowLevel,
    pub window_icon: Option<Icon>,
    pub theme: Option<winit::window::Theme>,
    pub resize_increments: Option<(Pp, Pp)>,
    pub content_protected: bool,
    pub active: bool,
    pub cursor: Cursor,
    pub enabled_buttons: winit::window::WindowButtons,
    pub blur: bool,
    pub present_mode: wgpu::PresentMode,
    pub anti_aliasing: AntiAliasing,
    /// 期望最大在途帧（`SurfaceConfiguration::desired_maximum_frame_latency`）。
    /// DX12 下 swapchain buffer 数 = latency + 1：默认 2 → 3 buffer（CPU 可超前
    /// 2 帧，无 vsync 时峰值更高）；设 1 → 2 buffer（在途封顶 1，vsync 拖动时
    /// camera 时差更小）。可在创建后经 [`VireoWindow::set_frame_latency`] 运行时调整。
    pub frame_latency: u32,
}

impl WindowDesc {
    /// 创建窗口描述。裸宽高按 **vireo 逻辑像素** 处理（vireo 用户坐标系）——
    /// `Some(dpi_override)` 下物理 = 逻辑 × dpi，`None` 下物理 = 逻辑 × OS 系统 DPI。
    /// 需要物理像素时用 [`WindowDesc::size`] builder 显式声明 `Px` 意图。
    pub fn new(title: &str, width: u32, height: u32) -> Self {
        Self {
            title: title.to_string(),
            size: (Pp::Dp(dp(width as f64)), Pp::Dp(dp(height as f64))),
            dpi_override: None,
            min_size: None,
            max_size: None,
            position: None,
            parent_window: None,
            resizable: true,
            fullscreen: None,
            maximized: false,
            visible: true,
            preparable: true,
            transparent: false,
            frame_style: FrameStyle::Normal,
            window_level: WindowLevel::default(),
            window_icon: None,
            theme: None,
            resize_increments: None,
            content_protected: false,
            active: true,
            cursor: Cursor::default(),
            enabled_buttons: winit::window::WindowButtons::all(),
            blur: false,
            present_mode: wgpu::PresentMode::AutoVsync,
            anti_aliasing: AntiAliasing::None,
            frame_latency: 2,
        }
    }

    /// 自定义 vireo 层 dpi 覆盖（vireo 逻辑像素 → 物理像素换算因子）。
    ///
    /// - `None`（默认）：vireo 逻辑即 winit 逻辑，OS 系统 DPI 正常参与
    ///   （物理 = 逻辑 × OS 缩放）。
    /// - `Some(v)`：**vireo 全自持像素**——vireo 逻辑为源真相，物理 = 逻辑 × v，
    ///   窗口对 OS 的系统 DPI 缩放被忽略（150% 显示器上物理窗口会显得比其它应用小，
    ///   普通 UI 应用需斟酌；适合「以固定像素设计」的游戏/谱面编辑器）。
    /// - `Some(1.0)`：逻辑 = 物理（旧 `high_dpi(true)` 行为）。
    ///
    /// 这是 vireo 层的坐标约定，**不**设置 winit 的 `scale_factor_override`——窗口的
    /// `dpi_override` 只参与 vireo 内部 scale / logical 换算、鼠标坐标换算与
    /// `metrics().scale_factor`，以及本 desc 尺寸族字段的物理化。运行时可用
    /// [`VireoWindow::set_dpi_override`] 切换（保持 vireo 逻辑尺寸、resize 物理窗口）。
    pub fn dpi_override(mut self, dpi: Option<f64>) -> Self {
        debug_assert!(dpi.map_or(true, |d| d.is_finite() && d > 0.0), "dpi_override must be None or finite >0");
        self.dpi_override = dpi;
        self
    }

    /// 显式设置初始客户区尺寸，覆盖 [`WindowDesc::new`] 的默认值。尺寸族
    /// （`size`/`min_size`/`max_size`/`resize_increments`）与 `position` 的数值
    /// 均由调用点类型声明意图（[`Pp::Px`] = 物理像素，[`Pp::Dp`] = vireo 逻辑像素；
    /// 裸数值默认 = [`Pp::Dp`]），不再随 `dpi_override` 翻转语义。
    pub fn size<W: Into<Pp>, H: Into<Pp>>(mut self, width: W, height: H) -> Self {
        self.size = (width.into(), height.into());
        self
    }

    pub fn min_size<W: Into<Pp>, H: Into<Pp>>(mut self, width: W, height: H) -> Self {
        self.min_size = Some((width.into(), height.into()));
        self
    }

    pub fn max_size<W: Into<Pp>, H: Into<Pp>>(mut self, width: W, height: H) -> Self {
        self.max_size = Some((width.into(), height.into()));
        self
    }

    pub fn position<W: Into<Pp>, H: Into<Pp>>(mut self, x: W, y: H) -> Self {
        self.position = Some((x.into(), y.into()));
        self
    }

    /// 设置父窗口句柄（rwh_06 `RawWindowHandle`），本窗口成为其子窗口。
    /// `None`（默认）= 顶层窗口。
    ///
    /// ## Safety
    /// 传入的句柄必须有效且在本窗口存活期间保持存在（rwh_06 winit 契约）。
    ///
    /// ## Platform-specific
    /// - **Windows**：子窗口带 `WS_CHILD`，被限制在父窗口客户区内。
    /// - **X11**：子窗口被限制在父窗口客户区内。
    /// - **Android / iOS / Wayland / Web**：不支持。
    pub unsafe fn parent_window(mut self, handle: winit::raw_window_handle::RawWindowHandle) -> Self {
        self.parent_window = Some(SendRawWindowHandle(handle));
        self
    }

    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    pub fn fullscreen(mut self, fullscreen: Fullscreen) -> Self {
        self.fullscreen = Some(fullscreen);
        self
    }

    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// 创建时先隐藏、首帧渲染完成后再显示（`visible=true` 时生效）。默认 `true`。
    /// 见字段文档；设 `false` 回到创建即显示的旧行为。
    pub fn preparable(mut self, preparable: bool) -> Self {
        self.preparable = preparable;
        self
    }

    pub fn transparent(mut self, transparent: bool) -> Self {
        self.transparent = transparent;
        self
    }

    /// 窗口边框样式（标题栏 + resize 边框的组合语义）。默认
    /// [`FrameStyle::Normal`]。见 [`FrameStyle`] 各变体文档。
    pub fn frame_style(mut self, frame_style: FrameStyle) -> Self {
        self.frame_style = frame_style;
        self
    }

    pub fn window_level(mut self, level: WindowLevel) -> Self {
        self.window_level = level;
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.window_icon = Some(icon);
        self
    }

    /// 从图片文件加载窗口图标（PNG/JPG/BMP）
    pub fn icon_from_path(mut self, path: impl AsRef<std::path::Path>) -> Self {
        if let Ok(data) = std::fs::read(path.as_ref()) {
            if let Ok(img) = image::load_from_memory(&data) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if let Ok(icon) = Icon::from_rgba(rgba.into_raw(), w, h) {
                    self.window_icon = Some(icon);
                }
            }
        }
        self
    }

    pub fn theme(mut self, theme: winit::window::Theme) -> Self {
        self.theme = Some(theme);
        self
    }

    pub fn resize_increments<W: Into<Pp>, H: Into<Pp>>(mut self, width: W, height: H) -> Self {
        self.resize_increments = Some((width.into(), height.into()));
        self
    }

    pub fn content_protected(mut self, protected: bool) -> Self {
        self.content_protected = protected;
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn cursor(mut self, cursor: Cursor) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn enabled_buttons(mut self, buttons: winit::window::WindowButtons) -> Self {
        self.enabled_buttons = buttons;
        self
    }

    pub fn blur(mut self, blur: bool) -> Self {
        self.blur = blur;
        self
    }

    pub fn present_mode(mut self, mode: wgpu::PresentMode) -> Self {
        self.present_mode = mode;
        self
    }

    /// 期望最大在途帧（`desired_maximum_frame_latency`）。DX12 下 buffer 数 =
    /// latency + 1；默认 2（3 buffer）。设为 1 则只有 2 buffer（vsync 拖动时
    /// camera 时差更小，但无 vsync 峰值下降）。详见 [`WindowDesc::frame_latency`]。
    pub fn frame_latency(mut self, latency: u32) -> Self {
        self.frame_latency = latency;
        self
    }

    pub fn anti_aliasing(mut self, aa: AntiAliasing) -> Self {
        self.anti_aliasing = aa;
        self
    }
}
