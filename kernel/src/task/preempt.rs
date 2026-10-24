//! Reschedule on wake (docs/performance-plan.md, P1.1).
//!
//! A wake only marks a task `Runnable`; on its own it waited for the next
//! scheduler entry, which on an idle CPU is the next 100 Hz tick: the task
//! halted in its wait loop re-halts after the interrupt that woke someone
//! else. [`note_wake`] therefore raises a `need_resched` flag when the woken
//! task should run *now*:
//!
//! * the CPU is idle: the current task is blocked (halted in its wait loop)
//!   or done (halted in `exit`), so any runnable task beats it; or
//! * the woken task is in a strictly higher scheduling class than the
//!   running one, the same rule the scheduler applies at every entry; or
//! * it is in the same class and *deserves* the CPU (P6.2): the next
//!   selection would pick it (`pick_next_best`, the run queues make that
//!   cheap). Preempting makes that selection now instead of at the next tick.
//!
//! # Charging by use, and the bound
//!
//! A selection charges a whole stride (one quantum of virtual time). A task
//! that leaves the CPU early, because a wake preempted it or because it
//! parked, gets back the part of that quantum it did not use ([`refund`]),
//! but never below a tenth of it ([`MIN_CHARGE_DIV`], 1 ms of the 10 ms
//! quantum). So a task that sleeps most of the time stays behind its
//! CPU-bound peers and its wakes deserve the CPU, while each of its runs
//! still costs at least a tenth of a quantum: per quantum of virtual time its
//! peers consume, a waker can take the CPU at most ten times. Together with
//! the minimum slice below (one same-class wake preemption per millisecond
//! of CPU at most) that is the bound that keeps a stream of same-class wakes
//! from becoming a stream of switches, measured by
//! `preempt_wake_suite::same_class` (a 5 kHz waker against two CPU hogs).
//! CPU-bound tasks are charged full strides exactly as before, so their
//! shares, and the stride scheduler's fairness bound between them, are
//! unchanged.
//!
//! # The minimum slice
//!
//! A selected task keeps the CPU against same-class wakes for at least
//! [`MIN_SLICE_NS`] (the same tenth of a quantum it is charged at least). A
//! deserving wake that comes sooner is deferred ([`DEFERRED`]): the deadline
//! timer is armed for the end of the slice and the preemption happens there
//! (or at any earlier preemption point after it), unless a selection made in
//! between already settled it. This is what guarantees progress: without it
//! a task resumed with an interrupt pending (a driver's line re-asserting the
//! moment interrupts come back on) could be preempted before running a single
//! instruction, every time, while being charged for each selection (the
//! `dev_irq_prompt_storm_slow_claimant` livelock this rule fixed). It also
//! bounds same-class wake preemptions to one per [`MIN_SLICE_NS`] of CPU.
//!
//! # Direct handoff
//!
//! A synchronous Messenger call wakes its callee and parks the caller; a
//! reply wakes the caller and the callee soon parks in `recv`. [`hand_off`]
//! records that pairing, and the parking task's scheduler entry runs the
//! partner directly ([`take_handoff`]) instead of searching the run queues,
//! unless that would pass over a runnable task of a higher class. The partner
//! is charged its stride like any selection, so handoff changes who runs
//! first, never who gets the CPU over time.
//!
//! [`preempt_point`] acts on the flag. It is called where the timer could
//! have switched tasks anyway and nothing is held: on the way out of every
//! device interrupt (`arch::idt`, `arch::irq_stubs`) and of every syscall
//! (the native gate and the Linux `syscall` stub). An interrupt that is taken
//! at all interrupted code running with interrupts on, which by the #382
//! rule holds no spin lock, so yielding there is exactly as safe as the
//! timer preempting it there; a syscall return holds nothing by
//! construction. The one exception is an interrupt taken inside an interrupt
//! window (`arch::irq_window`), which stops a syscall that may hold locks:
//! [`preempt_point`] does nothing while a window is open. The yield is the ordinary voluntary gate, so the task
//! resumes right after it (inside its interrupt handler or at its syscall
//! return) the next time it is picked.
//!
//! When nothing is runnable the scheduler resumes the CPU's idle context
//! (`task::idle`, S8.3b), which halts in [`super::nap`]; the interrupt that
//! wakes anyone leaves that halt through [`preempt_point`] (the current
//! "task" is idle, so any runnable task beats it). Before S8.3b the task
//! that had just parked halted in its own wait loop instead, which kept its
//! CPU on its stack and its page table.

use super::*;

/// The per-CPU preemption state (`arch::percpu::PreemptState`): a shorthand
/// so the bodies below read as before.
macro_rules! state {
    () => {
        &crate::arch::percpu::current().preempt
    };
}

/// The least CPU time a selection guarantees against same-class wakes.
pub(super) const MIN_SLICE_NS: u64 = NS_PER_TICK / MIN_CHARGE_DIV;

/// Record that `woken` became runnable while `cur` is on the CPU, raising the
/// flag if it should run before the next tick. Called with the task table
/// held (from `wake_task_with`). Returns whether it deferred a preemption to
/// the end of `cur`'s minimum slice, which the caller must arm the deadline
/// timer for.
pub(super) fn note_wake(tasks: &[Option<Task>; MAX_TASKS], woken: usize, cur: usize) -> bool {
    if woken == cur {
        // The current task woke itself (an interrupt stopped its halt): it
        // simply continues when the handler returns.
        return false;
    }
    let Some(new) = tasks.get(woken).and_then(|task| task.as_ref()) else {
        return false;
    };
    let mut deferred = false;
    let outranks = super::kick::may_run_here(woken)
        && match tasks.get(cur).and_then(|task| task.as_ref()) {
            Some(running) if running.state == TaskState::Runnable => {
                if new.class.rank() != running.class.rank() {
                    new.class.rank() > running.class.rank()
                } else if pick_next_best(tasks, cur) == Some(woken) {
                    // Deserving: now if `cur` had its slice, else at its end.
                    if !slice_left(cur) {
                        true
                    } else {
                        // Another CPU may run it now, else at the end of
                        // `cur`'s slice here. Both: the placement is made at
                        // this CPU's next preemption point, when the CPU it
                        // found may be busy, and a deferral that finds the
                        // task gone costs one scheduler entry.
                        super::kick::place(tasks, woken);
                        deferred = defer(cur);
                        return deferred;
                    }
                } else {
                    false
                }
            }
            // Blocked in its wait loop, done in `exit`, or an empty slot:
            // the CPU is idle and any runnable task should have it.
            _ => true,
        };
    if outranks {
        state!().need_resched.store(true, Ordering::Relaxed);
    } else {
        // Not now, not here: another CPU may run it at once (S8.3c).
        super::kick::place(tasks, woken);
    }
    deferred
}

/// Whether `cur` is still inside its minimum slice. A task running without a
/// selection of its own on record (it woke on an idle CPU) gets a full slice
/// from now.
fn slice_left(cur: usize) -> bool {
    let state = state!();
    state.selected.load(Ordering::Relaxed) as usize != cur
        || crate::arch::clock::monotonic_ns() < state.slice_end.load(Ordering::Relaxed)
}

/// Defer a deserving same-class wake to the end of `cur`'s slice, keeping the
/// earliest pending one. Returns whether the timer must be (re)armed.
fn defer(cur: usize) -> bool {
    let state = state!();
    let due = if state.selected.load(Ordering::Relaxed) as usize == cur {
        state.slice_end.load(Ordering::Relaxed)
    } else {
        crate::arch::clock::monotonic_ns().saturating_add(MIN_SLICE_NS)
    }
    .max(1);
    let pending = state.deferred.load(Ordering::Relaxed);
    if pending != 0 && pending <= due {
        return false;
    }
    state.deferred.store(due, Ordering::Relaxed);
    true
}

/// A selection is being made: whatever raised the flag, or was deferred, is
/// accounted for.
pub(super) fn clear() {
    let state = state!();
    state.need_resched.store(false, Ordering::Relaxed);
    state.deferred.store(0, Ordering::Relaxed);
}

/// Whether a wake asked for a reschedule that has not happened yet.
#[allow(dead_code)] // test hook
pub fn pending() -> bool {
    state!().need_resched.load(Ordering::Relaxed)
}

/// Test hook: record `slot` as selected now, with its minimum slice ending
/// at `slice_end` (monotonic ns), so a test can stage a running task inside
/// or past its slice.
#[cfg(lazyos_tests)]
pub(crate) fn set_selected(slot: usize, slice_end: u64) {
    let now = crate::arch::clock::monotonic_ns();
    let state = state!();
    state.selected.store(slot as u64, Ordering::Relaxed);
    state.selected_at.store(now, Ordering::Relaxed);
    state.slice_end.store(slice_end, Ordering::Relaxed);
}

/// The deferred preemption's due time, for the deadline timer.
pub(super) fn deferred() -> Option<u64> {
    match state!().deferred.load(Ordering::Relaxed) {
        0 => None,
        due => Some(due),
    }
}

/// Whether a deferred preemption is due now.
fn deferred_due() -> bool {
    let due = state!().deferred.load(Ordering::Relaxed);
    due != 0 && crate::arch::clock::monotonic_ns() >= due
}

/// Give the CPU away if a wake asked for it.
///
/// Call only with interrupts off, holding no lock, at a point the timer
/// could have preempted (interrupt return, syscall return): see the module
/// docs.
pub fn preempt_point() {
    // An interrupt taken inside an interrupt window (`arch::irq_window`)
    // stopped a syscall that may hold any lock: switching there could hand
    // the CPU to a task that spins on one forever. The syscall's own return
    // is the next preemption point.
    if crate::arch::irq_window::is_open() {
        return;
    }
    // What this CPU woke and did not take goes elsewhere now (`task::kick`).
    super::kick::flush_unlocked();
    // An interrupt that stopped the idle context never switches from inside
    // its handler: the idle loop does, right after the handler returns
    // (`switch_if_wanted`). The `iretq` back to the halt turns interrupts on
    // for an instant, and that lets in a pending interrupt of a lower
    // priority first. Under the I/O APIC the tick (vector 0x20) ranks below
    // every device line, so a device that re-asserts the moment its driver
    // acks would otherwise starve the clock: each of its interrupts would
    // switch to the driver with interrupts off, and the tick would never be
    // taken (`dev_irq_prompt_storm_slow_claimant`).
    if super::idle::is_idle(super::current()) {
        return;
    }
    switch_if_wanted();
}

/// The switch [`preempt_point`] makes, without its checks: the idle loop's
/// own preemption point (`task::idle`), where no interrupt window is open.
pub(super) fn switch_if_wanted() {
    let state = state!();
    if state.need_resched.load(Ordering::Relaxed) || deferred_due() {
        state.preempting.store(true, Ordering::Relaxed);
        switch::yield_now();
    }
}

/// Whether this scheduler entry is the yield of a [`preempt_point`]
/// (consumed: the flag covers one entry).
pub(super) fn take_preempting() -> bool {
    state!().preempting.swap(false, Ordering::Relaxed)
}

/// `slot` was just selected and charged a quantum: start timing its use.
pub(super) fn note_selected(slot: usize) {
    let now = crate::arch::clock::monotonic_ns();
    let state = state!();
    state.selected_at.store(now, Ordering::Relaxed);
    state
        .slice_end
        .store(now.saturating_add(MIN_SLICE_NS), Ordering::Relaxed);
    state.selected.store(slot as u64, Ordering::Relaxed);
}

/// When `cur`'s open charge was made, if it has one: read it before the
/// next selection replaces it. A charge is refunded at most once.
pub(super) fn open_charge(cur: usize) -> Option<u64> {
    let state = state!();
    (state.selected.load(Ordering::Relaxed) as usize == cur)
        .then(|| state.selected_at.load(Ordering::Relaxed))
}

/// The least share of a quantum a selection costs, as a divisor: a run is
/// charged at least `stride / MIN_CHARGE_DIV` however short it was.
pub(super) const MIN_CHARGE_DIV: u64 = 10;

/// `slot` left the CPU before its quantum ran out (a wake preempted it, or it
/// parked): give back the unused share of the stride it was charged at its
/// selection, keeping at least a tenth of a quantum (see the module docs).
/// `charged_at` is the slot's [`open_charge`].
pub(super) fn refund(tasks: &mut [Option<Task>; MAX_TASKS], slot: usize, charged_at: u64) {
    let used = crate::arch::clock::monotonic_ns()
        .saturating_sub(charged_at)
        .clamp(NS_PER_TICK / MIN_CHARGE_DIV, NS_PER_TICK);
    if let Some(task) = tasks[slot].as_mut() {
        let unused = u128::from(stride(task.weight)) * u128::from(NS_PER_TICK - used)
            / u128::from(NS_PER_TICK);
        task.pass = task.pass.saturating_sub(unused as u64);
    }
}

/// Run `to` as soon as the current task parks (see the module docs). The
/// Messenger fabric calls this when a call wakes its callee and when a reply
/// wakes its caller.
pub fn hand_off(to: usize) {
    let state = state!();
    state
        .handoff_from
        .store(super::current() as u64, Ordering::Relaxed);
    state.handoff_to.store(to as u64, Ordering::Relaxed);
}

/// The handoff target for this scheduler entry of `cur`, if `cur` recorded
/// one and is now parked, the target is runnable, and no runnable task of a
/// higher class would be passed over. Every entry consumes the record.
pub(super) fn take_handoff(tasks: &[Option<Task>; MAX_TASKS], cur: usize) -> Option<usize> {
    let state = state!();
    let from = state.handoff_from.swap(u64::MAX, Ordering::Relaxed);
    let to = state.handoff_to.load(Ordering::Relaxed);
    if from != cur as u64 || to == cur as u64 || to as usize >= MAX_TASKS {
        return None;
    }
    let to = to as usize;
    let parked = tasks[cur]
        .as_ref()
        .is_some_and(|task| matches!(task.state, TaskState::Blocked { .. }));
    let target = tasks[to].as_ref()?;
    if !parked || target.state != TaskState::Runnable || super::oncpu::elsewhere(to) {
        return None;
    }
    let rank = target.class.rank() as usize;
    let higher = (rank + 1..PriorityClass::ALL.len()).any(|above| {
        runq::runnable(above)
            .iter()
            .any(|slot| runq::check(tasks, slot, above))
    });
    (!higher).then_some(to)
}

/// Leave the CPU for good: the current task is finished (`Done`). Yields at
/// once, so the next task runs now instead of at the next tick (P1.5); with
/// nothing else runnable the CPU's idle context runs (`task::idle`), and a
/// finished task is never selected again. Only a CPU without an idle context
/// (`sched::fallback`) resumes this task, which then halts until an
/// interrupt makes someone runnable. Call with interrupts off and no lock
/// held (a syscall body).
pub fn exit_cpu() -> ! {
    // The finished syscall's interrupts-off stretch ends here, for good
    // (`arch::irqoff`): no later switch back may reopen one.
    crate::arch::irqoff::exit();
    loop {
        switch::yield_now();
        // The halt below runs with interrupts on: it ends the syscall's
        // interrupts-off stretch (the latency hooks would otherwise count the
        // idle time of a CPU with nothing else to run as interrupts-off).
        crate::perf::irqoff_pause();
        crate::sync::entry::halt();
    }
}

/// The Linux `syscall` stub's return hook, called after `linux_dispatch` with
/// the result saved on the task's kernel stack: deliver device interrupts
/// raised meanwhile (as the native gate does on entry), then let a task
/// either woke run.
#[no_mangle]
extern "C" fn linux_syscall_return() {
    crate::dev::intx::service();
    preempt_point();
    // Back to ring 3: drop the entry lock `linux_dispatch_gate` took.
    crate::sync::entry::leave_user();
}

// ---------------------------------------------------------------------------
// Where an interrupt landed (P1.2)
// ---------------------------------------------------------------------------

/// Per task: set while it is inside [`super::nap`] with interrupts on, from
/// just before the halt to just after interrupts are masked again. Per task,
/// not per CPU: a task switched away mid-nap (by the tick, or by an
/// interrupt's preemption point) resumes in the same window, and an
/// interrupt pending at that moment arrives the instant it is resumed with
/// interrupts on, before its `cli`. That window holds no lock either.
static NAPPING: [AtomicBool; MAX_TASKS] = [const { AtomicBool::new(false) }; MAX_TASKS];

/// [`super::nap`] is about to halt with interrupts on.
pub(super) fn nap_begin() {
    if let Some(flag) = NAPPING.get(super::current()) {
        flag.store(true, Ordering::Relaxed);
    }
}

/// [`super::nap`] masked interrupts again.
pub(super) fn nap_end() {
    if let Some(flag) = NAPPING.get(super::current()) {
        flag.store(false, Ordering::Relaxed);
    }
}

/// Whether the interrupt being handled stopped code that holds no kernel
/// lock: user code (`code_segment` is ring 3) or the current task inside
/// `nap`. Call with interrupts off, at the start of an interrupt handler or
/// a scheduler entry (before anything switches tasks).
///
/// This is the precondition for running the device bottom half
/// (`dev::intx::service`, which takes the claim, device-table and channel
/// locks and allocates) in interrupt context. Any other ring-0 code running
/// with interrupts on — the kernel task's loop above all — might hold one of
/// those locks, and the bottom half would then spin on it forever; such an
/// interrupt leaves the work to the next syscall, tick or mux pass.
pub fn interrupted_quiet_context(code_segment: u64) -> bool {
    let napping = NAPPING
        .get(super::current())
        .is_some_and(|flag| flag.load(Ordering::Relaxed));
    napping || crate::arch::fault::from_user(code_segment)
}
