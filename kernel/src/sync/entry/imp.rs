//! The entry lock of a `LAZYOS_SMP=1` kernel (the rules are in the parent
//! module): a [`TicketLock`] whose holder is a CPU, plus where it was taken
//! and how contended it is.

use core::panic::Location;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use super::super::ticket::TicketLock;
use crate::arch::percpu;

/// A waiter that has spun this long (calibrated seconds) panics with the
/// holder's CPU and where it took the lock.
pub const WATCHDOG_SECONDS: u64 = 2;

static LOCK: TicketLock = TicketLock::new();
/// Where the holder took it (the gate), for the watchdog's report.
static SITE: AtomicPtr<Location<'static>> = AtomicPtr::new(ptr::null_mut());
/// Acquisitions that had to wait, and the longest wait (TSC cycles): the
/// number that motivates each later SMP stage.
static CONTENDED: AtomicU64 = AtomicU64::new(0);
static LONGEST: AtomicU64 = AtomicU64::new(0);

/// This CPU's mark as a holder.
fn me() -> u32 {
    percpu::current().index() + 1
}

/// Whether this CPU holds the lock.
pub fn held_here() -> bool {
    LOCK.holder() == me()
}

/// Take the lock for this CPU, which must not hold it. Interrupts must be
/// off: the hold is the CPU's, and an interrupt taken mid-acquire would
/// enter as if it held nothing.
#[track_caller]
fn acquire() {
    debug_assert!(!x86_64::instructions::interrupts::are_enabled());
    let mine = me();
    // Zero before the clock is calibrated: nothing can contend that early
    // (one CPU), so no watchdog is needed there.
    let bound = crate::arch::clock::cycles_per_tick() * 100 * WATCHDOG_SECONDS;
    let waited = LOCK.lock(mine, bound, |holder| watchdog(mine, holder));
    SITE.store(
        Location::caller() as *const Location<'static> as *mut _,
        Ordering::Relaxed,
    );
    if waited != 0 {
        CONTENDED.fetch_add(1, Ordering::Relaxed);
        LONGEST.fetch_max(waited, Ordering::Relaxed);
    }
}

/// A wait that will not end: report who holds the lock and where they took
/// it. The panic stops the other CPUs, the holder with them.
#[cold]
fn watchdog(mine: u32, holder: u32) -> ! {
    // 0 between two holders; the site may still be the previous holder's
    // for the few instructions after a new holder's mark (diagnostic only).
    if holder == 0 {
        panic!(
            "entry lock: CPU{} waited {WATCHDOG_SECONDS} s; no holder now (turns stuck)",
            mine - 1
        );
    }
    let site = SITE.load(Ordering::Relaxed);
    // SAFETY: `SITE` only ever holds null or a `&'static Location` stored
    // by `acquire`.
    match unsafe { site.as_ref() } {
        Some(site) => panic!(
            "entry lock: CPU{} waited {WATCHDOG_SECONDS} s; held by CPU{} since {}:{}",
            mine - 1,
            holder.wrapping_sub(1),
            site.file(),
            site.line()
        ),
        None => panic!(
            "entry lock: CPU{} waited {WATCHDOG_SECONDS} s; held by CPU{}",
            mine - 1,
            holder.wrapping_sub(1)
        ),
    }
}

/// Drop this CPU's hold. Interrupts go off first: the gate's code after the
/// release runs unheld until its `iretq`/`sysretq` (which restores the
/// resumed context's own `IF`), and an interrupt taken there would enter as
/// if from held kernel code.
fn release() {
    x86_64::instructions::interrupts::disable();
    SITE.store(ptr::null_mut(), Ordering::Relaxed);
    LOCK.unlock();
}

/// See the parent module.
#[track_caller]
pub fn boot_hold() {
    acquire();
}

/// See the parent module: a ring-3 entry takes the hold, a ring-0 entry
/// inherits it. `rip` (the interrupted instruction) only names the site of
/// a broken rule.
#[track_caller]
pub fn enter(cs: u64, rip: u64) {
    let user = crate::arch::fault::from_user(cs);
    match (user, held_here()) {
        (true, false) => acquire(),
        (false, true) => {}
        (true, true) => {
            panic!("entry lock: a ring-3 entry (rip {rip:#x}) found this CPU holding it")
        }
        (false, false) => panic!(
            "entry lock: a ring-0 entry (rip {rip:#x}, CPU{}) found this CPU not holding it",
            me() - 1
        ),
    }
}

/// How a fault gate entered ([`enter_fault`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FaultEntry {
    /// Under the hold, as [`enter`] would: leave with [`leave`].
    Held,
    /// A ring-0 fault on a CPU that does not hold the lock: the fault's own
    /// handler runs first (its report has the error code, `CR2` and the
    /// frame), and [`unheld_fault_resolved`] panics if it resumes.
    Unheld,
}

/// A fault gate's entry. Like [`enter`], except that a ring-0 fault on a
/// CPU without the hold does not panic here: that fault is either fatal
/// (its handler halts with the full report, which a panic here would lose)
/// or kernel code that ran unheld, which [`unheld_fault_resolved`] reports
/// once the handler returns.
#[track_caller]
pub fn enter_fault(cs: u64, rip: u64) -> FaultEntry {
    if !crate::arch::fault::from_user(cs) && !held_here() {
        return FaultEntry::Unheld;
    }
    enter(cs, rip);
    FaultEntry::Held
}

/// A fault taken without the hold was resolved: the code it resumes runs in
/// the kernel without the entry lock, which the rule forbids.
#[track_caller]
pub fn unheld_fault_resolved(rip: u64) -> ! {
    panic!(
        "entry lock: a ring-0 fault (rip {rip:#x}, CPU{}) was resolved on a CPU not holding it",
        me() - 1
    )
}

/// See the parent module: a resume of ring 3 releases the hold.
#[track_caller]
pub fn leave(cs: u64) {
    if !held_here() {
        panic!("entry lock: a gate is leaving without the hold");
    }
    if crate::arch::fault::from_user(cs) {
        release();
    }
}

/// The Linux `syscall` entry: always from ring 3 (the user RIP is in the
/// per-CPU context, not needed here).
#[track_caller]
pub fn enter_user() {
    enter(3, 0);
}

/// The Linux `syscall` return: always to ring 3.
#[track_caller]
pub fn leave_user() {
    leave(3);
}

/// Acquisitions that waited for another CPU since boot, and the longest
/// wait in TSC cycles.
#[cfg(lazyos_tests)]
pub fn contended() -> (u64, u64) {
    (
        CONTENDED.load(Ordering::Relaxed),
        LONGEST.load(Ordering::Relaxed),
    )
}

/// Tickets taken and not yet served (the holder's and every waiter's).
#[cfg(lazyos_tests)]
pub fn outstanding() -> u32 {
    LOCK.outstanding()
}

/// Test hook: take the lock as an entry from ring 3 would (this CPU must not
/// hold it; interrupts off). The suite contends for it from the APs.
#[cfg(lazyos_tests)]
#[track_caller]
pub fn acquire_for_test() {
    assert!(!held_here(), "acquire_for_test: already held here");
    acquire();
}

/// Test hook: drop this CPU's hold, as a return to ring 3 would.
#[cfg(lazyos_tests)]
pub fn release_for_test() {
    assert!(held_here(), "release_for_test: not held here");
    release();
}
