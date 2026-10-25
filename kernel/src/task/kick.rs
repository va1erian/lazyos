//! Placing a task on another CPU (docs/smp-plan.md S8.3c).
//!
//! The run queue is one, and every CPU picks from it at each of its
//! scheduler entries; what this adds is promptness. A task that became
//! runnable and that the local CPU will not run now (a wake that does not
//! preempt here, the task a local preemption put back, a new task) goes to
//! another CPU instead of waiting for that CPU's next tick: an idle one
//! first, else the one running the lowest class below the task's. That
//! CPU's `need_resched` is set and it is sent the kick (`arch::smp::kick`),
//! whose gate is a preemption point. A CPU already asked is not asked again.
//!
//! **Placement waits for this CPU's next preemption point.** [`place`] only
//! remembers the task (a few per CPU); [`flush`] kicks for the ones still
//! runnable and current nowhere when this CPU leaves the kernel or switches:
//! a syscall, interrupt or ring-3 fault return, a scheduler entry, a nap,
//! the idle loop. A
//! Messenger call wakes its callee and then parks the caller, whose
//! scheduler entry runs the callee here directly (`preempt::hand_off`); a
//! kick sent at the wake would only have brought another CPU to queue for
//! the entry lock and find nothing (the latency harness saw the round trip
//! double and the lock waited on 200000 times). Nothing is lost by waiting:
//! a kicked CPU needs the entry lock this CPU holds until that point anyway.
//!
//! **A syscall's return gives a Messenger partner a grace.** A server's
//! `REPLY` wakes its caller and returns to ring 3, and only its next
//! syscall, `RECV`, parks; a client's one-way sends wake the server and its
//! closing call parks. Kicked at the first syscall's return, the woken task
//! ran on another CPU while its partner, about to park, still had work in
//! the kernel: every round trip then crossed CPUs, took turns at the entry
//! lock and woke a halted CPU (Messenger throughput fell from 526k to 326k
//! messages/s at four CPUs). So when everything remembered is a partner
//! wake ([`partner_wakes`], the Messenger fabric's) of a task this CPU may
//! run (never the kernel task on an AP), a syscall return
//! ([`flush_after_syscall`]) keeps it for [`GRACE_NS`] after the first of
//! it: a partner that parks within it runs the woken task here, through the
//! hand-off or the run queue, and the placement finds it current and kicks
//! nobody. Any other wake (a futex, a pipe, a new task), and every other
//! preemption point, places at once, and so does the next syscall return
//! after the grace or this CPU's next tick, which bounds the wait of a task
//! whose waker went on computing in ring 3 (or any idle CPU's tick, which
//! finds the task in the run queue). So a kept window this CPU closes
//! (switching, or placing what it kept) more than [`MISS_NS`] after it
//! began is a miss, wherever the task then ran: its waker (a server that
//! replies and then works in ring 3, a server with a backlog, a compositor
//! drawing after its answer) gets no grace for a while, and its wakes are
//! placed at once, as before the grace. The penalty starts at
//! [`PENALTY_MIN_NS`] and doubles with each miss in a row up to
//! [`PENALTY_MAX_NS`]; a window closed in time ends the streak. So one
//! stall (a vCPU the host preempted) costs a partner pair the grace for a
//! moment, and a waker that never parks in time is denied nearly always:
//! once the streak is long, one wake in a second waits past the bound.
//! Only a task's own sends, calls and replies are
//! partner wakes: the kernel's posts of a device interrupt, a channel's
//! teardown and a call's timeout are placed at once.
//!
//! The kernel task (slot 0, the multiplexer and, in test kernels, the
//! suite itself) runs on the boot CPU only: it runs on the boot stack, the
//! device interrupts it serves land there, and the suite's cases assume
//! their CPU does not change under them.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

use super::{idle, oncpu, Task, KERNEL_TASK, MAX_TASKS, TASKS};
use crate::arch::percpu::{self, MAX_CPUS};

/// Per CPU: up to four slots to place at its next preemption point, 16 bits
/// each, [`EMPTY`] where free.
static PENDING: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];
/// A free lane of [`PENDING`].
const EMPTY: u64 = 0xFFFF;
/// Per CPU: when (monotonic ns) its [`PENDING`] word last went from empty
/// to holding a task, which starts [`GRACE_NS`].
static SINCE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// How long a syscall return keeps its placements for a partner to park
/// (see the module docs): several Messenger round trips, a fraction of the
/// minimum slice.
pub const GRACE_NS: u64 = 50_000;
/// Per CPU: whether every task in [`PENDING`] came from a partner wake (the
/// grace applies), and whether wakes made now are ([`partner_wakes`]).
static GRACED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static PARTNER: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// Per CPU: the task whose syscall return kept [`PENDING`] for the grace
/// (`u64::MAX`: none), whom a miss is held against.
static KEEPER: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];
/// Per task: until when (monotonic ns) its returns get no grace, and how
/// many of its kept windows in a row were misses.
static DENIED_UNTIL: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
static STREAK: [AtomicU8; MAX_TASKS] = [const { AtomicU8::new(0) }; MAX_TASKS];
/// A kept window closed this long after it began is a miss: its waker did
/// not park in the grace, and what it kept waited for a later entry (a
/// tick) or for the waker's next call.
pub const MISS_NS: u64 = 4 * GRACE_NS;
/// The first miss's penalty, and the longest a streak of them reaches.
pub const PENALTY_MIN_NS: u64 = 20_000_000;
pub const PENALTY_MAX_NS: u64 = 1_000_000_000;
/// Kicks sent, and syscall returns that kept their placements for the
/// grace (`PERF:entry`, `LAZYOS_PERF=1` only).
#[cfg(lazyos_perf)]
static KICKS: AtomicU64 = AtomicU64::new(0);
#[cfg(lazyos_perf)]
static KEPT: AtomicU64 = AtomicU64::new(0);
#[cfg(lazyos_perf)]
static MISSES: AtomicU64 = AtomicU64::new(0);

/// Kicks sent, placements kept for the grace and graces missed since boot
/// (`PERF:entry`).
#[cfg(lazyos_perf)]
pub fn counts() -> (u64, u64, u64) {
    (
        KICKS.load(Ordering::Relaxed),
        KEPT.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
    )
}

/// This CPU's area index.
fn me() -> usize {
    percpu::current().index() as usize
}

/// Whether CPU `cpu` may run `slot` at all.
fn may_run(slot: usize, cpu: usize) -> bool {
    (slot != KERNEL_TASK || cpu == 0) && !(cpu != 0 && idle::paused())
}

/// Whether this CPU may run `slot` (selection and the local wake rule ask).
pub(super) fn may_run_here(slot: usize) -> bool {
    may_run(slot, me())
}

/// Find `woken` a CPU (see the module docs) and remember it for this CPU's
/// next preemption point. Call with the task table held. Returns whether a
/// CPU would take it now.
pub(super) fn place(tasks: &[Option<Task>; MAX_TASKS], woken: usize) -> bool {
    if !oncpu::several() || choose(tasks, woken).is_none() {
        return false;
    }
    if !remember(woken) {
        // No lane free: place it now rather than drop it.
        if let Some(cpu) = choose(tasks, woken) {
            kick(cpu);
        }
    }
    true
}

/// The CPU that should take `woken` now, if any. Call with the table held.
fn choose(tasks: &[Option<Task>; MAX_TASKS], woken: usize) -> Option<usize> {
    let me = me();
    if let Some(cpu) = oncpu::cpu_of(woken) {
        // Current somewhere already (napping there): that CPU looks again.
        return (cpu != me).then_some(cpu);
    }
    let (rank, pass) = tasks
        .get(woken)
        .and_then(|task| task.as_ref())
        .map(|task| (task.class.rank(), task.pass))?;
    // The kernel task can only go to the boot CPU: ask it unless it runs
    // something of a higher class (the same class competes by stride there,
    // as a local wake would).
    let pinned = woken == KERNEL_TASK;
    let mut lowest: Option<(u8, usize)> = None;
    for cpu in 0..idle::scheduling_end() {
        if cpu == me || !idle::scheduling(cpu) || !may_run(woken, cpu) {
            continue;
        }
        let Some(area) = percpu::area(cpu) else {
            continue;
        };
        if area.preempt.need_resched.load(Ordering::Relaxed) {
            continue;
        }
        let running = area.current.load(Ordering::Relaxed) as usize;
        if idle::is_idle(running) {
            return Some(cpu);
        }
        let theirs = tasks
            .get(running)
            .and_then(|task| task.as_ref())
            .map(|task| (task.class.rank(), task.pass));
        if let Some((theirs, their_pass)) = theirs {
            // Same class: only where the woken task would win the pick (its
            // pass is behind), or the kick would re-select the running task
            // and charge it a quantum for nothing.
            let below = theirs < rank || (pinned && theirs == rank && pass < their_pass);
            if below && lowest.is_none_or(|(low, _)| theirs < low) {
                lowest = Some((theirs, cpu));
            }
        }
    }
    lowest.map(|(_, cpu)| cpu)
}

/// Remember `slot` in a free lane of this CPU's [`PENDING`]; false when all
/// four are taken. Only this CPU touches its word (with interrupts off).
fn remember(slot: usize) -> bool {
    let cpu = me();
    let pending = &PENDING[cpu];
    let word = pending.load(Ordering::Relaxed);
    // A task this CPU may not run gains nothing from waiting for it.
    let partner = PARTNER[cpu].load(Ordering::Relaxed) && may_run(slot, cpu);
    if word == u64::MAX {
        SINCE[cpu].store(crate::arch::clock::monotonic_ns(), Ordering::Relaxed);
        GRACED[cpu].store(partner, Ordering::Relaxed);
    } else if !partner {
        GRACED[cpu].store(false, Ordering::Relaxed);
    }
    for lane in 0..4 {
        let shift = lane * 16;
        let held = (word >> shift) & 0xFFFF;
        if held == slot as u64 {
            return true;
        }
        if held == EMPTY {
            let word = (word & !(0xFFFF << shift)) | ((slot as u64) << shift);
            pending.store(word, Ordering::Relaxed);
            return true;
        }
    }
    false
}

/// Kick for every remembered task that is still runnable and current
/// nowhere (one that this CPU switched to, or that another CPU took, needs
/// nothing). Call with the task table held.
pub(super) fn flush(tasks: &[Option<Task>; MAX_TASKS]) {
    let cpu = me();
    let word = PENDING[cpu].swap(u64::MAX, Ordering::Relaxed);
    let keeper = KEEPER[cpu].swap(u64::MAX, Ordering::Relaxed);
    if word == u64::MAX {
        return;
    }
    for lane in 0..4 {
        let slot = ((word >> (lane * 16)) & 0xFFFF) as usize;
        if slot as u64 == EMPTY || oncpu::cpu_of(slot).is_some() {
            continue;
        }
        if !super::sched::runnable(tasks, slot) {
            continue;
        }
        if let Some(target) = choose(tasks, slot) {
            kick(target);
        }
    }
    if keeper != u64::MAX {
        window_closed(keeper as usize, SINCE[cpu].load(Ordering::Relaxed));
    }
}

/// A grace kept by `keeper` ends: this flush runs when its CPU switches
/// (to the kept task or another) or places what it kept, so if the window
/// begun at `since` lasted past [`MISS_NS`] the kept task waited that long
/// (here, or until another CPU's tick took it): a miss, and `keeper` gets
/// no grace for a penalty that doubles with each miss in a row. A window
/// closed in time ends the streak.
fn window_closed(keeper: usize, since: u64) {
    let (Some(until), Some(streak)) = (DENIED_UNTIL.get(keeper), STREAK.get(keeper)) else {
        return;
    };
    let now = crate::arch::clock::monotonic_ns();
    if now.saturating_sub(since) <= MISS_NS {
        streak.store(0, Ordering::Relaxed);
        return;
    }
    let misses = streak.load(Ordering::Relaxed).saturating_add(1).min(16);
    streak.store(misses, Ordering::Relaxed);
    let penalty = PENALTY_MIN_NS
        .saturating_mul(1 << (misses - 1))
        .min(PENALTY_MAX_NS);
    until.store(now.saturating_add(penalty), Ordering::Relaxed);
    #[cfg(lazyos_perf)]
    MISSES.fetch_add(1, Ordering::Relaxed);
}

/// [`flush`] from a preemption point: interrupts off, no lock held. Takes
/// the table only when something is remembered, and never when it is held
/// (then the entries wait for the next point rather than spin on it).
pub(crate) fn flush_unlocked() {
    if PENDING[me()].load(Ordering::Relaxed) == u64::MAX || TASKS.is_locked() {
        return;
    }
    flush(&TASKS.lock());
}

/// [`flush_unlocked`] from a syscall's return: when only partner wakes are
/// remembered, keep them for [`GRACE_NS`] after the first, for a partner
/// about to park (see the module docs).
pub(crate) fn flush_after_syscall() {
    let cpu = me();
    if PENDING[cpu].load(Ordering::Relaxed) == u64::MAX {
        return;
    }
    if GRACED[cpu].load(Ordering::Relaxed) {
        let now = crate::arch::clock::monotonic_ns();
        let young = now.saturating_sub(SINCE[cpu].load(Ordering::Relaxed)) < GRACE_NS;
        let waker = super::current();
        let allowed = || {
            DENIED_UNTIL
                .get(waker)
                .is_some_and(|until| now >= until.load(Ordering::Relaxed))
        };
        if young && allowed() {
            keep(cpu, waker);
            return;
        }
    }
    flush_unlocked();
}

/// A syscall return of `waker` keeps this CPU's placements for the grace.
fn keep(cpu: usize, waker: usize) {
    KEEPER[cpu].store(waker as u64, Ordering::Relaxed);
    #[cfg(lazyos_perf)]
    KEPT.fetch_add(1, Ordering::Relaxed);
}

/// Run `wake` with the wakes it makes counted as a partner's: the Messenger
/// fabric's, whose waker usually parks in its next call (see the module
/// docs). Interrupts stay off throughout (they already are in a syscall
/// body), so no interrupt's wake is counted as a partner's and the task
/// cannot move to another CPU between setting the mark and clearing it.
pub(crate) fn partner_wakes<R>(wake: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mark = &PARTNER[me()];
        let outer = mark.swap(true, Ordering::Relaxed);
        let result = wake();
        mark.store(outer, Ordering::Relaxed);
        result
    })
}

/// A new task in `slot` owes nothing for its last owner's misses.
pub(super) fn forget(slot: usize) {
    if let (Some(until), Some(streak)) = (DENIED_UNTIL.get(slot), STREAK.get(slot)) {
        until.store(0, Ordering::Relaxed);
        streak.store(0, Ordering::Relaxed);
    }
}

/// Whether this CPU remembers a task to place (the suite's view of the
/// grace).
#[cfg(lazyos_tests)]
pub fn remembered_here() -> bool {
    PENDING[me()].load(Ordering::Relaxed) != u64::MAX
}

/// Lift a miss's penalty and streak from `slot` (the suite, between
/// rounds).
#[cfg(lazyos_tests)]
pub fn forgive(slot: usize) {
    forget(slot);
}

/// Lift `slot`'s denial but keep its streak (the suite: the next window is
/// kept, and a miss there extends the streak).
#[cfg(lazyos_tests)]
pub fn lift_denial(slot: usize) {
    if let Some(until) = DENIED_UNTIL.get(slot) {
        until.store(0, Ordering::Relaxed);
    }
}

/// How long `slot` is denied the grace from now, in ns (the suite).
#[cfg(lazyos_tests)]
pub fn denied_for(slot: usize) -> u64 {
    DENIED_UNTIL.get(slot).map_or(0, |until| {
        until
            .load(Ordering::Relaxed)
            .saturating_sub(crate::arch::clock::monotonic_ns())
    })
}

/// `slot`'s state changed under it (it finished, or a signal waits for it)
/// while it may be current on another CPU: that CPU looks again now (it
/// must switch away from a finished task before it can be reaped, and its
/// own sweep delivers the signal).
pub(super) fn nudge(slot: usize) {
    if let Some(cpu) = oncpu::cpu_of(slot) {
        if cpu != me() {
            kick(cpu);
        }
    }
}

/// Ask CPU `cpu` to reschedule now. Only a scheduling CPU: the kick's gate
/// takes the entry lock, which an AP still idling in `ap_entry` (holding
/// nothing, unmarked) must never be entered by.
fn kick(cpu: usize) {
    if !idle::scheduling(cpu) {
        return;
    }
    if let Some(area) = percpu::area(cpu) {
        #[cfg(lazyos_perf)]
        KICKS.fetch_add(1, Ordering::Relaxed);
        area.preempt.need_resched.store(true, Ordering::Relaxed);
        crate::arch::smp::kick(cpu);
    }
}
