//! The terminal multiplexer's window layout (issue #218).

use super::*;
use crate::mux::{column, wrap_rows};

/// One window fills the screen between the margins; two split it evenly.
pub fn layout_widths() -> Result<(), String> {
    let (x, w) = column(1280, 1, 0);
    check!(x == 8 && w == 1280 - 16, "lone window at {x}, width {w}");
    let (x0, w0) = column(1280, 2, 0);
    let (x1, w1) = column(1280, 2, 1);
    check!(
        w0 == w1 && w0 == (1280 - 24) / 2,
        "two columns {w0} and {w1}"
    );
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
                check!(
                    x + w <= screen,
                    "{screen}/{count}/{slot} overruns: {}",
                    x + w
                );
            }
        }
    }
    Ok(())
}

/// Long lines wrap into rows that lose no bytes and never exceed the width;
/// short and empty lines pass through; a multi-byte character is not split.
pub fn wrap_keeps_every_byte() -> Result<(), String> {
    let long = [b'x'; 25];
    let lines: [&[u8]; 3] = [b"short", b"", &long];
    let rows = wrap_rows(&lines, 10);
    check!(rows.len() == 5, "rows {}", rows.len());
    check!(
        rows[0] == b"short" && rows[1].is_empty(),
        "short rows changed"
    );
    check!(
        rows[2..]
            .iter()
            .map(|r| r.len())
            .collect::<alloc::vec::Vec<_>>()
            == [10, 10, 5],
        "long line split wrongly"
    );
    let text = "aé".repeat(6);
    let multibyte: [&[u8]; 1] = [text.as_bytes()];
    for row in wrap_rows(&multibyte, 5) {
        check!(row.len() <= 5, "row {} bytes", row.len());
        check!(core::str::from_utf8(row).is_ok(), "a character was split");
    }
    check!(
        wrap_rows(&lines, 0).len() >= 3,
        "zero columns must not loop"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("mux_layout_widths", layout_widths),
    ("mux_layout_bounds", layout_bounds),
    ("mux_wrap_keeps_every_byte", wrap_keeps_every_byte),
];
