#![forbid(unsafe_code)]

//! The stroke buffer: coverage for the stroke in progress.
//!
//! Dabs, segments and shapes write coverage with `max`, so overlapping dabs
//! inside one stroke do not build up alpha (a 50 % brush stays 50 % where the
//! stroke crosses itself). [`StrokeBuffer::composite`] then applies
//! `coverage x clip x colour alpha` to the layer through `blend.rs`.
//!
//! The buffer also remembers the layer pixels it is about to cover, in the same
//! (growing) rectangle. That is what makes per-frame compositing exact: every
//! pass recomputes `base + coverage`, never `layer + coverage` twice. It is
//! also the undo "before" picture and the way a cancel puts the layer back.

use crate::model::bitmap::{Bitmap, Pixel};
use crate::model::blend::{div255, source_over};
use crate::model::rect::Rect;
use crate::model::selection::mix_by;

use super::coverage::{Clip, Coverage};

/// How composited coverage changes the layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaintMode {
    /// Source-over of the colour.
    #[default]
    Paint,
    /// Reduces alpha by `coverage x colour alpha`; the colour is ignored.
    Erase,
    /// Writes the colour over the pixel, alpha included, weighted by the
    /// coverage (the Pencil: exact colour, no blending with what is there).
    Replace,
}

/// Coverage for one stroke over one layer.
#[derive(Debug)]
pub struct StrokeBuffer {
    /// The layer's pixels; nothing outside is ever written.
    pub(super) bounds: Rect,
    /// What `cov` and `base` span (a padded superset of `dirty`).
    alloc: Rect,
    /// The union of every pixel given coverage.
    dirty: Rect,
    /// Coverage written since the last composite.
    pending: Rect,
    cov: Vec<u8>,
    base: Vec<u8>,
}

impl StrokeBuffer {
    /// An empty stroke over `layer`.
    pub fn new(layer: &Bitmap) -> StrokeBuffer {
        StrokeBuffer {
            bounds: layer.rect(),
            alloc: Rect::EMPTY,
            dirty: Rect::EMPTY,
            pending: Rect::EMPTY,
            cov: Vec::new(),
            base: Vec::new(),
        }
    }

    /// The pixels given coverage so far.
    pub fn dirty(&self) -> Rect {
        self.dirty
    }

    /// The pixels not yet composited.
    pub fn pending(&self) -> Rect {
        self.pending
    }

    /// Whether nothing was drawn.
    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }

    /// The stroke's coverage at `(x, y)`.
    pub fn coverage_at(&self, x: i32, y: i32) -> u8 {
        if !self.alloc.contains(x, y) {
            return 0;
        }
        self.cov[self.index(x, y)]
    }

    fn index(&self, x: i32, y: i32) -> usize {
        (y - self.alloc.y0) as usize * self.alloc.width() as usize + (x - self.alloc.x0) as usize
    }

    /// Makes `need` (clipped to the layer) addressable, snapshotting the layer
    /// under any newly spanned pixels. Growth is geometric so a long stroke
    /// does not copy the buffer on every segment.
    pub(super) fn reserve(&mut self, layer: &Bitmap, need: Rect) -> Rect {
        let need = need.intersect(self.bounds);
        if need.is_empty() || (!self.alloc.is_empty() && self.alloc.intersect(need) == need) {
            return need;
        }
        let union = self.alloc.union(need);
        let pad = (union.width().max(union.height()) / 4).max(32) as i32;
        let grown = union.inflate(pad).intersect(self.bounds);
        let mut base = vec![0u8; grown.area() * 4];
        let width = grown.width() as usize;
        for y in grown.y0..grown.y1 {
            let at = (y - grown.y0) as usize * width * 4;
            base[at..at + width * 4].copy_from_slice(layer.row(y, grown.x0, width));
        }
        let mut cov = vec![0u8; grown.area()];
        if !self.alloc.is_empty() {
            let old_w = self.alloc.width() as usize;
            for y in self.alloc.y0..self.alloc.y1 {
                let from = (y - self.alloc.y0) as usize * old_w;
                let to = (y - grown.y0) as usize * width + (self.alloc.x0 - grown.x0) as usize;
                cov[to..to + old_w].copy_from_slice(&self.cov[from..from + old_w]);
                // The old snapshot, not the layer: it has been painted on.
                base[to * 4..(to + old_w) * 4]
                    .copy_from_slice(&self.base[from * 4..(from + old_w) * 4]);
            }
        }
        self.alloc = grown;
        self.cov = cov;
        self.base = base;
        need
    }

    /// Raises coverage at `(x, y)` to at least `value`. `(x, y)` must be inside
    /// the reserved rectangle.
    #[inline]
    pub(super) fn raise(&mut self, x: i32, y: i32, value: u8) {
        let at = self.index(x, y);
        if self.cov[at] < value {
            self.cov[at] = value;
        }
    }

    /// Records that `rect` (already reserved) received coverage.
    pub(super) fn mark(&mut self, rect: Rect) {
        self.dirty = self.dirty.union(rect);
        self.pending = self.pending.union(rect);
    }

    /// Merges a mask (shape, glyphs, bucket region) with `max`, returning the
    /// pixels it touched.
    pub fn add_coverage(&mut self, layer: &Bitmap, mask: &Coverage) -> Rect {
        let rect = self.reserve(layer, mask.rect());
        if rect.is_empty() {
            return Rect::EMPTY;
        }
        for y in rect.y0..rect.y1 {
            let row = mask.row(y, rect.x0, rect.x1);
            let at = self.index(rect.x0, y);
            for (slot, &value) in self.cov[at..at + row.len()].iter_mut().zip(row) {
                if *slot < value {
                    *slot = value;
                }
            }
        }
        self.mark(rect);
        rect
    }

    /// Writes the Bresenham line between two pixels at full coverage: the
    /// Pencil, exact and aliased.
    pub fn pixel_line(&mut self, layer: &Bitmap, from: (i32, i32), to: (i32, i32)) -> Rect {
        const LIMIT: i32 = 1 << 20;
        let (mut x, mut y) = (from.0.clamp(-LIMIT, LIMIT), from.1.clamp(-LIMIT, LIMIT));
        let (x1, y1) = (to.0.clamp(-LIMIT, LIMIT), to.1.clamp(-LIMIT, LIMIT));
        let area = Rect::from_points((x, y), (x1, y1)).intersect(self.bounds);
        let area = self.reserve(layer, area);
        if area.is_empty() {
            return Rect::EMPTY;
        }
        let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
        let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
        let mut error = dx + dy;
        loop {
            if area.contains(x, y) {
                self.raise(x, y, 255);
            }
            if (x, y) == (x1, y1) {
                break;
            }
            let e2 = 2 * error;
            if e2 >= dy {
                error += dy;
                x += sx;
            }
            if e2 <= dx {
                error += dx;
                y += sy;
            }
        }
        self.mark(area);
        area
    }

    /// Applies the coverage written since the last call to `layer`, returning
    /// the rectangle that changed. `clip` multiplies the coverage (a
    /// selection's mask; pixels outside its rectangle are untouched).
    ///
    /// `layer` must be the bitmap the buffer was made over, changed by nothing
    /// else since.
    pub fn composite(
        &mut self,
        layer: &mut Bitmap,
        color: Pixel,
        mode: PaintMode,
        clip: Option<Clip<'_>>,
    ) -> Rect {
        let mut rect = self.pending.intersect(self.alloc).intersect(layer.rect());
        // A rectangle clip passes everything inside it: just narrow the work.
        if let Some(Clip::Rect(inside)) = clip {
            rect = rect.intersect(inside);
        }
        self.pending = Rect::EMPTY;
        if rect.is_empty() {
            return Rect::EMPTY;
        }
        let width = rect.width() as usize;
        let mut gated = vec![0u8; width];
        for y in rect.y0..rect.y1 {
            let at = self.index(rect.x0, y);
            let cover = &self.cov[at..at + width];
            let weights: &[u8] = match clip {
                None | Some(Clip::Rect(_)) => cover,
                Some(clip @ Clip::Mask { .. }) => {
                    gated.fill(0);
                    let inside = Rect {
                        x0: rect.x0,
                        y0: y,
                        x1: rect.x1,
                        y1: y + 1,
                    }
                    .intersect(clip.rect());
                    if let Some(mask) = clip
                        .span(y, inside.x0, inside.x1)
                        .filter(|_| !inside.is_empty())
                    {
                        let from = (inside.x0 - rect.x0) as usize;
                        for (i, &m) in mask.iter().enumerate() {
                            gated[from + i] =
                                div255(u32::from(cover[from + i]) * u32::from(m)) as u8;
                        }
                    }
                    &gated
                }
            };
            let out = layer_row_mut(layer, y, rect.x0, width);
            let base = &self.base[at * 4..(at + width) * 4];
            apply_row(out, base, weights, color, mode);
        }
        rect
    }

    /// The layer pixels the stroke has covered, as they were before it, for an
    /// undo entry. `None` when nothing was drawn.
    pub fn before(&self) -> Option<(Rect, Bitmap)> {
        if self.dirty.is_empty() {
            return None;
        }
        let rect = self.dirty;
        let mut out = Bitmap::transparent(rect.width(), rect.height());
        let width = rect.width() as usize;
        for y in rect.y0..rect.y1 {
            let at = self.index(rect.x0, y) * 4;
            let to = (y - rect.y0) as usize * width * 4;
            out.pixels_mut()[to..to + width * 4].copy_from_slice(&self.base[at..at + width * 4]);
        }
        Some((rect, out))
    }

    /// Discards the stroke: puts back every pixel it composited and returns
    /// the rectangle that changed.
    pub fn cancel(self, layer: &mut Bitmap) -> Rect {
        let rect = self.dirty.intersect(layer.rect());
        if rect.is_empty() {
            return Rect::EMPTY;
        }
        let width = rect.width() as usize;
        for y in rect.y0..rect.y1 {
            let at = self.index(rect.x0, y) * 4;
            layer_row_mut(layer, y, rect.x0, width).copy_from_slice(&self.base[at..at + width * 4]);
        }
        rect
    }
}

/// The bytes of `count` pixels of row `y` from column `x`, inside the bitmap.
fn layer_row_mut(layer: &mut Bitmap, y: i32, x: i32, count: usize) -> &mut [u8] {
    let at = (y as usize * layer.width() as usize + x as usize) * 4;
    &mut layer.pixels_mut()[at..at + count * 4]
}

/// `out = blend(base, weight)` for one row; zero weight leaves `out` as is.
fn apply_row(out: &mut [u8], base: &[u8], weights: &[u8], color: Pixel, mode: PaintMode) {
    for ((out, base), &weight) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(base.as_chunks::<4>().0)
        .zip(weights)
    {
        if weight == 0 {
            continue;
        }
        *out = match mode {
            PaintMode::Paint => source_over(*base, color, weight),
            PaintMode::Replace => mix_by(weight, *base, color),
            PaintMode::Erase => {
                let strength = div255(u32::from(weight) * u32::from(color[3]));
                let alpha = div255(u32::from(base[3]) * (255 - strength));
                if alpha == 0 {
                    [0, 0, 0, 0]
                } else {
                    [base[0], base[1], base[2], alpha as u8]
                }
            }
        };
    }
}
