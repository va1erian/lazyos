//! HID boot-protocol reports (HID 1.11 appendix B) to bus edges.
//!
//! A boot keyboard report is the *state* of the keyboard: a modifier byte, a
//! reserved byte and up to six key usages. [`BootKeyboard`] diffs each report
//! against the previous one and emits press and release edges, which is what
//! the raw input bus carries. A report whose key slots hold the phantom
//! codes (`ErrorRollOver` and friends, usages 1..=3) says "too many keys",
//! not "these keys": it is ignored and the previous state kept.
//!
//! A boot mouse report is buttons, `dx`, `dy` and (usually) a wheel byte.
//! [`BootMouse`] emits motion, then wheel, then button edges, the same order
//! the PS/2 tap uses, so a press lands after the motion and wheel of its
//! report. HID axes already match the bus: `dy > 0` is down, a positive wheel
//! is away from the user (up).

use crate::Error;

/// The longest boot keyboard report: modifiers, reserved, six keys.
pub const KEYBOARD_REPORT: usize = 8;

/// The first usage of the modifier bits (Left Control); bit `n` is `0xE0 + n`.
const MODIFIER_BASE: u16 = 0xE0;
/// Usages a key slot may legitimately hold.
const KEY_USAGES: core::ops::RangeInclusive<u16> = 0x04..=0xE7;
/// `ErrorRollOver`, `POSTFail`, `ErrorUndefined`.
const PHANTOM: core::ops::RangeInclusive<u8> = 0x01..=0x03;

/// A key changed state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEdge {
    /// HID usage, page 0x07, in `0x04..=0xE7`.
    pub usage: u16,
    pub pressed: bool,
}

/// The keys a boot keyboard holds, across reports.
#[derive(Clone, Debug, Default)]
pub struct BootKeyboard {
    /// One bit per usage 0..=255.
    held: [u64; 4],
    /// Reports ignored as phantom (rollover) states.
    pub rollovers: u64,
    /// Key slot bytes outside the keyboard usages, dropped.
    pub rejected: u64,
}

impl BootKeyboard {
    pub const fn new() -> BootKeyboard {
        BootKeyboard {
            held: [0; 4],
            rollovers: 0,
            rejected: 0,
        }
    }

    /// Whether `usage` is held.
    pub fn is_held(&self, usage: u16) -> bool {
        usage < 256 && self.held[usize::from(usage >> 6)] & (1 << (usage & 63)) != 0
    }

    /// Apply one report, emitting releases (ascending usage) then presses
    /// (ascending usage). A report shorter than modifiers + reserved is
    /// refused; bytes past the eighth are ignored.
    pub fn feed(&mut self, report: &[u8], mut emit: impl FnMut(KeyEdge)) -> Result<(), Error> {
        if report.len() < 2 {
            return Err(Error::Short);
        }
        let keys = &report[2..report.len().min(KEYBOARD_REPORT)];
        if keys.iter().any(|key| PHANTOM.contains(key)) {
            self.rollovers += 1;
            return Ok(());
        }
        let mut next = [0u64; 4];
        for bit in 0..8 {
            if report[0] & (1 << bit) != 0 {
                set(&mut next, MODIFIER_BASE + bit);
            }
        }
        for &key in keys {
            match u16::from(key) {
                0 => {}
                usage if KEY_USAGES.contains(&usage) => set(&mut next, usage),
                _ => self.rejected += 1,
            }
        }
        self.apply(next, &mut emit);
        Ok(())
    }

    /// Release every held key (detach, or the driver going away).
    pub fn release_all(&mut self, mut emit: impl FnMut(KeyEdge)) {
        self.apply([0; 4], &mut emit);
    }

    fn apply(&mut self, next: [u64; 4], emit: &mut impl FnMut(KeyEdge)) {
        for pressed in [false, true] {
            for word in 0..4 {
                let changed = self.held[word] ^ next[word];
                let mut bits = if pressed {
                    changed & next[word]
                } else {
                    changed & self.held[word]
                };
                while bits != 0 {
                    let bit = bits.trailing_zeros() as u16;
                    bits &= bits - 1;
                    emit(KeyEdge {
                        usage: word as u16 * 64 + bit,
                        pressed,
                    });
                }
            }
        }
        self.held = next;
    }
}

fn set(bits: &mut [u64; 4], usage: u16) {
    bits[usize::from(usage >> 6)] |= 1 << (usage & 63);
}

/// One decoded boot mouse report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseReport {
    /// Bit `n` is HID button `n + 1`.
    pub buttons: u8,
    pub dx: i8,
    pub dy: i8,
    /// Wheel notches, positive away from the user; 0 for a 3-byte report.
    pub wheel: i8,
}

/// Decode a boot mouse report (3 bytes, or 4 with a wheel; extra ignored).
pub fn parse_mouse(report: &[u8]) -> Result<MouseReport, Error> {
    if report.len() < 3 {
        return Err(Error::Short);
    }
    Ok(MouseReport {
        buttons: report[0],
        dx: report[1] as i8,
        dy: report[2] as i8,
        wheel: report.get(3).map_or(0, |&wheel| wheel as i8),
    })
}

/// HID buttons forwarded to the bus: usages 1..=5 (left, right, middle,
/// back, forward); higher bits are ignored.
pub const MOUSE_BUTTONS: u8 = 5;

/// What one mouse report means, in publication order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseOut {
    Motion { dx: i16, dy: i16 },
    Wheel(i32),
    Button { usage: u16, pressed: bool },
}

/// The buttons a boot mouse holds, across reports.
#[derive(Clone, Copy, Debug, Default)]
pub struct BootMouse {
    held: u8,
}

impl BootMouse {
    pub const fn new() -> BootMouse {
        BootMouse { held: 0 }
    }

    /// The held-button bits (bit `n` is usage `n + 1`).
    pub fn held(&self) -> u8 {
        self.held
    }

    /// Emit motion (if any), wheel (if any), then one edge per changed button.
    pub fn feed(&mut self, report: &MouseReport, mut emit: impl FnMut(MouseOut)) {
        if report.dx != 0 || report.dy != 0 {
            emit(MouseOut::Motion {
                dx: i16::from(report.dx),
                dy: i16::from(report.dy),
            });
        }
        if report.wheel != 0 {
            emit(MouseOut::Wheel(i32::from(report.wheel)));
        }
        self.buttons(report.buttons & ((1 << MOUSE_BUTTONS) - 1), &mut emit);
    }

    /// Release every held button.
    pub fn release_all(&mut self, mut emit: impl FnMut(MouseOut)) {
        self.buttons(0, &mut emit);
    }

    fn buttons(&mut self, next: u8, emit: &mut impl FnMut(MouseOut)) {
        for bit in 0..MOUSE_BUTTONS {
            let mask = 1 << bit;
            if (self.held ^ next) & mask != 0 {
                emit(MouseOut::Button {
                    usage: u16::from(bit) + 1,
                    pressed: next & mask != 0,
                });
            }
        }
        self.held = next;
    }
}
