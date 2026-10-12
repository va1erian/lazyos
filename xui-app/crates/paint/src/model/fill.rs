#![forbid(unsafe_code)]

//! Iterative scanline flood fill. No recursion, so a full-canvas fill cannot
//! overflow the stack.

use std::collections::VecDeque;

use super::bitmap::{Bitmap, Pixel};
use super::rect::Rect;

/// Flood-fills the connected region of the colour at `(x, y)` with `color`.
///
/// Returns whether any pixel changed. An out-of-bounds seed, or one already the
/// target colour, is a no-op.
#[cfg(test)]
pub fn flood_fill(bitmap: &mut Bitmap, x: i32, y: i32, color: Pixel) -> bool {
    flood_fill_spans(bitmap, x, y, color).is_some()
}

/// What a fill changed. Every filled pixel was `target` before, so the
/// spans are enough to rebuild the "before" picture for undo.
pub struct Filled {
    /// The bounding rectangle of the filled pixels.
    pub bounds: Rect,
    /// The colour every filled pixel had.
    pub target: Pixel,
    /// The filled runs as `(y, x0, x1)`, `x1` exclusive.
    pub spans: Vec<(i32, i32, i32)>,
}

/// Like [`flood_fill`], returning the changed runs, or `None` for a no-op.
pub fn flood_fill_spans(bitmap: &mut Bitmap, x: i32, y: i32, color: Pixel) -> Option<Filled> {
    let target = bitmap.get(x, y)?;
    if target == color {
        return None;
    }
    let mut spans = Vec::new();
    let mut bounds = Rect::EMPTY;
    let width = bitmap.width() as i32;
    let height = bitmap.height() as i32;
    let mut queue: VecDeque<(i32, i32)> = VecDeque::new();
    queue.push_back((x, y));
    while let Some((seed_x, seed_y)) = queue.pop_front() {
        if bitmap.get(seed_x, seed_y) != Some(target) {
            continue;
        }
        let mut left = seed_x;
        while left > 0 && bitmap.get(left - 1, seed_y) == Some(target) {
            left -= 1;
        }
        let mut right = seed_x;
        while right + 1 < width && bitmap.get(right + 1, seed_y) == Some(target) {
            right += 1;
        }
        for px in left..=right {
            bitmap.put(px, seed_y, color);
        }
        spans.push((seed_y, left, right + 1));
        bounds = bounds.union(Rect {
            x0: left,
            y0: seed_y,
            x1: right + 1,
            y1: seed_y + 1,
        });
        for row in [seed_y - 1, seed_y + 1] {
            if row < 0 || row >= height {
                continue;
            }
            let mut span = false;
            for px in left..=right {
                if bitmap.get(px, row) == Some(target) {
                    if !span {
                        queue.push_back((px, row));
                        span = true;
                    }
                } else {
                    span = false;
                }
            }
        }
    }
    Some(Filled {
        bounds,
        target,
        spans,
    })
}

/// The largest per-channel difference (0-255) a wand tolerance of `percent`
/// accepts: 0% is an exact match, 100% matches everything.
pub fn tolerance_threshold(percent: u8) -> u8 {
    ((u32::from(percent.min(100)) * 255 + 50) / 100) as u8
}

/// Whether two pixels differ by at most `threshold` in every channel (alpha
/// included, so a transparent pixel is far from an opaque one of any colour).
fn similar(a: Pixel, b: Pixel, threshold: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= threshold)
}

/// The runs `(y, x0, x1)` (`x1` exclusive) of pixels similar to the one at
/// `(x, y)`, within `threshold` per channel. `contiguous` keeps only the
/// 4-connected region around the seed (the same scanline walk as the bucket);
/// otherwise every similar pixel of the bitmap counts. Nothing is written.
/// `None` for a seed outside the bitmap.
pub fn similar_spans(
    bitmap: &Bitmap,
    x: i32,
    y: i32,
    threshold: u8,
    contiguous: bool,
) -> Option<Vec<(i32, i32, i32)>> {
    let seed = bitmap.get(x, y)?;
    let (width, height) = (bitmap.width() as i32, bitmap.height() as i32);
    let matches = |px: i32, py: i32| {
        bitmap
            .get(px, py)
            .is_some_and(|p| similar(p, seed, threshold))
    };
    let mut spans = Vec::new();
    if !contiguous {
        for row in 0..height {
            let mut start = None;
            for px in 0..=width {
                match (px < width && matches(px, row), start) {
                    (true, None) => start = Some(px),
                    (false, Some(from)) => {
                        spans.push((row, from, px));
                        start = None;
                    }
                    _ => {}
                }
            }
        }
        return Some(spans);
    }
    let mut seen = vec![false; width as usize * height as usize];
    let mut queue: VecDeque<(i32, i32)> = VecDeque::from([(x, y)]);
    while let Some((seed_x, seed_y)) = queue.pop_front() {
        if seen[(seed_y * width + seed_x) as usize] || !matches(seed_x, seed_y) {
            continue;
        }
        let mut left = seed_x;
        while left > 0 && matches(left - 1, seed_y) {
            left -= 1;
        }
        let mut right = seed_x;
        while right + 1 < width && matches(right + 1, seed_y) {
            right += 1;
        }
        let row = (seed_y * width) as usize;
        seen[row + left as usize..=row + right as usize].fill(true);
        spans.push((seed_y, left, right + 1));
        for next in [seed_y - 1, seed_y + 1] {
            if next < 0 || next >= height {
                continue;
            }
            let mut run = false;
            for px in left..=right {
                if matches(px, next) && !seen[(next * width + px) as usize] {
                    if !run {
                        queue.push_back((px, next));
                        run = true;
                    }
                } else {
                    run = false;
                }
            }
        }
    }
    Some(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Pixel = [255, 0, 0, 255];
    const BLUE: Pixel = [0, 0, 255, 255];

    #[test]
    fn fills_a_bounded_region_only() {
        let mut bitmap = Bitmap::white(8, 8);
        // A wall down the middle splits the canvas in two.
        for y in 0..8 {
            bitmap.put(4, y, [0, 0, 0, 255]);
        }
        assert!(flood_fill(&mut bitmap, 0, 0, RED));
        assert_eq!(bitmap.get(0, 0), Some(RED));
        assert_eq!(bitmap.get(3, 7), Some(RED));
        assert_eq!(bitmap.get(4, 0), Some([0, 0, 0, 255]));
        assert_eq!(bitmap.get(5, 0), Some([255, 255, 255, 255]));
    }

    #[test]
    fn fills_a_full_canvas_without_recursing() {
        let mut bitmap = Bitmap::white(1024, 1024);
        assert!(flood_fill(&mut bitmap, 0, 0, RED));
        assert_eq!(bitmap.get(1023, 1023), Some(RED));
    }

    #[test]
    fn filling_with_the_same_colour_changes_nothing() {
        let mut bitmap = Bitmap::white(4, 4);
        assert!(!flood_fill(&mut bitmap, 1, 1, [255, 255, 255, 255]));
    }

    #[test]
    fn an_out_of_bounds_seed_is_a_no_op() {
        let mut bitmap = Bitmap::white(4, 4);
        assert!(!flood_fill(&mut bitmap, -1, 0, RED));
        assert!(!flood_fill(&mut bitmap, 4, 0, RED));
        assert!(!flood_fill(&mut bitmap, 0, i32::MAX, RED));
    }

    #[test]
    fn the_seed_edge_is_filled() {
        let mut bitmap = Bitmap::white(1, 1);
        assert!(flood_fill(&mut bitmap, 0, 0, BLUE));
        assert_eq!(bitmap.get(0, 0), Some(BLUE));
    }
}
