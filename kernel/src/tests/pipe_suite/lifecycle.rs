//! EOF/EPIPE/non-blocking semantics, `dup`/`fork`/`CLOEXEC`,
//! `vfork`-style clone, and the throughput/lifecycle soak.

use super::*;

/// Empty reads are EOF after the last writer closes; writes to a pipe with
/// no readers are `BrokenPipe` (no SIGPIPE; see `ipc::pipe` docs); a
/// non-blocking end reports `WouldBlock` instead of parking.
pub fn eof_epipe_nonblock() -> Result<(), String> {
    fresh()?;
    let pipe = pipe::Pipe::new().ok_or("Pipe::new failed")?;
    pipe.acquire(End::Read);
    pipe.acquire(End::Write);
    let mut buf = [0u8; 8];

    check!(
        pipe.read(End::Read, &mut buf, true) == Err(pipe::Error::WouldBlock),
        "an empty non-blocking read did not report WouldBlock"
    );
    let fill = vec![0x5Au8; pipe::CAPACITY];
    check!(
        pipe.write(&fill, End::Write, true).map_err(io_err)? == pipe::CAPACITY,
        "the pipe did not accept a full ring"
    );
    check!(
        pipe.write(&fill, End::Write, true) == Err(pipe::Error::WouldBlock),
        "a full non-blocking write did not report WouldBlock"
    );
    let mut full = vec![0u8; pipe::CAPACITY];
    let drained = pipe.read(End::Read, &mut full, true).map_err(io_err)?;
    check!(
        drained == pipe::CAPACITY,
        "drained {drained} of {}",
        pipe::CAPACITY
    );

    // Last writer closes: reads return 0 (EOF), poll reports POLLHUP.
    pipe.release(End::Write);
    check!(
        pipe.read(End::Read, &mut buf, true).map_err(io_err)? == 0,
        "read after last writer close is not EOF"
    );
    check!(
        pipe.poll(End::Read, pipe::POLLIN) & pipe::POLLHUP != 0,
        "poll on an EOF read end did not report POLLHUP"
    );

    // Last reader closes: writes fail with -EPIPE, poll reports POLLERR.
    pipe.release(End::Read);
    check!(
        pipe.write(&fill, End::Write, true) == Err(pipe::Error::BrokenPipe),
        "write with no readers did not report BrokenPipe"
    );
    check!(
        pipe.poll(End::Write, pipe::POLLOUT) & pipe::POLLERR != 0,
        "poll on a readerless write end did not report POLLERR"
    );

    drop(pipe);
    check!(pipe::Pipe::live() == 0, "the pipe was not freed");
    Ok(())
}

/// `dup` shares the pipe's open file description, `fork` inherits the ends,
/// and `execve`'s `FD_CLOEXEC` sweep closes only the marked descriptors.
pub fn dup_fork_cloexec() -> Result<(), String> {
    fresh()?;

    // dup2 clears FD_CLOEXEC on the new descriptor; the exec sweep then
    // closes the originals and keeps the copy.
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(293, fds.as_mut_ptr() as u64, O_CLOEXEC, 0);
    check!(ret == 0, "pipe2 returned {ret:#x}");
    let (r, w) = (fds[0] as usize, fds[1] as usize);
    let ret = process::linux::dispatch_for_test(33, r as u64, 9, 0);
    check!(ret == 9, "dup2 returned {ret:#x}");
    check!(
        !task::fd_cloexec(9) && task::fd_kind(9) == task::FdKind::Pipe,
        "dup2 did not clear FD_CLOEXEC on fd 9"
    );
    let closed = process::linux::close_cloexec_fds();
    check!(
        closed == 2,
        "the exec sweep closed {closed} descriptors, expected 2"
    );
    check!(
        task::fd_kind(r) == task::FdKind::Closed && task::fd_kind(w) == task::FdKind::Closed,
        "the exec sweep kept an FD_CLOEXEC end"
    );
    check!(
        task::fd_kind(9) == task::FdKind::Pipe,
        "the exec sweep closed fd 9"
    );
    // fd 9 is the only reader left and no writers remain: EOF.
    let mut buf = [0u8; 4];
    let n = process::linux::dispatch_for_test(0, 9, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(n == 0, "read on the duped, writerless end returned {n:#x}");
    check!(task::fd_close(9), "closing fd 9 failed");

    // fork inherits both ends; the child can read what the parent wrote.
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "pipe returned {ret:#x}");
    let (r, w) = (fds[0] as usize, fds[1] as usize);
    let child = task::spawn_fork().map_err(to_string)?;
    check!(
        task::harness::fd_kind_at(child, r) == task::FdKind::Pipe
            && task::harness::fd_kind_at(child, w) == task::FdKind::Pipe,
        "the forked child did not inherit the pipe ends"
    );
    let n = task::fd_stream_write(w, b"kid").map_err(io_err)?;
    check!(n == 3, "parent write returned {n}");
    task::harness::switch_current(child);
    let mut got = [0u8; 4];
    let n = task::fd_stream_read(r, &mut got).map_err(io_err)?;
    check!(n == 3 && &got[..3] == b"kid", "child read returned {n}");
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        task::fd_close(r) && task::fd_close(w),
        "parent cleanup failed"
    );
    // Dropping the child's task closes its copies; the pipe is freed.
    task::harness::reset();
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes survived the fork test",
        pipe::Pipe::live()
    );
    check!(fds_clean(), "a descriptor was left open");
    Ok(())
}

/// `clone` with `CLONE_VM` but without `CLONE_THREAD` (musl's posix_spawn
/// vfork child) creates a child that inherits a *copy* of the descriptor
/// table, is parented to the caller so `wait4`/reaping works, and does not
/// share the caller's address space (so its `exit_group` fallback cannot
/// kill the parent).
pub fn vfork_clone_child() -> Result<(), String> {
    fresh()?;
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "pipe returned {ret:#x}");
    let (r, w) = (fds[0] as usize, fds[1] as usize);

    // CLONE_VM | CLONE_VFORK | SIGCHLD, as musl's posix_spawn passes.
    const CLONE_VM: u64 = 0x0000_0100;
    const CLONE_VFORK: u64 = 0x0000_4000;
    const SIGCHLD: u64 = 17;
    let child = process::linux::dispatch_args_for_test(
        56,
        CLONE_VM | CLONE_VFORK | SIGCHLD,
        0x1fff_0000,
        0,
        0,
    );
    let child = child as usize;
    check!(
        (1..task::MAX_TASKS).contains(&child),
        "vfork clone returned {child:#x}"
    );
    check!(
        task::harness::fd_kind_at(child, r) == task::FdKind::Pipe
            && task::harness::fd_kind_at(child, w) == task::FdKind::Pipe,
        "the vfork child did not inherit the descriptor table"
    );
    check!(
        task::harness::state(child) == Some(task::TaskState::Runnable),
        "the vfork child is not runnable"
    );

    // The parent owns it: finish and reap like a fork child.
    task::harness::finish(child, 0x21);
    let (slot, status) = task::reap_child().ok_or("the vfork child is not reapable")?;
    check!(
        slot == child && status == 0x21,
        "reaped slot {slot} status {status:#x}"
    );
    check!(task::fd_close(r) && task::fd_close(w), "cleanup failed");
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes survived the vfork test",
        pipe::Pipe::live()
    );
    Ok(())
}

/// Soak: 4 MiB through one pipe, then thousands of pipe create/destroy
/// cycles. Asserts exact data, no pipe/fd leaks and a stable descriptor
/// table (a leak would trip the live-pipe cap or `fds_clean`).
pub fn soak_throughput_and_lifecycle() -> Result<(), String> {
    fresh()?;
    const TOTAL: usize = 4 * 1024 * 1024;
    const CHUNK: usize = 4096;
    let pipe = pipe::Pipe::new().ok_or("Pipe::new failed")?;
    pipe.acquire(End::Read);
    pipe.acquire(End::Write);

    let payload: Vec<u8> = (0..CHUNK).map(|i| (i % 251) as u8).collect();
    let mut out = [0u8; CHUNK];
    let (mut written, mut read) = (0usize, 0usize);
    while read < TOTAL {
        if written < TOTAL {
            match pipe.write(&payload, End::Write, true) {
                Ok(n) => written += n,
                Err(pipe::Error::WouldBlock) => {}
                Err(error) => return Err(io_err(error)),
            }
        }
        match pipe.read(End::Read, &mut out, true) {
            Ok(0) => return Err(String::from("soak pipe hit unexpected EOF")),
            Ok(n) => {
                for (i, &byte) in out[..n].iter().enumerate() {
                    // The stream is the payload repeated, so the expected
                    // byte at stream offset `k` is `(k % CHUNK) % 251`.
                    let expected = (((read + i) % CHUNK) % 251) as u8;
                    check!(
                        byte == expected,
                        "soak mismatch at {read}+{i}: {byte} != {expected}"
                    );
                }
                read += n;
            }
            Err(pipe::Error::WouldBlock) => {}
            Err(error) => return Err(io_err(error)),
        }
    }
    check!(written == TOTAL, "pipe accepted {written} of {TOTAL} bytes");
    pipe.release(End::Read);
    pipe.release(End::Write);
    drop(pipe);
    check!(pipe::Pipe::live() == 0, "the soaked pipe was not freed");

    // Create/destroy churn through the descriptor table.
    for round in 0..2000 {
        let pipe = pipe::Pipe::new().ok_or_else(|| format!("round {round}: Pipe::new failed"))?;
        let r = task::fd_open(task::Fd::pipe_end(
            alloc::sync::Arc::clone(&pipe),
            End::Read,
        ))
        .ok_or_else(|| format!("round {round}: fd_open(read) failed"))?;
        let w = task::fd_open(task::Fd::pipe_end(
            alloc::sync::Arc::clone(&pipe),
            End::Write,
        ))
        .ok_or_else(|| format!("round {round}: fd_open(write) failed"))?;
        check!(task::fd_close(r), "round {round}: close(read) failed");
        check!(task::fd_close(w), "round {round}: close(write) failed");
    }
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes leaked after the create/destroy churn",
        pipe::Pipe::live()
    );
    check!(fds_clean(), "the churn leaked a descriptor");
    Ok(())
}
