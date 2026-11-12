//! `WaitFor` against signals and task death: a killed waiter, a deferred
//! handler frame, a parked task torn down with its slot reused.

use super::*;

/// Set by [`kill_the_waiter`]: what `must_unwind` reads for the waiter.
static KILLED: AtomicBool = AtomicBool::new(false);

/// What a `SIGKILL` does to a parked task: the kill is recorded, then the wake.
fn kill_the_waiter() {
    KILLED.store(true, Ordering::Release);
    task::harness::interrupt(task::harness::nap_hook_owner());
}

/// A waiter really parked in `WaitFor` is killed: the wait ends with
/// `ECANCELED` at once, and its ticket and queue entry go. (The suite's only
/// runnable task is the kernel task, which is never killed, so the "being
/// killed" test is injected; `signal::must_unwind` itself is the signal
/// suite's.)
pub fn killed_waiter_is_canceled() -> Result<(), String> {
    fresh()?;
    KILLED.store(false, Ordering::Release);
    task::harness::set_nap_hook(Some(kill_the_waiter));
    let start = task::ticks();
    let outcome = wait::wait_for_unless(
        task::KERNEL_TASK,
        "os.example.k",
        Some(task::ticks() + 500),
        || true,
        || KILLED.load(Ordering::Acquire),
    );
    task::harness::set_nap_hook(None);
    check!(
        outcome == Err(RegistryError::Canceled),
        "a killed waiter got {outcome:?}"
    );
    check!(
        task::ticks() < start + 100,
        "the kill took {} ticks to reach the wait",
        task::ticks() - start
    );
    nothing_waits()
}

/// Give `slot` a caught, pending `SIGUSR1`: what a deferred handler frame
/// stands for (`frame_deferred` is only true while something is deliverable).
pub(super) fn deliverable_signal(slot: usize) -> Result<(), String> {
    use crate::task::signal::{self, Disposition, SigInfo};
    let handler = Disposition::Handler {
        handler: 0x0040_1000,
        flags: 0,
        restorer: 0x0040_2000,
        mask: 0,
    };
    signal::set_action(slot, signal::SIGUSR1, handler).map_err(|e| format!("sigaction {e:?}"))?;
    signal::send_to_slot(
        task::KERNEL_TASK,
        slot,
        signal::SIGUSR1,
        SigInfo::user(task::KERNEL_TASK, signal::SI_USER),
    )
    .map_err(|e| format!("send {e:?}"))
}

/// A thread whose handler frame the sweep deferred is not parked by `wait`
/// (`signal::frame_deferred`); `WaitFor` must end with `EINTR` at once and
/// not loop looking for a park that never comes (interrupts are off in a
/// syscall, so a loop there never ends, whatever the deadline).
pub fn deferred_frame_returns_eintr() -> Result<(), String> {
    fresh()?;
    let child = task::spawn_fork().map_err(to_string)?;
    deliverable_signal(child)?;
    task::harness::switch_current(child);
    task::signal::harness::set_frame_deferred(child, true);
    let start = task::ticks();
    let outcome = wait::wait_for(child, "os.example.d", None, || true);
    task::signal::harness::set_frame_deferred(child, false);
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        outcome == Err(RegistryError::Interrupted),
        "a deferred frame gave {outcome:?}"
    );
    check!(
        task::ticks() <= start + 1,
        "the wait took {} ticks",
        task::ticks() - start
    );
    // The name registered meanwhile still wins over the signal.
    let (_service, callable) = endpoint()?;
    later(PLAN_NONE, 0, 0, callable)?;
    no_later();
    publish(NAMES[0]);
    task::harness::switch_current(child);
    task::signal::harness::set_frame_deferred(child, true);
    let found = wait::wait_for(child, NAMES[0], None, || true);
    task::signal::harness::set_frame_deferred(child, false);
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        found.is_ok(),
        "a registered name lost to a signal: {found:?}"
    );
    nothing_waits()?;
    task::harness::finish(child, 0);
    channels::reset();
    registry::reset();
    Ok(())
}

/// A task parked in the wait is torn down (`ipc::teardown_task`) and its slot
/// is reused: the queue and the table forget it, a later registration wakes
/// nobody, and the new occupant can wait and is not woken by the old one's
/// leftovers.
pub fn teardown_then_slot_reuse() -> Result<(), String> {
    fresh()?;
    let (_service, callable) = endpoint()?;
    later(PLAN_NONE, 0, 0, callable)?;
    no_later();
    let child = task::spawn_fork().map_err(to_string)?;
    let old = Ticket::take(child, NAMES[0]).map_err(reason)?;
    wait::park_for_test(child, None);
    check!(wait::queue_len() == 1, "the child is not on the queue");
    let table = task::pml4_of(child).ok_or("no table")?;
    crate::ipc::teardown_task(child, table, false);
    check!(
        wait::queue_len() == 0 && registry::stats().waiters == 0,
        "teardown left {} queued and {} ticketed",
        wait::queue_len(),
        registry::stats().waiters
    );
    drop(old);
    // Reuse: the lowest free slot comes back to the next task.
    task::harness::reset();
    let new = task::spawn_fork().map_err(to_string)?;
    check!(new == child, "slot {child} was not reused (got {new})");
    let ticket = Ticket::take(new, NAMES[0]).map_err(reason)?;
    check!(
        wait::waiting_slots(NAMES[0]) == [new],
        "the table names {:?}",
        wait::waiting_slots(NAMES[0])
    );
    publish(NAMES[0]);
    check!(
        task::harness::take_wake_reason(new).is_none(),
        "the new occupant was woken by the old one's wait"
    );
    drop(ticket);
    nothing_waits()?;
    channels::reset();
    registry::reset();
    Ok(())
}
