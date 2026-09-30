//! The compact/expanded toggle the dashboards (`sysmon`, `fabricmon`) share.
//!
//! A dashboard is compact when its window is small. The mode is derived from
//! the window size alone ([`is_compact`]), so dragging the window small,
//! pressing `c`, or clicking the chip all end in the same state, and the
//! compositor's `Configure` is the only thing the app has to follow. The
//! toggle asks the compositor for a size (`RequestSize`) instead of flipping a
//! flag: [`toggle_target`] picks which.

use xui_core::backend::TextAlign;
use xui_core::{Canvas, Color, Dip, Point, Rect, TextStyle, Theme};

/// The content size a compact dashboard asks the compositor for.
pub const COMPACT_SIZE: (u32, u32) = (320, 150);
/// The smallest content size a dashboard declares in its size hints; small
/// enough for [`COMPACT_SIZE`] and a little below it.
pub const MIN_SIZE: (u32, u32) = (220, 110);
/// Below this width the full layout no longer fits its columns.
const COMPACT_BELOW_W: i32 = 480;
/// Below this height the full layout no longer fits its sections.
const COMPACT_BELOW_H: i32 = 300;

/// Chip size and its inset from the top-right corner of the client area.
const CHIP_W: i32 = 64;
const CHIP_H: i32 = 16;
const CHIP_INSET: i32 = 8;

/// Whether a `width` x `height` client area is too small for the full view.
pub fn is_compact(width: i32, height: i32) -> bool {
    width < COMPACT_BELOW_W || height < COMPACT_BELOW_H
}

/// The content size to request when the user toggles: the remembered full
/// size from a compact view, [`COMPACT_SIZE`] from a full one. A remembered
/// size that is itself compact (the app was opened small) falls back to
/// `fallback`, so the toggle can never request the size it is already at.
pub fn toggle_target(
    current: (i32, i32),
    remembered: (i32, i32),
    fallback: (u32, u32),
) -> (u32, u32) {
    if !is_compact(current.0, current.1) {
        return COMPACT_SIZE;
    }
    let size = if is_compact(remembered.0, remembered.1) {
        (fallback.0 as i32, fallback.1 as i32)
    } else {
        remembered
    };
    (size.0.max(0) as u32, size.1.max(0) as u32)
}

/// The chip's rectangle in a client area of `bounds`.
pub fn chip_rect(bounds: Rect) -> Rect {
    let right = bounds.right - CHIP_INSET;
    let top = bounds.top + 2;
    Rect::new(right - CHIP_W, top, right, top + CHIP_H)
}

/// Whether a press at `(x, y)` hits the chip of a client area `bounds`.
pub fn hit_chip(bounds: Rect, x: i32, y: i32) -> bool {
    let chip = chip_rect(bounds);
    x >= chip.left && x < chip.right && y >= chip.top && y < chip.bottom
}

/// Paint the clickable chip: "compact" in the full view, "expand" in the
/// compact one.
pub fn paint_chip(canvas: &mut dyn Canvas, theme: Theme, bounds: Rect) {
    let chip = chip_rect(bounds);
    let label = if is_compact(bounds.width(), bounds.height()) {
        "expand [c]"
    } else {
        "compact [c]"
    };
    canvas.fill_rounded_rect(chip, 8.0, theme.raised);
    canvas.stroke_rounded_rect(chip, 8.0, theme.border, 1.0);
    let mut style = TextStyle::new(theme.text_secondary, Dip(11.0)).middle();
    style.align = TextAlign::Center;
    canvas.draw_text(label, chip, &style);
}

/// A thin rule under a compact view's title row.
pub fn rule(canvas: &mut dyn Canvas, color: Color, bounds: Rect, y: i32) {
    canvas.draw_line(
        Point::new(bounds.left, y),
        Point::new(bounds.right, y),
        color,
        1.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_windows_are_compact_and_large_ones_are_not() {
        assert!(is_compact(320, 150));
        assert!(is_compact(860, 200), "a short window is compact");
        assert!(is_compact(300, 600), "a narrow window is compact");
        assert!(!is_compact(860, 600));
        assert!(!is_compact(480, 300), "the thresholds are exclusive");
        assert!(is_compact(479, 300));
        assert!(is_compact(480, 299));
    }

    #[test]
    fn the_compact_size_is_compact_and_inside_the_hints() {
        assert!(is_compact(COMPACT_SIZE.0 as i32, COMPACT_SIZE.1 as i32));
        assert!(COMPACT_SIZE.0 >= MIN_SIZE.0 && COMPACT_SIZE.1 >= MIN_SIZE.1);
    }

    #[test]
    fn toggling_a_full_window_requests_the_compact_size() {
        assert_eq!(
            toggle_target((860, 600), (860, 600), (860, 600)),
            COMPACT_SIZE
        );
    }

    #[test]
    fn toggling_a_compact_window_restores_the_remembered_size() {
        assert_eq!(
            toggle_target((320, 150), (700, 500), (860, 600)),
            (700, 500)
        );
    }

    #[test]
    fn a_compact_remembered_size_falls_back_to_the_default() {
        assert_eq!(
            toggle_target((320, 150), (320, 150), (860, 600)),
            (860, 600)
        );
    }

    #[test]
    fn the_chip_sits_in_the_top_right_and_hits_only_itself() {
        let bounds = Rect::new(0, 0, 320, 150);
        let chip = chip_rect(bounds);
        assert!(chip.right <= bounds.right && chip.top >= bounds.top);
        assert!(hit_chip(bounds, chip.left + 1, chip.top + 1));
        assert!(!hit_chip(bounds, chip.left - 1, chip.top + 1));
        assert!(!hit_chip(bounds, chip.left + 1, chip.bottom));
    }
}
