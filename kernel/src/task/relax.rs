//! Locks that may be held across a park: the filesystem mount tables and an
//! ext2 volume, which a task holds while it waits for a user-space block
//! provider (`block::provider`, docs/architecture/usb-storage.md) or for
//! virtio-blk (`block::iowait`).
//!
//! Syscalls run with interrupts off on one CPU, so a plain spin lock is only
//! ever contended when its holder parked (or was preempted) inside the
//! critical section. A contender that spins with interrupts off then never
//! lets the holder (or the driver it waits for) run again: the machine hangs.
//! [`Yield`] makes the contender give the CPU away instead. Uncontended, a
//! [`YieldMutex`] costs exactly what a spin lock does.
//!
//! # Contenders park, they do not just yield (issue #609)
//!
//! Scheduling classes are strict (`task::sched`): a runnable task of a higher
//! class always wins the CPU. A contender that merely yields stays runnable,
//! so when it outranks the holder (the `Interactive` compositor wanting the
//! VFS a `Normal` shell holds while it waits for the disk) every pick chooses
//! the contender again and the holder never runs to release the lock: a
//! livelock that hung desktop boots at a `stat` from `xuid`. So a contender
//! parks for [`PARK_NS`] (blocked tasks are never picked), and every class
//! below it gets the CPU meanwhile. Only where parking is impossible (the task
//! table is held, the stack is too short, the scheduler has not started) does
//! it fall back to yielding and halting until the next interrupt.
//!
//! The rule that keeps this sound: a provider's own syscall path (the driver
//! fetching and completing requests) never takes one of these locks.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::instructions::interrupts;

use super::TASKS;

/// How long a contender parks before it looks at the lock again. Short
/// against a tick (10 ms): a lock is contended only while its holder parks
/// or was preempted, and the one-shot deadline timer makes this exact.
pub const PARK_NS: u64 = 200_000;

/// Parks taken by contenders since boot (diagnostics and the kernel suite).
static PARKS: AtomicU64 = AtomicU64::new(0);

/// Test builds: let kernel test threads park before the scheduler starts.
static TEST_PARK: AtomicBool = AtomicBool::new(false);

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
        if may_park() {
            park();
        } else {
            yield_and_halt();
        }
    }
}

/// A spin lock whose contenders yield the CPU (see the module docs).
pub type YieldMutex<T> = spin::mutex::Mutex<T, Yield>;

/// Whether the contender can park for [`PARK_NS`]: the scheduler runs (or a
/// test allowed its threads to park) and [`can_block`] holds.
fn may_park() -> bool {
    let started = super::scheduling()
        || (TEST_PARK.load(Ordering::Relaxed) && super::current() != super::KERNEL_TASK);
    started && can_block()
}

/// Park the current task for [`PARK_NS`], whatever its interrupt state; the
/// caller's state comes back. The park is deadline-bounded, so it needs no
/// queue and nobody to notify it, and it is taken even by a task that is
/// being killed (unlike `WaitQueue::wait`): the deadline ends it anyway, and
/// a killed contender that kept spinning would starve the holder just the
/// same.
fn park() {
    let enabled = interrupts::are_enabled();
    interrupts::disable();
    let me = super::current();
    PARKS.fetch_add(1, Ordering::Relaxed);
    let deadline = crate::arch::clock::monotonic_ns().saturating_add(PARK_NS);
    super::block_task(me, super::WaitKind::Sleep, Some(deadline));
    super::switch::yield_now();
    // Picked again before the deadline (a stray wake): sleep through ticks
    // until the deadline sweep marks the park over.
    while super::take_wake_reason(me).is_none() {
        super::nap();
    }
    if enabled {
        interrupts::enable();
    }
}

/// The fallback where parking is impossible: give the CPU to whatever is
/// runnable, and if picked again with the lock still held, sleep until the
/// next interrupt so the timer can wake the parked holder.
fn yield_and_halt() {
    let enabled = interrupts::are_enabled();
    super::switch::yield_now();
    if enabled {
        x86_64::instructions::hlt();
    } else {
        // Let one interrupt in (the tick that wakes a parked holder or
        // its driver), then restore the caller's interrupt state.
        crate::arch::irqoff::paused(|| {
            interrupts::enable_and_hlt();
            interrupts::disable();
        });
    }
}

/// Parks taken by lock contenders since boot.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn parks() -> u64 {
    PARKS.load(Ordering::Relaxed)
}

/// Test builds: allow kernel threads (never the suite's own kernel task) to
/// park on a contended lock while the suite runs without a started scheduler.
#[cfg(lazyos_tests)]
pub fn set_test_park(allowed: bool) {
    TEST_PARK.store(allowed, Ordering::Relaxed);
}

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
