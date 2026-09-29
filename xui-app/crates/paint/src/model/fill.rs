#![forbid(unsafe_code)]

//! Iterative scanline flood fill. No recursion, so a full-canvas fill cannot
//! overflow the stack.

use std::collections::VecDeque;

use super::bitmap::{Bitmap, Pixel};

/// Flood-fills the connected region of the colour at `(x, y)` with `color`.
///
/// Returns whether any pixel changed. An out-of-bounds seed, or one already the
/// target colour, is a no-op.
pub fn flood_fill(bitmap: &mut Bitmap, x: i32, y: i32, color: Pixel) -> bool {
    let Some(target) = bitmap.get(x, y) else {
        return false;
    };
    if target == color {
        return false;
    }
    let width = bitmap.width() as i32;
    let height = bitmap.height() as i32;
    let mut queue: VecDeque<(i32, i32)> = VecDeque::new();
    queue.push_back((x, y));
    let mut changed = false;
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
            changed = true;
        }
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
    changed
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
