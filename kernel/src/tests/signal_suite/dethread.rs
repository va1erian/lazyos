//! `execve` ends the other threads of its process first (Linux's
//! `de_thread`): each by the group-exit rule (on the spot at a user frame,
//! woken to unwind inside the kernel), but alone, with no group exit: the
//! parent hears of the process only when the new image ends, under the same
//! pid. The live counterpart, with threads that really run, is
//! `dethread_live`.

use super::*;
use crate::task::family::{notifications, reset_notifications};
use crate::task::signal::ExecRace;
use crate::task::wait::WaitQueue;

static QUEUE: WaitQueue = WaitQueue::new(WaitKind::Block);

fn here(slot: usize) {
    task::harness::switch_current(slot);
}

/// A process forked by `parent` with `threads` threads: `(leader, threads)`.
fn process_of(parent: usize, threads: usize) -> Result<(usize, Vec<usize>), String> {
    here(parent);
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    here(leader);
    let mut made = Vec::new();
    for _ in 0..threads {
        let thread = task::spawn_thread("exec-thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        made.push(thread);
    }
    here(task::KERNEL_TASK);
    Ok((leader, made))
}

/// What `wait4(-1, WNOHANG)` of `parent` collects: `(pid, status, signal)`.
fn reap_as(parent: usize) -> Option<(usize, u64, u8)> {
    here(parent);
    let reaped = task::reap_child_filtered(task::ChildFilter::Any);
    here(task::KERNEL_TASK);
    reaped
}

fn sigchld_pending(slot: usize) -> bool {
    signal::pending(slot) & (1 << signal::SIGCHLD) != 0
}

/// Free the scheduler's finished threads, as its next entries would.
fn reclaim() {
    here(task::KERNEL_TASK);
    task::harness::simulate_tick();
    here(task::KERNEL_TASK);
    task::reclaim_pending();
}

fn state(slot: usize) -> Option<TaskState> {
    task::harness::state(slot)
}

/// The leader execs while one thread is at a user frame, one parked inside
/// the kernel and one running inside it: the first ends on the spot, the
/// others are woken or left to unwind, with `SIGKILL` in their own pending
/// set only; the parked one dies in the sweep once back at a user frame, the
/// running one by its own `exit_group` on the way out, and neither starts a
/// group exit. The parent hears of the process once, with the new image's
/// status.
pub fn exec_ends_the_other_threads() -> Result<(), String> {
    fresh()?;
    let parent = task::spawn_fork().map_err(|error| format!("parent: {error}"))?;
    let (leader, threads) = process_of(parent, 3)?;
    let (user, parked, running) = (threads[0], threads[1], threads[2]);
    let pml4 = task::harness::pml4(leader).ok_or("no address space")?;
    let parked_cs = task::harness::set_kernel_frame(parked).ok_or("no frame")?;
    QUEUE.park(parked, None);
    task::harness::set_kernel_frame(running).ok_or("no frame")?;
    reset_notifications();

    here(leader);
    let ended = signal::begin_exec(pml4, leader).map_err(|race| format!("{race:?}"))?;
    check!(ended == 1, "{ended} threads ended on the spot, want 1");
    check!(
        state(user) == Some(TaskState::Done),
        "user: {:?}",
        state(user)
    );
    check!(
        state(parked) == Some(TaskState::Runnable)
            && task::harness::take_wake_reason(parked) == Some(WakeReason::Interrupted),
        "the parked thread was not woken to unwind: {:?}",
        state(parked)
    );
    check!(
        state(running) == Some(TaskState::Runnable),
        "running: {:?}",
        state(running)
    );
    check!(
        signal::killed(parked) && signal::killed(running) && !signal::killed(leader),
        "killed: parked {} running {} leader {}",
        signal::killed(parked),
        signal::killed(running),
        signal::killed(leader)
    );
    check!(
        signal::pending(leader) & (1 << signal::SIGKILL) == 0,
        "the exec'ing thread has a kill pending"
    );
    check!(
        signal::exec_victim(pml4, parked) && !signal::exec_victim(pml4, leader),
        "victims misjudged"
    );
    check!(
        QUEUE.wait(parked, None) == WakeReason::Interrupted,
        "a victim parked again"
    );
    check!(
        signal::begin_exec(pml4, running) == Err(ExecRace::OtherExec),
        "a second execve in the process was let through"
    );
    // A victim makes no new thread the exec would then wait on.
    here(running);
    let late = task::spawn_thread("exec-late", process::USER_STACK_TOP, 0, 0);
    here(leader);
    check!(late.is_err(), "a thread being killed made thread {late:?}");
    // Nor a process: a copy of the old image would outlive the exec.
    here(running);
    let forked = task::spawn_fork();
    here(leader);
    check!(forked.is_err(), "a thread being killed forked {forked:?}");

    // The parked thread is back at a user frame: the sweep ends it alone.
    task::harness::set_frame_cs(parked, parked_cs);
    check!(
        task::harness::resume_delivery(parked),
        "the sweep did not end the parked victim"
    );
    // The running one calls `exit_group` on its way out: alone as well.
    here(running);
    task::exit_thread_group(0);
    here(task::KERNEL_TASK);
    check!(
        state(parked) == Some(TaskState::Done) && state(running) == Some(TaskState::Done),
        "victims left: parked {:?} running {:?}",
        state(parked),
        state(running)
    );
    check!(
        state(leader) == Some(TaskState::Runnable) && signal::pending(leader) == 0,
        "the exec'ing thread was hit: {:?} pending {:#x}",
        state(leader),
        signal::pending(leader)
    );

    here(leader);
    check!(task::dethread().is_ok(), "dethread with no thread left");
    let table = crate::mem::new_user_table().ok_or("no table")?;
    task::set_pml4(table.as_u64());
    check!(
        task::linuxstate::tgid() == leader,
        "pid after exec: {}",
        task::linuxstate::tgid()
    );
    check!(
        reap_as(parent).is_none() && notifications(parent) == 0 && !sigchld_pending(parent),
        "the parent heard of the exec (told {} times)",
        notifications(parent)
    );
    reclaim();
    check!(
        threads.iter().all(|slot| state(*slot).is_none()),
        "threads not reclaimed"
    );

    here(leader);
    task::exit_thread_group(5);
    check!(
        reap_as(parent) == Some((leader, 5, 0)) && notifications(parent) == 1,
        "the final exit (told {} times)",
        notifications(parent)
    );
    end(parent)
}

/// A thread that is not the leader execs: the leader, killed by the
/// de-thread, comes along as the zombie that holds the pid, so `getpid`,
/// `getppid` and the parent's `wait4` see the same process, and the parent
/// collects the leader's pid with the new image's status.
pub fn exec_from_a_thread_keeps_the_pid() -> Result<(), String> {
    fresh()?;
    let parent = task::spawn_fork().map_err(|error| format!("parent: {error}"))?;
    let (leader, threads) = process_of(parent, 2)?;
    let (execer, other) = (threads[0], threads[1]);
    reset_notifications();
    here(execer);
    check!(task::dethread().is_ok(), "dethread refused");
    check!(
        state(leader) == Some(TaskState::Done) && state(other) == Some(TaskState::Done),
        "left: leader {:?} other {:?}",
        state(leader),
        state(other)
    );
    let table = crate::mem::new_user_table().ok_or("no table")?;
    task::set_pml4(table.as_u64());
    check!(
        task::linuxstate::tgid() == leader
            && task::ppid() == parent
            && task::harness::recorded_leader(execer) == leader,
        "after exec: pid {} ppid {}",
        task::linuxstate::tgid(),
        task::ppid()
    );
    check!(
        task::harness::pml4(leader) == Some(table.as_u64()),
        "the leader did not follow the exec"
    );
    check!(
        reap_as(parent).is_none() && notifications(parent) == 0 && !sigchld_pending(parent),
        "the parent heard of the exec"
    );
    here(execer);
    task::exit_thread_group(7);
    check!(
        reap_as(parent) == Some((leader, 7, 0)) && notifications(parent) == 1,
        "the final exit (told {} times)",
        notifications(parent)
    );
    reclaim();
    check!(
        state(execer).is_none() && state(other).is_none(),
        "threads not reclaimed: {:?} {:?}",
        state(execer),
        state(other)
    );
    end(parent)
}

/// The CPU-aware cases: a sibling that has just parked while still current on
/// another CPU is woken (a nudge alone would leave it asleep with the kill
/// pending), and a finished sibling still current there keeps the exec
/// waiting (its CPU still runs on the old table).
pub fn exec_wakes_siblings_parked_elsewhere() -> Result<(), String> {
    use crate::task::oncpu;
    fresh()?;
    let parent = task::spawn_fork().map_err(|error| format!("parent: {error}"))?;
    let (leader, threads) = process_of(parent, 2)?;
    let (parked, done) = (threads[0], threads[1]);
    let pml4 = task::harness::pml4(leader).ok_or("no address space")?;
    let other = crate::arch::percpu::current().index() as usize + 1;
    QUEUE.park(parked, None);
    oncpu::set_for_test(parked, Some(other));
    task::harness::finish(done, 0);
    oncpu::set_for_test(done, Some(other));
    here(leader);
    let ended = signal::begin_exec(pml4, leader).map_err(|race| format!("{race:?}"))?;
    check!(ended == 0, "{ended} ended on the spot");
    check!(
        state(parked) == Some(TaskState::Runnable)
            && task::harness::take_wake_reason(parked) == Some(WakeReason::Interrupted),
        "the sibling parked elsewhere was not woken: {:?}",
        state(parked)
    );
    oncpu::set_for_test(parked, None);
    task::harness::finish(parked, 0);
    check!(
        signal::exec_siblings_left(pml4, leader) == 1,
        "a finished sibling still current elsewhere was not waited for"
    );
    oncpu::set_for_test(done, None);
    check!(
        signal::exec_siblings_left(pml4, leader) == 0,
        "siblings left after the CPU let go"
    );
    here(task::KERNEL_TASK);
    task::harness::finish(leader, 0);
    while reap_as(parent).is_some() {}
    end(parent)
}

/// An exec in a process that is being killed is refused (the kill wins), and
/// a single-threaded exec has nothing to end.
pub fn exec_refused_while_dying() -> Result<(), String> {
    fresh()?;
    let parent = task::spawn_fork().map_err(|error| format!("parent: {error}"))?;
    let (single, _) = process_of(parent, 0)?;
    here(single);
    check!(task::dethread().is_ok(), "a lone thread's dethread");
    let (leader, threads) = process_of(parent, 1)?;
    let pml4 = task::harness::pml4(leader).ok_or("no address space")?;
    task::harness::set_kernel_frame(threads[0]).ok_or("no frame")?;
    QUEUE.park(threads[0], None);
    here(leader);
    send(threads[0], signal::SIGKILL)?;
    check!(
        signal::begin_exec(pml4, leader) == Err(ExecRace::Dying),
        "an exec during a kill"
    );
    check!(task::dethread().is_err(), "dethread during a kill");
    here(task::KERNEL_TASK);
    task::harness::finish(threads[0], 0);
    task::harness::finish(leader, 0);
    task::harness::finish(single, 0);
    while reap_as(parent).is_some() {}
    end(parent)
}

fn end(parent: usize) -> Result<(), String> {
    here(task::KERNEL_TASK);
    task::harness::finish(parent, 0);
    while task::reap_child().is_some() {}
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}
