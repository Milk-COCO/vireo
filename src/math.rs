//! 数学与几何基础类型：2D 坐标、轴对齐矩形、UV 矩形与仿射变换（含矩阵辅助函数）。

use rustc_hash::FxHashMap;

/// 轴对齐矩形，用于 culling 的包围盒/视口。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn left(&self) -> f32 { self.x }
    pub fn right(&self) -> f32 { self.x + self.w }
    pub fn top(&self) -> f32 { self.y }
    pub fn bottom(&self) -> f32 { self.y + self.h }

    pub fn contains(&self, p: [f32; 2]) -> bool {
        p[0] >= self.x && p[0] <= self.x + self.w
            && p[1] >= self.y && p[1] <= self.y + self.h
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        let l = self.x.max(other.x);
        let r = (self.x + self.w).min(other.x + other.w);
        if r < l { return false; }
        let t = self.y.max(other.y);
        let b = (self.y + self.h).min(other.y + other.h);
        b >= t
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let l = self.x.min(other.x);
        let r = (self.x + self.w).max(other.x + other.w);
        let t = self.y.min(other.y);
        let b = (self.y + self.h).max(other.y + other.h);
        Rect::new(l, t, r - l, b - t)
    }
}

/// 2D 坐标位置。语义上为"画在哪"（WHERE），与"画什么"（WHAT）、"如何画"（OVERRIDE）区分。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

impl Pos {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 形状仿射变换：线性部分 + 平移 + 局部 pivot。
///
/// 线性 `[a b; c d]` 常为 `sx*cos, -sx*sin; sy*sin, sy*cos`（由 [`Transform::trs`] 构建）。
/// 顶点变换顺序：`T(x,y) * [a b; c d] * T(-pivot)`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// 线性部分：`[a b; c d]`
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    /// 世界坐标位置（局部坐标系原点）
    pub x: f32,
    pub y: f32,
    /// 局部空间旋转/缩放中心
    pub px: f32,
    pub py: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// 单位变换。
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        x: 0.0,
        y: 0.0,
        px: 0.0,
        py: 0.0,
    };

    /// 仅平移。
    pub fn translation(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            ..Self::IDENTITY
        }
    }

    /// 从平移 / pivot / 旋转（弧度，顺时针）/ 缩放构建。
    pub fn trs(x: f32, y: f32, px: f32, py: f32, rotation: f32, sx: f32, sy: f32) -> Self {
        let (c, s) = (rotation.cos(), rotation.sin());
        Self {
            a: sx * c,
            b: -sx * s,
            c: sy * s,
            d: sy * c,
            x,
            y,
            px,
            py,
        }
    }

    /// 原始 3×3 仿射 6 分量（列主序语义：`[a b tx; c d ty; 0 0 1]`），pivot 归零。
    pub fn matrix(a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) -> Self {
        Self {
            a,
            b,
            c,
            d,
            x: tx,
            y: ty,
            px: 0.0,
            py: 0.0,
        }
    }

    /// 返回 3x3 仿射变换矩阵的 3 个列（WGSL 列主序）。
    /// 变换顺序：T(x,y) * [a b; c d] * T(-pivot)，即 pivot → 线性 → 平移。
    pub(crate) fn to_cols(&self) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let tx = self.x - self.px * self.a - self.py * self.b;
        let ty = self.y - self.px * self.c - self.py * self.d;
        ([self.a, self.c, 0.0], [self.b, self.d, 0.0], [tx, ty, 1.0])
    }

    /// 组合：先应用 `child`，再应用 `self`（`M = self * child`，pivot 已烘焙进平移）。
    pub fn then(&self, child: &Transform) -> Transform {
        let (p0, p1, p2) = self.to_cols();
        let (c0, c1, c2) = child.to_cols();
        let (r0, r1, r2) = mul_affine_cols(p0, p1, p2, c0, c1, c2);
        Transform::matrix(r0[0], r1[0], r0[1], r1[1], r2[0], r2[1])
    }
}

/// `P * C` 的列向量（2D 仿射，第三行视为 0 0 1）。
pub(crate) fn mul_affine_cols(
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    c0: [f32; 3],
    c1: [f32; 3],
    c2: [f32; 3],
) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let mul = |c: [f32; 3]| -> [f32; 3] {
        [
            p0[0] * c[0] + p1[0] * c[1] + p2[0] * c[2],
            p0[1] * c[0] + p1[1] * c[1] + p2[1] * c[2],
            0.0,
        ]
    };
    let mut r0 = mul([c0[0], c0[1], 0.0]);
    let mut r1 = mul([c1[0], c1[1], 0.0]);
    let mut r2 = mul([c2[0], c2[1], 1.0]);
    r0[2] = 0.0;
    r1[2] = 0.0;
    r2[2] = 1.0;
    (r0, r1, r2)
}

/// 对矩形的 4 个角应用 2D 仿射列矩阵，返回外接 AABB（保守，含旋转/缩放）。
pub(crate) fn affine_rect_bounds(r: &Rect, c0: [f32; 3], c1: [f32; 3], c2: [f32; 3]) -> Rect {
    let tx = |x: f32, y: f32| c0[0] * x + c1[0] * y + c2[0];
    let ty = |x: f32, y: f32| c0[1] * x + c1[1] * y + c2[1];
    let (xs, ys) = ([r.x, r.x + r.w], [r.y, r.y + r.h]);
    let mut minx = f32::INFINITY;
    let mut maxx = f32::NEG_INFINITY;
    let mut miny = f32::INFINITY;
    let mut maxy = f32::NEG_INFINITY;
    for &x in &xs {
        for &y in &ys {
            let (wx, wy) = (tx(x, y), ty(x, y));
            if wx < minx { minx = wx; }
            if wx > maxx { maxx = wx; }
            if wy < miny { miny = wy; }
            if wy > maxy { maxy = wy; }
        }
    }
    Rect::new(minx, miny, maxx - minx, maxy - miny)
}

/// 把 `view` 左乘到 `table` 每一行（12 f32 的列主序矩阵），写入 `out`。
/// `view` 为单位阵时直接整表拷贝；否则逐行 `view.to_cols() * row`。
pub(crate) fn left_mul_view_table(view: &Transform, table: &[f32], out: &mut Vec<f32>) {
    out.clear();
    if view.a == 1.0
        && view.b == 0.0
        && view.c == 0.0
        && view.d == 1.0
        && view.x == 0.0
        && view.y == 0.0
        && view.px == 0.0
        && view.py == 0.0
    {
        out.extend_from_slice(table);
        return;
    }
    let (v0, v1, v2) = view.to_cols();
    for row in table.chunks_exact(12) {
        let m0 = [row[0], row[1], 0.0];
        let m1 = [row[4], row[5], 0.0];
        let m2 = [row[8], row[9], 1.0];
        let (r0, r1, r2) = mul_affine_cols(v0, v1, v2, m0, m1, m2);
        out.extend_from_slice(&[
            r0[0], r0[1], 0.0, 0.0, //
            r1[0], r1[1], 0.0, 0.0, //
            r2[0], r2[1], 1.0, 0.0, //
        ]);
    }
}

/// 6 个 f32 bit pattern 的 hash（batch 内去重用）。
/// 使用乘法混合降低对称碰撞（旧 XOR 旋转在对称矩阵上较易冲突）。
pub(crate) fn transform_key(c0: [f32; 3], c1: [f32; 3], c2: [f32; 3]) -> u64 {
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h = c0[0].to_bits() as u64;
    h = h.wrapping_mul(K).wrapping_add(c0[1].to_bits() as u64);
    h = h.wrapping_mul(K).wrapping_add(c1[0].to_bits() as u64);
    h = h.wrapping_mul(K).wrapping_add(c1[1].to_bits() as u64);
    h = h.wrapping_mul(K).wrapping_add(c2[0].to_bits() as u64);
    h = h.wrapping_mul(K).wrapping_add(c2[1].to_bits() as u64);
    h ^ (h >> 32)
}

/// 单位矩阵一行（12 f32，mat3x3 列 vec4-padded），用作 `transform_table` 槽 0。
pub(crate) const IDENTITY_TRANSFORM_ROW: [f32; 12] = [
    1.0, 0.0, 0.0, 0.0, // col0
    0.0, 1.0, 0.0, 0.0, // col1
    0.0, 0.0, 1.0, 0.0, // col2
];

/// 清空并写入槽 0 = 单位矩阵；`transform_map` 同步登记 index 0。
///
/// **约定**：batch 内 `transform_index == 0` 恒表示单位变换（与 `Renderer` 全局表槽 0 一致）。
/// `draw_text` / glyphon 默认写 0，必须不能被第一个形状的平移占用。
pub(crate) fn seed_identity_transform_table(table: &mut Vec<f32>, map: &mut FxHashMap<u64, u32>) {
    table.clear();
    map.clear();
    table.extend_from_slice(&IDENTITY_TRANSFORM_ROW);
    let key = transform_key(
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
    );
    map.insert(key, 0);
}

/// 纹理坐标子区域，控制形状内部 UV 映射范围。
#[derive(Clone, Copy, Debug)]
pub struct UvRect {
    pub u0: f32, pub v0: f32,
    pub u1: f32, pub v1: f32,
}

impl Default for UvRect {
    fn default() -> Self { Self { u0: 0.0, v0: 0.0, u1: 1.0, v1: 1.0 } }
}

impl UvRect {
    /// 四角 UV：(左上, 右上, 右下, 左下)，对应包围盒四元组 (-1,-1)/(1,-1)/(1,1)/(-1,1)。
    pub fn corners(&self) -> [(f32, f32); 4] {
        [
            (self.u0, self.v0),
            (self.u1, self.v0),
            (self.u1, self.v1),
            (self.u0, self.v1),
        ]
    }
}
