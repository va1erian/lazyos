//! The Xbox 360-style X-input report (`docs/usb-gamepad-plan.md` G6).
//!
//! A pad in X-input mode is not HID: its interface is vendor class
//! `FF`/`5D`/`01` and its interrupt reports are a fixed layout with no report
//! descriptor to read. This decoder translates that layout to the same
//! canonical usages a HID gamepad reports, so everything above the driver
//! sees one device.
//!
//! | bytes | field |
//! |---|---|
//! | 0 | message type: `0x00` is the input report ([`INPUT_REPORT`]) |
//! | 1 | message length (`0x14`) |
//! | 2..4 | buttons, little-endian |
//! | 4 | left trigger, `0..=255` |
//! | 5 | right trigger, `0..=255` |
//! | 6..8, 8..10 | left stick X, Y (`i16`, Y up is positive) |
//! | 10..12, 12..14 | right stick X, Y (`i16`, Y up is positive) |
//! | 14..20 | reserved |
//!
//! The same endpoint also carries other messages (LED and rumble status,
//! type `0x01`/`0x03`): only type `0x00` is an input report. The sticks' Y
//! axes point up, HID's down (a HID pad reports its logical minimum at the
//! top), so the decoder negates them: one vocabulary means a stick pushed up
//! is negative Y whichever protocol the pad speaks.

use crate::report::gamepad::{PadOut, HAT_CENTRED};
use crate::report::usage;

/// Bytes this decoder reads; the report itself is 20 bytes and the tail is
/// reserved.
pub const REPORT_LEN: usize = 14;
/// The message type of an input report (byte 0).
pub const INPUT_REPORT: u8 = 0x00;

/// The report's button bits (the Xbox 360 layout).
pub mod button {
    pub const UP: u16 = 0x0001;
    pub const DOWN: u16 = 0x0002;
    pub const LEFT: u16 = 0x0004;
    pub const RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const L3: u16 = 0x0040;
    pub const R3: u16 = 0x0080;
    pub const LB: u16 = 0x0100;
    pub const RB: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

/// The report bits that become canonical button usages: A, B, X, Y, LB, RB,
/// Back, Start, L3, R3, Guide.
const BUTTONS: [(u16, u16); 11] = [
    (button::A, 1),
    (button::B, 2),
    (button::X, 3),
    (button::Y, 4),
    (button::LB, 5),
    (button::RB, 6),
    (button::BACK, 7),
    (button::START, 8),
    (button::L3, 9),
    (button::R3, 10),
    (button::GUIDE, 11),
];

/// The axes this decoder keeps, in report order: the two sticks and the two
/// triggers (`Z` is the left trigger, `Rz` the right).
const AXES: [u16; 6] = [
    usage::X,
    usage::Y,
    usage::Z,
    usage::RX,
    usage::RY,
    usage::RZ,
];

/// Turns successive X-input reports into the same bus records a HID gamepad
/// produces: one `Axis` per changed axis, then one `Button` edge per changed
/// button. The d-pad bits become the hat axis.
#[derive(Clone, Copy, Debug)]
pub struct Decoder {
    /// Last value per axis, in `X, Y, Z, RX, RY, RZ` order; `None` until the
    /// first report.
    axes: [Option<i32>; 6],
    hat: Option<i32>,
    /// Canonical button bits held after the last report.
    buttons: u32,
}

impl Default for Decoder {
    fn default() -> Decoder {
        Decoder::new()
    }
}

impl Decoder {
    pub const fn new() -> Decoder {
        Decoder {
            axes: [None; 6],
            hat: None,
            buttons: 0,
        }
    }

    /// Decode `report`; `false` when it is not an input report or is
    /// shorter than the fields it needs, which changes nothing.
    pub fn feed(&mut self, report: &[u8], mut emit: impl FnMut(PadOut)) -> bool {
        // The length byte (`report[1]`) is not trusted or checked, as Linux's
        // xpad does not: the transfer's own length is what bounds the read.
        if report.len() < REPORT_LEN || report[0] != INPUT_REPORT {
            return false;
        }
        let stick = |at: usize| i32::from(i16::from_le_bytes([report[at], report[at + 1]]));
        let word = u16::from_le_bytes([report[2], report[3]]);
        let axes = [
            (AXES[0], stick(6)),
            (AXES[1], down(stick(8))),
            // Triggers are `0..=255` in the report, `0..=0x7FFF` on the bus.
            (AXES[2], trigger(report[4])),
            (AXES[3], stick(10)),
            (AXES[4], down(stick(12))),
            (AXES[5], trigger(report[5])),
        ];
        for (slot, (usage, value)) in axes.into_iter().enumerate() {
            if self.axes[slot] != Some(value) {
                self.axes[slot] = Some(value);
                emit(PadOut::Axis { usage, value });
            }
        }
        let hat = hat_of(word);
        if self.hat != Some(hat) {
            self.hat = Some(hat);
            emit(PadOut::Axis {
                usage: usage::HAT,
                value: hat,
            });
        }
        let mut next = 0u32;
        for (bit, usage) in BUTTONS {
            if word & bit != 0 {
                next |= 1 << (usage - 1);
            }
        }
        self.apply_buttons(next, &mut emit);
        true
    }

    /// The pad is going away: every axis returns to its rest value and every
    /// held button is released (an unplugged pad must not leave a stick
    /// deflected).
    pub fn release_all(&mut self, mut emit: impl FnMut(PadOut)) {
        for (slot, last) in self.axes.iter_mut().enumerate() {
            if *last == Some(0) || last.is_none() {
                continue;
            }
            *last = Some(0);
            emit(PadOut::Axis {
                usage: AXES[slot],
                value: 0,
            });
        }
        if let Some(hat) = self.hat {
            if hat != HAT_CENTRED {
                self.hat = Some(HAT_CENTRED);
                emit(PadOut::Axis {
                    usage: usage::HAT,
                    value: HAT_CENTRED,
                });
            }
        }
        self.apply_buttons(0, &mut emit);
    }

    fn apply_buttons(&mut self, next: u32, emit: &mut impl FnMut(PadOut)) {
        for (_, usage) in BUTTONS {
            let mask = 1u32 << (usage - 1);
            if (self.buttons ^ next) & mask != 0 {
                emit(PadOut::Button {
                    usage,
                    pressed: next & mask != 0,
                });
            }
        }
        self.buttons = next;
    }
}

/// A stick's Y axis, up positive in the report, as HID's down-positive one.
/// The report's `-32768` (fully down) saturates to `32767`; a centred stick
/// stays at 0.
fn down(y: i32) -> i32 {
    (-y).min(i32::from(i16::MAX))
}

/// A trigger's `0..=255` as `0..=0x7FFF`.
fn trigger(raw: u8) -> i32 {
    i32::from(raw) * 0x7FFF / 0xFF
}

/// The hat direction for a report word: `0..=7` clockwise from up, or
/// [`HAT_CENTRED`] when no direction (or an opposed pair) is pressed.
fn hat_of(word: u16) -> i32 {
    let up = word & button::UP != 0;
    let down = word & button::DOWN != 0;
    let left = word & button::LEFT != 0;
    let right = word & button::RIGHT != 0;
    match (up, down, left, right) {
        (true, false, true, false) => 7,
        (true, false, false, false) => 0,
        (true, false, false, true) => 1,
        (false, false, false, true) => 2,
        (false, true, false, true) => 3,
        (false, true, false, false) => 4,
        (false, true, true, false) => 5,
        (false, false, true, false) => 6,
        _ => HAT_CENTRED,
    }
}
