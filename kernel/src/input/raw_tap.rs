//! The PS/2 driver's tap onto the raw event bus.
//!
//! [`Tap`] turns the i8042 byte stream into [`bus::kind::KEY`] events with HID
//! usage codes. It is the only place PS/2 specifics (set 1, `E0`/`E1`
//! prefixes, typematic) touch the bus.
//!
//! Hardware typematic re-sends the *make* code while a key is held. The bus
//! contract is press/release only (`inputd` owns repeat), so a make code for a
//! key the tap already knows is down is swallowed and counted rather than
//! forwarded. Releases always pass: a release for a key the tap never saw
//! pressed (held across boot) is harmless to consumers and lets them resync.

use core::sync::atomic::{AtomicU64, Ordering};

use super::bus::{self, device, kind, value};
use super::hid::{Set1Decoder, Step};

/// Bytes the decoder could not map (multimedia keys, corrupt input).
static UNKNOWN: AtomicU64 = AtomicU64::new(0);
/// Typematic make codes suppressed because the key was already down.
static REPEATS: AtomicU64 = AtomicU64::new(0);

/// Bytes with no HID mapping seen so far.
pub fn unknown_count() -> u64 {
    UNKNOWN.load(Ordering::Relaxed)
}

/// Hardware auto-repeat make codes suppressed so far.
pub fn suppressed_repeats() -> u64 {
    REPEATS.load(Ordering::Relaxed)
}

pub struct Tap {
    decoder: Set1Decoder,
    /// One bit per HID usage 0..=255 (every usage the table produces fits).
    down: [u64; 4],
}

impl Tap {
    pub const fn new() -> Self {
        Tap {
            decoder: Set1Decoder::new(),
            down: [0; 4],
        }
    }

    /// Forget partial sequences and held keys (test hook).
    pub fn reset(&mut self) {
        self.decoder.reset();
        self.down = [0; 4];
    }

    /// Feed one scancode byte from IRQ1.
    pub fn feed(&mut self, byte: u8) {
        match self.decoder.feed(byte) {
            Step::Key(usage, true) => {
                if self.set_down(usage, true) {
                    emit(usage, value::PRESS);
                } else {
                    REPEATS.fetch_add(1, Ordering::Relaxed);
                }
            }
            Step::Key(usage, false) => {
                self.set_down(usage, false);
                emit(usage, value::RELEASE);
            }
            Step::Tap(usage) => {
                emit(usage, value::PRESS);
                emit(usage, value::RELEASE);
            }
            Step::Unknown => {
                UNKNOWN.fetch_add(1, Ordering::Relaxed);
            }
            Step::Pending | Step::Ignored => {}
        }
    }

    /// Record the new state of `usage`; returns whether it changed.
    fn set_down(&mut self, usage: u16, down: bool) -> bool {
        let (word, bit) = ((usage as usize >> 6) & 3, 1u64 << (usage & 63));
        let was = self.down[word] & bit != 0;
        if down {
            self.down[word] |= bit;
        } else {
            self.down[word] &= !bit;
        }
        was != down
    }
}

fn emit(usage: u16, state: i32) {
    bus::publish(device::PS2_KEYBOARD, kind::KEY, usage, state);
}
