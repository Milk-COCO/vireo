//! 绘制区域（Area）：include ∪ exclude 的布尔组合，供 per-batch 模板裁切。
//!
//! CPU 侧只存树；`Renderer::draw` 解释为 stencil 掩码 pass（无 compute CSG）。
//! 与 `clips_children` / `InheritFromParent` 正交。

use crate::gpu::Vertex;

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
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty() || self.indices.is_empty()
    }

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
    #[inline]
    pub fn full() -> Self {
        Area::Full
    }

    #[inline]
    pub fn empty() -> Self {
        Area::Empty
    }

    #[inline]
    pub fn geom(g: AreaGeom) -> Self {
        if g.is_empty() {
            Area::Empty
        } else {
            Area::Geom(g)
        }
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        matches!(self, Area::Full)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, Area::Empty)
    }

    /// A ∪ B（带 Full/Empty 化简）。
    pub fn union(self, other: Self) -> Self {
        match (self, other) {
            (Area::Full, _) | (_, Area::Full) => Area::Full,
            (Area::Empty, b) => b,
            (a, Area::Empty) => a,
            (a, b) => Area::Union(Box::new(a), Box::new(b)),
        }
    }

    /// A ∩ B。
    pub fn intersect(self, other: Self) -> Self {
        match (self, other) {
            (Area::Empty, _) | (_, Area::Empty) => Area::Empty,
            (Area::Full, b) => b,
            (a, Area::Full) => a,
            (a, b) => Area::Intersect(Box::new(a), Box::new(b)),
        }
    }

    /// A \ B。
    pub fn difference(self, other: Self) -> Self {
        match (self, other) {
            (Area::Empty, _) => Area::Empty,
            (a, Area::Empty) => a,
            (_, Area::Full) => Area::Empty,
            (a, b) => Area::Difference(Box::new(a), Box::new(b)),
        }
    }

    /// 递归化简 Full/Empty。
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
            Area::Full => out.push(AreaStencilOp::EraseFull {
                stencil_ref: level,
            }),
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
            Area::Intersect(a, b) => {
                // erase(a) 后 a∩b 与 a\b 均在 level-1；再 cover(a\b) 把 a\b 抬回 level。
                if level == 0 {
                    a.compile_cover(0, out);
                    b.compile_cover(1, out);
                    out.push(AreaStencilOp::EraseFull {
                        stencil_ref: 1,
                    });
                    out.push(AreaStencilOp::EraseFull {
                        stencil_ref: 2,
                    });
                    out.push(AreaStencilOp::EraseFull {
                        stencil_ref: 1,
                    });
                } else {
                    a.compile_erase(level, out);
                    Area::Difference(a.clone(), b.clone()).compile_cover(level - 1, out);
                }
            }
        }
    }

    /// 掩码清理：全屏 Dec 掉 `mask_level`（通常 base+1）。
    pub fn compile_cleanup(mask_level: u32, out: &mut Vec<AreaStencilOp>) {
        out.push(AreaStencilOp::EraseFull {
            stencil_ref: mask_level,
        });
    }

    /// cover 过程中可能触达的最高 stencil 值（含 base+1 结果层）。
    pub fn max_stencil_level(&self, base: u32) -> u32 {
        match self {
            Area::Full | Area::Empty | Area::Geom(_) => base + 1,
            Area::Union(a, b) => a
                .max_stencil_level(base)
                .max(b.max_stencil_level(base)),
            Area::Difference(a, b) => a
                .max_stencil_level(base)
                .max(b.max_stencil_level(base + 1)),
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
    /// 4 = Equal+Inc 无色, 3 = Equal+Dec 无色（与 clips Pop 相同）。
    #[inline]
    pub fn stencil_pipeline_op(&self) -> u32 {
        match self {
            AreaStencilOp::CoverGeom { .. } | AreaStencilOp::CoverFull { .. } => 4,
            AreaStencilOp::EraseGeom { .. } | AreaStencilOp::EraseFull { .. } => 3,
        }
    }

    #[inline]
    pub fn stencil_ref(&self) -> u32 {
        match self {
            AreaStencilOp::CoverGeom { stencil_ref, .. }
            | AreaStencilOp::CoverFull { stencil_ref }
            | AreaStencilOp::EraseGeom { stencil_ref, .. }
            | AreaStencilOp::EraseFull { stencil_ref } => *stencil_ref,
        }
    }

    #[inline]
    pub fn geom(&self) -> Option<&AreaGeom> {
        match self {
            AreaStencilOp::CoverGeom { geom, .. } | AreaStencilOp::EraseGeom { geom, .. } => {
                Some(geom)
            }
            AreaStencilOp::CoverFull { .. } | AreaStencilOp::EraseFull { .. } => None,
        }
    }

    #[inline]
    pub fn is_fullscreen(&self) -> bool {
        matches!(
            self,
            AreaStencilOp::CoverFull { .. } | AreaStencilOp::EraseFull { .. }
        )
    }
}

/// include \ exclude；二者皆 `None` 表示不走 Area 路径。
pub fn effective_area(
    include: Option<&Area>,
    exclude: Option<&Area>,
) -> Option<Area> {
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
            transform_table: vec![
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
            ],
            polygon_edges: Vec::new(),
            has_sdf: false,
            sdf_feather: None,
        }
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
    fn effective_none_none() {
        assert!(effective_area(None, None).is_none());
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
