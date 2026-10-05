//! The terminal multiplexer's window layout (issue #218).

use super::*;
use crate::mux::column;

/// One window fills the screen between the margins; two split it evenly.
pub fn layout_widths() -> Result<(), String> {
    let (x, w) = column(1280, 1, 0);
    check!(x == 8 && w == 1280 - 16, "lone window at {x}, width {w}");
    let (x0, w0) = column(1280, 2, 0);
    let (x1, w1) = column(1280, 2, 1);
    check!(w0 == w1 && w0 == (1280 - 24) / 2, "two columns {w0} and {w1}");
    check!(x0 == 8 && x1 == x0 + w0 + 8, "column starts {x0}, {x1}");
    check!(x1 + w1 <= 1280 - 8, "right column overruns the screen");
    Ok(())
}

/// Every width and count stays on screen and a count of zero or more than the
/// layout holds degrades to a valid column instead of dividing by zero.
pub fn layout_bounds() -> Result<(), String> {
    for screen in [64, 640, 800, 1280, 1920, 3840] {
        for count in 0..6usize {
            for slot in 0..count.clamp(1, 2) {
                let (x, w) = column(screen, count, slot);
                check!(x >= 0 && w > 0, "{screen}/{count}/{slot}: x={x} w={w}");
                check!(x + w <= screen, "{screen}/{count}/{slot} overruns: {}", x + w);
            }
        }
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("mux_layout_widths", layout_widths),
    ("mux_layout_bounds", layout_bounds),
];
