//! A process's descriptors close when it exits, not when it is reaped.
//!
//! A shell running `X=$(cmd)` forks `cmd` with the write end of a pipe, reads
//! the pipe to end-of-file, and only then reaps the child. If the zombie kept
//! its write end until `wait4`, the read never returned and the shell hung
//! (the desktop Terminal and the console alike). Each test here has the
//! parent read a child that has exited but is still unreaped.

use super::*;
use crate::task::signal::{self, SigInfo};
use alloc::sync::Arc;

/// A non-blocking pipe whose write end only `child` holds: the parent (the
/// kernel task) forks with both ends and closes its write end, as `sh` does.
/// Returns `(child, read fd, write fd)`.
fn fork_writer() -> Result<(usize, usize, usize), String> {
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(293, fds.as_mut_ptr() as u64, O_NONBLOCK, 0);
    check!(ret == 0, "pipe2 returned {ret:#x}");
    let (r, w) = (fds[0] as usize, fds[1] as usize);
    let child = task::spawn_fork().map_err(to_string)?;
    check!(
        task::fd_close(w),
        "the parent could not close its write end"
    );
    Ok((child, r, w))
}

/// Write `bytes` as `child`, then return to the parent.
fn child_writes(child: usize, w: usize, bytes: &[u8]) -> Result<(), String> {
    task::harness::switch_current(child);
    let written = task::fd_stream_write(w, bytes);
    task::harness::switch_current(task::KERNEL_TASK);
    let n = written.map_err(io_err)?;
    check!(
        n == bytes.len(),
        "the child wrote {n} of {} bytes",
        bytes.len()
    );
    Ok(())
}

/// Read everything the child wrote, then require end-of-file (not `EAGAIN`).
fn read_to_eof(r: usize, expected: &[u8]) -> Result<(), String> {
    let mut got = [0u8; 64];
    let n = task::fd_stream_read(r, &mut got).map_err(io_err)?;
    check!(
        &got[..n] == expected,
        "read {:?}, expected {:?}",
        &got[..n],
        expected
    );
    match task::fd_stream_read(r, &mut got) {
        Ok(0) => Ok(()),
        Ok(n) => Err(format!("read {n} more bytes where EOF was expected")),
        Err(error) => Err(format!(
            "the exited, unreaped writer still holds the pipe: {error:?}"
        )),
    }
}

/// The pipe object behind descriptor `fd` of the current task.
fn pipe_of(fd: usize) -> Result<Arc<pipe::Pipe>, String> {
    match task::fd_clone(fd) {
        Some(task::Fd::Pipe { ref pipe, .. }) => Ok(Arc::clone(pipe)),
        _ => Err(format!("fd {fd} is not a pipe")),
    }
}

/// `$(...)`: the child writes and exits; the parent, which has not reaped it,
/// reads the output and then end-of-file. A reader already parked on the
/// empty pipe is woken by the exit itself. The zombie's descriptor table is
/// empty and it is still reapable with its status.
pub fn eof_on_unreaped_exit() -> Result<(), String> {
    fresh()?;
    task::harness::reset();
    let (child, r, w) = fork_writer()?;
    let pipe = pipe_of(r)?;
    let mut buf = [0u8; 8];
    check!(
        task::fd_stream_read(r, &mut buf) == Err(pipe::Error::WouldBlock),
        "the pipe reported EOF while the child still runs"
    );

    child_writes(child, w, b"hi\n")?;
    let me = task::current();
    let n = task::fd_stream_read(r, &mut buf).map_err(io_err)?;
    check!(
        n == 3 && &buf[..3] == b"hi\n",
        "read {n} bytes before the exit"
    );
    pipe.park_reader(me);
    task::harness::finish(child, 7);
    check!(
        task::harness::state(me) == Some(task::TaskState::Runnable)
            && task::harness::take_wake_reason(me) == Some(task::WakeReason::Woken),
        "the child's exit did not wake the parked reader: {:?}",
        task::harness::state(me)
    );
    check!(
        pipe.writers() == 0,
        "{} writers survive the exit",
        pipe.writers()
    );
    check!(
        task::harness::state(child) == Some(task::TaskState::Done)
            && (0..task::FD_COUNT)
                .all(|fd| task::harness::fd_kind_at(child, fd) == task::FdKind::Closed),
        "the zombie kept a descriptor"
    );
    read_to_eof(r, b"")?;

    let (slot, status) = task::reap_child().ok_or("the zombie is not reapable")?;
    check!(
        slot == child && status == 7,
        "reaped {slot} with status {status}"
    );
    check!(task::fd_close(r), "closing the read end failed");
    drop(pipe);
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes leaked",
        pipe::Pipe::live()
    );
    check!(fds_clean(), "a descriptor was left open");
    Ok(())
}

/// A child ended by `SIGKILL` while preempted in user mode (the timer sweep's
/// path, which runs on the scheduler's lock and only flags the slot) closes
/// its descriptors at the next task-context reclaim, still before any reap.
pub fn eof_on_killed_writer() -> Result<(), String> {
    fresh()?;
    task::harness::reset();
    signal::harness::reset();
    let (child, r, w) = fork_writer()?;
    child_writes(child, w, b"k")?;
    check!(
        task::harness::set_user_frame(child, 0x40_0500, 0x70_0800),
        "could not shape the child's frame"
    );
    let me = task::current();
    signal::send_to_slot(
        me,
        child,
        signal::SIGKILL,
        SigInfo::user(me, signal::SI_USER),
    )
    .map_err(|error| format!("SIGKILL: {error:?}"))?;
    task::harness::run_sweep();
    task::reclaim_pending();
    check!(
        task::harness::state(child) == Some(task::TaskState::Done),
        "SIGKILL left the child {:?}",
        task::harness::state(child)
    );
    read_to_eof(r, b"k")?;
    check!(
        task::reap_child().is_some(),
        "the killed child is not reapable"
    );
    check!(task::fd_close(r), "closing the read end failed");
    signal::harness::reset();
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes leaked",
        pipe::Pipe::live()
    );
    Ok(())
}

/// Soak: many substitution cycles (half by exit, half by `SIGKILL`), each
/// read to EOF before the reap. Nothing leaks: pipes, descriptors and task
/// slots all return to where they started.
pub fn soak_unreaped_exit() -> Result<(), String> {
    fresh()?;
    task::harness::reset();
    signal::harness::reset();
    let slots = task::free_slots();
    for round in 0..1000u32 {
        let (child, r, w) = fork_writer().map_err(|error| format!("round {round}: {error}"))?;
        let payload = round.to_le_bytes();
        child_writes(child, w, &payload).map_err(|error| format!("round {round}: {error}"))?;
        if round % 2 == 0 {
            task::harness::finish(child, u64::from(round % 256));
        } else {
            let me = task::current();
            signal::send_to_slot(
                me,
                child,
                signal::SIGKILL,
                SigInfo::user(me, signal::SI_USER),
            )
            .map_err(|error| format!("round {round}: SIGKILL: {error:?}"))?;
            task::harness::run_sweep();
            task::reclaim_pending();
        }
        read_to_eof(r, &payload).map_err(|error| format!("round {round}: {error}"))?;
        let (slot, _) = task::reap_child().ok_or_else(|| format!("round {round}: no zombie"))?;
        check!(
            slot == child,
            "round {round}: reaped {slot}, expected {child}"
        );
        check!(
            task::fd_close(r),
            "round {round}: closing the read end failed"
        );
        check!(
            pipe::Pipe::live() == 0,
            "round {round}: {} pipes live",
            pipe::Pipe::live()
        );
    }
    check!(
        task::free_slots() == slots,
        "{} task slots free after the soak, {slots} before",
        task::free_slots()
    );
    check!(fds_clean(), "the soak leaked a descriptor");
    signal::harness::reset();
    Ok(())
}

/// `epoll_ctl(ADD)` as the current task, watching `fd` for `EPOLLIN`.
fn epoll_add(epfd: usize, fd: usize) -> u64 {
    let mut event = [0u8; 12]; // packed `struct epoll_event`
    event[..4].copy_from_slice(&1u32.to_le_bytes());
    process::linux::dispatch_args_for_test(233, epfd as u64, 1, fd as u64, event.as_ptr() as u64)
}

/// An epoll instance shared across `fork`: the child registers its write end
/// and exits. The interest goes with the child's descriptor (as `close`
/// would drop it), so the parent's reader still gets end-of-file; the
/// parent's own interest in its read end, which the child also inherited
/// under the same number, stays registered.
pub fn exit_drops_epoll_interest() -> Result<(), String> {
    fresh()?;
    task::harness::reset();
    let epfd = process::linux::dispatch_for_test(291, 0, 0, 0) as usize;
    check!(
        (3..task::FD_COUNT).contains(&epfd),
        "epoll_create1 returned {epfd:#x}"
    );
    let (child, r, w) = fork_writer()?;
    let ret = epoll_add(epfd, r);
    check!(
        ret == 0,
        "the parent's ADD of its read end returned {ret:#x}"
    );

    // As the child (the harness cannot dispatch a syscall from a forked
    // task, which has no syscall-entry frame): what `epoll_ctl(ADD)` does.
    task::harness::switch_current(child);
    let epoll = match task::fd_clone(epfd) {
        Some(task::Fd::Epoll { ref epoll }) => Arc::clone(epoll),
        _ => return Err(String::from("the child did not inherit the epoll fd")),
    };
    let target = task::fd_clone(w).ok_or("the child has no write end")?;
    let added = crate::ipc::epoll::Epoll::add(&epoll, w, target, 1, 0);
    task::harness::switch_current(task::KERNEL_TASK);
    drop(epoll);
    check!(added.is_ok(), "the child's ADD of its write end failed");
    child_writes(child, w, b"e")?;
    task::harness::finish(child, 0);
    read_to_eof(r, b"e")?;

    // EPOLL_CTL_DEL succeeds only for a registered descriptor.
    let del = |fd: usize| process::linux::dispatch_args_for_test(233, epfd as u64, 2, fd as u64, 0);
    check!(
        del(r) == 0,
        "the parent's interest in its read end was dropped"
    );
    check!(
        del(w) != 0,
        "the exited child's interest in its write end survived"
    );
    check!(task::reap_child().is_some(), "the child is not reapable");
    check!(task::fd_close(r) && task::fd_close(epfd), "cleanup failed");
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes leaked",
        pipe::Pipe::live()
    );
    check!(fds_clean(), "a descriptor was left open");
    Ok(())
}
