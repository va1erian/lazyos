//! The device-independent gamepad axis encoding (`docs/usb-gamepad-plan.md`
//! G1).
//!
//! | kind | code | value |
//! |---|---|---|
//! | `AXIS` | a Generic Desktop usage: `X`, `Y`, `Z`, `Rx`, `Ry`, `Rz`, `Slider`, `Dial`, `Hat switch` | `-32768..=32767` for a signed range, `0..=32767` for an unsigned one; the hat `0..=7` clockwise from up, [`HAT_CENTRED`] centred |
//!
//! An axis is absolute state, not an edge: the producer publishes it only when
//! its value changed, and the bus merges it into the newest queued record by
//! *replacing* the value, so repeated updates of one axis behind a starved
//! consumer cost one slot rather than one per report. Buttons stay edges and
//! never merge.

/// Generic Desktop usages a gamepad axis may carry.
pub mod usage {
    pub const X: u16 = 0x30;
    pub const Y: u16 = 0x31;
    pub const Z: u16 = 0x32;
    pub const RX: u16 = 0x33;
    pub const RY: u16 = 0x34;
    pub const RZ: u16 = 0x35;
    pub const SLIDER: u16 = 0x36;
    pub const DIAL: u16 = 0x37;
    pub const HAT: u16 = 0x39;
}

/// The hat's centred (null) value: its eight directions are `0..=7`.
pub const HAT_CENTRED: i32 = 8;

/// Axes a source can hold: `X..=Dial` (8) plus the hat.
pub const SLOTS: u32 = 9;

/// Whether `code` is an axis usage the bus carries.
pub fn is_usage(code: u16) -> bool {
    slot(code).is_some()
}

/// The index of `code` among the axes: `0..=7` for `X..=Dial`, 8 for the hat,
/// `None` for anything else.
pub fn slot(code: u16) -> Option<u32> {
    if code == usage::HAT {
        Some(SLOTS - 1)
    } else if (usage::X..=usage::DIAL).contains(&code) {
        Some(u32::from(code - usage::X))
    } else {
        None
    }
}

/// The usage of `slot`, the inverse of [`slot`] for the slots it yields.
pub fn usage_of(slot: u32) -> u16 {
    debug_assert!(slot < SLOTS, "slot {slot} is not an axis");
    if slot == SLOTS - 1 {
        usage::HAT
    } else {
        usage::X + slot as u16
    }
}

/// The value an axis rests at: the hat is centred, every other axis 0.
pub fn rest(code: u16) -> i32 {
    if code == usage::HAT {
        HAT_CENTRED
    } else {
        0
    }
}

/// Whether `code` with `value` is a record the bus accepts: a known usage,
/// the hat inside its eight directions plus centred, every other axis in the
/// `i16` range the normalization produces.
pub fn valid(code: u16, value: i32) -> bool {
    if code == usage::HAT {
        (0..=HAT_CENTRED).contains(&value)
    } else {
        is_usage(code) && (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&value)
    }
}
