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
//!
//! **A lost release** (issue #400). A break code that never arrived (QEMU's
//! 16-byte PS/2 queue discards what an oversized injection batch does not fit;
//! a real controller can drop a byte too) leaves the key marked down, and its
//! next press would be swallowed as typematic, so the key looks held until it
//! is released again: a stuck Shift types capitals, a game keeps running. But
//! typematic is periodic: a keyboard repeats after at most 1 s and then at
//! 2 Hz or faster, so a make for a "held" key that arrives [`STALE_NS`] after
//! that key's previous make cannot be a repeat. It is a new press whose
//! release was lost: the tap publishes the missing release and then the press,
//! so consumers resynchronise on a clean edge pair.

use core::sync::atomic::{AtomicU64, Ordering};

use super::bus::{self, device, kind, value};
use super::hid::{Set1Decoder, Step};

/// The longest gap between two makes of one held key that typematic can
/// produce (1 s delay at the slowest setting), with margin for interrupt
/// latency. A longer gap means the release in between was lost.
pub const STALE_NS: u64 = 1_500_000_000;

/// Bytes the decoder could not map (multimedia keys, corrupt input).
static UNKNOWN: AtomicU64 = AtomicU64::new(0);
/// Typematic make codes suppressed because the key was already down.
static REPEATS: AtomicU64 = AtomicU64::new(0);
/// Lost releases repaired (a stale re-press of a held key).
static RESYNCS: AtomicU64 = AtomicU64::new(0);

/// Lost releases repaired so far.
#[cfg(lazyos_tests)]
pub fn resynced_presses() -> u64 {
    RESYNCS.load(Ordering::Relaxed)
}

/// Bytes with no HID mapping seen so far.
#[cfg(lazyos_tests)]
pub fn unknown_count() -> u64 {
    UNKNOWN.load(Ordering::Relaxed)
}

/// Hardware auto-repeat make codes suppressed so far.
#[cfg(lazyos_tests)]
pub fn suppressed_repeats() -> u64 {
    REPEATS.load(Ordering::Relaxed)
}

pub struct Tap {
    decoder: Set1Decoder,
    /// One bit per HID usage 0..=255 (every usage the table produces fits).
    down: [u64; 4],
    /// When each held key's newest make (press or repeat) arrived.
    last_make: [u64; 256],
}

impl Tap {
    pub const fn new() -> Self {
        Tap {
            decoder: Set1Decoder::new(),
            down: [0; 4],
            last_make: [0; 256],
        }
    }

    /// Forget partial sequences and held keys (test hook).
    #[cfg(lazyos_tests)]
    pub fn reset(&mut self) {
        self.decoder.reset();
        self.down = [0; 4];
        self.last_make = [0; 256];
    }

    /// Bytes were lost: drop any half-received sequence and publish a release
    /// for every key still marked down, so no consumer keeps a key held whose
    /// release may have been among them. Returns how many were released.
    pub fn release_all(&mut self) -> usize {
        self.decoder.reset();
        let mut released = 0;
        for word in 0..self.down.len() {
            while self.down[word] != 0 {
                let bit = self.down[word].trailing_zeros();
                self.down[word] &= !(1u64 << bit);
                emit((word as u16) << 6 | bit as u16, value::RELEASE);
                released += 1;
            }
        }
        released
    }

    /// Feed one scancode byte from IRQ1.
    pub fn feed(&mut self, byte: u8) {
        self.feed_at(byte, crate::arch::clock::monotonic_ns());
    }

    /// [`Tap::feed`] at monotonic time `now_ns` (tests pass their own clock).
    pub fn feed_at(&mut self, byte: u8, now_ns: u64) {
        match self.decoder.feed(byte) {
            Step::Key(usage, true) => {
                let previous = core::mem::replace(&mut self.last_make[usage as usize], now_ns);
                if self.set_down(usage, true) {
                    emit(usage, value::PRESS);
                } else if now_ns.saturating_sub(previous) >= STALE_NS {
                    // Too late for typematic: the release in between was
                    // lost. Publish it, then this press.
                    RESYNCS.fetch_add(1, Ordering::Relaxed);
                    emit(usage, value::RELEASE);
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
