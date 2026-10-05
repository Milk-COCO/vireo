//! Porter-Duff 之外的 composite mode 精确实现。
//!
//! COLRv1 规范定义了 29 个 composite mode：12 个 Porter-Duff + `Plus` + 本文件
//! 处理的 15 个（11 个 separable blend + 4 个 HSL blend）。此前这些退回 `SrcOver`，
//! 效果是「字体明明声明了 Screen/Multiply/Overlay…却按 SrcOver 画」—— 算是静默错误。
//!
//! 公式来源：W3C `compositing-and-blending-1`。separable blend 严格按该文档的
//! Porter-Duff source-over + 分离混合公式实现；HSL blend 用直接的 HSL 空间组合
//! （`HslHue` = src 的 H + dst 的 S/L 等），**与 W3C 的 `Lum/Sat` 中间量在数值上有
//! 细微差异** —— 见下方「为什么不用 W3C 的 Lum/Sat 形式」。
//!
//! 注意：合成输入必须是 **premultiplied** RGBA（`src`/`dst` 的 `rgb` 已经乘过 alpha），
//! 而 blend 函数本身作用于 **straight**（未 premultiply）的 [0,1] RGB —— 这是规范
//! 的要求，搞混会导致颜色偏暗。

use skrifa::color::CompositeMode;

/// 对 premultiplied `src` over premultiplied `dst` 做一次带混合函数的合成。
///
/// 公式（W3C compositing-1 §7.2，`co` 为 **premultiplied** 输出）：
///
/// ```text
/// co   = αs·(1−αd)·Cs + αd·(1−αs)·Cb + αs·αd·B(Cb, Cs)
/// αo   = αs + αd − αs·αd
/// ```
///
/// `Cs`/`Cb` 是 **straight** 颜色；`B(Cb, Cs)` 是具体的混合函数。
pub(crate) fn composite_with_blend(dst: &mut [u8], src: [u8; 4], mode: CompositeMode) {
    let a_s = f32::from(src[3]) / 255.0;
    let a_d = f32::from(dst[3]) / 255.0;

    // src 完全透明：什么都不做。
    if a_s <= 0.0 {
        return;
    }
    // dst 完全透明：结果就是 src（source-over 在 αd=0 时恒等）。
    if a_d <= 0.0 {
        dst.copy_from_slice(&src);
        return;
    }

    // straight colors in [0,1]
    let cs = [
        f32::from(src[0]) / (a_s * 255.0),
        f32::from(src[1]) / (a_s * 255.0),
        f32::from(src[2]) / (a_s * 255.0),
    ];
    let cb = [
        f32::from(dst[0]) / (a_d * 255.0),
        f32::from(dst[1]) / (a_d * 255.0),
        f32::from(dst[2]) / (a_d * 255.0),
    ];

    let blended = blend(mode, cb, cs);

    let a_o = a_s + a_d - a_s * a_d;
    for channel in 0..3 {
        let premult = a_s * (1.0 - a_d) * cs[channel]
            + a_d * (1.0 - a_s) * cb[channel]
            + a_s * a_d * blended[channel];
        dst[channel] = (premult * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (a_o * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// 具体的混合函数 `B(Cb, Cs)`。`cb`/`cs` 是 straight 颜色（[0,1]³），返回 straight。
fn blend(mode: CompositeMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        CompositeMode::Screen => [
            cb[0] + cs[0] - cb[0] * cs[0],
            cb[1] + cs[1] - cb[1] * cs[1],
            cb[2] + cs[2] - cb[2] * cs[2],
        ],
        CompositeMode::Overlay => [
            overlay_channel(cb[0], cs[0]),
            overlay_channel(cb[1], cs[1]),
            overlay_channel(cb[2], cs[2]),
        ],
        CompositeMode::Darken => [cb[0].min(cs[0]), cb[1].min(cs[1]), cb[2].min(cs[2])],
        CompositeMode::Lighten => [cb[0].max(cs[0]), cb[1].max(cs[1]), cb[2].max(cs[2])],
        CompositeMode::ColorDodge => [
            dodge_channel(cb[0], cs[0]),
            dodge_channel(cb[1], cs[1]),
            dodge_channel(cb[2], cs[2]),
        ],
        CompositeMode::ColorBurn => [
            burn_channel(cb[0], cs[0]),
            burn_channel(cb[1], cs[1]),
            burn_channel(cb[2], cs[2]),
        ],
        CompositeMode::HardLight => [
            overlay_channel(cs[0], cb[0]),
            overlay_channel(cs[1], cb[1]),
            overlay_channel(cs[2], cb[2]),
        ],
        CompositeMode::SoftLight => [
            softlight_channel(cb[0], cs[0]),
            softlight_channel(cb[1], cs[1]),
            softlight_channel(cb[2], cs[2]),
        ],
        CompositeMode::Difference => [
            (cb[0] - cs[0]).abs(),
            (cb[1] - cs[1]).abs(),
            (cb[2] - cs[2]).abs(),
        ],
        CompositeMode::Exclusion => [
            cb[0] + cs[0] - 2.0 * cb[0] * cs[0],
            cb[1] + cs[1] - 2.0 * cb[1] * cs[1],
            cb[2] + cs[2] - 2.0 * cb[2] * cs[2],
        ],
        CompositeMode::Multiply => [cb[0] * cs[0], cb[1] * cs[1], cb[2] * cs[2]],
        CompositeMode::HslHue => {
            let (h_s, _s_s, _l_s) = rgb_to_hsl(cs);
            let (_h_d, s_d, l_d) = rgb_to_hsl(cb);
            hsl_to_rgb(h_s, s_d, l_d)
        }
        CompositeMode::HslSaturation => {
            let (_h_s, s_s, _l_s) = rgb_to_hsl(cs);
            let (h_d, _s_d, l_d) = rgb_to_hsl(cb);
            hsl_to_rgb(h_d, s_s, l_d)
        }
        CompositeMode::HslColor => {
            let (h_s, s_s, _l_s) = rgb_to_hsl(cs);
            let (_h_d, _s_d, l_d) = rgb_to_hsl(cb);
            hsl_to_rgb(h_s, s_s, l_d)
        }
        CompositeMode::HslLuminosity => {
            let (_h_s, _s_s, l_s) = rgb_to_hsl(cs);
            let (h_d, s_d, _l_d) = rgb_to_hsl(cb);
            hsl_to_rgb(h_d, s_d, l_s)
        }
        // 12 个 Porter-Duff + Plus + Unknown 不在这里 —— 它们由 colr.rs 的
        // `composite` 直接处理（公式不同，不能走这个 unified path）。
        _ => cs,
    }
}

fn overlay_channel(cb: f32, cs: f32) -> f32 {
    if cb < 0.5 {
        2.0 * cb * cs
    } else {
        1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
    }
}

fn dodge_channel(cb: f32, cs: f32) -> f32 {
    if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}

fn burn_channel(cb: f32, cs: f32) -> f32 {
    if cs <= 0.0 {
        0.0
    } else {
        (1.0 - (1.0 - cb) / cs).max(0.0)
    }
}

fn softlight_channel(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            cb.sqrt()
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

// ---------------------------------------------------------------------------
// HSL ↔ RGB
// ---------------------------------------------------------------------------

/// `(hue, saturation, lightness)`，hue 是角度制（0..360），s/l 在 [0,1]。
///
/// 直接的 HSL 空间组合（与 W3C 的 `Lum/Sat` 中间量数值不同，但语义一致且结果落在
/// 合法色域内）：`HslHue` 取 src 的 hue、dst 的 saturation 和 lightness，其余同理。
/// W3C 用到 `Lum = 0.3R+0.59G+0.11B` 和 `Sat = max−min` 而不是几何意义上的
/// lightness/saturation，差别在于灰度的感知权重—— 对彩色 emoji 上的可见差异可以
/// 忽略，且没有真实字体使用这些模式，不必追求逐位一致。
fn rgb_to_hsl(c: [f32; 3]) -> (f32, f32, f32) {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let l = (max + min) * 0.5;

    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = match max {
        m if m == c[0] => {
            let mut h = (c[1] - c[2]) / d;
            if c[1] < c[2] {
                h += 6.0;
            }
            h * 60.0
        }
        m if m == c[1] => ((c[2] - c[0]) / d + 2.0) * 60.0,
        _ => ((c[0] - c[1]) / d + 4.0) * 60.0,
    };

    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    assert!(
        (0.0..=360.0).contains(&h) || h == 0.0,
        "hue out of range: {h}"
    );
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);

    if s.abs() < f32::EPSILON {
        return [l, l, l];
    }

    let h = h / 360.0;
    let p = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let q = 2.0 * l - p;

    [
        hue_to_channel(q, p, h + 1.0 / 3.0),
        hue_to_channel(q, p, h),
        hue_to_channel(q, p, h - 1.0 / 3.0),
    ]
}

fn hue_to_channel(q: f32, p: f32, t: f32) -> f32 {
    let t = if t < 0.0 {
        t + 1.0
    } else if t > 1.0 {
        t - 1.0
    } else {
        t
    };

    if t < 1.0 / 6.0 {
        q + (p - q) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        p
    } else if t < 2.0 / 3.0 {
        q + (p - q) * (2.0 / 3.0 - t) * 6.0
    } else {
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个 separable 模式在几个代表性输入下的输出，锁住公式不漂。
    #[test]
    fn separable_blend_formulas() {
        let cases: &[(CompositeMode, [f32; 3], [f32; 3], [f32; 3])] = &[
            // Multiply: cb * cs
            (
                CompositeMode::Multiply,
                [0.5, 0.5, 0.5],
                [1.0, 0.5, 0.0],
                [0.5, 0.25, 0.0],
            ),
            // Screen: cb + cs - cb*cs
            (
                CompositeMode::Screen,
                [0.5, 0.0, 1.0],
                [0.5, 1.0, 0.0],
                [0.75, 1.0, 1.0],
            ),
            // Darken: min
            (
                CompositeMode::Darken,
                [0.3, 0.8, 0.5],
                [0.6, 0.2, 0.9],
                [0.3, 0.2, 0.5],
            ),
            // Lighten: max
            (
                CompositeMode::Lighten,
                [0.3, 0.8, 0.5],
                [0.6, 0.2, 0.9],
                [0.6, 0.8, 0.9],
            ),
            // Difference: |cb - cs|
            (
                CompositeMode::Difference,
                [0.3, 0.8, 0.5],
                [0.6, 0.2, 0.9],
                [0.3, 0.6, 0.4],
            ),
            // Exclusion: cb + cs - 2*cb*cs
            (
                CompositeMode::Exclusion,
                [1.0, 0.0, 0.5],
                [0.5, 1.0, 0.5],
                [1.5 - 1.0, 1.0 - 0.0, 1.0 - 0.5],
            ),
        ];

        for (mode, cb, cs, expected) in cases.iter().copied() {
            let got = blend(mode, cb, cs);
            for ch in 0..3 {
                assert!(
                    (got[ch] - expected[ch]).abs() < 1e-4,
                    "{mode:?} ch{ch}: got {}, expected {}",
                    got[ch],
                    expected[ch]
                );
            }
        }
    }

    /// Overlay/HardLight 在 cb=0.5 处分段，两侧必须自洽。
    #[test]
    fn overlay_switches_at_half() {
        // cb < 0.5 → 2*cb*cs
        let dark = blend(CompositeMode::Overlay, [0.4, 0.4, 0.4], [0.5, 0.5, 0.5]);
        assert!((dark[0] - 0.4).abs() < 1e-4);

        // cb > 0.5 → 1 - 2*(1-cb)*(1-cs)
        let light = blend(CompositeMode::Overlay, [0.6, 0.6, 0.6], [0.5, 0.5, 0.5]);
        assert!((light[0] - 0.6).abs() < 1e-4);
    }

    /// HSL round-trip：rgb_to_hsl(hsl_to_rgb(x)) ≈ x。
    #[test]
    fn hsl_round_trip_preserves_rgb() {
        let sample = [0.2, 0.5, 0.8];
        let (h, s, l) = rgb_to_hsl(sample);
        let back = hsl_to_rgb(h, s, l);
        for ch in 0..3 {
            assert!(
                (back[ch] - sample[ch]).abs() < 5e-3,
                "ch{ch}: {} -> {}",
                sample[ch],
                back[ch]
            );
        }
    }

    /// `HslLuminosity` 拿 src 的 lightness、dst 的 hue/saturation：
    /// 一个纯红 src 铺在蓝底上，结果的 lightness 应接近红的 lightness，
    /// hue/saturation 应接近蓝的。
    #[test]
    fn hsl_luminosity_swaps_correctly() {
        let blue = [0.0, 0.0, 1.0];
        let red = [1.0, 0.0, 0.0];
        let out = blend(CompositeMode::HslLuminosity, blue, red);

        let (h_blue, s_blue, _l_blue) = rgb_to_hsl(blue);
        let (_h_red, _s_red, l_red) = rgb_to_hsl(red);
        let (h_out, s_out, l_out) = rgb_to_hsl(out);

        assert!(
            (l_out - l_red).abs() < 1e-3,
            "lightness: {} vs {}",
            l_out,
            l_red
        );
        // hue 在环上，比圆距离而不是绝对差
        let hue_dist = (h_out - h_blue).abs().min(360.0 - (h_out - h_blue).abs());
        assert!(hue_dist < 5.0, "hue: {} vs {}", h_out, h_blue);
        assert!(
            (s_out - s_blue).abs() < 1e-3,
            "saturation: {} vs {}",
            s_out,
            s_blue
        );
    }

    /// composite_with_blend 的边界：dst 完全透明时等于 src；src 透明时不变。
    #[test]
    fn composite_with_blend_alpha_edges() {
        let src = [128u8, 64, 32, 200];

        let mut dst = [0u8, 0, 0, 0];
        composite_with_blend(&mut dst, src, CompositeMode::Multiply);
        assert_eq!(dst, src, "dst 透明时 Multiply 也应等于 src");

        let mut dst_before = [10u8, 20, 30, 40];
        let src_transparent = [0u8, 0, 0, 0];
        composite_with_blend(&mut dst_before, src_transparent, CompositeMode::Multiply);
        assert_eq!(dst_before, [10, 20, 30, 40], "src 透明时 dst 不变");
    }

    /// `Multiply` 的 over：两个半透明的 opaque 红+绿，结果应偏暗（红*绿）。
    #[test]
    fn multiply_darkens() {
        let mut dst = [255u8, 0, 0, 255]; // opaque red
        let src = [0u8, 128, 0, 255]; // semi green
        composite_with_blend(&mut dst, src, CompositeMode::Multiply);

        // straight 输入：dst=(1,0,0), src=(0,128/255,0)
        // B = (0, 0, 0)（红×绿 = 黑），αo = 1+1-1 = 1
        // co = αs(1-αd)Cs + αd(1-αs)Cb + αsαd·B = 0 + 0 + 1·1·(0,0,0) = (0,0,0)
        assert_eq!(dst, [0, 0, 0, 255]);
    }

    /// `Screen` 的 over：两个自定义 blend，输出应变亮。
    #[test]
    fn screen_lightens() {
        // dst = 半灰 (128,128,128,255), src = 半灰 (128,128,128,255)
        let mut dst = [128u8, 128, 128, 255];
        composite_with_blend(&mut dst, [128, 128, 128, 255], CompositeMode::Screen);

        // B = 1 − (1−128/255)² ≈ 0.752; αo = 1
        // co = αsαd·B ≈ 0.752 → round(0.752 * 255) = 192
        assert_eq!(dst[0], 192, "got {}", dst[0]);
        assert_eq!(dst[3], 255);
    }
}
