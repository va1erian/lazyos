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
//! but never below a quarter of it ([`MIN_CHARGE_DIV`]). So a task that
//! sleeps most of the time stays behind its CPU-bound peers and its wakes
//! deserve the CPU, while each of its runs still costs at least a quarter
//! quantum: per quantum of virtual time its peers consume, a waker can take
//! the CPU at most four times. That is the bound that keeps a stream of
//! same-class wakes from becoming a stream of switches: at most four wake
//! preemptions per waker per peer quantum (10 ms), measured by
//! `preempt_wake_suite::same_class` (a 5 kHz waker against two CPU hogs).
//! CPU-bound tasks are charged full strides exactly as before, so their
//! shares, and the stride scheduler's fairness bound between them, are
//! unchanged.
//!
//! # The minimum slice
//!
//! A selected task keeps the CPU against same-class wakes for at least
//! [`MIN_SLICE_NS`] (the same quarter quantum it is charged at least). A
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
//! construction. The yield is the ordinary voluntary gate, so the task
//! resumes right after it (inside its interrupt handler or at its syscall
//! return) the next time it is picked.
//!
//! There is no dedicated idle task: the task the scheduler resumes when
//! nothing is runnable halts in its own wait loop, and the interrupt that
//! wakes anyone else now leaves that loop through [`preempt_point`]. An idle
//! task would add a slot, a kernel stack and special cases in accounting and
//! introspection for no latency gain on this single CPU.

use super::*;

/// A wake made a task more deserving of the CPU than the current one.
static NEED_RESCHED: AtomicBool = AtomicBool::new(false);
/// Set by [`preempt_point`] for the yield it makes: the scheduler entry that
/// follows is a preemption, not a park or a voluntary yield.
static PREEMPTING: AtomicBool = AtomicBool::new(false);
/// The task whose last selection charge is still open (not yet refunded or
/// superseded by another selection), and when it was charged (monotonic ns).
static SELECTED: AtomicUsize = AtomicUsize::new(usize::MAX);
static SELECTED_AT: AtomicU64 = AtomicU64::new(0);
/// The end of the selected task's minimum slice (monotonic ns).
static SLICE_END: AtomicU64 = AtomicU64::new(0);
/// When a deserving same-class wake deferred by the minimum slice falls due
/// (monotonic ns; 0: none pending).
static DEFERRED: AtomicU64 = AtomicU64::new(0);
/// The least CPU time a selection guarantees against same-class wakes.
pub(super) const MIN_SLICE_NS: u64 = NS_PER_TICK / MIN_CHARGE_DIV;
/// The pending handoff: the task that will park, and the one to run then.
static HANDOFF_FROM: AtomicUsize = AtomicUsize::new(usize::MAX);
static HANDOFF_TO: AtomicUsize = AtomicUsize::new(usize::MAX);

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
    let outranks = match tasks.get(cur).and_then(|task| task.as_ref()) {
        Some(running) if running.state == TaskState::Runnable => {
            if new.class.rank() != running.class.rank() {
                new.class.rank() > running.class.rank()
            } else if pick_next_best(tasks, cur) == Some(woken) {
                // Deserving: now if `cur` had its slice, else at its end.
                if !slice_left(cur) {
                    true
                } else {
                    return defer(cur);
                }
            } else {
                false
            }
        }
        // Blocked in its wait loop, done in `exit`, or an empty slot: the CPU
        // is idle and any runnable task should have it.
        _ => true,
    };
    if outranks {
        NEED_RESCHED.store(true, Ordering::Relaxed);
    }
    false
}

/// Whether `cur` is still inside its minimum slice. A task running without a
/// selection of its own on record (it woke on an idle CPU) gets a full slice
/// from now.
fn slice_left(cur: usize) -> bool {
    SELECTED.load(Ordering::Relaxed) != cur
        || crate::arch::clock::monotonic_ns() < SLICE_END.load(Ordering::Relaxed)
}

/// Defer a deserving same-class wake to the end of `cur`'s slice, keeping the
/// earliest pending one. Returns whether the timer must be (re)armed.
fn defer(cur: usize) -> bool {
    let due = if SELECTED.load(Ordering::Relaxed) == cur {
        SLICE_END.load(Ordering::Relaxed)
    } else {
        crate::arch::clock::monotonic_ns().saturating_add(MIN_SLICE_NS)
    }
    .max(1);
    let pending = DEFERRED.load(Ordering::Relaxed);
    if pending != 0 && pending <= due {
        return false;
    }
    DEFERRED.store(due, Ordering::Relaxed);
    true
}

/// Test hook: record `slot` as selected now, with its minimum slice ending
/// at `slice_end` (monotonic ns), so a test can stage a running task inside
/// or past its slice.
#[cfg(lazyos_tests)]
pub(super) fn set_selected(slot: usize, slice_end: u64) {
    SELECTED.store(slot, Ordering::Relaxed);
    SELECTED_AT.store(crate::arch::clock::monotonic_ns(), Ordering::Relaxed);
    SLICE_END.store(slice_end, Ordering::Relaxed);
}

/// The deferred preemption's due time, for the deadline timer.
pub(super) fn deferred() -> Option<u64> {
    match DEFERRED.load(Ordering::Relaxed) {
        0 => None,
        due => Some(due),
    }
}

/// Whether a deferred preemption is due now.
fn deferred_due() -> bool {
    let due = DEFERRED.load(Ordering::Relaxed);
    due != 0 && crate::arch::clock::monotonic_ns() >= due
}

/// A selection is being made: whatever raised the flag, or was deferred, is
/// accounted for.
pub(super) fn clear() {
    NEED_RESCHED.store(false, Ordering::Relaxed);
    DEFERRED.store(0, Ordering::Relaxed);
}

/// Whether a wake asked for a reschedule that has not happened yet.
#[allow(dead_code)] // test hook
pub fn pending() -> bool {
    NEED_RESCHED.load(Ordering::Relaxed)
}

/// Give the CPU away if a wake asked for it.
///
/// Call only with interrupts off, holding no lock, at a point the timer
/// could have preempted (interrupt return, syscall return): see the module
/// docs.
pub fn preempt_point() {
    if NEED_RESCHED.load(Ordering::Relaxed) || deferred_due() {
        PREEMPTING.store(true, Ordering::Relaxed);
        switch::yield_now();
    }
}

/// Whether this scheduler entry is the yield of a [`preempt_point`]
/// (consumed: the flag covers one entry).
pub(super) fn take_preempting() -> bool {
    PREEMPTING.swap(false, Ordering::Relaxed)
}

/// `slot` was just selected and charged a quantum: start timing its use.
pub(super) fn note_selected(slot: usize) {
    let now = crate::arch::clock::monotonic_ns();
    SELECTED_AT.store(now, Ordering::Relaxed);
    SLICE_END.store(now.saturating_add(MIN_SLICE_NS), Ordering::Relaxed);
    SELECTED.store(slot, Ordering::Relaxed);
}

/// When `cur`'s open charge was made, if it has one: read it before the
/// next selection replaces it. A charge is refunded at most once.
pub(super) fn open_charge(cur: usize) -> Option<u64> {
    (SELECTED.load(Ordering::Relaxed) == cur).then(|| SELECTED_AT.load(Ordering::Relaxed))
}

/// The least share of a quantum a selection costs, as a divisor: a run is
/// charged at least `stride / MIN_CHARGE_DIV` however short it was.
pub(super) const MIN_CHARGE_DIV: u64 = 4;

/// `slot` left the CPU before its quantum ran out (a wake preempted it, or it
/// parked): give back the unused share of the stride it was charged at its
/// selection, keeping at least a quarter quantum (see the module docs).
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
    HANDOFF_FROM.store(super::current(), Ordering::Relaxed);
    HANDOFF_TO.store(to, Ordering::Relaxed);
}

/// The handoff target for this scheduler entry of `cur`, if `cur` recorded
/// one and is now parked, the target is runnable, and no runnable task of a
/// higher class would be passed over. Every entry consumes the record.
pub(super) fn take_handoff(tasks: &[Option<Task>; MAX_TASKS], cur: usize) -> Option<usize> {
    let from = HANDOFF_FROM.swap(usize::MAX, Ordering::Relaxed);
    let to = HANDOFF_TO.load(Ordering::Relaxed);
    if from != cur || to == cur || to >= MAX_TASKS {
        return None;
    }
    let parked = tasks[cur]
        .as_ref()
        .is_some_and(|task| matches!(task.state, TaskState::Blocked { .. }));
    let target = tasks[to].as_ref()?;
    if !parked || target.state != TaskState::Runnable {
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
/// once, so the next task runs now instead of at the next tick (P1.5). When
/// nothing else is runnable the scheduler resumes this task, which halts
/// until an interrupt makes someone runnable; that interrupt's preemption
/// point (the CPU is idle) or the next tick switches away. Call with
/// interrupts off and no lock held (a syscall body).
pub fn exit_cpu() -> ! {
    loop {
        switch::yield_now();
        x86_64::instructions::interrupts::enable_and_hlt();
        x86_64::instructions::interrupts::disable();
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
