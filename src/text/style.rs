//! 文本样式：图层栈（描边 / 投影 / 发光 / 内阴影 / 浮雕 / 渐变字 / 纹理字）。
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
    /// 线性渐变：复用引擎的渐变通道（与 `draw_rect_gradient` 同一份数据与取色函数）。
    Gradient(TextGradient),
    /// 批基础贴图（`DrawBatch::set_texture` 的那张）。
    Texture(UvRect),
}

impl TextPaint {
    pub fn solid(color: Color) -> Self {
        Self::Solid(color)
    }
}

/// 文字用的线性渐变：角度 + 停点，语义与 [`crate::shapes::LinearGradient`] 一致，
/// 但**拥有**停点（`TextStyle` 要能长期存在）。
#[derive(Clone, Debug)]
pub struct TextGradient {
    /// 度，顺时针；0 = 自上而下，90 = 自左而右。
    pub angle_deg: f32,
    pub stops: Vec<crate::shapes::GradientStop>,
}

impl TextGradient {
    pub fn new(angle_deg: f32, stops: Vec<crate::shapes::GradientStop>) -> Self {
        Self { angle_deg, stops }
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

/// 一层：对距离场取阈 + 偏移 + 填充。
#[derive(Clone, Debug)]
pub struct TextLayer {
    /// 阈值位置：`> 0` 字形外侧（外描边 / 投影），`< 0` 内侧（内描边），`0` = 轮廓。
    pub threshold: Pp,
    /// 软边（≈ 模糊半径）：距离场下边缘按误差函数过渡，0 = 硬边（1px 解析 AA）。
    pub softness: Pp,
    /// 偏移。
    pub offset: (Pp, Pp),
    pub paint: TextPaint,
    pub blend: TextBlend,
}

impl TextLayer {
    /// 硬边一层（无偏移、无软边）。
    pub fn hard(threshold: impl Into<Pp>, paint: TextPaint) -> Self {
        Self {
            threshold: threshold.into(),
            softness: Pp::Dp(crate::dpi::Dp(0.0)),
            offset: (Pp::Dp(crate::dpi::Dp(0.0)), Pp::Dp(crate::dpi::Dp(0.0))),
            paint,
            blend: TextBlend::Over,
        }
    }

    /// 软边一层。
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

    /// 折算成设备像素：(阈值, 软边, 偏移 x, 偏移 y)，以及"该层向字形外侧的最大外扩"。
    pub(crate) fn to_px(&self, dpi: f64) -> TextLayerPx {
        let threshold = self.threshold.to_px(dpi).0 as f32;
        let softness = self.softness.to_px(dpi).0.max(0.0) as f32;
        let ox = self.offset.0.to_px(dpi).0 as f32;
        let oy = self.offset.1.to_px(dpi).0 as f32;
        // 3σ 之外的 erf 尾部不足 1/255，按 3σ 计外扩（外加 1px 兜底）。
        let reach = threshold + softness * 3.0 + 1.0 + ox.abs().max(oy.abs());
        TextLayerPx {
            threshold,
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
    pub softness: f32,
    pub offset: (f32, f32),
    /// 该层向外的最大外扩（决定图集 padding 与 band）。
    pub reach: f32,
}

/// 文本样式：有序图层 + 填充。
///
/// 绘制顺序：`layers` 从**下到上**依次叠加，最后画 `fill`。
#[derive(Clone, Debug, Default)]
pub struct TextStyle {
    pub layers: Vec<TextLayer>,
    /// 最后绘制；`None` = 用 `TextOverride` / `TextDef` 给的默认颜色。
    pub fill: Option<TextPaint>,
}

impl TextStyle {
    pub fn new() -> Self {
        Self::default()
    }

    /// 外描边（硬边，`width` 向外）。
    pub fn outline(width: impl Into<Pp>, color: Color) -> Self {
        Self {
            layers: vec![TextLayer::hard(width, TextPaint::solid(color))],
            fill: None,
        }
    }

    /// 内描边（向内）。
    pub fn inner_outline(width: impl Into<Pp>, color: Color) -> Self {
        let w = width.into();
        let mut layer = TextLayer::hard(w, TextPaint::solid(color));
        layer.threshold = negate_pp(w);
        Self {
            layers: vec![layer],
            fill: None,
        }
    }

    /// 硬/软投影。
    pub fn shadow(
        offset: (impl Into<Pp>, impl Into<Pp>),
        blur: impl Into<Pp>,
        color: Color,
    ) -> Self {
        Self {
            layers: vec![
                TextLayer::soft(Pp::Dp(crate::dpi::Dp(0.0)), blur, TextPaint::solid(color))
                    .offset(offset.0, offset.1),
            ],
            fill: None,
        }
    }

    /// 发光（无偏移的软阴影）。
    pub fn glow(blur: impl Into<Pp>, color: Color) -> Self {
        Self {
            layers: vec![TextLayer::soft(
                Pp::Dp(crate::dpi::Dp(0.0)),
                blur,
                TextPaint::solid(color),
            )],
            fill: None,
        }
    }

    /// 浮雕：一亮一暗两层的经典组合。
    pub fn emboss(offset: impl Into<Pp>, light: Color, dark: Color) -> Self {
        let d = offset.into();
        let mut hi = TextLayer::hard(Pp::Dp(crate::dpi::Dp(0.0)), TextPaint::solid(light));
        hi.offset = (negate_pp(d), negate_pp(d));
        hi.blend = TextBlend::Add;
        let mut lo = TextLayer::hard(Pp::Dp(crate::dpi::Dp(0.0)), TextPaint::solid(dark));
        lo.offset = (d, d);
        Self {
            layers: vec![lo, hi],
            fill: None,
        }
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

/// 填充类型（与着色器里的 `paint_kind` 一致）。
pub(crate) const PAINT_DEFAULT: u32 = 0;
pub(crate) const PAINT_SOLID: u32 = 1;

/// 样式表：CPU 侧累积、去重，最后整块上传到 `group(1) @binding(1..2)`。
#[derive(Default)]
pub(crate) struct StyleTable {
    /// `styles`：`(layer_first, layer_count, fill_kind, 0)` + `fill_color` + `fill_aux`
    pub(crate) styles: Vec<u32>,
    /// `layers`：`geom` + `paint` + `color` + `aux`
    pub(crate) layers: Vec<u32>,
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
        let band = (style.reach_px(dpi).ceil()).clamp(2.0, 32.0);
        let layer_count = style.layers.len().min(8) as u32;

        let mut style_rec = [0u32; STYLE_STRIDE_U32];
        let mut layers: Vec<u32> = Vec::with_capacity(layer_count as usize * LAYER_STRIDE_U32);

        for layer in style.layers.iter().take(layer_count as usize) {
            let px = layer.to_px(dpi);
            let (kind, color, aux) = encode_paint(&layer.paint);
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
            rec[8..12].copy_from_slice(&color);
            rec[12..16].copy_from_slice(&aux);
            layers.extend_from_slice(&rec);
        }

        // fill 永远最后画；`None` = 用调用方的默认颜色。
        let (fill_kind, fill_color, fill_aux) = match style.fill.as_ref() {
            Some(paint) => encode_paint(paint),
            None => (PAINT_DEFAULT, [0u32; 4], [0u32; 4]),
        };
        style_rec[1] = layer_count;
        style_rec[2] = fill_kind;
        style_rec[4..8].copy_from_slice(&fill_color);
        style_rec[8..12].copy_from_slice(&fill_aux);

        // 去重：样式头（不含 layer_first）+ 层 + 停点
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for v in style_rec.iter().chain(layers.iter()) {
            hash ^= *v as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        if let Some(cands) = self.map.get(&hash) {
            for &idx in cands {
                let at = (idx as usize - 1) * STYLE_STRIDE_U32;
                if self.styles.len() < at + STYLE_STRIDE_U32 {
                    continue;
                }
                // 逐字段比较样式头（`layer_first` 单独比，因为它指向旧层）
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
        self.styles.extend_from_slice(&style_rec);
        let idx = (self.styles.len() / STYLE_STRIDE_U32) as u32;
        self.map.entry(hash).or_default().push(idx);
        (idx, band)
    }
}

/// 编码一种填充，返回 `(kind, color vec4, aux vec4)`。
///
/// P1 只支持纯色：渐变/纹理填充会**降级为纯色**（渐变取首个停点、纹理取白）并 warn-once，
/// 真正的渐变/纹理字填充排在 P2（需要给文字建立自己的渐变坐标基准）。
fn encode_paint(paint: &TextPaint) -> (u32, [u32; 4], [u32; 4]) {
    let color = match paint {
        TextPaint::Solid(c) => *c,
        TextPaint::Gradient(g) => {
            warn_once(
                &TEXT_PAINT_WARNED,
                "vireo: 文字渐变填充尚未实现（P2）—— 已按首个停点纯色绘制",
            );
            g.stops
                .first()
                .map(|s| s.color)
                .unwrap_or(crate::color::colors::WHITE)
        }
        TextPaint::Texture(_) => {
            warn_once(
                &TEXT_PAINT_WARNED,
                "vireo: 文字纹理填充尚未实现（P2）—— 已按纯白绘制",
            );
            crate::color::colors::WHITE
        }
    };
    (
        PAINT_SOLID,
        [
            color.r.to_bits(),
            color.g.to_bits(),
            color.b.to_bits(),
            color.a.to_bits(),
        ],
        [0; 4],
    )
}

/// 进程级 warn-once（P1 的文字填充降级提示）。
static TEXT_PAINT_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
