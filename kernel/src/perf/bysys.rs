//! The longest interrupts-off stretch per syscall number (P5): the global
//! worst says which syscall is the worst offender, this says how every other
//! one fares (storage calls next to IPC and display ones, for instance).

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Distinct syscall numbers tracked; later ones are dropped.
const SLOTS: usize = 48;
/// How many are printed, longest first.
pub const SHOWN: usize = 12;
/// A free slot (no syscall has this number).
pub const EMPTY: u64 = u64::MAX;

static NUMBERS: [AtomicU64; SLOTS] = [const { AtomicU64::new(EMPTY) }; SLOTS];
static WORST: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static CHANGED: AtomicBool = AtomicBool::new(false);

/// Note a stretch of `cycles` inside syscall `nr`. Runs with interrupts off
/// on the only CPU, so a slot is never claimed twice.
pub fn record(nr: u64, cycles: u64) {
    for (number, worst) in NUMBERS.iter().zip(&WORST) {
        let current = number.load(Ordering::Relaxed);
        if current == EMPTY {
            number.store(nr, Ordering::Relaxed);
        } else if current != nr {
            continue;
        }
        if cycles > worst.load(Ordering::Relaxed) {
            worst.store(cycles, Ordering::Relaxed);
            CHANGED.store(true, Ordering::Relaxed);
        }
        return;
    }
}

/// The [`SHOWN`] longest, as `(nr, cycles)` (unused entries hold [`EMPTY`]),
/// when anything changed since the last call.
pub fn take_changed() -> Option<[(u64, u64); SHOWN]> {
    if !CHANGED.swap(false, Ordering::Relaxed) {
        return None;
    }
    let mut top = [(EMPTY, 0u64); SHOWN];
    for (number, worst) in NUMBERS.iter().zip(&WORST) {
        let entry = (
            number.load(Ordering::Relaxed),
            worst.load(Ordering::Relaxed),
        );
        if entry.0 == EMPTY {
            break;
        }
        if let Some(place) = top
            .iter()
            .position(|&(nr, cycles)| nr == EMPTY || entry.1 > cycles)
        {
            top[place..].rotate_right(1);
            top[place] = entry;
        }
    }
    Some(top)
}
