#![forbid(unsafe_code)]

//! The selection: a bounding rectangle plus an optional 8-bit mask.
//!
//! `mask: None` means the rectangle exactly, the common case, kept free. A mask
//! holds one coverage byte per pixel of `bounds`, row-major; v1 builds only
//! hard-edged masks (0 or 255) but every operation is written for the full
//! 8-bit range so feathering (v2) changes the producers, not the consumers.
//!
//! The invariants, kept by [`Selection::from_mask`] (the only way a mask gets
//! in): `bounds` is not empty, the mask (when present) has `bounds.area()`
//! bytes, is not all 255 (that is the rectangle) and touches every edge of
//! `bounds` (so the bounds are tight). "Nothing selected" is `None` at the
//! document level, never an empty `Selection`.

mod combine;
mod crop;
mod outline;
mod shapes;

pub use combine::CombineMode;
pub(crate) use crop::clear_outside_mask;
pub use outline::Edge;

use super::rect::Rect;

/// A set of pixels with a coverage per pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    bounds: Rect,
    mask: Option<Vec<u8>>,
}

impl Selection {
    /// The rectangle `bounds`, or `None` when it is empty.
    pub fn rect(bounds: Rect) -> Option<Selection> {
        (!bounds.is_empty()).then_some(Selection { bounds, mask: None })
    }

    /// A selection over `bounds` with `mask` (one byte per pixel of `bounds`),
    /// normalised: tight bounds, no mask when everything is selected, `None`
    /// when nothing is. A mask of the wrong length selects nothing.
    pub fn from_mask(bounds: Rect, mask: Vec<u8>) -> Option<Selection> {
        if bounds.is_empty() || mask.len() != bounds.area() {
            return None;
        }
        let width = bounds.width() as usize;
        let mut tight = Rect::EMPTY;
        for (row, line) in mask.chunks_exact(width).enumerate() {
            let Some(first) = line.iter().position(|c| *c != 0) else {
                continue;
            };
            let last = line.iter().rposition(|c| *c != 0).unwrap_or(first);
            let y = bounds.y0 + row as i32;
            tight = tight.union(Rect {
                x0: bounds.x0 + first as i32,
                x1: bounds.x0 + last as i32 + 1,
                y0: y,
                y1: y + 1,
            });
        }
        if tight.is_empty() {
            return None;
        }
        let mask = if tight == bounds {
            mask
        } else {
            let mut cropped = Vec::with_capacity(tight.area());
            for y in tight.y0..tight.y1 {
                let from = (y - bounds.y0) as usize * width + (tight.x0 - bounds.x0) as usize;
                cropped.extend_from_slice(&mask[from..from + tight.width() as usize]);
            }
            cropped
        };
        let full = mask.iter().all(|c| *c == 255);
        Some(Selection {
            bounds: tight,
            mask: (!full).then_some(mask),
        })
    }

    /// The bounding rectangle.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Whether the selection is exactly its bounding rectangle.
    pub fn is_rect(&self) -> bool {
        self.mask.is_none()
    }

    /// The width and height of the bounds.
    pub fn size(&self) -> (u32, u32) {
        (self.bounds.width(), self.bounds.height())
    }

    /// The coverage of pixel `(x, y)`: 0 outside, 255 inside, in between for a
    /// soft edge.
    pub fn coverage(&self, x: i32, y: i32) -> u8 {
        if !self.bounds.contains(x, y) {
            return 0;
        }
        match &self.mask {
            None => 255,
            Some(mask) => {
                let width = self.bounds.width() as usize;
                mask[(y - self.bounds.y0) as usize * width + (x - self.bounds.x0) as usize]
            }
        }
    }

    /// Whether `(x, y)` is at least half selected.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.coverage(x, y) >= 128
    }

    /// The bytes the mask holds (what the history counts).
    pub fn bytes(&self) -> usize {
        self.mask.as_ref().map_or(0, Vec::len)
    }

    /// The selection moved by `(dx, dy)`.
    pub fn translated(&self, dx: i32, dy: i32) -> Selection {
        let shift = |v: i32, d: i32| v.saturating_add(d);
        Selection {
            bounds: Rect {
                x0: shift(self.bounds.x0, dx),
                y0: shift(self.bounds.y0, dy),
                x1: shift(self.bounds.x1, dx),
                y1: shift(self.bounds.y1, dy),
            },
            mask: self.mask.clone(),
        }
    }

    /// The part of the selection inside `area`, or `None` when none is.
    pub fn clipped(&self, area: Rect) -> Option<Selection> {
        let inside = self.bounds.intersect(area);
        if inside.is_empty() {
            return None;
        }
        if inside == self.bounds {
            return Some(self.clone());
        }
        match &self.mask {
            None => Selection::rect(inside),
            Some(_) => self.with_bounds(inside),
        }
    }

    /// The same coverage seen through `area` (which may be larger than the
    /// bounds), renormalised.
    fn with_bounds(&self, area: Rect) -> Option<Selection> {
        let mut mask = Vec::with_capacity(area.area());
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                mask.push(self.coverage(x, y));
            }
        }
        Selection::from_mask(area, mask)
    }

    /// Everything of `area` this selection does not cover; `None` when it
    /// covers all of it.
    pub fn inverted_within(&self, area: Rect) -> Option<Selection> {
        let mut mask = Vec::with_capacity(area.area());
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                mask.push(255 - self.coverage(x, y));
            }
        }
        Selection::from_mask(area, mask)
    }
}

/// `new` blended over `old` by `cover` (0 keeps `old`, 255 takes `new`): how a
/// tool's pixel lands through a selection edge.
#[inline]
pub(crate) fn mix_by(cover: u8, old: [u8; 4], new: [u8; 4]) -> [u8; 4] {
    match cover {
        0 => old,
        255 => new,
        c => {
            let c = u32::from(c);
            let m = |o: u8, n: u8| super::div255(u32::from(o) * (255 - c) + u32::from(n) * c) as u8;
            [
                m(old[0], new[0]),
                m(old[1], new[1]),
                m(old[2], new[2]),
                m(old[3], new[3]),
            ]
        }
    }
}

#[cfg(test)]
mod tests;
