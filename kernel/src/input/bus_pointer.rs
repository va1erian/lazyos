//! The device-independent pointer encoding (`docs/usb-hid-plan.md`).
//!
//! | kind | code | value |
//! |---|---|---|
//! | `REL_MOTION` | 0 | `dx` low `i16`, `dy` high `i16`: counts, screen-oriented (`dy > 0` is down) |
//! | `ABS_MOTION` | 0 | `x` low `u16`, `y` high `u16`, normalised to `0..=0xFFFF` |
//! | `BUTTON` | HID button usage ([`button`](pointer::button)) | `0` release, `1` press |
//! | `SCROLL` | [`VERTICAL`](pointer::VERTICAL) or [`HORIZONTAL`](pointer::HORIZONTAL) | signed notches, `> 0` is up / right |
//!
//! Producers split deltas wider than `i16` across records.

use super::kind;

/// HID button usages (page 0x09).
#[allow(dead_code)] // back/forward have no producer until USB mice
pub mod button {
    pub const LEFT: u16 = 1;
    pub const RIGHT: u16 = 2;
    pub const MIDDLE: u16 = 3;
    pub const BACK: u16 = 4;
    pub const FORWARD: u16 = 5;
}

/// `SCROLL` codes.
pub const VERTICAL: u16 = 0;
#[allow(dead_code)] // no PS/2 horizontal wheel
pub const HORIZONTAL: u16 = 1;

/// Pack a relative motion.
pub fn pack_rel(dx: i16, dy: i16) -> i32 {
    (u32::from(dx as u16) | (u32::from(dy as u16) << 16)) as i32
}

/// Unpack a relative motion: `(dx, dy)`.
pub fn unpack_rel(value: i32) -> (i16, i16) {
    (
        value as u32 as u16 as i16,
        ((value as u32) >> 16) as u16 as i16,
    )
}

/// Pack an absolute position.
#[allow(dead_code)] // no kernel producer: absolute devices are USB
pub fn pack_abs(x: u16, y: u16) -> i32 {
    (u32::from(x) | (u32::from(y) << 16)) as i32
}

/// Unpack an absolute position: `(x, y)`.
#[allow(dead_code)]
pub fn unpack_abs(value: i32) -> (u16, u16) {
    (value as u32 as u16, ((value as u32) >> 16) as u16)
}

/// Whether records of `kind` may be folded into their predecessor.
pub fn mergeable(kind: u8) -> bool {
    matches!(kind, kind::REL_MOTION | kind::ABS_MOTION | kind::SCROLL)
}

/// `older` and `newer` combined: deltas add (saturating), positions
/// replace. Only meaningful for [`mergeable`] kinds.
pub fn merge(kind: u8, older: i32, newer: i32) -> i32 {
    match kind {
        kind::REL_MOTION => {
            let (ax, ay) = unpack_rel(older);
            let (bx, by) = unpack_rel(newer);
            pack_rel(ax.saturating_add(bx), ay.saturating_add(by))
        }
        kind::SCROLL => older.saturating_add(newer),
        _ => newer,
    }
}

/// Whether relative motion `newer` turns back on an axis from `older`.
/// `inputd` clamps the cursor at the screen edges, and clamping a sum
/// equals clamping each step only while the steps share a direction
/// (`-300` into the corner then `+128` must land at 128, not at 0).
pub fn turns(kind: u8, older: i32, newer: i32) -> bool {
    if kind != kind::REL_MOTION {
        return false;
    }
    let ((ax, ay), (bx, by)) = (unpack_rel(older), unpack_rel(newer));
    let opposed = |a: i16, b: i16| (a < 0 && b > 0) || (a > 0 && b < 0);
    opposed(ax, bx) || opposed(ay, by)
}
