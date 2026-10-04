//! How a block request waits for its device (docs/performance-plan.md P5).
//!
//! Syscalls run with interrupts off, and virtio-blk used to busy-wait for
//! every request inside them: the whole machine stopped for each disk access
//! (a 1.6 s stretch in one `write` that met the cache's dirty limit). A
//! caller that holds no plain spin lock now passes [`Wait::MaySleep`], and
//! [`wait_until`] parks it on a deadline instead, so interrupts are served
//! and other tasks run while the device works.
//!
//! # Why a deadline and not the device's interrupt
//!
//! The legacy virtio-blk function's INTx line is shared: QEMU wires it to the
//! same PIC line as the network card (line 11 on the default machine), whose
//! interrupt belongs to the user-space `netdrv` (`dev::intx`). A kernel
//! handler cannot own a line a user-space claimant arms, so the waiter polls
//! the used ring at deadlines instead: the first one at about the time the
//! device usually takes (an average kept per device and direction), then
//! short slices up to [`MAX_SLICE_NS`]. The one-shot APIC deadline timer
//! (P2) makes those deadlines tens of microseconds exact.
//!
//! # Who may sleep
//!
//! The caller must hold no spin lock that another task could take without
//! first meeting a lock whose contenders yield (`task::relax::YieldMutex`):
//! the ext2 adapter qualifies (its volume gate and both mount tables are such
//! locks, the library's own locks are only reached through the gate; the
//! USB block provider already parks there). Beyond the caller's word, the
//! context must be able to park at all: the scheduler runs and
//! `task::relax::can_block` holds. Everything else spins as before, with the
//! i8042 drained so keys are not lost.
//!
//! A task killed while it waits keeps waiting, in naps, until the device is
//! done with its buffers: a request is never abandoned while the device may
//! still write into memory the caller is about to free (#558's deferral).

#[cfg(lazyos_tests)]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::interrupts;

use super::Wait;
use crate::arch::clock::monotonic_ns;
use crate::task::{self, wait::WaitQueue, WaitKind, WakeReason};

/// Where waiting requesters park. Nothing notifies it: every wait has a
/// deadline, and a notify from elsewhere would only cause an early re-check.
static WAITERS: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// Shortest and longest park between two looks at the device.
const MIN_SLICE_NS: u64 = 20_000;
pub const MAX_SLICE_NS: u64 = 250_000;
/// The first look comes after this share of the usual completion time.
const FIRST_LOOK_PERCENT: u64 = 75;

/// Spins between two drains of the i8042 when busy-waiting.
const SPINS_PER_SERVICE: u64 = 1024;
/// Spin bound for a busy-wait whose clock does not advance (interrupts off).
const SPIN_BACKSTOP: u64 = 4_000_000_000;

/// Test builds: let kernel test threads sleep before the scheduler starts.
#[cfg(lazyos_tests)]
static TEST_SLEEP: AtomicBool = AtomicBool::new(false);

/// Whether a caller that passed `wait` may really park here.
pub fn can_sleep(wait: Wait) -> bool {
    if wait != Wait::MaySleep || !task::relax::can_block() {
        return false;
    }
    #[cfg(lazyos_tests)]
    if TEST_SLEEP.load(Ordering::Relaxed) && task::current() != task::KERNEL_TASK {
        return true;
    }
    task::scheduling()
}

/// Test builds: allow kernel threads (never the suite's own kernel task) to
/// sleep in I/O while the suite runs without a started scheduler.
#[cfg(lazyos_tests)]
pub fn set_test_sleep(allowed: bool) {
    TEST_SLEEP.store(allowed, Ordering::Relaxed);
}

/// A running average of how long a device takes to answer, in nanoseconds,
/// for the first deadline of a wait.
pub struct Expect(AtomicU64);

impl Expect {
    pub const fn new() -> Expect {
        Expect(AtomicU64::new(0))
    }

    /// The current estimate (0: none yet).
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Fold one observation in (an eighth of the weight).
    pub fn note(&self, ns: u64) {
        let old = self.0.load(Ordering::Relaxed);
        let new = if old == 0 { ns } else { old - old / 8 + ns / 8 };
        self.0.store(new, Ordering::Relaxed);
    }
}

/// Wait until `done()` says so, for at most `timeout_ns` (or, busy-waiting
/// with a stopped clock, [`SPIN_BACKSTOP`] spins). Returns whether `done()`
/// held. `expect` is the device's usual answer time. Interrupts are as the
/// caller had them on return.
pub fn wait_until(
    wait: Wait,
    expect: &Expect,
    timeout_ns: u64,
    mut done: impl FnMut() -> bool,
) -> bool {
    let start = monotonic_ns();
    let answered = if can_sleep(wait) {
        sleep_until(expect, start, timeout_ns, &mut done)
    } else {
        spin_until(start, timeout_ns, &mut done)
    };
    if answered {
        expect.note(monotonic_ns().saturating_sub(start));
    }
    answered
}

/// Park on deadlines until `done()`.
fn sleep_until(
    expect: &Expect,
    start: u64,
    timeout_ns: u64,
    done: &mut impl FnMut() -> bool,
) -> bool {
    let enabled = interrupts::are_enabled();
    interrupts::disable();
    let mut slice = (expect.get() * FIRST_LOOK_PERCENT / 100).clamp(MIN_SLICE_NS, MAX_SLICE_NS);
    let answered = loop {
        if done() {
            break true;
        }
        let now = monotonic_ns();
        if now.saturating_sub(start) >= timeout_ns {
            break done();
        }
        park(now + slice);
        slice = (slice * 2).min(MAX_SLICE_NS);
    };
    if enabled {
        interrupts::enable();
    }
    answered
}

/// Park the current task until `deadline` (interrupts off). A killed task's
/// wait returns at once, so it naps instead: the device must finish first.
fn park(deadline: u64) {
    let me = task::current();
    #[cfg(lazyos_tests)]
    {
        test_hooks::PARKS.fetch_add(1, Ordering::Relaxed);
        if test_hooks::KILLED.load(Ordering::Relaxed) == me {
            // As a killed task: its waits return at once.
            test_hooks::KILLED_NAPS.fetch_add(1, Ordering::Relaxed);
            task::nap();
            return;
        }
    }
    if WAITERS.wait_ns(me, Some(deadline)) == WakeReason::Interrupted && task::signal::killed(me) {
        task::nap();
    }
}

/// What the kernel suite observes and steers of the waits.
#[cfg(lazyos_tests)]
pub mod test_hooks {
    use core::sync::atomic::{AtomicU64, AtomicUsize};

    /// Parks (and killed naps) since boot.
    pub static PARKS: AtomicU64 = AtomicU64::new(0);
    pub static KILLED_NAPS: AtomicU64 = AtomicU64::new(0);
    /// A slot whose waits behave as a killed task's (`usize::MAX`: none).
    pub static KILLED: AtomicUsize = AtomicUsize::new(usize::MAX);
}

/// Busy-wait for `done()` with interrupts as they are.
fn spin_until(start: u64, timeout_ns: u64, done: &mut impl FnMut() -> bool) -> bool {
    let mut spins = 0u64;
    loop {
        if done() {
            return true;
        }
        spins += 1;
        // The wait runs with interrupts off: keep the i8042 drained.
        if spins.is_multiple_of(SPINS_PER_SERVICE) {
            crate::input::ps2::service();
            if monotonic_ns().saturating_sub(start) >= timeout_ns || spins >= SPIN_BACKSTOP {
                return done();
            }
        }
        core::hint::spin_loop();
    }
}

/// Between two pieces of a long filesystem call: let pending interrupts in,
/// deliver what they raised to user-space drivers, and give the CPU to a task
/// they woke if it should run first, exactly as the display `present` does
/// between chunks (P3.2). Only when the caller may sleep (`wait`); the caller
/// holds no lock but yielding ones there.
pub fn breathe(wait: Wait) {
    if !can_sleep(wait) || interrupts::are_enabled() {
        return;
    }
    // Pause points are frequent (every directory block a lookup scans);
    // only one per [`BREATH_EVERY_NS`] opens the window.
    let now = crate::perf::rdtsc();
    let every = crate::arch::clock::cycles_per_tick() / (10_000_000 / BREATH_EVERY_NS);
    if now.wrapping_sub(LAST_BREATH.load(Ordering::Relaxed)) < every {
        return;
    }
    crate::perf::irqoff_pause();
    interrupts::enable();
    // `sti` takes effect after the next instruction, so a pending interrupt
    // is taken right after this one.
    core::hint::spin_loop();
    interrupts::disable();
    crate::perf::irqoff_resume();
    crate::dev::intx::service();
    task::preempt_point();
    LAST_BREATH.store(crate::perf::rdtsc(), Ordering::Relaxed);
}

/// Least time between two breaths.
const BREATH_EVERY_NS: u64 = 50_000;
/// TSC at the end of the last breath (one CPU, so one value).
static LAST_BREATH: AtomicU64 = AtomicU64::new(0);
