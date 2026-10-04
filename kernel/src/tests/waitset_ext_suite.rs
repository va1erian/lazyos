//! The wait set's nanosecond deadlines and descriptor doorbell
//! (`channels::WAIT_DEADLINE_NS`, `channels::WAIT_FD`): the compositor's
//! 60 Hz frames, finer client timers and the Terminal's pty.
//!
//! The waiting side is `waitset_suite`'s kernel thread, so every wake is a
//! real timer expiry or a real `notify_poll` followed by a real context
//! switch. Its descriptors are opened in its own table: the kernel task acts
//! "as" the thread (`switch_current`) to create and write them, exactly as a
//! second Linux process would write the other end of its pty.

use core::sync::atomic::Ordering;

use super::waitset_suite::{
    parcel_bytes, rig, Rig, DEADLINE, HANDLES, RAW, RETURNED_NS, TIMED_OUT, WAITS,
};
use super::*;
use crate::arch::clock::monotonic_ns;
use crate::ipc::channels::{
    self, harness as chan, Error as ChannelError, FD_READY, WAIT_DEADLINE_NS, WAIT_FD,
    WAIT_FD_SHIFT,
};

/// A Linux syscall with up to three arguments, as the current task.
fn sys(nr: u64, args: [u64; 3]) -> u64 {
    process::linux::dispatch_args6_for_test(nr, [args[0], args[1], args[2], 0, 0, 0])
}

/// Run `f` as `slot` (its descriptor table), then as the kernel task again,
/// and give the CPU away once: a wake `f` caused was made while `slot` was
/// "current", so it raised no reschedule of its own.
fn as_task<T>(slot: usize, f: impl FnOnce() -> T) -> T {
    task::harness::switch_current(slot);
    let out = f();
    task::harness::switch_current(task::KERNEL_TASK);
    task::switch::yield_now();
    out
}

/// A pipe in the current task's table: `(read, write)`.
fn pipe() -> Result<(u64, u64), String> {
    let mut fds = [0i32; 2];
    let ret = sys(22, [fds.as_mut_ptr() as u64, 0, 0]);
    check!(ret == 0, "pipe returned {ret:#x}");
    Ok((fds[0] as u64, fds[1] as u64))
}

fn write(fd: u64, bytes: &[u8]) -> u64 {
    sys(1, [fd, bytes.as_ptr() as u64, bytes.len() as u64])
}

fn read(fd: u64, buf: &mut [u8]) -> u64 {
    sys(0, [fd, buf.as_mut_ptr() as u64, buf.len() as u64])
}

/// The flags of a wait on descriptor `fd`.
fn fd_flags(fd: u64) -> u64 {
    WAIT_FD | fd << WAIT_FD_SHIFT
}

/// Let the kernel task sleep until the thread's wait has returned (at most
/// `limit_ns`); the instant it was seen.
fn await_return(before: u64, limit_ns: u64) -> u64 {
    let give_up = monotonic_ns() + limit_ns;
    while WAITS.load(Ordering::Relaxed) == before && monotonic_ns() < give_up {
        task::wait_sleep_ns(monotonic_ns() + 50_000);
    }
    monotonic_ns()
}

/// Readiness without parking, from the kernel task: an empty pipe is not
/// ready, a written or hung-up one is, a closed descriptor and descriptor
/// bits without `WAIT_FD` are refused, and nothing stays flagged.
pub fn fd_ready_masks() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    channels::reset();
    let (r, w) = pipe()?;
    let expired = Some(monotonic_ns());
    let flags = fd_flags(r) | WAIT_DEADLINE_NS;
    check!(
        channels::wait_any(&[], flags, expired) == Err(ChannelError::TimedOut),
        "an empty pipe was ready"
    );
    check!(write(w, b"x") == 1, "write");
    check!(
        channels::wait_any(&[], flags, expired) == Ok(FD_READY),
        "a written pipe was not ready"
    );
    let mut buf = [0u8; 4];
    check!(read(r, &mut buf) == 1, "drain");
    sys(3, [w, 0, 0]);
    check!(
        channels::wait_any(&[], flags, expired) == Ok(FD_READY),
        "a hung-up pipe was not ready (its read must see EOF)"
    );
    sys(3, [r, 0, 0]);
    check!(
        channels::wait_any(&[], flags, expired) == Err(ChannelError::WrongKind),
        "a closed descriptor was accepted"
    );
    check!(
        channels::wait_any(&[], 7 << WAIT_FD_SHIFT | WAIT_DEADLINE_NS, expired)
            == Err(ChannelError::BadParcel),
        "descriptor bits without WAIT_FD were accepted"
    );
    check!(
        channels::fd_watchers() == 0,
        "{} descriptor watchers left",
        channels::fd_watchers()
    );
    Ok(())
}

/// A nanosecond deadline expires on time, far inside one 10 ms tick (read
/// as ticks it would never come), and a send still wakes such a wait.
pub fn ns_deadline() -> Result<(), String> {
    let rig = rig(1)?;
    RAW.store(WAIT_DEADLINE_NS, Ordering::Relaxed);
    let mut worst = 0;
    for _ in 0..20 {
        let before = WAITS.load(Ordering::Relaxed);
        let start = monotonic_ns();
        DEADLINE.store(start + 2_000_000, Ordering::Relaxed);
        rig.start_wait()?;
        await_return(before, 100_000_000);
        let mask = rig.finish_wait(before)?;
        check!(mask == TIMED_OUT, "an idle 2 ms wait returned {mask:#x}");
        let took = RETURNED_NS.load(Ordering::Relaxed).saturating_sub(start);
        check!(took >= 2_000_000, "a 2 ms wait ended after {took} ns");
        worst = worst.max(took);
    }
    // Generous for a loaded TCG host, yet below the tick the old rounding
    // would have cost.
    check!(
        worst < 9_000_000,
        "a 2 ms wait took {worst} ns (the 10 ms tick?)"
    );
    DEADLINE.store(monotonic_ns() + 1_000_000_000, Ordering::Relaxed);
    let before = WAITS.load(Ordering::Relaxed);
    rig.start_wait()?;
    channels::send(rig.senders[0], &parcel_bytes()?).map_err(|e| format!("{e:?}"))?;
    let mask = rig.finish_wait(before)?;
    check!(mask == 1, "a send to a ns-deadline wait gave {mask:#x}");
    serial_println!("TEST:ipc_waitset_ns_deadline:INFO:worst_2ms_wait_ns={worst}");
    rig.teardown()
}

/// The thread's pipe, opened in its own table.
fn thread_pipe(rig: &Rig) -> Result<(u64, u64), String> {
    as_task(rig.thread, pipe)
}

/// Parked on an endpoint and a descriptor, the thread wakes for a write to
/// the descriptor with `FD_READY`, for a send with bit 0, and leaves no
/// registration or watcher behind.
pub fn fd_wake() -> Result<(), String> {
    let rig = rig(1)?;
    let (r, w) = thread_pipe(&rig)?;
    RAW.store(fd_flags(r), Ordering::Relaxed);
    let before = WAITS.load(Ordering::Relaxed);
    rig.start_wait()?;
    check!(
        channels::fd_watchers() == 1,
        "the parked thread is not watching its descriptor"
    );
    check!(as_task(rig.thread, || write(w, b"out")) == 3, "write");
    let mask = rig.finish_wait(before)?;
    check!(mask == FD_READY, "a pipe write gave {mask:#x}");
    let mut buf = [0u8; 8];
    check!(
        as_task(rig.thread, || read(r, &mut buf)) == 3,
        "the bytes were not there to read"
    );
    let before = WAITS.load(Ordering::Relaxed);
    rig.start_wait()?;
    channels::send(rig.senders[0], &parcel_bytes()?).map_err(|e| format!("{e:?}"))?;
    let mask = rig.finish_wait(before)?;
    check!(mask == 1, "a send beside a descriptor gave {mask:#x}");
    check!(
        channels::fd_watchers() == 0 && chan::total_waiters() == 0,
        "{} watchers, {} registrations left",
        channels::fd_watchers(),
        chan::total_waiters()
    );
    // An unrelated `notify_poll` (another pipe) wakes the wait, which finds
    // its own descriptor empty and parks again rather than returning.
    let taken = as_task(rig.thread, || {
        channels::try_recv(HANDLES[0].load(Ordering::Relaxed))
    });
    check!(matches!(taken, Ok(Some(_))), "the message was not there");
    let (other_r, other_w) = thread_pipe(&rig)?;
    let before = WAITS.load(Ordering::Relaxed);
    rig.start_wait()?;
    check!(as_task(rig.thread, || write(other_w, b"!")) == 1, "write");
    task::preempt_point();
    check!(
        WAITS.load(Ordering::Relaxed) == before,
        "a write to another descriptor ended the wait"
    );
    as_task(rig.thread, || sys(3, [w, 0, 0]));
    let mask = rig.finish_wait(before)?;
    check!(mask == FD_READY, "the hang-up gave {mask:#x}");
    as_task(rig.thread, || {
        for fd in [r, other_r, other_w] {
            sys(3, [fd, 0, 0]);
        }
    });
    rig.teardown()
}

/// Soak: 20 000 rounds, each a pipe write, a send or a 2 ms nanosecond
/// timeout picked pseudo-randomly, each ending the parked wait with exactly
/// the right result; nothing leaks.
pub fn fd_soak() -> Result<(), String> {
    const ROUNDS: u64 = 20_000;
    let rig = rig(1)?;
    let (r, w) = thread_pipe(&rig)?;
    let message = parcel_bytes()?;
    let mut buf = [0u8; 8];
    let (mut writes, mut sends, mut timeouts) = (0u64, 0u64, 0u64);
    for round in 0..ROUNDS {
        let pick = ((round * 2_654_435_761) >> 9) % 3;
        let flags = fd_flags(r) | WAIT_DEADLINE_NS;
        RAW.store(flags, Ordering::Relaxed);
        // Long enough that the thread is parked before it passes, even
        // under TCG; the timeout must be the wait's own, not a race.
        let deadline = if pick == 2 { 2_000_000 } else { 1_000_000_000 };
        DEADLINE.store(monotonic_ns() + deadline, Ordering::Relaxed);
        let before = WAITS.load(Ordering::Relaxed);
        rig.start_wait()
            .map_err(|e| format!("round {round} (pick {pick}): {e}"))?;
        let expected = match pick {
            0 => {
                check!(as_task(rig.thread, || write(w, b"s")) == 1, "write");
                writes += 1;
                FD_READY
            }
            1 => {
                channels::send(rig.senders[0], &message).map_err(|e| format!("{e:?}"))?;
                sends += 1;
                1
            }
            _ => {
                await_return(before, 50_000_000);
                timeouts += 1;
                TIMED_OUT
            }
        };
        let mask = rig.finish_wait(before)?;
        check!(
            mask == expected,
            "round {round}: {mask:#x}, expected {expected:#x}"
        );
        match pick {
            0 => check!(
                as_task(rig.thread, || read(r, &mut buf)) == 1,
                "round {round}: nothing to read"
            ),
            1 => {
                let taken = as_task(rig.thread, || {
                    channels::try_recv(HANDLES[0].load(Ordering::Relaxed))
                });
                check!(matches!(taken, Ok(Some(_))), "round {round}: nothing to take");
            }
            _ => {}
        }
        check!(
            channels::fd_watchers() == 0 && chan::total_waiters() == 0,
            "round {round}: {} watchers, {} registrations",
            channels::fd_watchers(),
            chan::total_waiters()
        );
    }
    // Every byte written was read back: the pipe is empty. Asked by `poll`
    // readiness, never by a wait: a wait made "as" the thread would park the
    // kernel task in the thread's slot.
    let left = as_task(rig.thread, || task::fd_poll(r as usize, crate::ipc::pipe::POLLIN));
    check!(left == Some(0), "bytes left over: {left:?}");
    as_task(rig.thread, || {
        sys(3, [r, 0, 0]);
        sys(3, [w, 0, 0]);
    });
    serial_println!(
        "TEST:ipc_waitset_fd_soak:INFO:rounds={ROUNDS} writes={writes} sends={sends} \
         timeouts={timeouts}"
    );
    rig.teardown()
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_waitset_fd_ready_masks", fd_ready_masks),
    ("ipc_waitset_ns_deadline", ns_deadline),
    ("ipc_waitset_fd_wake", fd_wake),
    ("ipc_waitset_fd_soak", fd_soak),
];
