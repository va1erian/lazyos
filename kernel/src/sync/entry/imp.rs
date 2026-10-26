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
/// Long, because a waiter may be an AP's tick behind a long syscall (a
/// program image streamed from a slow disk holds the lock for seconds,
/// interrupt windows and all); a deadlock still ends in a report.
pub const WATCHDOG_SECONDS: u64 = 30;

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

/// The CPU holding the lock, if any (the NMI's `HANG:` report; lock-free).
pub fn holder() -> Option<u32> {
    LOCK.holder().checked_sub(1)
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
    report_holder(holder);
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

/// Ask the holder for its `HANG:` report (an NMI lands through any
/// interrupts-off spin) and wait until it is printed (at most 2 s) before
/// the panic stops it: the report names the loop that kept the lock, which
/// the panic's own line (where the lock was taken) cannot. Only the first
/// waiter asks (two reports at once would interleave on the raw serial
/// port); every waiter waits, so no panic writes over the report.
fn report_holder(holder: u32) {
    static BASE: AtomicU64 = AtomicU64::new(u64::MAX);
    if BASE
        .compare_exchange(
            u64::MAX,
            crate::arch::nmi::reports_done(),
            Ordering::AcqRel,
            Ordering::Relaxed,
        )
        .is_ok()
    {
        if let Some(cpu) = percpu::area(holder as usize - 1) {
            crate::arch::lapic::send_nmi(cpu.apic_id());
        }
    }
    let start = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 200;
    while crate::arch::nmi::reports_done() <= BASE.load(Ordering::Acquire)
        && rdtsc().wrapping_sub(start) < bound
    {
        core::hint::spin_loop();
    }
}

fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` only reads the time-stamp counter.
    unsafe { core::arch::x86_64::_rdtsc() }
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
        // The one ring-0 context that runs unheld: a halt in `halt`. The
        // first gate to interrupt it takes the lock for it.
        (false, false) if take_halt_mark() => acquire(),
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
    if !crate::arch::fault::from_user(cs) && !held_here() && !halting_here() {
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

/// Whether this CPU is halted in [`halt`] without the hold.
fn halting_here() -> bool {
    percpu::current().entry_halt.load(Ordering::Relaxed)
}

/// Consume this CPU's halt mark: true if it was halted in [`halt`], in
/// which case the caller takes the lock on the halt's behalf.
fn take_halt_mark() -> bool {
    percpu::current().entry_halt.swap(false, Ordering::Relaxed)
}

/// See the parent module: halt until an interrupt with the hold dropped,
/// then take it back. Interrupts must be off (they are again on return).
///
/// The mark is set before the release and the release masks interrupts, so
/// nothing can interrupt this CPU between "not holding" and "marked". The
/// `sti; hlt` pair is the only place interrupts come on, and `sti`'s shadow
/// means the first one lands at the `hlt`, never before it: an interrupt
/// gate finds the mark, takes the lock and clears the mark, and may switch
/// tasks under that hold (the switch carries it). Whoever resumes here —
/// this task after that interrupt, or after it was switched out and picked
/// again — therefore usually holds already; an NMI, which never takes the
/// lock, is the case that wakes the `hlt` with the CPU still unheld.
#[track_caller]
pub fn halt() {
    halt_while(|| false);
}

/// See the parent module: [`halt`] for the idle context, which halts again
/// (still unheld, still marked) after an interrupt that did not take the
/// lock — an IPI or an NMI — for as long as `keep_halting` says there is
/// nothing for it to do. It takes the lock only for work: a gate that took
/// it already (a tick, a kick, a device), or `keep_halting` turning false.
/// So an idle CPU that a shootdown or a test-rig IPI wakes does not queue
/// for the lock behind a long holder. `keep_halting` runs unheld with
/// interrupts off: it may read only this CPU's own area.
#[track_caller]
pub fn halt_while(keep_halting: impl Fn() -> bool) {
    debug_assert!(!x86_64::instructions::interrupts::are_enabled());
    if !held_here() {
        // Releasing would hand out a turn this CPU never had.
        panic!(
            "entry lock: a halt on CPU{} that does not hold it",
            me() - 1
        );
    }
    let cpu = percpu::current();
    cpu.entry_halt.store(true, Ordering::Relaxed);
    release();
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
        x86_64::instructions::interrupts::disable();
        if held_here() {
            break;
        }
        if !keep_halting() {
            acquire();
            break;
        }
        // Not consumed: nothing took the lock, so the mark still stands.
    }
    // Ours or a stale one an NMI left: this CPU holds now either way.
    cpu.entry_halt.store(false, Ordering::Relaxed);
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
/// wait in TSC cycles (the suite, and `PERF:entry` under `LAZYOS_PERF=1`).
#[cfg(any(lazyos_tests, lazyos_perf))]
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
