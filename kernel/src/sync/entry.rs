//! The kernel entry lock (docs/smp-plan.md S8.3; as built in
//! docs/architecture/smp.md, "The entry lock").
//!
//! **What it guarantees.** At most one CPU executes kernel code. With that,
//! every argument in the kernel of the form "interrupts are off, so nothing
//! else mutates this" keeps its meaning when several CPUs run user tasks:
//! the only new concurrency is user code against user code (separate
//! address spaces) and user code against the kernel elsewhere (the S8.2
//! shootdowns).
//!
//! **The hold belongs to the CPU, not to a task.** A CPU takes the lock when
//! it enters the kernel from ring 3 and drops it when it resumes ring 3.
//! Everything in between — a park, a preemptive switch, an interrupt taken
//! in ring 0, a kernel thread's whole run — happens under that one hold, and
//! a context switch carries it from the task that leaves to the task that
//! resumes: every switch is kernel to kernel (a task is only ever resumed
//! from inside the scheduler), so the resumed task needs the hold exactly as
//! much as the one that left. Releasing at the switch instead (the plan's
//! first wording) would let another CPU pick the leaving task while its
//! stack is still in use here.
//!
//! **The checks.** Each gate calls [`enter`] with the interrupted `CS` and
//! [`leave`] with the resumed one. A ring-3 entry must find this CPU not
//! holding the lock and takes it; a ring-0 entry must find it held (it
//! interrupted kernel code, which runs held) and inherits it; a resume of
//! ring 3 releases it. Anything else is a kernel bug that a second CPU would
//! turn into silent corruption, so it panics, naming the gate, in every
//! `LAZYOS_SMP=1` kernel.
//!
//! **Who never takes it.** The IPIs, the NMI, the spurious vector, `#DF`
//! and the interrupt-window tick (`arch::irq_window`): they are
//! window-safe — no lock but the per-CPU area — so they run whether or not
//! their CPU holds anything. An AP that only idles never holds it.
//!
//! **Halting.** Kernel code that waits for an interrupt with interrupts on
//! and holds no lock (`task::nap`, the idle task, `exit_cpu`) calls
//! [`halt`], which drops the hold around the `hlt`: holding it there would
//! keep every other CPU out of the kernel while this one sleeps. A halt is
//! the one ring-0 context that runs unheld, so it marks its CPU
//! (`Cpu::entry_halt`), and a ring-0 entry that finds the CPU unheld *and*
//! marked takes the lock instead of panicking. The rule is keyed on "does
//! this CPU hold it" and the interrupted ring together; a ring-keyed rule
//! alone has no answer for an interrupt in an idle loop.
//!
//! **Waiting.** A waiter spins with interrupts off (every gate runs with
//! them off), so it serves its own shootdown mailbox meanwhile
//! (`smp::serve_pending`, the S8.2 rule for every interrupts-off spin), and
//! a wait longer than 30 seconds panics naming the holder. The lock is a
//! ticket lock (`sync::ticket`): entries are served first come, first served.
//!
//! With `cfg(lazyos_smp)` off every function here is an empty inline: the
//! single-CPU kernel is the kernel it was.

#[cfg(lazyos_smp)]
mod imp;
/// The single-CPU kernel's entry lock: nothing to take, and every CPU (the
/// one there is) "holds" it.
#[cfg(not(lazyos_smp))]
mod imp {
    #[inline(always)]
    pub fn boot_hold() {}
    #[inline(always)]
    pub fn enter(_cs: u64, _rip: u64) {}
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum FaultEntry {
        Held,
        #[allow(dead_code)]
        Unheld,
    }
    #[inline(always)]
    pub fn enter_fault(_cs: u64, _rip: u64) -> FaultEntry {
        FaultEntry::Held
    }
    #[inline(always)]
    pub fn unheld_fault_resolved(_rip: u64) -> ! {
        unreachable!("a single-CPU kernel always holds the entry lock")
    }
    #[inline(always)]
    pub fn leave(_cs: u64) {}
    #[inline(always)]
    pub fn halt() {
        super::halt_holding();
    }
    #[inline(always)]
    pub fn halt_while(_keep_halting: impl Fn() -> bool) {
        super::halt_holding();
    }
    #[inline(always)]
    pub fn holder() -> Option<u32> {
        None
    }
    // Read by `PERF:entry`: a single-CPU kernel never waits.
    #[cfg(lazyos_perf)]
    #[inline(always)]
    pub fn contended() -> (u64, u64) {
        (0, 0)
    }
    #[inline(always)]
    pub fn enter_user() {}
    #[inline(always)]
    pub fn leave_user() {}
    // Read by the suite only.
    #[allow(dead_code)]
    #[inline(always)]
    pub fn held_here() -> bool {
        true
    }
}

/// `boot_hold`: the boot CPU takes the hold before anything can enter (the
/// boot is kernel code; the hold rides into the first task switch).
/// `enter(cs, rip)`/`leave(cs)`: a gate entered from the context whose `CS`
/// (and interrupted RIP, for a report) is given, and the context it
/// resumes. `enter_user`/`leave_user`: the Linux `syscall` pair, always
/// ring 3. `halt`: wait for an interrupt with interrupts on, without the
/// hold (see "Halting" above); called with interrupts off, returns with them
/// off and the hold taken.
pub use imp::{boot_hold, enter, enter_user, halt, halt_while, holder, leave, leave_user};
use imp::{enter_fault, unheld_fault_resolved, FaultEntry};

/// A fault gate's bracket: `handler` runs under the hold like any gate's
/// work, and `resumed_cs` names the `CS` of what it returns (the frame to
/// resume). A ring-0 fault on a CPU that does not hold the lock is the one
/// difference from [`enter`]: its handler runs first, so a fatal fault keeps
/// its own report (error code, `CR2`, the frame) instead of a bare panic,
/// and a fault that is resolved anyway panics before resuming kernel code
/// that ran without the lock.
#[track_caller]
#[inline(always)]
pub fn fault_gate<R>(
    cs: u64,
    rip: u64,
    handler: impl FnOnce() -> R,
    resumed_cs: impl FnOnce(&R) -> u64,
) -> R {
    let entry = enter_fault(cs, rip);
    let resumed = handler();
    if entry == FaultEntry::Unheld {
        unheld_fault_resolved(rip);
    }
    leave(resumed_cs(&resumed));
    resumed
}
// The suite's view (`tests/smp_entry_suite.rs`).
#[cfg(any(all(lazyos_smp, lazyos_tests), lazyos_perf))]
pub use imp::contended;
#[allow(unused_imports)]
pub use imp::held_here;
#[cfg(all(lazyos_smp, lazyos_tests))]
pub use imp::{acquire_for_test, outstanding, release_for_test};

/// Wait for an interrupt with interrupts on **keeping** the hold: for a
/// halt whose caller may hold other locks (the yielding lock's last resort,
/// `task::relax`), where dropping the entry lock could let another CPU in to
/// spin on one of them. Other CPUs wait meanwhile, so only rare fallbacks
/// use it. Called and returns with interrupts off.
pub fn halt_holding() {
    x86_64::instructions::interrupts::enable_and_hlt();
    x86_64::instructions::interrupts::disable();
}

/// Test hook for `arch::linux::probe_entry`, which runs the real `syscall`
/// entry body from ring 0: drop the harness's hold so the body's ring-3
/// take restores it.
#[cfg(lazyos_tests)]
pub fn release_for_probe() {
    #[cfg(lazyos_smp)]
    imp::release_for_test();
}
