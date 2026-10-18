//! 绘制区域（Area）：include ∪ exclude 的布尔组合，供 per-batch 模板裁切。
//!
//! CPU 侧只存树；`Renderer::draw` 解释为 stencil 掩码 pass（无 compute CSG）。
//! 与 `clips_children` / `InheritFromParent` 正交。

use crate::gpu::Vertex;
use crate::math::Rect;

/// 一次性告警：Area 选了 SDF 路径，但所有顶点 `sdf_type == 0`（掩码退化成外接矩形）。
static SDF_MISMATCH_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn warn_sdf_area_without_sdf_vertices() {
    if !SDF_MISMATCH_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!(
            "Area::geom: the SDF mask path is selected but every vertex has sdf_type = 0; \
             the mask degenerates to its bounding rectangle. Use `Area::rect(..)` for a \
             rectangular mask, or bake geometry with `DrawBatch::to_area()`"
        );
    }
}

/// 已烘焙的填充几何（含 transform 表），用作 Area 叶子。
#[derive(Clone, Debug)]
pub struct AreaGeom {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub transform_table: Vec<f32>,
    pub polygon_edges: Vec<f32>,
    pub has_sdf: bool,
    /// 与产生该几何时的 `DrawBatch::sdf_feather` 一致；`None` = 几何路径。
    pub sdf_feather: Option<f32>,
}

impl AreaGeom {
    /// 判断几何是否为空（无顶点或无索引）。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty() || self.indices.is_empty()
    }

    /// 是否为几何路径（非 SDF、无 feather）。
    #[inline]
    pub fn geometry_mode(&self) -> bool {
        !self.has_sdf && self.sdf_feather.is_none()
    }
}

/// 二维区域：全集 / 空集 / 几何 / ∪ ∩ \。
#[derive(Clone, Debug)]
pub enum Area {
    Full,
    Empty,
    Geom(AreaGeom),
    Union(Box<Area>, Box<Area>),
    Intersect(Box<Area>, Box<Area>),
    Difference(Box<Area>, Box<Area>),
}

impl Area {
    /// 全集（覆盖整个客户区）。
    #[inline]
    pub fn full() -> Self {
        Area::Full
    }

    /// 空集（无裁切）。
    #[inline]
    pub fn empty() -> Self {
        Area::Empty
    }

    /// 由烘焙几何构建 Area（空几何自动化简为 Empty）。
    ///
    /// 校验（debug 构建）：索引必须整三角形且不越界。若走 SDF 路径却没有任何
    /// `sdf_type != 0` 的顶点，掩码会退化成外接矩形 —— 首次发生打一条 `warn!`，
    /// 矩形掩码请直接用 [`Area::rect`]。
    #[inline]
    pub fn geom(g: AreaGeom) -> Self {
        if g.is_empty() {
            return Area::Empty;
        }
        debug_assert!(
            g.indices.len() % 3 == 0,
            "Area::geom: indices length {} is not a multiple of 3",
            g.indices.len()
        );
        debug_assert!(
            g.indices.iter().all(|&i| (i as usize) < g.vertices.len()),
            "Area::geom: index out of range ({} vertices)",
            g.vertices.len()
        );
        let sdf_path = g.has_sdf || g.sdf_feather.is_some();
        if sdf_path && g.vertices.iter().all(|v| v.sdf_type == 0) {
            warn_sdf_area_without_sdf_vertices();
        }
        Area::Geom(g)
    }

    /// 由逻辑像素矩形直接构建 Area（SDF 矩形掩码），免去手搓 [`AreaGeom`]。
    ///
    /// - `w <= 0.0 || h <= 0.0` → [`Area::Empty`]；
    /// - 顶点是世界坐标，`transform_table` 为空（恒等）；
    /// - `sdf_feather = Some(0.0)`：掩码边缘与矩形严格对齐（不带 1px 柔边外扩）。
    pub fn rect(r: Rect) -> Self {
        if r.w <= 0.0 || r.h <= 0.0 {
            return Area::Empty;
        }
        let params = [r.x + r.w * 0.5, r.y + r.h * 0.5, r.w * 0.5, r.h * 0.5];
        let corners = [
            (r.x, r.y),
            (r.x + r.w, r.y),
            (r.x + r.w, r.y + r.h),
            (r.x, r.y + r.h),
        ];
        let mut vertices = Vec::with_capacity(4);
        for (x, y) in corners {
            let mut v = Vertex::new_uv_xform(x, y, 0.0, 0.0, crate::color::colors::WHITE, 0);
            v.sdf_params = params;
            v.sdf_type = 2;
            v.sdf_feather = 0.0;
            vertices.push(v);
        }
        Area::geom(AreaGeom {
            vertices,
            indices: vec![0, 1, 2, 0, 2, 3],
            transform_table: Vec::new(),
            polygon_edges: Vec::new(),
            has_sdf: true,
            sdf_feather: Some(0.0),
        })
    }

    /// 是否为 Full。
    #[inline]
    pub fn is_full(&self) -> bool {
        matches!(self, Area::Full)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, Area::Empty)
    }

    /// 并集 A ∪ B（带 Full/Empty 化简）。
    pub fn union(self, other: Self) -> Self {
        match (self, other) {
            (Area::Full, _) | (_, Area::Full) => Area::Full,
            (Area::Empty, b) => b,
            (a, Area::Empty) => a,
            (a, b) => Area::Union(Box::new(a), Box::new(b)),
        }
    }

    /// 交集 A ∩ B。
    pub fn intersect(self, other: Self) -> Self {
        match (self, other) {
            (Area::Empty, _) | (_, Area::Empty) => Area::Empty,
            (Area::Full, b) => b,
            (a, Area::Full) => a,
            (a, b) => Area::Intersect(Box::new(a), Box::new(b)),
        }
    }

    /// 差集 A \ B。
    pub fn difference(self, other: Self) -> Self {
        match (self, other) {
            (Area::Empty, _) => Area::Empty,
            (a, Area::Empty) => a,
            (_, Area::Full) => Area::Empty,
            (a, b) => Area::Difference(Box::new(a), Box::new(b)),
        }
    }

    /// 递归化简：展开 Full/Empty 规则（如 Full ∪ X = Full）。
    pub fn simplified(self) -> Self {
        match self {
            Area::Full | Area::Empty => self,
            Area::Geom(g) => Area::geom(g),
            Area::Union(a, b) => a.simplified().union(b.simplified()),
            Area::Intersect(a, b) => a.simplified().intersect(b.simplified()),
            Area::Difference(a, b) => a.simplified().difference(b.simplified()),
        }
    }

    /// 将「在 parent 可见区（stencil==base）内，把本 Area 盖成 base+1」编译为掩码 op。
    ///
    /// 结束后：Area 内为 `base+1`，Area 外（仍在 parent 内）为 `base`。
    /// Intersect 可能短暂使用 `base+2`。
    pub fn compile_cover(&self, base: u32, out: &mut Vec<AreaStencilOp>) {
        match self {
            Area::Full => out.push(AreaStencilOp::CoverFull { stencil_ref: base }),
            Area::Empty => {}
            Area::Geom(g) => {
                if !g.is_empty() {
                    out.push(AreaStencilOp::CoverGeom {
                        geom: g.clone(),
                        stencil_ref: base,
                    });
                }
            }
            Area::Union(a, b) => {
                a.compile_cover(base, out);
                b.compile_cover(base, out);
            }
            Area::Difference(a, b) => {
                a.compile_cover(base, out);
                b.compile_erase(base + 1, out);
            }
            Area::Intersect(a, b) => {
                // a → base+1；再 cover b 于 base+1 → a∩b 为 base+2；
                // 清掉仍为 base+1 的 a\b；再把 base+2 降为 base+1。
                a.compile_cover(base, out);
                b.compile_cover(base + 1, out);
                out.push(AreaStencilOp::EraseFull {
                    stencil_ref: base + 1,
                });
                out.push(AreaStencilOp::EraseFull {
                    stencil_ref: base + 2,
                });
            }
        }
    }

    /// 在 stencil==level 且落在本 Area 内的像素上 Dec。
    pub(crate) fn compile_erase(&self, level: u32, out: &mut Vec<AreaStencilOp>) {
        match self {
            Area::Full => out.push(AreaStencilOp::EraseFull { stencil_ref: level }),
            Area::Empty => {}
            Area::Geom(g) => {
                if !g.is_empty() {
                    out.push(AreaStencilOp::EraseGeom {
                        geom: g.clone(),
                        stencil_ref: level,
                    });
                }
            }
            Area::Union(a, b) => {
                a.compile_erase(level, out);
                b.compile_erase(level, out);
            }
            Area::Difference(a, b) => {
                // 去掉 a\b：先盖住 b（level→level+1 于 b∩mask），再 erase a 于 level，
                // 再把 level+1 降回 level（恢复 a∩b）。
                b.compile_cover(level, out);
                a.compile_erase(level, out);
                out.push(AreaStencilOp::EraseFull {
                    stencil_ref: level + 1,
                });
            }
            Area::Intersect(a, _) => {
                // 目标：a∩b 从 level 降到 level-1，a\b 与 outside 不动。
                // `a.erase(level)` 等价于 "Dec a at stencil==level"——
                //   a∩b 处于 level（在 a 内）→ level-1
                //   a\b 处于 level-1（不在 level 上）→ 不变
                //   之前 `Difference(a,b).compile_cover(level-1)` 是错的：
                //   它会把 a∩b 重新 raise 到 level（cover 序列：a→b→erase→erase），
                //   抵消本次 erase。统一所有 level 用同一逻辑。
                a.compile_erase(level, out);
            }
        }
    }

    /// 掩码清理：全屏 Dec 掉 `mask_level`（通常为 base+1）。
    pub fn compile_cleanup(mask_level: u32, out: &mut Vec<AreaStencilOp>) {
        out.push(AreaStencilOp::EraseFull {
            stencil_ref: mask_level,
        });
    }

    /// cover 过程中可能触达的最高 stencil 值（含 base+1 结果层）。
    pub fn max_stencil_level(&self, base: u32) -> u32 {
        match self {
            Area::Full | Area::Empty | Area::Geom(_) => base + 1,
            Area::Union(a, b) => a.max_stencil_level(base).max(b.max_stencil_level(base)),
            Area::Difference(a, b) => a.max_stencil_level(base).max(b.max_stencil_level(base + 1)),
            Area::Intersect(a, b) => {
                // cover a at base, cover b at base+1 → up to base+2
                a.max_stencil_level(base)
                    .max(b.max_stencil_level(base + 1))
                    .max(base + 2)
            }
        }
    }
}

/// 单次 Area 掩码绘制指令（无颜色写入）。
#[derive(Clone, Debug)]
pub enum AreaStencilOp {
    CoverGeom { geom: AreaGeom, stencil_ref: u32 },
    CoverFull { stencil_ref: u32 },
    EraseGeom { geom: AreaGeom, stencil_ref: u32 },
    EraseFull { stencil_ref: u32 },
}

impl AreaStencilOp {
    /// 对应的 stencil 管线 op：4 = Cover (Equal+Inc)，3 = Erase (Equal+Dec)。
    #[inline]
    pub fn stencil_pipeline_op(&self) -> u32 {
        match self {
            AreaStencilOp::CoverGeom { .. } | AreaStencilOp::CoverFull { .. } => 4,
            AreaStencilOp::EraseGeom { .. } | AreaStencilOp::EraseFull { .. } => 3,
        }
    }

    /// 当前 op 使用的 stencil_ref 值。
    #[inline]
    pub fn stencil_ref(&self) -> u32 {
        match self {
            AreaStencilOp::CoverGeom { stencil_ref, .. }
            | AreaStencilOp::CoverFull { stencil_ref }
            | AreaStencilOp::EraseGeom { stencil_ref, .. }
            | AreaStencilOp::EraseFull { stencil_ref } => *stencil_ref,
        }
    }

    /// 若为 Geom 变体返回其引用，否则 None。
    #[inline]
    pub fn geom(&self) -> Option<&AreaGeom> {
        match self {
            AreaStencilOp::CoverGeom { geom, .. } | AreaStencilOp::EraseGeom { geom, .. } => {
                Some(geom)
            }
            AreaStencilOp::CoverFull { .. } | AreaStencilOp::EraseFull { .. } => None,
        }
    }

    /// 是否为全屏操作（CoverFull/EraseFull，无 Geom）。
    #[inline]
    pub fn is_fullscreen(&self) -> bool {
        matches!(
            self,
            AreaStencilOp::CoverFull { .. } | AreaStencilOp::EraseFull { .. }
        )
    }
}

/// include \ exclude；二者皆 `None` 表示不走 Area 路径。
pub fn effective_area(include: Option<&Area>, exclude: Option<&Area>) -> Option<Area> {
    match (include, exclude) {
        (None, None) => None,
        (inc, exc) => {
            let a = inc.cloned().unwrap_or(Area::Full);
            let b = exc.cloned().unwrap_or(Area::Empty);
            Some(a.difference(b).simplified())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::colors::WHITE;
    use crate::gpu::Vertex;

    fn unit_quad() -> AreaGeom {
        let idx = 0u32;
        AreaGeom {
            vertices: vec![
                Vertex::new_uv_xform(0.0, 0.0, 0.0, 0.0, WHITE, idx),
                Vertex::new_uv_xform(1.0, 0.0, 0.0, 0.0, WHITE, idx),
                Vertex::new_uv_xform(1.0, 1.0, 0.0, 0.0, WHITE, idx),
                Vertex::new_uv_xform(0.0, 1.0, 0.0, 0.0, WHITE, idx),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            transform_table: vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            polygon_edges: Vec::new(),
            has_sdf: false,
            sdf_feather: None,
        }
    }

    #[test]
    fn rect_area_builds_sdf_quad_in_world_space() {
        let a = Area::rect(Rect::new(10.0, 20.0, 100.0, 50.0));
        let g = match &a {
            Area::Geom(g) => g,
            _ => panic!("expected Area::Geom"),
        };
        assert_eq!(g.vertices.len(), 4);
        assert_eq!(g.indices, vec![0, 1, 2, 0, 2, 3]);
        assert!(g.has_sdf);
        assert_eq!(g.sdf_feather, Some(0.0));
        assert!(g.transform_table.is_empty());
        assert!(g.vertices.iter().all(|v| v.sdf_type == 2));
        assert!(
            g.vertices
                .iter()
                .all(|v| v.sdf_params == [60.0, 45.0, 50.0, 25.0])
        );
    }

    #[test]
    fn rect_area_degenerate_is_empty() {
        assert!(Area::rect(Rect::new(0.0, 0.0, 0.0, 10.0)).is_empty());
        assert!(Area::rect(Rect::new(0.0, 0.0, 10.0, -1.0)).is_empty());
    }

    #[test]
    fn simplify_union_full() {
        let a = Area::geom(unit_quad()).union(Area::Full);
        assert!(a.simplified().is_full());
    }

    #[test]
    fn simplify_intersect_empty() {
        let a = Area::geom(unit_quad()).intersect(Area::Empty);
        assert!(a.simplified().is_empty());
    }

    #[test]
    fn simplify_difference_full_exclude() {
        let a = Area::geom(unit_quad()).difference(Area::Full);
        assert!(a.simplified().is_empty());
    }

    #[test]
    fn effective_include_only() {
        let inc = Area::geom(unit_quad());
        let e = effective_area(Some(&inc), None).unwrap();
        assert!(matches!(e, Area::Geom(_)));
    }

    #[test]
    fn effective_exclude_only_is_full_minus() {
        let exc = Area::geom(unit_quad());
        let e = effective_area(None, Some(&exc)).unwrap();
        assert!(matches!(e, Area::Difference(_, _)));
    }

    #[test]
    fn compile_cover_geom_one_op() {
        let a = Area::geom(unit_quad());
        let mut ops = Vec::new();
        a.compile_cover(0, &mut ops);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].stencil_pipeline_op(), 4);
        assert_eq!(ops[0].stencil_ref(), 0);
    }

    #[test]
    fn compile_union_two_covers() {
        let a = Area::geom(unit_quad()).union(Area::geom(unit_quad()));
        let mut ops = Vec::new();
        a.compile_cover(1, &mut ops);
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|o| o.stencil_ref() == 1));
    }

    #[test]
    fn compile_difference_cover_then_erase() {
        let a = Area::geom(unit_quad()).difference(Area::geom(unit_quad()));
        let mut ops = Vec::new();
        a.compile_cover(0, &mut ops);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].stencil_pipeline_op(), 4);
        assert_eq!(ops[1].stencil_pipeline_op(), 3);
        assert_eq!(ops[1].stencil_ref(), 1);
    }

    #[test]
    fn compile_intersect_uses_temp_level() {
        let a = Area::geom(unit_quad()).intersect(Area::geom(unit_quad()));
        let mut ops = Vec::new();
        a.compile_cover(0, &mut ops);
        assert!(ops.len() >= 4);
        assert_eq!(a.max_stencil_level(0), 2);
    }

    #[test]
    fn compile_intersect_erase_is_one_dec() {
        // 修 Bug 6.1：原非零路径会通过 Difference.cover 错误地把 a∩b 重新 raise 回来。
        // 统一后 Intersect.erase(level) 应等价于 a.erase(level)：单个 EraseGeom。
        let a = Area::geom(unit_quad()).intersect(Area::geom(unit_quad()));
        let mut ops = Vec::new();
        a.compile_erase(1, &mut ops);
        assert_eq!(
            ops.len(),
            1,
            "expected 1 op, got {:?}: {:#?}",
            ops.len(),
            ops
        );
        assert_eq!(ops[0].stencil_pipeline_op(), 3); // EraseGeom
        assert_eq!(ops[0].stencil_ref(), 1);
    }

    #[test]
    fn empty_geom_becomes_empty_area() {
        let g = AreaGeom {
            vertices: Vec::new(),
            indices: Vec::new(),
            transform_table: Vec::new(),
            polygon_edges: Vec::new(),
            has_sdf: false,
            sdf_feather: None,
        };
        assert!(Area::geom(g).is_empty());
    }
}
