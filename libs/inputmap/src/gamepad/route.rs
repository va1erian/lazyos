//! Which engine owns a device's records (`docs/usb-gamepad-plan.md`,
//! decision 6).
//!
//! A pad's `BUTTON` records use the same codes as a mouse's, and the records
//! themselves do not say what sent them: `DEVICE` is accepted from every
//! source class, the first record of a pad may be lost in a `Dropped` gap, and
//! a device id is the kernel source *slot's*, so a mouse registered after a
//! pad was unplugged gets the pad's old id. The class therefore comes from the
//! kernel: its `SOURCE` records (the fast path, [`Classifier::source_record`])
//! and the live source table (the truth, [`Classifier::table`]), which the
//! caller re-reads when it opens the bus and after every `Dropped`, since a
//! `SOURCE` record can be evicted from the ring like any other.
//!
//! The rules, per device:
//!
//! * a known gamepad's `BUTTON` and `AXIS` go to the pad engine, never to the
//!   pointer, and anything else it sends is dropped;
//! * a device of another known class keeps its pointer routing, and an `AXIS`
//!   from it is dropped;
//! * an unknown device keeps today's pointer routing for pointer kinds (an
//!   older kernel, or a table read that failed, must not silence a mouse) and
//!   drops `AXIS`;
//! * a `SOURCE` for a device already known starts it over, and a table entry
//!   whose class or generation differs from what was known is a new source:
//!   the old pad, if it was one, is detached first.

use alloc::vec::Vec;

use super::{raw, RawPad};
use crate::pointer::raw as pointer_raw;

/// The kernel's gamepad source class (`lazyos_sys::input::source_class`).
pub const GAMEPAD_CLASS: u8 = 4;

/// Devices ids are one byte.
const DEVICES: usize = 256;

/// One entry of the kernel's source table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceEntry {
    pub device: u8,
    pub class: u8,
    /// Changes every time the slot is registered.
    pub generation: u16,
}

/// Where a record goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// The cursor engine.
    Pointer,
    /// The gamepad engine.
    Pad,
    /// Nobody: it cannot be from a source of that class.
    Drop,
}

/// A device that became, or stopped being, a gamepad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    PadAttached(u8),
    PadDetached(u8),
}

/// What is known of one device id. `class` 0 is "unknown".
#[derive(Clone, Copy, Default)]
struct Known {
    class: u8,
    /// Learned from a table; a `SOURCE` record does not carry it.
    generation: Option<u16>,
}

pub struct Classifier {
    known: [Known; DEVICES],
    rejected: u64,
}

impl Default for Classifier {
    fn default() -> Classifier {
        Classifier::new()
    }
}

impl Classifier {
    pub const fn new() -> Classifier {
        Classifier {
            known: [Known {
                class: 0,
                generation: None,
            }; DEVICES],
            rejected: 0,
        }
    }

    /// `SOURCE` records dropped as malformed so far.
    pub fn rejected(&self) -> u64 {
        self.rejected
    }

    /// Whether `device` is a known gamepad.
    pub fn is_pad(&self, device: u8) -> bool {
        self.known[usize::from(device)].class == GAMEPAD_CLASS
    }

    /// Where a record of `kind` from `device` goes.
    pub fn route(&self, device: u8, kind: u8) -> Route {
        let class = self.known[usize::from(device)].class;
        match kind {
            raw::BUTTON | raw::AXIS if class == GAMEPAD_CLASS => Route::Pad,
            _ if class == GAMEPAD_CLASS => Route::Drop,
            raw::AXIS => Route::Drop,
            kind if pointer_raw::is_pointer(kind) => Route::Pointer,
            _ => Route::Drop,
        }
    }

    /// A `SOURCE` record (`code` = class, `value` 1 registered / 0 closed).
    /// Returns whether it was well formed.
    pub fn source_record(&mut self, record: &RawPad, changes: &mut Vec<Change>) -> bool {
        let class = u8::try_from(record.code).ok().filter(|&class| class != 0);
        let (Some(class), 0 | 1) = (class, record.value) else {
            self.rejected += 1;
            return false;
        };
        // Whatever held the id before is gone, close record or not.
        self.forget(record.device, changes);
        if record.value == 1 {
            self.set(record.device, class, None, changes);
        }
        true
    }

    /// The live source table, which is the truth: devices not in it are gone,
    /// devices whose class or generation differ are new sources.
    pub fn table(&mut self, entries: &[SourceEntry], changes: &mut Vec<Change>) {
        let mut live = [false; DEVICES];
        for entry in entries {
            live[usize::from(entry.device)] = true;
            let known = self.known[usize::from(entry.device)];
            let same = known.class == entry.class
                && known.generation.is_none_or(|gen| gen == entry.generation);
            if same {
                self.known[usize::from(entry.device)].generation = Some(entry.generation);
            } else {
                self.forget(entry.device, changes);
                self.set(entry.device, entry.class, Some(entry.generation), changes);
            }
        }
        for (device, live) in live.into_iter().enumerate() {
            if !live {
                self.forget(device as u8, changes);
            }
        }
    }

    fn set(&mut self, device: u8, class: u8, generation: Option<u16>, changes: &mut Vec<Change>) {
        self.known[usize::from(device)] = Known { class, generation };
        if class == GAMEPAD_CLASS {
            changes.push(Change::PadAttached(device));
        }
    }

    fn forget(&mut self, device: u8, changes: &mut Vec<Change>) {
        if self.is_pad(device) {
            changes.push(Change::PadDetached(device));
        }
        self.known[usize::from(device)] = Known::default();
    }
}
