//! The `execve` de-thread with threads that really run (`task::kthread`
//! adopted into a process): the exec'ing thread waits until each sibling
//! parked in a kernel wait has woken, reached its syscall return
//! (`deliver_native`) and died there, alone; then it switches to the new
//! table and the process lives on under its pid. The soak repeats it with
//! many threads, with the leader and with a thread as the exec'ing one, and
//! checks that no task slot or frame is left behind and that the parent's
//! `wait4` sees only the final exit of each process.

use super::*;
use crate::task::family::{notifications, reset_notifications};
use crate::task::wait::WaitQueue;
use crate::task::PriorityClass;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Where the suite's threads park: the siblings for good, the exec'ing
/// thread until [`GO`] says start, and again once its exec is done.
static GATE: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// The round the exec'ing thread may run (it waits while it is behind).
static GO: AtomicU64 = AtomicU64::new(0);
/// The last round whose exec finished, and how (1 ok, 2 refused).
static DONE: AtomicU64 = AtomicU64::new(0);
static OUTCOME: AtomicU64 = AtomicU64::new(0);
/// The new table the exec'ing thread switched to.
static NEW_TABLE: AtomicU64 = AtomicU64::new(0);
/// The siblings that reached their syscall return to die there.
static UNWOUND: AtomicUsize = AtomicUsize::new(0);

/// A sibling inside a blocking syscall: parked until a signal ends the wait,
/// then back to its syscall return, which ends it when it is being killed.
extern "C" fn sibling() -> ! {
    let me = task::current();
    loop {
        GATE.wait(me, None);
        if signal::fatal_pending() {
            UNWOUND.fetch_add(1, Ordering::AcqRel);
            signal::deliver_native();
        }
    }
}

/// The thread that execs once [`GO`] reaches the round after [`DONE`], then
/// sits in the new image until the process is ended.
extern "C" fn execer() -> ! {
    let me = task::current();
    let round = DONE.load(Ordering::Acquire) + 1;
    while GO.load(Ordering::Acquire) < round {
        GATE.wait(me, None);
    }
    let outcome = match task::dethread() {
        Ok(()) => match crate::mem::new_user_table() {
            Some(table) => {
                // As `sys_execve`: the new table is live before the old one
                // can be let go.
                crate::mem::switch_to(table);
                task::set_pml4(table.as_u64());
                NEW_TABLE.store(table.as_u64(), Ordering::Release);
                1
            }
            None => 3,
        },
        Err(_) => 2,
    };
    OUTCOME.store(outcome, Ordering::Release);
    DONE.store(round, Ordering::Release);
    loop {
        GATE.wait(me, None);
        if signal::fatal_pending() {
            signal::deliver_native();
        }
    }
}

/// Let the suite's threads run until `done` holds or five seconds pass.
fn run_until(done: impl Fn() -> bool) -> bool {
    let until = task::ticks() + 500;
    while !done() {
        if task::ticks() > until {
            return false;
        }
        GATE.notify_all();
        // A sleep, not a yield: the threads run, and time passes.
        task::wait_sleep(task::ticks() + 1);
        task::reclaim_pending();
    }
    true
}

fn thread(entry: extern "C" fn() -> !) -> Result<usize, String> {
    task::kthread::spawn_kernel_thread("exec-live", entry, PriorityClass::Realtime)
        .map_err(|error| format!("spawn: {error}"))
}

/// One process forked by `parent`: `parked` threads in a kernel wait,
/// `at_user` threads at a user frame, and the exec'ing thread, which is the
/// leader when `leader_execs`. Runs the exec and checks what is left; then
/// ends the new image with `status` and checks the parent's `wait4`.
fn exec_round(
    parent: usize,
    parked: usize,
    at_user: usize,
    leader_execs: bool,
    status: u64,
) -> Result<(), String> {
    let round = DONE.load(Ordering::Acquire) + 1;
    let table = crate::mem::new_user_table().ok_or("no table")?;
    let pml4 = table.as_u64();
    let exec = thread(execer)?;
    // The threads that sleep in the kernel when the exec begins.
    let mut sleepers = Vec::new();
    let leader = if leader_execs {
        exec
    } else {
        let leader = thread(sibling)?;
        sleepers.push(leader);
        leader
    };
    task::harness::adopt(leader, pml4, leader, parent);
    task::harness::adopt(exec, pml4, leader, parent);
    for _ in 0..parked {
        let slot = thread(sibling)?;
        task::harness::adopt(slot, pml4, leader, 0);
        sleepers.push(slot);
    }
    // Threads stopped at a user frame. They have no code to run (the suite
    // makes them), so they stay parked; the de-thread ends them where they
    // stand.
    task::harness::switch_current(leader);
    let mut stopped = Vec::new();
    for _ in 0..at_user {
        let slot = task::spawn_thread("exec-user", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        park_stopped(slot);
        stopped.push(slot);
    }
    task::harness::switch_current(task::KERNEL_TASK);
    let members: Vec<usize> = core::iter::once(exec)
        .chain(sleepers.iter().copied())
        .chain(stopped.iter().copied())
        .collect();
    let parked_all = run_until(|| {
        sleepers
            .iter()
            .all(|&slot| matches!(task::harness::state(slot), Some(TaskState::Blocked { .. })))
    });
    check!(parked_all, "round {round}: the siblings did not park");
    reset_notifications();
    UNWOUND.store(0, Ordering::Release);
    GO.store(round, Ordering::Release);
    check!(
        run_until(|| DONE.load(Ordering::Acquire) == round),
        "round {round}: the exec never finished"
    );
    check!(
        OUTCOME.load(Ordering::Acquire) == 1,
        "round {round}: exec outcome {}",
        OUTCOME.load(Ordering::Acquire)
    );
    check!(
        UNWOUND.load(Ordering::Acquire) == sleepers.len(),
        "round {round}: {} of {} siblings unwound",
        UNWOUND.load(Ordering::Acquire),
        sleepers.len()
    );
    let new = NEW_TABLE.load(Ordering::Acquire);
    for &slot in members.iter().filter(|&&slot| slot != exec) {
        let ok = match task::harness::state(slot) {
            None => true,
            Some(TaskState::Done) => slot != leader || task::harness::pml4(slot) == Some(new),
            Some(_) => false,
        };
        check!(
            ok,
            "round {round}: member {slot} is {:?}",
            task::harness::state(slot)
        );
    }
    check!(
        task::harness::pml4(exec) == Some(new) && task::harness::state(exec).is_some(),
        "round {round}: the exec'ing thread is not in the new image"
    );
    check!(
        notifications(parent) == 0 && reap_as(parent).is_none(),
        "round {round}: the parent heard of the exec"
    );
    // The new image ends: the parent collects the leader's pid with its status.
    signal::terminate_process(new, status);
    check!(
        // Done, or (a thread, which has no parent) already reclaimed.
        run_until(
            || !matches!(task::harness::state(exec), Some(state) if state != TaskState::Done)
        ),
        "round {round}: the new image did not end"
    );
    let reaped = reap_as(parent);
    check!(
        reaped == Some((leader, status, 0)) && notifications(parent) == 1,
        "round {round}: wait4 saw {reaped:?}, told {} times",
        notifications(parent)
    );
    check!(reap_as(parent).is_none(), "round {round}: a second exit");
    // The threads have no parent: the scheduler frees them.
    run_until(|| {
        members
            .iter()
            .all(|&slot| task::harness::state(slot).is_none())
    });
    task::reclaim_pending();
    let left: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&slot| task::harness::state(slot).is_some())
        .collect();
    check!(left.is_empty(), "round {round}: slots left {left:?}");
    Ok(())
}

fn reap_as(parent: usize) -> Option<(usize, u64, u8)> {
    task::harness::switch_current(parent);
    let reaped = task::reap_child_filtered(task::ChildFilter::Any);
    task::harness::switch_current(task::KERNEL_TASK);
    reaped
}

fn setup() -> Result<usize, String> {
    fresh()?;
    GO.store(0, Ordering::Release);
    DONE.store(0, Ordering::Release);
    task::set_blocked(false);
    let parent = task::spawn_fork().map_err(|error| format!("parent: {error}"))?;
    // The suite stands in for the parent (it has no code to run): parked, so
    // the scheduler never resumes it while the threads run.
    park_stopped(parent);
    Ok(parent)
}

/// Park a suite-made task that has no code, as if stopped at a user frame.
fn park_stopped(slot: usize) {
    task::harness::set_state(
        slot,
        TaskState::Blocked {
            wait: WaitKind::Signal,
            deadline: None,
        },
    );
}

fn teardown(parent: usize) -> Result<(), String> {
    task::harness::finish(parent, 0);
    while task::reap_child().is_some() {}
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// The leader execs while two threads sleep in the kernel and one stands at
/// a user frame; then a thread execs while the leader sleeps in the kernel.
pub fn exec_waits_for_unwinding_threads() -> Result<(), String> {
    let parent = setup()?;
    exec_round(parent, 2, 1, true, 3)?;
    exec_round(parent, 1, 1, false, 4)?;
    teardown(parent)
}

/// Many processes with many threads exec in turn: no task slot, kernel stack
/// or frame is left behind, and each parent hears of each process once.
pub fn soak_exec_dethread() -> Result<(), String> {
    let parent = setup()?;
    // Warm up the heap and the registries before the baseline.
    exec_round(parent, 2, 2, true, 1)?;
    let slots = task::free_slots();
    let frames = crate::mem::frame_stats().free;
    for round in 0..64u64 {
        exec_round(
            parent,
            (round % 6) as usize,
            (round % 3) as usize,
            round % 2 == 0,
            round % 100 + 1,
        )?;
    }
    check!(
        task::free_slots() == slots,
        "task slots leaked: {slots} free before, {} after",
        task::free_slots()
    );
    let after = crate::mem::frame_stats().free;
    check!(
        after + 16 >= frames,
        "frames leaked: {frames} free before, {after} after"
    );
    teardown(parent)
}
