use parking_lot::Mutex;

use rustc_hash::FxHashMap;

use crate::math::{affine_rect_bounds, Rect, Transform};
use crate::render::{DrawBatch, DrawEvent};

pub(crate) type AabbMap = FxHashMap<usize, Option<Rect>>;
pub(crate) type ViewMap = FxHashMap<usize, Transform>;

pub(crate) fn compute_subtree_aabb(
    batch: &DrawBatch,
    map: &mut AabbMap,
    view: &Transform,
) -> Option<Rect> {
    let key = batch as *const DrawBatch as *const () as usize;
    let eff_view = view.then(&batch.view);
    let own = batch.compute_own_world_aabb().map(|b| {
        let (v0, v1, v2) = eff_view.to_cols();
        affine_rect_bounds(&b, v0, v1, v2)
    });
    let mut combined = own;
    for child in &batch.children {
        let ca = compute_subtree_aabb(child, map, &eff_view);
        if let Some(c) = ca {
            combined = match combined {
                Some(a) => Some(a.union(&c)),
                None => Some(c),
            };
        }
    }
    map.insert(key, combined);
    combined
}

pub(crate) fn viewport_for_culling(logical_width: f32, logical_height: f32) -> Rect {
    Rect::new(0.0, 0.0, logical_width, logical_height)
}

pub(crate) fn prepare_culling<'a>(
    batches: &[&'a DrawBatch],
    logical_width: f32,
    logical_height: f32,
    scratch_aabb_map: &Mutex<AabbMap>,
    scratch_view_map: &Mutex<ViewMap>,
    events: &mut Vec<DrawEvent<'a>>,
) -> (Rect, bool) {
    let viewport = viewport_for_culling(logical_width, logical_height);
    {
        let mut aabb_map = scratch_aabb_map.lock();
        aabb_map.clear();
        for b in batches {
            compute_subtree_aabb(b, &mut aabb_map, &Transform::IDENTITY);
        }
    }
    let mut uses_stencil = false;
    {
        let aabb_map = scratch_aabb_map.lock();
        let mut view_map = scratch_view_map.lock();
        view_map.clear();
        for b in batches {
            let event_start = events.len();
            b.flatten_events(
                events,
                0,
                Some(viewport),
                &aabb_map,
                &Transform::IDENTITY,
                &mut view_map,
            );
            uses_stencil |= events[event_start..]
                .iter()
                .any(|ev| matches!(ev, DrawEvent::StencilPop | DrawEvent::AreaOp { .. }));
        }
    }
    (viewport, uses_stencil)
}


