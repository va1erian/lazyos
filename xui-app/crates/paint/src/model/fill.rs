#![forbid(unsafe_code)]

//! Similar-colour regions for the Magic Wand: an iterative scanline walk, no
//! recursion, so a full-canvas region cannot overflow the stack. (The Paint
//! Bucket has its own, in [`tools::bucket`](super::tools).)

use std::collections::VecDeque;

use super::bitmap::{Bitmap, Pixel};

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

    fn area(spans: &[(i32, i32, i32)]) -> i32 {
        spans.iter().map(|(_, a, b)| b - a).sum()
    }

    #[test]
    fn a_contiguous_region_stops_at_a_wall() {
        let mut bitmap = Bitmap::white(8, 8);
        for y in 0..8 {
            bitmap.put(4, y, [0, 0, 0, 255]);
        }
        let spans = similar_spans(&bitmap, 0, 0, 0, true).unwrap();
        assert_eq!(area(&spans), 4 * 8);
        let global = similar_spans(&bitmap, 0, 0, 0, false).unwrap();
        assert_eq!(area(&global), 7 * 8, "global takes both sides");
    }

    #[test]
    fn a_full_canvas_does_not_recurse() {
        let bitmap = Bitmap::white(1024, 1024);
        let spans = similar_spans(&bitmap, 0, 0, 0, true).unwrap();
        assert_eq!(area(&spans), 1024 * 1024);
    }

    #[test]
    fn an_out_of_bounds_seed_selects_nothing() {
        let bitmap = Bitmap::white(4, 4);
        assert!(similar_spans(&bitmap, -1, 0, 0, true).is_none());
        assert!(similar_spans(&bitmap, 0, i32::MAX, 0, true).is_none());
    }
}
