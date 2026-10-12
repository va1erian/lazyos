#![forbid(unsafe_code)]

//! Combining a new shape with the selection already there.

use super::Selection;
use crate::model::div255;
use crate::model::rect::Rect;

/// How a new selection joins the existing one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CombineMode {
    /// The new shape replaces the selection.
    #[default]
    Replace,
    /// Everything in either.
    Union,
    /// The selection without the new shape.
    Exclude,
    /// Only what is in both.
    Intersect,
    /// What is in exactly one of the two.
    Xor,
}

impl CombineMode {
    /// Every mode, in the options bar's order.
    pub const ALL: [CombineMode; 5] = [
        CombineMode::Replace,
        CombineMode::Union,
        CombineMode::Exclude,
        CombineMode::Intersect,
        CombineMode::Xor,
    ];

    /// The name the options bar shows.
    pub const fn label(self) -> &'static str {
        match self {
            CombineMode::Replace => "Replace",
            CombineMode::Union => "Union",
            CombineMode::Exclude => "Exclude",
            CombineMode::Intersect => "Intersect",
            CombineMode::Xor => "Xor",
        }
    }

    /// The coverage of one pixel given the old (`a`) and new (`b`) coverage.
    /// Written for soft edges: on 0/255 inputs it is plain set algebra.
    fn apply(self, a: u8, b: u8) -> u8 {
        let (a, b) = (u32::from(a), u32::from(b));
        let both = div255(a * b);
        let out = match self {
            CombineMode::Replace => b,
            CombineMode::Union => a + b - both,
            CombineMode::Exclude => a - both,
            CombineMode::Intersect => both,
            CombineMode::Xor => (a + b).saturating_sub(2 * both),
        };
        out.min(255) as u8
    }
}

impl Selection {
    /// `shape` joined to `base` by `mode`. "No selection" is the empty set: a
    /// union with it is the shape, an exclusion or intersection leaves nothing.
    pub fn combine(
        base: Option<&Selection>,
        shape: Option<&Selection>,
        mode: CombineMode,
    ) -> Option<Selection> {
        let (base, shape) = match (base, shape, mode) {
            (_, shape, CombineMode::Replace) => return shape.cloned(),
            (None, shape, CombineMode::Union | CombineMode::Xor) => return shape.cloned(),
            (None, _, _) => return None,
            (Some(_), None, CombineMode::Intersect) => return None,
            (Some(base), None, _) => return Some(base.clone()),
            (Some(base), Some(shape), _) => (base, shape),
        };
        if mode == CombineMode::Intersect && base.bounds.intersect(shape.bounds).is_empty() {
            return None;
        }
        let area = match mode {
            CombineMode::Exclude => base.bounds,
            CombineMode::Intersect => base.bounds.intersect(shape.bounds),
            _ => base.bounds.union(shape.bounds),
        };
        combine_over(area, base, shape, mode)
    }
}

/// The result over `area`, which holds every pixel `mode` can leave set.
fn combine_over(
    area: Rect,
    base: &Selection,
    shape: &Selection,
    mode: CombineMode,
) -> Option<Selection> {
    // Two rectangles that nest or touch exactly stay rectangles without a mask.
    if base.is_rect() && shape.is_rect() {
        match mode {
            CombineMode::Union
                if base.bounds.union(shape.bounds).area() == union_area(base, shape) =>
            {
                return Selection::rect(area);
            }
            CombineMode::Intersect => return Selection::rect(area),
            _ => {}
        }
    }
    let mut mask = Vec::with_capacity(area.area());
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            mask.push(mode.apply(base.coverage(x, y), shape.coverage(x, y)));
        }
    }
    Selection::from_mask(area, mask)
}

/// The pixels two rectangles cover together.
fn union_area(a: &Selection, b: &Selection) -> usize {
    a.bounds.area() + b.bounds.area() - a.bounds.intersect(b.bounds).area()
}
