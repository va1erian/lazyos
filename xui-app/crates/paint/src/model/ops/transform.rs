#![forbid(unsafe_code)]

//! Flips, 90 degree turns and canvas placement of a bitmap.

use crate::model::bitmap::{Bitmap, Pixel};

/// Where the old picture sits when the canvas grows or shrinks: the nine
/// cells of the Canvas Size dialog's anchor grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// Top left corner.
    TopLeft,
    /// Top edge, centred.
    Top,
    /// Top right corner.
    TopRight,
    /// Left edge, centred.
    Left,
    /// Centre.
    Center,
    /// Right edge, centred.
    Right,
    /// Bottom left corner.
    BottomLeft,
    /// Bottom edge, centred.
    Bottom,
    /// Bottom right corner.
    BottomRight,
}

impl Anchor {
    /// Every anchor, row by row, as the dialog lays them out.
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];

    /// The (column, row) in the 3x3 grid, each 0, 1 or 2.
    fn cell(self) -> (i64, i64) {
        match self {
            Anchor::TopLeft => (0, 0),
            Anchor::Top => (1, 0),
            Anchor::TopRight => (2, 0),
            Anchor::Left => (0, 1),
            Anchor::Center => (1, 1),
            Anchor::Right => (2, 1),
            Anchor::BottomLeft => (0, 2),
            Anchor::Bottom => (1, 2),
            Anchor::BottomRight => (2, 2),
        }
    }

    /// Where the old top-left lands in a new canvas of `new` size (may be
    /// negative when the canvas shrinks). Truncating division, so growing and
    /// then shrinking by the same amount restores the picture.
    pub fn offset(self, old: (u32, u32), new: (u32, u32)) -> (i32, i32) {
        let (cx, cy) = self.cell();
        let along =
            |cell: i64, old: u32, new: u32| ((i64::from(new) - i64::from(old)) * cell / 2) as i32;
        (along(cx, old.0, new.0), along(cy, old.1, new.1))
    }
}

/// What the new area of a bigger canvas is filled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasFill {
    /// Nothing: alpha 0.
    Transparent,
    /// The secondary colour (on the bottom layer).
    Secondary,
}

/// A mirror axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flip {
    /// Left to right.
    Horizontal,
    /// Top to bottom.
    Vertical,
}

impl Flip {
    /// `src` mirrored.
    pub fn apply(self, src: &Bitmap) -> Bitmap {
        match self {
            Flip::Horizontal => flipped_h(src),
            Flip::Vertical => flipped_v(src),
        }
    }

    /// The menu wording ("Horizontal").
    pub fn label(self) -> &'static str {
        match self {
            Flip::Horizontal => "Horizontal",
            Flip::Vertical => "Vertical",
        }
    }
}

/// A quarter or half turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// 90 degrees clockwise.
    Cw90,
    /// 90 degrees counter-clockwise.
    Ccw90,
    /// 180 degrees.
    Half,
}

impl Turn {
    /// `src` turned (90 degree turns swap width and height).
    pub fn apply(self, src: &Bitmap) -> Bitmap {
        match self {
            Turn::Cw90 => rotated_cw(src),
            Turn::Ccw90 => rotated_ccw(src),
            Turn::Half => rotated_180(src),
        }
    }

    /// Whether width and height trade places.
    pub fn swaps_sides(self) -> bool {
        self != Turn::Half
    }

    /// The menu wording ("90 CW").
    pub fn label(self) -> &'static str {
        match self {
            Turn::Cw90 => "90 CW",
            Turn::Ccw90 => "90 CCW",
            Turn::Half => "180",
        }
    }
}

/// A `width` x `height` bitmap filled with `fill` holding `src` with its
/// top-left at `(x, y)`; what falls outside is cropped.
pub fn placed(src: &Bitmap, width: u32, height: u32, fill: Pixel, x: i32, y: i32) -> Bitmap {
    let mut out = Bitmap::new(width, height, fill);
    out.blit(x, y, src, src.rect());
    out
}

/// Copies every pixel of `src` to `map(x, y)` of a `width` x `height` result.
fn remap(src: &Bitmap, width: u32, height: u32, map: impl Fn(u32, u32) -> (u32, u32)) -> Bitmap {
    let mut out = Bitmap::transparent(width, height);
    let stride = out.width() as usize;
    let from = src.pixels().as_chunks::<4>().0;
    let to = out.pixels_mut().as_chunks_mut::<4>().0;
    let sw = src.width() as usize;
    for y in 0..src.height() {
        for x in 0..src.width() {
            let (dx, dy) = map(x, y);
            to[dy as usize * stride + dx as usize] = from[y as usize * sw + x as usize];
        }
    }
    out
}

/// Mirrored left to right.
pub fn flipped_h(src: &Bitmap) -> Bitmap {
    let w = src.width();
    remap(src, w, src.height(), |x, y| (w - 1 - x, y))
}

/// Mirrored top to bottom.
pub fn flipped_v(src: &Bitmap) -> Bitmap {
    let h = src.height();
    remap(src, src.width(), h, |x, y| (x, h - 1 - y))
}

/// Turned 180 degrees.
pub fn rotated_180(src: &Bitmap) -> Bitmap {
    let (w, h) = src.size();
    remap(src, w, h, |x, y| (w - 1 - x, h - 1 - y))
}

/// Turned 90 degrees clockwise: the result is `height` x `width`.
pub fn rotated_cw(src: &Bitmap) -> Bitmap {
    let h = src.height();
    remap(src, h, src.width(), |x, y| (h - 1 - y, x))
}

/// Turned 90 degrees counter-clockwise: the result is `height` x `width`.
pub fn rotated_ccw(src: &Bitmap) -> Bitmap {
    let w = src.width();
    remap(src, src.height(), w, |x, y| (y, w - 1 - x))
}
