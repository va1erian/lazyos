#![forbid(unsafe_code)]

//! An 8-bit coverage mask positioned in layer coordinates, and the clip mask
//! a selection hands to the compositor.

use crate::model::rect::Rect;

/// Per-pixel coverage (0 none, 255 full) for the pixels of `rect`, row-major.
///
/// Shapes, bucket fills and glyphs all come out as one of these; the stroke
/// buffer then merges and composites it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    rect: Rect,
    data: Vec<u8>,
}

impl Coverage {
    /// An all-zero mask over `rect` (empty when `rect` is).
    pub fn new(rect: Rect) -> Coverage {
        let rect = if rect.is_empty() { Rect::EMPTY } else { rect };
        Coverage {
            rect,
            data: vec![0; rect.area()],
        }
    }

    /// A mask over `rect` from `data`, which must hold exactly one byte per
    /// pixel.
    pub fn from_data(rect: Rect, data: Vec<u8>) -> Option<Coverage> {
        (data.len() == rect.area()).then_some(Coverage { rect, data })
    }

    /// The pixels the mask spans.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The row-major bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The mutable row-major bytes.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Whether the mask spans no pixel.
    pub fn is_empty(&self) -> bool {
        self.rect.is_empty()
    }

    /// The coverage at `(x, y)`; zero outside the mask.
    pub fn get(&self, x: i32, y: i32) -> u8 {
        if !self.rect.contains(x, y) {
            return 0;
        }
        self.data
            [(y - self.rect.y0) as usize * self.rect.width() as usize + (x - self.rect.x0) as usize]
    }

    /// The bytes of row `y` for columns `x0..x1`, which must lie inside.
    pub fn row(&self, y: i32, x0: i32, x1: i32) -> &[u8] {
        let width = self.rect.width() as usize;
        let at = (y - self.rect.y0) as usize * width + (x0 - self.rect.x0) as usize;
        &self.data[at..at + (x1 - x0) as usize]
    }

    /// The same mask moved by `(dx, dy)`.
    pub fn translated(mut self, dx: i32, dy: i32) -> Coverage {
        self.rect = Rect {
            x0: self.rect.x0.saturating_add(dx),
            y0: self.rect.y0.saturating_add(dy),
            x1: self.rect.x1.saturating_add(dx),
            y1: self.rect.y1.saturating_add(dy),
        };
        self
    }

    /// Hard-edges the mask: coverage of 128 and up becomes 255, the rest 0.
    /// This is what "antialias off" means for every tool.
    pub fn threshold(&mut self) {
        for value in &mut self.data {
            *value = if *value >= 128 { 255 } else { 0 };
        }
    }

    /// The bounding rectangle of the non-zero pixels, `EMPTY` when none.
    pub fn ink_bounds(&self) -> Rect {
        let width = self.rect.width() as usize;
        let mut bounds = Rect::EMPTY;
        for (row, line) in self.data.chunks(width.max(1)).enumerate() {
            let Some(first) = line.iter().position(|&v| v != 0) else {
                continue;
            };
            let last = line.iter().rposition(|&v| v != 0).unwrap_or(first);
            let y = self.rect.y0 + row as i32;
            bounds = bounds.union(Rect {
                x0: self.rect.x0 + first as i32,
                y0: y,
                x1: self.rect.x0 + last as i32 + 1,
                y1: y + 1,
            });
        }
        bounds
    }

    /// Sum of all coverage, in 255ths of a pixel; for tests and area checks.
    pub fn total(&self) -> u64 {
        self.data.iter().map(|&v| u64::from(v)).sum()
    }
}

/// A selection's mask as the compositor sees it: coverage for the pixels of
/// `rect`, and nothing at all outside it.
#[derive(Clone, Copy, Debug)]
pub struct Clip<'a> {
    /// The pixels the mask describes.
    pub rect: Rect,
    /// One byte per pixel of `rect`, row-major.
    pub mask: &'a [u8],
}

impl<'a> Clip<'a> {
    /// A clip over `rect` from `mask`, when the length agrees.
    pub fn new(rect: Rect, mask: &'a [u8]) -> Option<Clip<'a>> {
        (mask.len() == rect.area()).then_some(Clip { rect, mask })
    }

    /// The clip's bytes for columns `x0..x1` of row `y`, if the whole span is
    /// inside the clip's rectangle.
    pub(crate) fn span(&self, y: i32, x0: i32, x1: i32) -> Option<&'a [u8]> {
        if y < self.rect.y0 || y >= self.rect.y1 || x0 < self.rect.x0 || x1 > self.rect.x1 {
            return None;
        }
        let at =
            (y - self.rect.y0) as usize * self.rect.width() as usize + (x0 - self.rect.x0) as usize;
        self.mask.get(at..at + (x1 - x0) as usize)
    }

    /// The clip's value at `(x, y)`; zero outside its rectangle.
    pub fn get(&self, x: i32, y: i32) -> u8 {
        self.span(y, x, x + 1).map_or(0, |s| s[0])
    }
}
