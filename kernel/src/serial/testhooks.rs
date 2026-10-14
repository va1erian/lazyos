//! Hooks for the kernel test suite (`tests/serial_suite.rs`).

use super::*;
use core::sync::atomic::AtomicU64;

/// Test builds: send everything to the absent COM4 (`0x2E8`) instead of the
/// real port, at most `cap` bytes per drain call (a slow consumer), until the
/// returned guard is dropped. What was queued is discarded on both ends.
/// Nothing printed meanwhile reaches the real port: do not print from the
/// test body.
pub struct Diverted {
    saved: Option<(u16, usize)>,
}

pub fn divert_for_test(cap: usize) -> Diverted {
    const COM4: u16 = 0x2E8;
    let saved = port().as_mut().map(|port| {
        port.tx.drain_all();
        port.tx.divert(COM4, cap)
    });
    Diverted { saved }
}

impl Drop for Diverted {
    fn drop(&mut self) {
        if let (Some((base, cap)), Some(port)) = (self.saved, port().as_mut()) {
            port.tx.divert(base, cap);
            port.tx.set_never_empty(false);
        }
    }
}

/// Drain rounds [`mirror`] ran (a killed caller runs none).
pub static MIRROR_ROUNDS: AtomicU64 = AtomicU64::new(0);

/// Change the bytes one drain call may send on a diverted port.
pub fn set_cap_for_test(cap: usize) {
    if let Some(port) = port().as_mut() {
        port.tx.set_cap(cap);
    }
}

/// Make the diverted port never report its transmitter empty (a host that
/// stopped reading the serial port), or report normally again.
pub fn set_never_empty_for_test(on: bool) {
    if let Some(port) = port().as_mut() {
        port.tx.set_never_empty(on);
    }
}

/// How many drains in a row gave up on the port (the back-off level).
pub fn stall_level_for_test() -> u32 {
    port().as_ref().map_or(0, |port| port.tx.stall_level())
}

/// Test hook: bytes queued and sent so far, to judge a run.
pub fn counters_for_test() -> (usize, usize) {
    port()
        .as_ref()
        .map_or((0, 0), |port| (port.tx.queued(), port.tx.drained()))
}

/// The bytes waiting in the ring, oldest first.
pub fn ring_contents_for_test() -> alloc::vec::Vec<u8> {
    port()
        .as_ref()
        .map_or_else(alloc::vec::Vec::new, |port| port.tx.contents())
}

/// Whether [`note_lock`] is counting, and the locks it saw taken with
/// interrupts enabled (the #382 rule: a task preempted holding the port
/// would starve every syscall that logs with interrupts off).
static WATCH: AtomicBool = AtomicBool::new(false);
static LOCKED_WITH_INTERRUPTS: AtomicU64 = AtomicU64::new(0);

/// Called by every lock of the port in a test build.
pub(super) fn note_lock() {
    if WATCH.load(Ordering::Relaxed) && x86_64::instructions::interrupts::are_enabled() {
        LOCKED_WITH_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Start (or stop) counting port locks taken with interrupts on; returns the
/// count so far.
pub fn watch_interrupt_locks(on: bool) -> u64 {
    WATCH.store(on, Ordering::Relaxed);
    if on {
        LOCKED_WITH_INTERRUPTS.store(0, Ordering::Relaxed);
    }
    LOCKED_WITH_INTERRUPTS.load(Ordering::Relaxed)
}

/// Take the port's lock and never release it, as a context that panicked
/// while logging leaves it (`final_line` must not wait for it).
pub fn leak_port_lock_for_test() {
    core::mem::forget(port());
}

/// Whether the port's lock is held right now.
pub fn port_locked_for_test() -> bool {
    SERIAL1.is_locked()
}
