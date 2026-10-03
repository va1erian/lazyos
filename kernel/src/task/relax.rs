//! Locks that may be held across a park: the filesystem mount tables and an
//! ext2 volume, which a task holds while it waits for a user-space block
//! provider (`block::provider`, docs/architecture/usb-storage.md).
//!
//! Syscalls run with interrupts off on one CPU, so a plain spin lock is only
//! ever contended when its holder parked inside the critical section. A
//! contender that spins with interrupts off then never lets the holder (or
//! the driver it waits for) run again: the machine hangs. [`Yield`] makes the
//! contender give the CPU away instead: it yields to whatever is runnable and,
//! when it is picked again with the lock still held, sleeps until the next
//! interrupt so the timer can wake the parked tasks. Uncontended, a
//! [`YieldMutex`] costs exactly what a spin lock does.
//!
//! The rule that keeps this sound: a provider's own syscall path (the driver
//! fetching and completing requests) never takes one of these locks.

use x86_64::instructions::interrupts;

use super::TASKS;

/// The relax strategy of [`YieldMutex`].
pub struct Yield;

impl spin::RelaxStrategy for Yield {
    fn relax() {
        // Inside the scheduler (the task table is held) nothing may switch;
        // a contended lock there is a bug elsewhere, so just spin.
        if TASKS.is_locked() {
            core::hint::spin_loop();
            return;
        }
        let enabled = interrupts::are_enabled();
        super::switch::yield_now();
        if enabled {
            x86_64::instructions::hlt();
        } else {
            // Let one interrupt in (the tick that wakes a parked holder or
            // its driver), then restore the caller's interrupt state.
            interrupts::enable_and_hlt();
            interrupts::disable();
        }
    }
}

/// A spin lock whose contenders yield the CPU (see the module docs).
pub type YieldMutex<T> = spin::mutex::Mutex<T, Yield>;

/// Kernel stack a parking task must still have: the scheduler's frames
/// (`schedule`, `signal::sweep`) take about 10 KiB on top of the caller's.
const PARK_HEADROOM: u64 = 14 * 1024;

/// Whether the current context may park: it must not hold the task table
/// (parking takes it) and must leave the scheduler room on its kernel stack
/// (an overflow would silently corrupt the next task's). Callers that cannot
/// park fail their operation instead.
pub fn can_block() -> bool {
    !TASKS.is_locked() && super::kstack_headroom().is_none_or(|left| left >= PARK_HEADROOM)
}
