//! A per-line rate limit for legacy interrupt lines (issue #496).
//!
//! A level-triggered INTx line whose device nobody quiets re-asserts the
//! moment it is unmasked. The delivery contract (`dev::intx`) already holds
//! the line while a claimant owes an ack, but once a round ends the line is
//! unmasked and fires again at once: a driver that acks without servicing its
//! device, or a round of laggards let go, turns into an interrupt per bottom
//! half pass, and the bottom half runs on every syscall.
//!
//! So each legacy line may start at most [`ROUNDS_PER_TICK`] delivery rounds
//! per timer tick. Past that the line stays masked when its round ends
//! (*held*), and the bottom half lets it go on a later tick: the tick that
//! lands in user code or a nap runs it (`task::schedule`), or the next
//! syscall or mux pass. A storm then costs at most that many interrupts per
//! tick, and a healthy device (even a busy NIC coalescing frames per
//! interrupt) stays far below the limit.
//!
//! Only a line whose request survives being masked is held
//! (`arch::irqchip::masked_keeps_request`: any line on the 8259, a
//! level-triggered one on the I/O APIC): holding an edge-triggered I/O APIC
//! input would lose the edge. MSI vectors are edge-triggered messages that do
//! not re-assert, so they are not limited either.
//!
//! Everything here is protected by the claim lock, except
//! [`HELD`], which the interrupt-side check reads lock-free.

use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use super::irq::LINES;

/// Delivery rounds one legacy line may start per timer tick (100 Hz): 6,400
/// a second, far above any device that is serviced.
pub const ROUNDS_PER_TICK: u32 = 64;

/// Bit per legacy line held masked by the limit.
static HELD: AtomicU16 = AtomicU16::new(0);
/// Times a line was held since boot.
static HOLDS: AtomicU64 = AtomicU64::new(0);

/// One line's count for the current tick.
#[derive(Clone, Copy, Default)]
pub struct Throttle {
    tick: u64,
    rounds: u32,
    held: bool,
}

impl Throttle {
    pub const NEW: Throttle = Throttle {
        tick: 0,
        rounds: 0,
        held: false,
    };

    /// Count a raise of `line` at tick `now`; past the limit the line is
    /// held until a later tick.
    pub fn note_raise(&mut self, line: u8, now: u64) {
        if line >= LINES {
            return;
        }
        if now != self.tick {
            self.tick = now;
            self.rounds = 0;
        }
        self.rounds = self.rounds.saturating_add(1);
        if self.rounds > ROUNDS_PER_TICK
            && !self.held
            && crate::arch::irqchip::masked_keeps_request(line)
        {
            self.held = true;
            HELD.fetch_or(1 << line, Ordering::AcqRel);
            HOLDS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Whether the line may be unmasked now.
    pub fn allows_unmask(&self) -> bool {
        !self.held
    }

    /// At tick `now`, let a line held on an earlier tick go. Returns whether
    /// it was held (the caller then settles its mask).
    pub fn release(&mut self, line: u8, now: u64) -> bool {
        if !self.held || now == self.tick {
            return false;
        }
        self.held = false;
        self.tick = now;
        self.rounds = 0;
        HELD.fetch_and(!(1 << line), Ordering::AcqRel);
        true
    }
}

/// Whether some line is held: the bottom half has work on a later tick even
/// with nothing raised.
pub fn any_held() -> bool {
    HELD.load(Ordering::Acquire) != 0
}

/// Times a line was held since boot.
pub fn holds() -> u64 {
    HOLDS.load(Ordering::Relaxed)
}

/// Test-only: forget every hold.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    HELD.store(0, Ordering::Release);
}
