/// RGBA 颜色
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// 从 u8 RGBA 构造
    pub fn from_u8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// 从 hex 字符串构造（支持 `#RGB` `#RGBA` `#RRGGBB` `#RRGGBBAA`）
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim_start_matches('#');
        let (r, g, b, a) = match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                (r, g, b, 255)
            }
            4 => {
                let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                let a = u8::from_str_radix(&hex[3..4], 16).ok()? * 17;
                (r, g, b, a)
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                (r, g, b, 255)
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
                (r, g, b, a)
            }
            _ => return None,
        };
        Some(Self::from_u8(r, g, b, a))
    }

    /// 转为 hex 字符串 `#RRGGBBAA`
    pub fn to_hex(&self) -> String {
        let [r, g, b, a] = <[u8; 4]>::from(*self);
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }

    /// 设置透明度
    pub fn with_alpha(&self, a: f32) -> Self {
        Self { a, ..*self }
    }

    /// 完全透明
    pub fn transparent(&self) -> Self {
        Self { a: 0.0, ..*self }
    }

    /// 完全不透明
    pub fn opaque(&self) -> Self {
        Self { a: 1.0, ..*self }
    }

    /// 线性插值混合两个颜色
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }

    /// 预乘 alpha（将 RGB 乘以 alpha）
    pub fn premultiplied(&self) -> Self {
        Self {
            r: self.r * self.a,
            g: self.g * self.a,
            b: self.b * self.a,
            a: self.a,
        }
    }

    /// 分量取最小值
    pub fn min(&self, other: &Self) -> Self {
        Self {
            r: self.r.min(other.r),
            g: self.g.min(other.g),
            b: self.b.min(other.b),
            a: self.a.min(other.a),
        }
    }

    /// 分量取最大值
    pub fn max(&self, other: &Self) -> Self {
        Self {
            r: self.r.max(other.r),
            g: self.g.max(other.g),
            b: self.b.max(other.b),
            a: self.a.max(other.a),
        }
    }
}

impl From<Color> for [u8; 4] {
    fn from(c: Color) -> Self {
        [
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8,
            (c.a * 255.0) as u8,
        ]
    }
}

impl From<[u8; 4]> for Color {
    fn from(c: [u8; 4]) -> Self {
        Self {
            r: c[0] as f32 / 255.0,
            g: c[1] as f32 / 255.0,
            b: c[2] as f32 / 255.0,
            a: c[3] as f32 / 255.0,
        }
    }
}

impl From<Color> for glam::Vec4 {
    fn from(c: Color) -> Self {
        glam::vec4(c.r, c.g, c.b, c.a)
    }
}

impl From<glam::Vec4> for Color {
    fn from(v: glam::Vec4) -> Self {
        Self {
            r: v.x,
            g: v.y,
            b: v.z,
            a: v.w,
        }
    }
}

/// u8 构建颜色
#[macro_export]
macro_rules! color_u8 {
    ($r:expr, $g:expr, $b:expr, $a:expr) => {
        $crate::color::Color::new(
            $r as f32 / 255.0,
            $g as f32 / 255.0,
            $b as f32 / 255.0,
            $a as f32 / 255.0,
        )
    };
}

/// 常用颜色常量
pub mod colors {
    use super::Color;

    pub const LIGHTGRAY: Color = Color::new(0.78, 0.78, 0.78, 1.00);
    pub const GRAY: Color = Color::new(0.51, 0.51, 0.51, 1.00);
    pub const DARKGRAY: Color = Color::new(0.31, 0.31, 0.31, 1.00);
    pub const YELLOW: Color = Color::new(0.99, 0.98, 0.00, 1.00);
    pub const GOLD: Color = Color::new(1.00, 0.80, 0.00, 1.00);
    pub const ORANGE: Color = Color::new(1.00, 0.63, 0.00, 1.00);
    pub const PINK: Color = Color::new(1.00, 0.43, 0.76, 1.00);
    pub const RED: Color = Color::new(0.90, 0.16, 0.22, 1.00);
    pub const MAROON: Color = Color::new(0.75, 0.13, 0.22, 1.00);
    pub const GREEN: Color = Color::new(0.00, 0.89, 0.19, 1.00);
    pub const LIME: Color = Color::new(0.00, 0.62, 0.18, 1.00);
    pub const DARKGREEN: Color = Color::new(0.00, 0.46, 0.17, 1.00);
    pub const SKYBLUE: Color = Color::new(0.40, 0.75, 1.00, 1.00);
    pub const BLUE: Color = Color::new(0.00, 0.47, 0.95, 1.00);
    pub const DARKBLUE: Color = Color::new(0.00, 0.32, 0.67, 1.00);
    pub const PURPLE: Color = Color::new(0.78, 0.48, 1.00, 1.00);
    pub const VIOLET: Color = Color::new(0.53, 0.24, 0.75, 1.00);
    pub const DARKPURPLE: Color = Color::new(0.44, 0.12, 0.49, 1.00);
    pub const BEIGE: Color = Color::new(0.83, 0.69, 0.51, 1.00);
    pub const BROWN: Color = Color::new(0.50, 0.42, 0.31, 1.00);
    pub const DARKBROWN: Color = Color::new(0.30, 0.19, 0.10, 1.00);
    pub const WHITE: Color = Color::new(1.00, 1.00, 1.00, 1.00);
    pub const BLACK: Color = Color::new(0.00, 0.00, 0.00, 1.00);
    pub const BLANK: Color = Color::new(0.00, 0.00, 0.00, 0.00);
    pub const MAGENTA: Color = Color::new(1.00, 0.00, 1.00, 1.00);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_new() {
        let c = Color::new(0.5, 0.25, 0.75, 1.0);
        assert_eq!(c.r, 0.5);
        assert_eq!(c.g, 0.25);
        assert_eq!(c.b, 0.75);
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn color_default() {
        let c = Color::default();
        assert_eq!(c.r, 0.0);
        assert_eq!(c.g, 0.0);
        assert_eq!(c.b, 0.0);
        assert_eq!(c.a, 0.0);
    }

    #[test]
    fn color_to_u8_array() {
        let c = Color::new(1.0, 0.5, 0.0, 0.0);
        let arr: [u8; 4] = c.into();
        assert_eq!(arr, [255, 127, 0, 0]);
    }

    #[test]
    fn color_from_u8_array() {
        let c = Color::from([255, 128, 64, 0]);
        assert!((c.r - 1.0).abs() < 0.01);
        assert!((c.g - 0.502).abs() < 0.01);
        assert!((c.b - 0.251).abs() < 0.01);
        assert!((c.a - 0.0).abs() < 0.01);
    }

    #[test]
    fn color_roundtrip_u8() {
        let original = Color::new(0.2, 0.4, 0.6, 0.8);
        let arr: [u8; 4] = original.into();
        let back: Color = arr.into();
        let eps = 1.0 / 255.0;
        assert!((back.r - original.r).abs() < eps);
        assert!((back.g - original.g).abs() < eps);
        assert!((back.b - original.b).abs() < eps);
        assert!((back.a - original.a).abs() < eps);
    }

    #[test]
    fn color_to_glam_vec4() {
        let c = Color::new(0.1, 0.2, 0.3, 0.4);
        let v: glam::Vec4 = c.into();
        assert_eq!(v.x, 0.1);
        assert_eq!(v.y, 0.2);
        assert_eq!(v.z, 0.3);
        assert_eq!(v.w, 0.4);
    }

    #[test]
    fn color_from_glam_vec4() {
        let v = glam::vec4(0.9, 0.8, 0.7, 0.6);
        let c = Color::from(v);
        assert_eq!(c.r, 0.9);
        assert_eq!(c.g, 0.8);
        assert_eq!(c.b, 0.7);
        assert_eq!(c.a, 0.6);
    }

    #[test]
    fn color_roundtrip_glam() {
        let original = Color::new(0.33, 0.66, 0.99, 0.5);
        let v: glam::Vec4 = original.into();
        let back = Color::from(v);
        assert_eq!(back, original);
    }

    #[test]
    fn color_u8_macro() {
        let c = color_u8!(255, 128, 64, 32);
        assert!((c.r - 1.0).abs() < 0.01);
        assert!((c.g - 0.502).abs() < 0.01);
        assert!((c.b - 0.251).abs() < 0.01);
        assert!((c.a - 0.125).abs() < 0.01);
    }

    #[test]
    fn color_u8_macro_zero() {
        let c = color_u8!(0, 0, 0, 0);
        assert_eq!(c, Color::new(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn preset_colors_not_blank() {
        assert_ne!(colors::BLACK, colors::BLANK);
        assert_eq!(colors::BLACK.a, 1.0);
        assert_eq!(colors::BLANK.a, 0.0);
    }
}
