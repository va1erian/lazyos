//! Widget rectangles in design pixels (docs/hidpi-plan.md).
//!
//! The layout is written in pixel constants for a 96 DPI window. On a 2x
//! desktop the window runs at 192 DPI, so its text (in `Dip`) doubles; the
//! rectangles must too. [`set_dpi`] fixes the scale when the app is built,
//! and every [`rect`] is multiplied by it.

use std::cell::Cell;

use xui_core::Rect;

thread_local! {
    static SCALE: Cell<i32> = const { Cell::new(1) };
}

/// Lay out for a window at `dpi` (96 per scale step).
pub fn set_dpi(dpi: u32) {
    SCALE.with(|scale| scale.set((dpi / 96).max(1) as i32));
}

/// A rectangle at `(x, y)` of `w` x `h` design pixels, in window pixels.
pub fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    let s = SCALE.with(Cell::get);
    Rect::new(x * s, y * s, (x + w) * s, (y + h) * s)
}
