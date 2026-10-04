//! The child-exit doorbell (`task::childbell`, `WAIT_CHILD`,
//! docs/performance-plan.md P7.1): a supervisor parks on its endpoint and its
//! children's exits at once.
//!
//! The waiting thread of the wait-set rig is given children (kernel threads
//! re-parented to it); finishing one must wake its wait with `CHILD_READY`,
//! an unreaped finished child must satisfy the next wait at once
//! (level-triggered), and a reaped one must not.

use core::sync::atomic::Ordering;

use super::{rig, Rig, COUNT, GATE, LAST, RAW, WAITS};
use crate::ipc::channels::{self, CHILD_READY};
use crate::task::childbell;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind};
use crate::tests::*;

/// Children park here until they are finished.
static NURSERY: WaitQueue = WaitQueue::new(WaitKind::Sleep);

extern "C" fn child() -> ! {
    let me = task::current();
    loop {
        NURSERY.wait(me, None);
    }
}

/// A kernel thread that is a child of the rig's waiting thread.
fn adopt(rig: &Rig) -> Result<usize, String> {
    let slot = task::kthread::spawn_kernel_thread("waitchild", child, PriorityClass::Normal)
        .map_err(|e| format!("spawn child: {e}"))?;
    task::harness::set_parent(slot, rig.thread);
    Ok(slot)
}

/// Reap one finished child as the waiting thread would.
fn reap_as(rig: &Rig) -> Option<(usize, u64)> {
    task::harness::switch_current(rig.thread);
    let reaped = task::reap_child();
    task::harness::switch_current(task::KERNEL_TASK);
    reaped
}

/// Start a wait with no ready source; it must park with the bell armed.
fn park(rig: &Rig, label: &str) -> Result<u64, String> {
    let before = WAITS.load(Ordering::Relaxed);
    rig.start_wait()?;
    check!(
        WAITS.load(Ordering::Relaxed) == before,
        "{label}: the wait returned {:#x} at once",
        LAST.load(Ordering::Relaxed)
    );
    check!(
        childbell::armed(rig.thread),
        "{label}: the parked task did not arm the bell"
    );
    Ok(before)
}

/// One exit wakes the wait (alone and beside an endpoint); an unreaped child
/// keeps the next wait ready; after the reap the wait parks again and an
/// endpoint message still wakes it; the bell is disarmed after every wait.
pub fn child_doorbell() -> Result<(), String> {
    let rig = rig(1)?;
    RAW.store(channels::WAIT_CHILD, Ordering::Relaxed);
    for (count, label) in [(0usize, "bell only"), (1, "bell and an endpoint")] {
        COUNT.store(count, Ordering::Relaxed);
        let slot = adopt(&rig)?;
        let before = park(&rig, label)?;
        task::harness::finish(slot, 7);
        let mask = rig.finish_wait(before)?;
        check!(mask == CHILD_READY, "{label}: an exit gave {mask:#x}");
        check!(
            !childbell::armed(rig.thread),
            "{label}: the bell stayed armed"
        );
        // Not reaped yet: the next wait is ready without parking.
        let before = WAITS.load(Ordering::Relaxed);
        GATE.notify_one();
        task::preempt_point();
        check!(
            WAITS.load(Ordering::Relaxed) == before + 1
                && LAST.load(Ordering::Relaxed) == CHILD_READY,
            "{label}: an unreaped child did not satisfy the next wait"
        );
        check!(
            reap_as(&rig) == Some((slot, 7)),
            "{label}: the finished child was not reapable"
        );
        if count == 1 {
            // Reaped: the wait parks again, and a message still wakes it.
            let before = park(&rig, label)?;
            channels::send(rig.senders[0], &super::parcel_bytes()?)
                .map_err(|e| format!("send: {e:?}"))?;
            let mask = rig.finish_wait(before)?;
            check!(mask == 1, "{label}: a message gave {mask:#x}");
            check!(
                !childbell::armed(rig.thread),
                "{label}: the bell stayed armed after a message"
            );
            task::harness::switch_current(rig.thread);
            let taken = channels::try_recv(super::HANDLES[0].load(Ordering::Relaxed));
            task::harness::switch_current(task::KERNEL_TASK);
            check!(
                matches!(taken, Ok(Some(_))),
                "{label}: the message was not there"
            );
        }
    }
    // A child of someone else does not ring this task's bell.
    COUNT.store(0, Ordering::Relaxed);
    let stranger = task::kthread::spawn_kernel_thread("stranger", child, PriorityClass::Normal)
        .map_err(|e| format!("spawn: {e}"))?;
    let before = park(&rig, "stranger")?;
    task::harness::finish(stranger, 0);
    task::preempt_point();
    check!(
        WAITS.load(Ordering::Relaxed) == before && childbell::armed(rig.thread),
        "another task's child woke the wait"
    );
    let slot = adopt(&rig)?;
    task::harness::finish(slot, 0);
    let mask = rig.finish_wait(before)?;
    check!(
        mask == CHILD_READY,
        "the own child after a stranger gave {mask:#x}"
    );
    check!(reap_as(&rig).is_some(), "the own child was not reapable");
    while task::reap_child().is_some() {}
    NURSERY.notify_all();
    rig.teardown()
}

/// Many generations of spawn, exit, wake and reap: every exit wakes exactly
/// one wait, no registration or slot is left behind.
pub fn child_soak() -> Result<(), String> {
    const ROUNDS: usize = 300;
    let rig = rig(1)?;
    RAW.store(channels::WAIT_CHILD, Ordering::Relaxed);
    COUNT.store(1, Ordering::Relaxed);
    let slots_before = (1..task::MAX_TASKS)
        .filter(|slot| task::harness::state(*slot).is_some())
        .count();
    for round in 0..ROUNDS {
        let slot = adopt(&rig)?;
        let before = park(&rig, "soak")?;
        task::harness::finish(slot, round as u64 & 0xff);
        let mask = rig.finish_wait(before)?;
        check!(mask == CHILD_READY, "round {round}: an exit gave {mask:#x}");
        check!(
            reap_as(&rig) == Some((slot, round as u64 & 0xff)),
            "round {round}: the child was not reaped"
        );
    }
    let slots_after = (1..task::MAX_TASKS)
        .filter(|slot| task::harness::state(*slot).is_some())
        .count();
    check!(
        slots_after == slots_before,
        "slots {slots_before} -> {slots_after} after {ROUNDS} generations"
    );
    check!(!childbell::armed(rig.thread), "the bell stayed armed");
    rig.teardown()
}
