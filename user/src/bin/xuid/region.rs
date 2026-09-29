//! A small rectangle region used by the occlusion-aware repaint (issue #360):
//! "this rectangle minus everything opaque painted above it".
//!
//! The compositor's allocator never reclaims, so a repaint cannot allocate:
//! the region is a fixed-capacity array. When a subtraction would overflow it,
//! subtraction is skipped instead. That is always safe, because the
//! repaint paints bottom-up, so an over-large region only repaints pixels a
//! later, higher layer overwrites.

use user::messenger::display::Rect;

/// The most disjoint rectangles a region tracks. Subtracting one rectangle
/// from one splits it into at most four, so a handful of windows stays exact.
const CAPACITY: usize = 32;

/// A set of disjoint rectangles.
pub(super) struct Region {
    rects: [Rect; CAPACITY],
    len: usize,
}

impl Region {
    /// The region covering `rect` (empty when `rect` is).
    pub(super) fn new(rect: Rect) -> Region {
        let mut region = Region {
            rects: [Rect::new(0, 0, 0, 0); CAPACITY],
            len: 0,
        };
        if !rect.is_empty() {
            region.rects[0] = rect;
            region.len = 1;
        }
        region
    }

    /// Whether nothing is left.
    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The remaining disjoint rectangles.
    pub(super) fn rects(&self) -> &[Rect] {
        &self.rects[..self.len]
    }

    /// Remove `cover` from the region.
    pub(super) fn subtract(&mut self, cover: Rect) {
        if cover.is_empty() || self.is_empty() {
            return;
        }
        let mut out = [Rect::new(0, 0, 0, 0); CAPACITY];
        let mut out_len = 0;
        for &rect in &self.rects[..self.len] {
            let mut pieces = [Rect::new(0, 0, 0, 0); 4];
            let count = split_around(rect, cover, &mut pieces);
            if out_len + count > CAPACITY {
                // Out of room: skip this subtraction and keep the region
                // as it was (over-coverage only repaints, never miscolours).
                return;
            }
            out[out_len..out_len + count].copy_from_slice(&pieces[..count]);
            out_len += count;
        }
        self.rects = out;
        self.len = out_len;
    }
}

/// Write the parts of `rect` outside `cover` into `pieces` (top, bottom, left,
/// right) and return how many there are. `rect` itself when they are disjoint.
fn split_around(rect: Rect, cover: Rect, pieces: &mut [Rect; 4]) -> usize {
    let hit = rect.intersect(cover);
    if hit.is_empty() {
        pieces[0] = rect;
        return 1;
    }
    let mut count = 0;
    let mut push = |piece: Rect| {
        if !piece.is_empty() {
            pieces[count] = piece;
            count += 1;
        }
    };
    push(Rect::new(rect.x, rect.y, rect.w, hit.y - rect.y));
    push(Rect::new(
        rect.x,
        hit.y + hit.h,
        rect.w,
        rect.y + rect.h - (hit.y + hit.h),
    ));
    push(Rect::new(rect.x, hit.y, hit.x - rect.x, hit.h));
    push(Rect::new(
        hit.x + hit.w,
        hit.y,
        rect.x + rect.w - (hit.x + hit.w),
        hit.h,
    ));
    count
}
