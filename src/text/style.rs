//! 文本样式：图层栈（描边 / 投影 / 发光 / 内阴影 / 渐变字 / 纹理字）。
//!
//! 模型只有一条原语：**对字形的有符号距离场取带软边的阈值**。
//! 一层 = 一次阈值 + 一种填充；描边、投影、发光都是这一条的参数组合，
//! 所以新增效果不需要改着色器（见 `docs/reference/text-style.md`）。
//!
//! 单位是「像素意图」（[`Pp`]）：裸数值 / [`Dp`] 按 vireo 逻辑像素，[`Px`] 按物理像素，
//! 由 prepare 阶段按当前 `dpi_scale` 折算成设备像素 —— 所以 DPI 变化时描边的视觉宽度不变。

use std::sync::Arc;

use crate::color::Color;
use crate::dpi::{Pp, ToPx};
use crate::math::UvRect;

/// 一层的填充来源。
#[derive(Clone, Debug)]
pub enum TextPaint {
    /// 纯色。
    Solid(Color),
    /// 线性渐变：复用引擎的渐变通道（与 `draw_rect_gradient` 同一套取色规则）。
    Gradient(TextGradient),
    /// 批基础贴图（`DrawBatch::set_texture` 的那张）。
    ///
    /// `rect` 语义与 [`TextGradient::rect`] 相同：给了就按页面矩形铺（整行一张图），
    /// 不给就是每个字形各自铺满。
    Texture {
        uv: UvRect,
        rect: Option<(Pp, Pp, Pp, Pp)>,
    },
}

impl TextPaint {
    pub fn solid(color: Color) -> Self {
        Self::Solid(color)
    }

    pub fn texture(uv: UvRect) -> Self {
        Self::Texture { uv, rect: None }
    }

    /// 纹理填充 + 页面矩形参照系。
    pub fn texture_in_rect(
        uv: UvRect,
        x: impl Into<Pp>,
        y: impl Into<Pp>,
        w: impl Into<Pp>,
        h: impl Into<Pp>,
    ) -> Self {
        Self::Texture {
            uv,
            rect: Some((x.into(), y.into(), w.into(), h.into())),
        }
    }
}

/// 文字用的线性渐变：角度 + 停点，语义与 [`crate::shapes::LinearGradient`] 一致，
/// 但**拥有**停点（`TextStyle` 要能长期存在）。
///
/// `rect` 决定渐变坐标的参照系：
/// - `Some((x, y, w, h))`：跨满这个**页面矩形**（物理/逻辑按 `Pp` 意图折算）—— 标题、
///   卡片这类"整段文字一个渐变"用它；
/// - `None`：退化成**每个字形独立**的渐变（字形框内 0..1），只适合单字装饰。
#[derive(Clone, Debug)]
pub struct TextGradient {
    /// 度，顺时针；0 = 自上而下，90 = 自左而右。
    pub angle_deg: f32,
    pub stops: Vec<crate::shapes::GradientStop>,
    /// 页面矩形参照系；`None` = 逐字形。
    pub rect: Option<(Pp, Pp, Pp, Pp)>,
}

impl TextGradient {
    pub fn new(angle_deg: f32, stops: Vec<crate::shapes::GradientStop>) -> Self {
        Self {
            angle_deg,
            stops,
            rect: None,
        }
    }

    /// 指定页面矩形（跨满这个框）。
    pub fn in_rect(
        mut self,
        x: impl Into<Pp>,
        y: impl Into<Pp>,
        w: impl Into<Pp>,
        h: impl Into<Pp>,
    ) -> Self {
        self.rect = Some((x.into(), y.into(), w.into(), h.into()));
        self
    }

    /// 两色竖直（自上而下）。
    pub fn vertical(from: Color, to: Color) -> Self {
        Self::new(
            0.0,
            vec![
                crate::shapes::GradientStop::new(0.0, from),
                crate::shapes::GradientStop::new(1.0, to),
            ],
        )
    }
}

/// 合成方式（预乘空间）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextBlend {
    /// 普通覆盖（后画的盖住前面）。
    #[default]
    Over,
    /// 相加（发光叠加）。
    Add,
}

/// 一层：对距离场取**区间** `(cutoff, threshold]` + 偏移 + 填充。
///
/// `cutoff == None` 时退化成半空间 `d > threshold`（外描边/投影/发光）；
/// 给了 `cutoff` 就是一条**带**（内描边 = `(−w, 0]`）。
#[derive(Clone, Debug)]
pub struct TextLayer {
    /// 阈值下界：`> 0` 字形外侧（外描边 / 投影），`< 0` 内侧（内描边），`0` = 轮廓。
    pub threshold: Pp,
    /// 上界：给 `Some` 就只画 `cutoff < d < threshold` 的**带**（内描边/内阴影必须用它，
    /// 否则半空间会涂满整个字内、再被填充盖住）。
    pub cutoff: Option<Pp>,
    /// 软边（≈ 模糊半径）：距离场下边缘按误差函数过渡，0 = 硬边（1px 解析 AA）。
    pub softness: Pp,
    /// 偏移。
    pub offset: (Pp, Pp),
    pub paint: TextPaint,
    pub blend: TextBlend,
    /// 画在**填充之上**（内描边/内阴影必须为 `true`，否则会被填充盖住）。
    pub above_fill: bool,
    /// 内阴影：用「字形内部 − 偏移后的覆盖率」求值（需要两个采样点）。
    ///
    /// 由 [`TextStyle::inner_shadow`] 设置；手写层一般用不到。
    pub inner_shadow: bool,
}

impl TextLayer {
    /// 硬边一层（无偏移、无软边、半空间、画在填充之下）。
    pub fn hard(threshold: impl Into<Pp>, paint: TextPaint) -> Self {
        Self {
            threshold: threshold.into(),
            cutoff: None,
            softness: Pp::Dp(crate::dpi::Dp(0.0)),
            offset: (Pp::Dp(crate::dpi::Dp(0.0)), Pp::Dp(crate::dpi::Dp(0.0))),
            paint,
            blend: TextBlend::Over,
            above_fill: false,
            inner_shadow: false,
        }
    }

    /// 硬边**带**：只画 `cutoff < d < threshold`（内描边 / 内阴影用）。
    pub fn band(cutoff: impl Into<Pp>, threshold: impl Into<Pp>, paint: TextPaint) -> Self {
        let mut layer = Self::hard(threshold, paint);
        layer.cutoff = Some(cutoff.into());
        layer.above_fill = true;
        layer
    }

    /// 软边一层（半空间）。
    pub fn soft(threshold: impl Into<Pp>, softness: impl Into<Pp>, paint: TextPaint) -> Self {
        let mut layer = Self::hard(threshold, paint);
        layer.softness = softness.into();
        layer
    }

    pub fn offset(mut self, x: impl Into<Pp>, y: impl Into<Pp>) -> Self {
        self.offset = (x.into(), y.into());
        self
    }

    pub fn blend(mut self, blend: TextBlend) -> Self {
        self.blend = blend;
        self
    }

    /// 画在填充之上 / 之下。
    pub fn above_fill(mut self, above: bool) -> Self {
        self.above_fill = above;
        self
    }

    /// 折算成设备像素：(阈值, cutoff, 软边, 偏移 x/y, 该层向字形外侧的最大外扩)。
    pub(crate) fn to_px(&self, dpi: f64) -> TextLayerPx {
        let threshold = self.threshold.to_px(dpi).0 as f32;
        let cutoff = self.cutoff.map(|c| c.to_px(dpi).0 as f32);
        let softness = self.softness.to_px(dpi).0.max(0.0) as f32;
        let ox = self.offset.0.to_px(dpi).0 as f32;
        let oy = self.offset.1.to_px(dpi).0 as f32;
        // 3σ 之外的 erf 尾部不足 1/255，按 3σ 计外扩（外加 1px 兜底）。
        // 带（cutoff 在上方）也一样：带的外缘是 max(threshold, cutoff)。
        let outer = threshold.max(cutoff.unwrap_or(f32::NEG_INFINITY));
        let reach = outer + softness * 3.0 + 1.0 + ox.abs().max(oy.abs());
        TextLayerPx {
            threshold,
            cutoff,
            softness,
            offset: (ox, oy),
            reach: reach.max(threshold.abs() + 1.0),
        }
    }
}

/// 一层折算成设备像素后的结果（内部用）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct TextLayerPx {
    pub threshold: f32,
    pub cutoff: Option<f32>,
    pub softness: f32,
    pub offset: (f32, f32),
    /// 该层向外的最大外扩（决定图集 padding 与 band）。
    pub reach: f32,
}

/// 描边连接方式（几何描边用；距离场描边天然是圆角连接）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextJoin {
    /// 尖角：延长到自然交点，超过 `miter_limit` 退化成 `Bevel`（与 SVG 一致）。
    Miter,
    /// 斜切：两段各切一刀。
    #[default]
    Bevel,
    /// 圆角。
    Round,
}

/// 线帽（只对开放轮廓有意义；字形轮廓通常闭合）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextCap {
    #[default]
    Butt,
    Square,
    Round,
}

/// **几何**描边：从字体轮廓做真正的描边（有 miter/bevel 连接），不是距离场取阈。
///
/// 与 [`TextStyle::outline`] 的区别：
/// - 距离场描边：圆角连接、任意宽度都平滑、开销低（推荐默认）；
/// - 几何描边：尖角/斜切精确（SVG 观感），但每次宽度变化都要重新栅格化。
#[derive(Clone, Debug)]
pub struct TextStroke {
    /// 描边宽度（总宽，即两侧各半宽）。
    pub width: Pp,
    pub color: Color,
    pub join: TextJoin,
    pub cap: TextCap,
    /// `Miter` 的上限（超过就退化成 bevel），默认 4。
    pub miter_limit: f32,
}

impl TextStroke {
    pub fn new(width: impl Into<Pp>, color: Color) -> Self {
        Self {
            width: width.into(),
            color,
            join: TextJoin::Miter,
            cap: TextCap::Butt,
            miter_limit: 4.0,
        }
    }

    pub fn join(mut self, join: TextJoin) -> Self {
        self.join = join;
        self
    }

    pub fn cap(mut self, cap: TextCap) -> Self {
        self.cap = cap;
        self
    }

    pub fn miter_limit(mut self, limit: f32) -> Self {
        self.miter_limit = limit.max(1.0);
        self
    }
}

/// 折算成设备像素的几何描边参数（内部用，`Copy`，便于塞进 `AreaMeta`）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct TextStrokePx {
    pub width: f32,
    pub color: Color,
    pub join: u8,
    pub cap: u8,
    pub miter_limit: f32,
}

impl TextStrokePx {
    pub(crate) fn is_empty(&self) -> bool {
        self.width <= 0.0
    }
}

/// 文本样式：有序图层 + 填充。
///
/// 绘制顺序：`layers` 从**下到上**依次叠加，最后画 `fill`。
#[derive(Clone, Debug, Default)]
pub struct TextStyle {
    pub layers: Vec<TextLayer>,
    /// 最后绘制；`None` = 用 `TextOverride` / `TextDef` 给的默认颜色。
    pub fill: Option<TextPaint>,
    /// 几何描边（真 miter/bevel）。画在所有图层与 fill **之下**。
    pub vector_stroke: Option<TextStroke>,
}

impl TextStyle {
    pub fn new() -> Self {
        Self::default()
    }

    fn empty_with(layers: Vec<TextLayer>, fill: Option<TextPaint>) -> Self {
        Self {
            layers,
            fill,
            vector_stroke: None,
        }
    }

    /// 外描边（硬边，`width` 向外）。
    pub fn outline(width: impl Into<Pp>, color: Color) -> Self {
        Self::empty_with(vec![TextLayer::hard(width, TextPaint::solid(color))], None)
    }

    /// 内描边（字形内侧的一条**带** `(−w, 0]`，画在填充之上）。
    pub fn inner_outline(width: impl Into<Pp>, color: Color) -> Self {
        let w = width.into();
        Self::empty_with(
            vec![TextLayer::band(
                negate_pp(w),
                Pp::Dp(crate::dpi::Dp(0.0)),
                TextPaint::solid(color),
            )],
            None,
        )
    }

    /// 内阴影：字形内的暗部（`字形内部 − 偏移后的轮廓`），画在填充之上。
    pub fn inner_shadow(
        offset: (impl Into<Pp>, impl Into<Pp>),
        blur: impl Into<Pp>,
        color: Color,
    ) -> Self {
        let mut layer = TextLayer::hard(Pp::Dp(crate::dpi::Dp(0.0)), TextPaint::solid(color));
        layer.softness = blur.into();
        layer.offset = (offset.0.into(), offset.1.into());
        layer.above_fill = true;
        layer.inner_shadow = true;
        Self::empty_with(vec![layer], None)
    }

    /// 硬/软投影。
    pub fn shadow(
        offset: (impl Into<Pp>, impl Into<Pp>),
        blur: impl Into<Pp>,
        color: Color,
    ) -> Self {
        Self::empty_with(
            vec![
                TextLayer::soft(Pp::Dp(crate::dpi::Dp(0.0)), blur, TextPaint::solid(color))
                    .offset(offset.0, offset.1),
            ],
            None,
        )
    }

    /// 发光（无偏移的软阴影）。
    pub fn glow(blur: impl Into<Pp>, color: Color) -> Self {
        Self::empty_with(
            vec![TextLayer::soft(
                Pp::Dp(crate::dpi::Dp(0.0)),
                blur,
                TextPaint::solid(color),
            )],
            None,
        )
    }

    /// 几何描边（真 miter/bevel）：垫在填充与所有图层之下。
    pub fn vector_outline(stroke: TextStroke) -> Self {
        Self {
            layers: Vec::new(),
            fill: None,
            vector_stroke: Some(stroke),
        }
    }

    /// 加一条几何描边（可与距离场图层共存）。
    pub fn with_vector_stroke(mut self, stroke: TextStroke) -> Self {
        self.vector_stroke = Some(stroke);
        self
    }

    pub fn with_layer(mut self, layer: TextLayer) -> Self {
        self.layers.push(layer);
        self
    }

    pub fn with_fill(mut self, paint: TextPaint) -> Self {
        self.fill = Some(paint);
        self
    }

    /// 该样式向字形外侧的最大外扩（设备像素），用于 band 与视觉包围盒。
    pub(crate) fn reach_px(&self, dpi: f64) -> f32 {
        let mut reach = 1.0f32;
        for layer in &self.layers {
            reach = reach.max(layer.to_px(dpi).reach);
        }
        reach
    }

    /// 是否需要走样式通道（有图层，或显式指定了填充色）。
    ///
    /// 纯默认样式（无层、无 fill）等价于"没有样式"，走旧路径零开销。
    pub(crate) fn needs_style(&self) -> bool {
        !self.layers.is_empty() || self.fill.is_some()
    }
}

/// 取负（像素意图的负数）。
fn negate_pp(p: Pp) -> Pp {
    match p {
        Pp::Dp(v) => Pp::Dp(crate::dpi::Dp(-v.0)),
        Pp::Px(v) => Pp::Px(crate::dpi::Px(-v.0)),
    }
}

/// 共享句柄：`TextDef` 要被 `TextEntry` clone，样式必须便宜。
pub type TextStyleRef = Arc<TextStyle>;

// ---------------------------------------------------------------------------
// GPU 编码（样式表）
// ---------------------------------------------------------------------------

/// 单条样式在 GPU 上的记录（3 × vec4 = 48B）。
pub(crate) const STYLE_STRIDE_U32: usize = 12;
/// 单层（4 × vec4 = 64B）。
pub(crate) const LAYER_STRIDE_U32: usize = 16;

/// 距离场表示方式：**精确按尺寸 SDF** 与 **MSDF** 并存，由这里选择。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextSdfMode {
    /// 默认：带样式文字在合适时走 MSDF，否则精确 SDF；无样式文字保持覆盖度。
    #[default]
    Auto,
    /// 尽量用 MSDF（超出 range 阶梯或像素字体时 warn-once 后仍回退精确）；
    /// 无样式文字也会走 MSDF（顶点色填充）。
    Msdf,
    /// 一律用精确按尺寸 SDF（质量保险丝；无样式文字仍是覆盖度）。
    Exact,
}

/// 选 MSDF 档位：返回 `(生成 em, range(em 比例))`；`None` = 该字号/外扩不适合 MSDF。
pub(crate) fn pick_msdf(mode: TextSdfMode, em_px: f32, reach_px: f32) -> Option<(f32, f32)> {
    // ⚠️ `Auto` **暂时不用 MSDF**：MSDF 条目体积大（每档 4B/px + range padding），
    // 满屏带样式文字会把图集顶到 `grow()`，而 `grow()` 会重新栅格化**全部**已缓存字形
    // —— 实测 100 字投影 821ms/帧。等图集生命周期（预分配/不抖动）修好再切回默认。
    // 现在 MSDF 只能显式开启（`TextSdfMode::Msdf`），用于 A/B 与逐轮回归对比。
    if mode != TextSdfMode::Msdf || em_px <= 0.0 {
        return None;
    }
    let reach_em = reach_px.max(0.0) / em_px;
    let range_em = crate::glyphon::msdf_pick_range_em(reach_em)?;
    Some((crate::glyphon::msdf_pick_em(em_px), range_em))
}

/// **尺寸阶梯**：精确 SDF 的生成字号档（1.25× 步进，从 12px 到 256px）。
///
/// 为什么要有：精确 SDF 的缓存键含字号，改字号就得重栅格化。按阶梯取「不小于目标字号」
/// 的一档生成、再双线性缩放到目标字号，就得到和 MSDF 一样的「档内改字号不重建」，
/// 而条目仍是 R8 单通道、无 range padding。
///
/// 生成尺寸 ≥ 目标尺寸 ⇒ 永远是**缩小**采样（清晰度损失小于 1.25× 放大）；
/// 代价是条目像素最多多 1.56×。
pub(crate) const SDF_SIZE_LADDER: [f32; 32] = [
    12.0, 15.0, 19.0, 24.0, 30.0, 37.0, 46.0, 57.0, 71.0, 89.0, 111.0, 139.0, 174.0, 218.0, 256.0,
    256.0, 256.0, 256.0, 256.0, 256.0, 256.0, 256.0, 256.0, 256.0, 256.0, 256.0, 256.0, 256.0,
    256.0, 256.0, 256.0, 256.0,
];

/// 取「不小于 `em_px` 的最小阶梯尺寸」。超出最大档用最大档。
pub(crate) fn sdf_ladder_em(em_px: f32) -> f32 {
    for size in SDF_SIZE_LADDER {
        if size >= em_px {
            return size;
        }
    }
    SDF_SIZE_LADDER[SDF_SIZE_LADDER.len() - 1]
}

/// 精确 SDF 的带宽档位。
///
/// **为什么是档位而不是 `ceil(reach)`**：band 进了字形缓存键，任何一点变化都会让所有
/// 带样式的字形**重新栅格化**（从轮廓算距离）。按 1px / 1/8px 递增时，拖一次描边宽度
/// 滑块就会连续几十帧重算全屏文字 —— 卡的就是这里。档位够粗之后，调宽度几乎不重建。
///
/// 代价：档位定得大一点 = 精度略降（8bit 铺在 ±band 上：band=4 约 0.031px/级、
/// band=8 约 0.063、band=16 约 0.125、band=32 约 0.25），对 1px 级 AA 完全够用；
/// padding 也跟着变大（2*band），所以只留 4 档。
const BAND_TIERS: [f32; 5] = [4.0, 8.0, 16.0, 32.0, 64.0];

/// 带宽上限（像素）：超过它的外扩会被钳制（着色器里饱和区域强制 0 覆盖，
/// 所以表现是"描边被切掉"，而不是"整块矩形被涂满"）。
pub(crate) const BAND_MAX: f32 = 64.0;

/// 把「样式外扩」映射到带宽档位（向上取档）。
pub(crate) fn band_for_reach(reach: f32) -> f32 {
    for tier in BAND_TIERS {
        if reach <= tier {
            return tier;
        }
    }
    BAND_TIERS[BAND_TIERS.len() - 1]
}
/// 单个渐变停点（2 × vec4 = 32B）。
pub(crate) const STOP_STRIDE_U32: usize = 8;

/// 填充类型（与着色器里的 `paint_kind` 一致）。
pub(crate) const PAINT_DEFAULT: u32 = 0;
pub(crate) const PAINT_SOLID: u32 = 1;
pub(crate) const PAINT_GRADIENT: u32 = 2;
pub(crate) const PAINT_TEXTURE: u32 = 3;
/// 只画图层、不画 fill（彩色字形垫底用；填充由彩色字形自己负责）。
pub(crate) const PAINT_NONE: u32 = 4;

/// 层标志位（编码进 `paint.w`）：有 cutoff ⇒ 区间；否则半空间。
pub(crate) const LAYER_FLAG_CUTOFF: u32 = 1;
/// 画在填充之上（内描边/内阴影）。
pub(crate) const LAYER_FLAG_ABOVE: u32 = 2;
/// **内阴影**：`cov = 字形内部(未偏移) − 偏移后的覆盖率`（两个采样点相减）。
///
/// 只用一个采样点做差是错的：`coverage(d) − coverage(d + w)` 在字形内部恒为 0，
/// 只剩边缘一条缝 —— 看起来就像发光而不是内阴影。
pub(crate) const LAYER_FLAG_INNER_SHADOW: u32 = 4;

/// 样式表的 pass 选择（编码进顶点的 `style_index` 高 8 位）。
///
/// 关键点：pass 是**按层**而不是"所有下层一起"——只有每层单独一趟，才能让相邻字形
/// 的同一层先全部铺完（字符串级并集），而不是后一个字的描边盖住前一个字的描边/发光。
/// - `1..=8`：画**填充之下**的第 `pass-1` 层
/// - `9`：画填充
/// - `10..=17`：画**填充之上**的第 `pass-10` 层
pub(crate) const PASS_FILL: u32 = 9;
pub(crate) const PASS_SHIFT: u32 = 24;

/// 填充之下的第 `layer` 层。
pub(crate) fn pass_below(layer: u32) -> u32 {
    1 + layer
}

/// 填充之上的第 `layer` 层。
pub(crate) fn pass_above(layer: u32) -> u32 {
    10 + layer
}

/// 样式表：CPU 侧累积、去重，最后整块上传到 `group(1) @binding(1..3)`。
#[derive(Default)]
pub(crate) struct StyleTable {
    /// `styles`：`(layer_first, layer_count, fill_kind, 0)` + `fill_color` + `fill_aux`
    pub(crate) styles: Vec<u32>,
    /// `layers`：`geom` + `paint` + `color` + `aux`
    pub(crate) layers: Vec<u32>,
    /// `stops`：`(at,0,0,0)` + 预乘 rgba（渐变停点，去重前先攒在局部，命中就丢弃）
    pub(crate) stops: Vec<u32>,
    /// 去重表：内容哈希 → styles 里的 1-based 下标
    map: rustc_hash::FxHashMap<u64, Vec<u32>>,
}

impl StyleTable {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn style_count(&self) -> u32 {
        (self.styles.len() / STYLE_STRIDE_U32) as u32
    }

    /// 登记一条样式，返回 `(1-based 下标, 距离场带宽)`。
    ///
    /// 同样的 `(层, 填充)` 只登记一次（去重），所以同一样式在多段文字间共享。
    /// 去重键**不含** `layer_first`（那是登记后才确定的），命中时复用旧下标。
    pub(crate) fn register(&mut self, style: &TextStyle, dpi: f64) -> (u32, f32) {
        self.register_impl(style, dpi, false)
    }

    /// 登记「只画图层、不画 fill」的变体（彩色字形垫底用），返回 `(下标, band)`。
    pub(crate) fn register_layers_only(&mut self, style: &TextStyle, dpi: f64) -> (u32, f32) {
        self.register_impl(style, dpi, true)
    }

    fn register_impl(&mut self, style: &TextStyle, dpi: f64, layers_only: bool) -> (u32, f32) {
        let reach = style.reach_px(dpi);
        if reach > BAND_MAX {
            warn_once(
                &CLAMP_WARNED,
                "vireo: 文本样式外扩 > 32px，距离场带宽已钳制到 32（描边/阴影会变淡）",
            );
        }
        if style.layers.len() > 8 {
            warn_once(&LAYER_WARNED, "vireo: 文本样式图层数 > 8，多余的层已忽略");
        }
        let band = band_for_reach(reach);
        let layer_count = style.layers.len().min(8) as u32;

        let mut style_rec = [0u32; STYLE_STRIDE_U32];
        let mut layers: Vec<u32> = Vec::with_capacity(layer_count as usize * LAYER_STRIDE_U32);
        let mut stops: Vec<u32> = Vec::new();

        for layer in style.layers.iter().take(layer_count as usize) {
            let px = layer.to_px(dpi);
            let (kind, color, aux) = encode_paint(&layer.paint, &mut stops, dpi);
            let mut rec = [0u32; LAYER_STRIDE_U32];
            rec[0] = px.offset.0.to_bits();
            rec[1] = px.offset.1.to_bits();
            rec[2] = px.softness.to_bits();
            rec[3] = px.threshold.to_bits();
            rec[4] = kind;
            rec[5] = match layer.blend {
                TextBlend::Over => 0,
                TextBlend::Add => 1,
            };
            // rec[6] = cutoff（有 cutoff 时有效），rec[7] = 标志位
            if let Some(cutoff) = px.cutoff {
                rec[6] = cutoff.to_bits();
            }
            rec[7] = (if px.cutoff.is_some() {
                LAYER_FLAG_CUTOFF
            } else {
                0
            }) | (if layer.above_fill {
                LAYER_FLAG_ABOVE
            } else {
                0
            }) | (if layer.inner_shadow {
                LAYER_FLAG_INNER_SHADOW
            } else {
                0
            });
            rec[8..12].copy_from_slice(&color);
            rec[12..16].copy_from_slice(&aux);
            layers.extend_from_slice(&rec);
        }

        // fill 永远最后画；`None` = 用调用方的默认颜色。
        // `layers_only`（彩色字形）：`fill_kind = PAINT_NONE` ⇒ 着色器一层都不填。
        let (fill_kind, fill_color, fill_aux) = if layers_only {
            (PAINT_NONE, [0u32; 4], [0u32; 4])
        } else {
            match style.fill.as_ref() {
                Some(paint) => encode_paint(paint, &mut stops, dpi),
                None => (PAINT_DEFAULT, [0u32; 4], [0u32; 4]),
            }
        };
        style_rec[1] = layer_count;
        style_rec[2] = fill_kind;
        style_rec[4..8].copy_from_slice(&fill_color);
        style_rec[8..12].copy_from_slice(&fill_aux);

        // 去重：样式头（不含 layer_first）+ 层 + 停点
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for v in style_rec.iter().chain(layers.iter()).chain(stops.iter()) {
            hash ^= *v as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        if let Some(cands) = self.map.get(&hash) {
            for &idx in cands {
                let at = (idx as usize - 1) * STYLE_STRIDE_U32;
                if self.styles.len() < at + STYLE_STRIDE_U32 {
                    continue;
                }
                let existing = &self.styles[at..at + STYLE_STRIDE_U32];
                let same_header = existing[1] == style_rec[1]
                    && existing[2..]
                        .iter()
                        .zip(style_rec[2..].iter())
                        .all(|(a, b)| a == b);
                if !same_header {
                    continue;
                }
                let layer_at = existing[0] as usize * LAYER_STRIDE_U32;
                if self.layers.len() >= layer_at + layers.len()
                    && self.layers[layer_at..layer_at + layers.len()] == layers[..]
                {
                    return (idx, band);
                }
            }
        }

        // 未命中：落表（此时才确定 layer_first）
        let layer_first = (self.layers.len() / LAYER_STRIDE_U32) as u32;
        style_rec[0] = layer_first;
        self.layers.extend_from_slice(&layers);
        self.stops.extend_from_slice(&stops);
        self.styles.extend_from_slice(&style_rec);
        let idx = (self.styles.len() / STYLE_STRIDE_U32) as u32;
        self.map.entry(hash).or_default().push(idx);
        (idx, band)
    }
}

/// 编码一种填充，返回 `(kind, color vec4, aux vec4)`；渐变停点追加到 `stops`。
///
/// - 纯色：`color` = rgba。
/// - 渐变：`color` = `(center_x, center_y, span, 0)`（设备像素；`span == 0` ⇒ 逐字形），
///   `aux` = `(cos, sin, first_stop, count)`。
/// - 纹理：当前仍降级为纯白（下一轮做，需要文字自己的 UV 参照系）。
fn encode_paint(paint: &TextPaint, stops: &mut Vec<u32>, dpi: f64) -> (u32, [u32; 4], [u32; 4]) {
    match paint {
        TextPaint::Solid(c) => (
            PAINT_SOLID,
            [c.r.to_bits(), c.g.to_bits(), c.b.to_bits(), c.a.to_bits()],
            [0; 4],
        ),
        TextPaint::Gradient(g) => {
            let (sin, cos) = g.angle_deg.to_radians().sin_cos();
            let (cx, cy, span) = match g.rect {
                Some((x, y, w, h)) => {
                    let (px, py, pw, ph) = rect_px(x, y, w, h, dpi);
                    (
                        px + pw * 0.5,
                        py + ph * 0.5,
                        pw * sin.abs() + ph * cos.abs(),
                    )
                }
                // 逐字形：span = 0 让着色器改用字形局部 uv
                None => (0.0, 0.0, 0.0),
            };
            let first = (stops.len() / STOP_STRIDE_U32) as u32;
            let n = g.stops.len().min(16);
            for stop in &g.stops[..n] {
                let c = stop.color;
                let a = c.a;
                stops.extend_from_slice(&[
                    stop.at.to_bits(),
                    0,
                    0,
                    0,
                    (c.r * a).to_bits(),
                    (c.g * a).to_bits(),
                    (c.b * a).to_bits(),
                    a.to_bits(),
                ]);
            }
            (
                PAINT_GRADIENT,
                [cx.to_bits(), cy.to_bits(), span.to_bits(), 0],
                // 方向 = (sinθ, cosθ)：0° ⇒ (0,1) 自上而下（与形状渐变同一约定）
                [sin.to_bits(), cos.to_bits(), first, n as u32],
            )
        }
        TextPaint::Texture { uv, rect } => {
            let (cx, cy, hw, hh) = match rect {
                Some((x, y, w, h)) => {
                    let (px, py, pw, ph) = rect_px(*x, *y, *w, *h, dpi);
                    (px + pw * 0.5, py + ph * 0.5, pw * 0.5, ph * 0.5)
                }
                None => (0.0, 0.0, 0.0, 0.0),
            };
            (
                PAINT_TEXTURE,
                [
                    uv.u0.to_bits(),
                    uv.v0.to_bits(),
                    uv.u1.to_bits(),
                    uv.v1.to_bits(),
                ],
                [cx.to_bits(), cy.to_bits(), hw.to_bits(), hh.to_bits()],
            )
        }
    }
}

/// 页面矩形 → 设备像素 `(x, y, w, h)`。
fn rect_px(x: Pp, y: Pp, w: Pp, h: Pp, dpi: f64) -> (f32, f32, f32, f32) {
    (
        x.to_px(dpi).0 as f32,
        y.to_px(dpi).0 as f32,
        w.to_px(dpi).0 as f32,
        h.to_px(dpi).0 as f32,
    )
}

/// 进程级 warn-once（外扩钳制 / 图层截断）。
static CLAMP_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LAYER_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn warn_once(flag: &std::sync::atomic::AtomicBool, msg: &str) {
    if !flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!("{msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::colors::{BLACK, WHITE};
    use crate::dpi::Dp;

    #[test]
    fn layer_px_scales_with_dpi() {
        let layer = TextLayer::soft(Dp(1.0), Dp(2.0), TextPaint::solid(WHITE));
        let a = layer.to_px(1.0);
        let b = layer.to_px(2.0);
        assert!((a.threshold - 1.0).abs() < 1e-6);
        assert!(
            (b.threshold - 2.0).abs() < 1e-6,
            "逻辑像素意图要随 dpi 放大"
        );
        assert!((a.softness - 2.0).abs() < 1e-6);
        assert!((b.softness - 4.0).abs() < 1e-6);
    }

    #[test]
    fn px_intent_does_not_scale() {
        let layer = TextLayer::hard(crate::dpi::Px(1.5), TextPaint::solid(WHITE));
        assert!((layer.to_px(1.0).threshold - 1.5).abs() < 1e-6);
        assert!(
            (layer.to_px(2.0).threshold - 1.5).abs() < 1e-6,
            "物理像素意图不随 dpi 变"
        );
    }

    #[test]
    fn reach_covers_offset_and_softness() {
        let style = TextStyle::shadow((Dp(3.0), Dp(4.0)), Dp(2.0), BLACK);
        let reach = style.reach_px(1.0);
        assert!(
            reach >= 3.0 + 2.0 * 3.0 + 1.0,
            "reach 应覆盖 3σ 与偏移，实际 {reach}"
        );
    }

    /// band 是**档位**：小幅调宽度不会换档，也就不会触发重新栅格化。
    #[test]
    fn band_snaps_to_tiers() {
        assert_eq!(band_for_reach(0.5), 4.0);
        assert_eq!(band_for_reach(4.0), 4.0);
        assert_eq!(band_for_reach(4.1), 8.0);
        assert_eq!(band_for_reach(15.9), 16.0);
        assert_eq!(band_for_reach(40.0), 64.0, "32 < reach <= 64 用 64 档");
        assert_eq!(band_for_reach(100.0), 64.0, "超上限钳到最后一档");

        // 描边 0.5..8 + 3σ/偏移余量 ⇒ 只有少数几档，滑块拖动不会每帧重建。
        let mut tiers = std::collections::BTreeSet::new();
        let mut w = 0.5f64;
        while w <= 8.0 {
            let style = TextStyle::outline(crate::dpi::Dp(w), BLACK);
            tiers.insert(band_for_reach(style.reach_px(1.0)) as u32);
            w += 0.5;
        }
        assert!(
            tiers.len() <= 3,
            "宽度 0.5..8 只该落到 ≤3 档，实际 {tiers:?}"
        );
    }

    /// 多图层样式的编码：层必须连续、`layer_first`/`layer_count` 必须对得上。
    #[test]
    fn multi_layer_style_encodes_all_layers() {
        let style = TextStyle::new()
            .with_layer(TextLayer::hard(3.0, TextPaint::solid(WHITE)))
            .with_layer(TextLayer::hard(1.0, TextPaint::solid(BLACK)));
        let mut table = StyleTable::new();
        let (idx, band) = table.register(&style, 1.0);
        assert_eq!(idx, 1);
        assert_eq!(band, 4.0, "外扩 3+1 ⇒ band 4");

        let sbase = 0usize;
        assert_eq!(table.styles[sbase], 0, "layer_first");
        assert_eq!(table.styles[sbase + 1], 2, "layer_count");
        // 第 0 层 threshold = 3，第 1 层 threshold = 1
        let thr0 = f32::from_bits(table.layers[3]);
        let thr1 = f32::from_bits(table.layers[LAYER_STRIDE_U32 + 3]);
        assert!(
            (thr0 - 3.0).abs() < 1e-6,
            "第 0 层 threshold 应为 3，实际 {thr0}"
        );
        assert!(
            (thr1 - 1.0).abs() < 1e-6,
            "第 1 层 threshold 应为 1，实际 {thr1}"
        );
        // 颜色：第 0 层白、第 1 层黑
        assert_eq!(table.layers[8 + 3], f32::from(WHITE.a).to_bits());
        assert_eq!(
            table.layers[LAYER_STRIDE_U32 + 8 + 3],
            f32::from(BLACK.a).to_bits()
        );
        // 同内容重复登记必须命中同一份
        let (idx2, _) = table.register(&style, 1.0);
        assert_eq!(idx2, 1, "同内容应去重");
        assert_eq!(table.style_count(), 1);
    }

    #[test]
    fn empty_style_needs_no_sdf() {
        assert!(!TextStyle::new().needs_style());
        assert!(TextStyle::outline(Dp(1.0), BLACK).needs_style());
        assert!(
            TextStyle::new()
                .with_fill(TextPaint::solid(BLACK))
                .needs_style(),
            "只有 fill 也要走样式通道（否则填充色被忽略）"
        );
    }
}
