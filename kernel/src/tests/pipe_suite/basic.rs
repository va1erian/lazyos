//! `pipe`/`pipe2`/`socketpair` creation, ring-buffer wraparound, and
//! blocking read/write wakeups.

use super::*;

/// `pipe`, `pipe2` and `socketpair` through the real syscall dispatch:
/// creation flags (`O_CLOEXEC`, `O_NONBLOCK`), `F_GETFL`/`F_SETFL`,
/// `F_GETFD`, data flow, EOF and close.
pub fn syscalls_create_and_io() -> Result<(), String> {
    fresh()?;

    // pipe(fds): no flags, blocking ends.
    let mut fds = [0i32; 2];
    let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "pipe returned {ret:#x}");
    let (r, w) = (fds[0] as usize, fds[1] as usize);
    check!(
        task::fd_kind(r) == task::FdKind::Pipe && task::fd_kind(w) == task::FdKind::Pipe,
        "pipe fds {r}/{w} have wrong kinds"
    );
    check!(
        process::linux::dispatch_for_test(72, r as u64, F_GETFL, 0) == 0,
        "F_GETFL on a plain read end is not O_RDONLY"
    );
    let msg = b"ping";
    let n = process::linux::dispatch_for_test(1, w as u64, msg.as_ptr() as u64, msg.len() as u64);
    check!(n == msg.len() as u64, "pipe write returned {n:#x}");
    let mut buf = [0u8; 16];
    let n =
        process::linux::dispatch_for_test(0, r as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(n == 4 && &buf[..4] == b"ping", "pipe read returned {n}");
    check!(task::fd_close(w), "closing the write end failed");
    let n =
        process::linux::dispatch_for_test(0, r as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(
        n == 0,
        "read after last writer close returned {n:#x}, expected EOF"
    );

    // pipe2(fds, O_CLOEXEC | O_NONBLOCK).
    let ret =
        process::linux::dispatch_for_test(293, fds.as_mut_ptr() as u64, O_CLOEXEC | O_NONBLOCK, 0);
    check!(ret == 0, "pipe2 returned {ret:#x}");
    let (r2, w2) = (fds[0] as usize, fds[1] as usize);
    check!(
        task::fd_cloexec(r2) && task::fd_cloexec(w2),
        "pipe2 ignored O_CLOEXEC"
    );
    check!(
        process::linux::dispatch_for_test(72, r2 as u64, F_GETFL, 0) == O_NONBLOCK,
        "F_GETFL does not report O_NONBLOCK"
    );
    check!(
        process::linux::dispatch_for_test(72, r2 as u64, F_GETFD, 0) == 1,
        "F_GETFD does not report FD_CLOEXEC"
    );
    let got =
        process::linux::dispatch_for_test(0, r2 as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(got == EAGAIN, "empty O_NONBLOCK read returned {got:#x}");
    check!(
        process::linux::dispatch_for_test(72, r2 as u64, F_SETFL, 0) == 0,
        "F_SETFL(0) failed"
    );
    check!(
        task::fd_status(r2) == Some(0),
        "F_SETFL(0) did not clear O_NONBLOCK: {:?}",
        task::fd_status(r2)
    );
    // F_DUPFD_CLOEXEC shares the pipe end and sets FD_CLOEXEC on the copy.
    let dup = process::linux::dispatch_for_test(72, r2 as u64, F_DUPFD_CLOEXEC, 7);
    let dup = dup as usize;
    check!(
        dup >= 7 && task::fd_kind(dup) == task::FdKind::Pipe && task::fd_cloexec(dup),
        "F_DUPFD_CLOEXEC returned {dup}"
    );
    check!(
        task::fd_close(dup),
        "closing the F_DUPFD_CLOEXEC copy failed"
    );

    // socketpair: AF_UNIX + SOCK_STREAM, data crosses both ways.
    let mut sv = [0i32; 2];
    let ret = process::linux::dispatch_args_for_test(
        53,
        1,
        SOCK_STREAM | SOCK_CLOEXEC,
        0,
        sv.as_mut_ptr() as u64,
    );
    check!(ret == 0, "socketpair returned {ret:#x}");
    let (a, b) = (sv[0] as usize, sv[1] as usize);
    check!(
        task::fd_kind(a) == task::FdKind::Socket && task::fd_kind(b) == task::FdKind::Socket,
        "socketpair fds {a}/{b} have wrong kinds"
    );
    check!(
        task::fd_cloexec(a) && task::fd_cloexec(b),
        "SOCK_CLOEXEC ignored"
    );
    let n = process::linux::dispatch_for_test(1, a as u64, msg.as_ptr() as u64, msg.len() as u64);
    check!(n == 4, "socketpair write returned {n:#x}");
    let n =
        process::linux::dispatch_for_test(0, b as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(
        n == 4 && &buf[..4] == b"ping",
        "socketpair read returned {n}"
    );
    // musl implements send/recv with sendto/recvfrom: std's capture path
    // reads the socket with `recvfrom`, so both must work (and pipes must
    // answer -ENOTSOCK).
    let n = process::linux::dispatch_for_test(44, a as u64, msg.as_ptr() as u64, msg.len() as u64);
    check!(n == 4, "sendto returned {n:#x}");
    let n =
        process::linux::dispatch_for_test(45, b as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(n == 4 && &buf[..4] == b"ping", "recvfrom returned {n}");
    let enotsock = (-88i64) as u64;
    let n =
        process::linux::dispatch_for_test(45, r as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(n == enotsock, "recvfrom on a pipe returned {n:#x}");

    check!(task::fd_close(a), "closing socket side A failed");
    let n =
        process::linux::dispatch_for_test(0, b as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(n == 0, "socketpair read after peer close returned {n:#x}");

    // Cleanup: closing every fd drops the live-pipe count back to zero.
    for fd in [r, r2, w2, b] {
        check!(task::fd_close(fd), "cleanup close of {fd} failed");
    }
    check!(fds_clean(), "a descriptor was left open");
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes survived the test",
        pipe::Pipe::live()
    );
    Ok(())
}

/// A pipe whose tail wraps the ring returns exactly the bytes written, in
/// order, across the wrap.
pub fn ring_wrap_roundtrip() -> Result<(), String> {
    fresh()?;
    let pipe = pipe::Pipe::new().ok_or("Pipe::new failed")?;
    pipe.acquire(End::Read);
    pipe.acquire(End::Write);
    let cap = pipe::CAPACITY;

    let first: Vec<u8> = (0..cap - 4).map(|i| (i % 251) as u8).collect();
    let n = pipe.write(&first, End::Write, true).map_err(io_err)?;
    check!(
        n == first.len(),
        "first write accepted {n} of {}",
        first.len()
    );
    let mut head = [0u8; 16];
    let n = pipe.read(End::Read, &mut head, true).map_err(io_err)?;
    check!(n == 16 && head == first[..16], "head read is wrong");

    // This write lands where the head used to be, so the tail wraps. The
    // ring has exactly 20 bytes free, so the non-blocking write is partial.
    let tail: Vec<u8> = (0..20u8).map(|i| 0xA0u8.wrapping_add(i)).collect();
    let n = pipe.write(&tail, End::Write, true).map_err(io_err)?;
    check!(n == tail.len(), "wrap write accepted {n}");

    let mut out = vec![0u8; first.len() - 16 + tail.len()];
    let n = pipe.read(End::Read, &mut out, true).map_err(io_err)?;
    check!(n == out.len(), "tail read returned {n} of {}", out.len());
    check!(
        &out[..first.len() - 16] == &first[16..],
        "wrapped data mismatch"
    );
    check!(&out[first.len() - 16..] == &tail[..], "tail data mismatch");

    pipe.release(End::Read);
    pipe.release(End::Write);
    drop(pipe);
    check!(pipe::Pipe::live() == 0, "the wrapped pipe was not freed");
    Ok(())
}

/// Parking on the pipe queues and triggering the event wakes the task
/// synchronously with `Woken` (the mechanism blocking I/O is built on).
pub fn blocking_read_write_wake() -> Result<(), String> {
    fresh()?;
    let pipe = pipe::Pipe::new().ok_or("Pipe::new failed")?;
    pipe.acquire(End::Read);
    pipe.acquire(End::Write);
    let me = task::current();

    // A reader parked on an empty pipe is woken by a write.
    pipe.park_reader(me);
    check!(
        matches!(
            task::harness::state(me),
            Some(task::TaskState::Blocked { .. })
        ),
        "park_reader did not block the current task"
    );
    let n = pipe.write(b"x", End::Write, true).map_err(io_err)?;
    check!(n == 1, "write accepted {n}");
    check!(
        task::harness::state(me) == Some(task::TaskState::Runnable),
        "a write did not wake the parked reader: {:?}",
        task::harness::state(me)
    );
    check!(
        task::harness::take_wake_reason(me) == Some(task::WakeReason::Woken),
        "reader wake reason is not Woken"
    );
    let mut one = [0u8; 1];
    check!(
        pipe.read(End::Read, &mut one, true).map_err(io_err)? == 1 && one[0] == b'x',
        "the woken reader did not get the written byte"
    );

    // A writer parked on a full pipe is woken by a read.
    let fill = vec![7u8; pipe::CAPACITY];
    let n = pipe.write(&fill, End::Write, true).map_err(io_err)?;
    check!(n == fill.len(), "fill write accepted {n}");
    check!(
        pipe.write(b"y", End::Write, true) == Err(pipe::Error::WouldBlock),
        "a full non-blocking pipe accepted more bytes"
    );
    pipe.park_writer(me);
    check!(
        matches!(
            task::harness::state(me),
            Some(task::TaskState::Blocked { .. })
        ),
        "park_writer did not block the current task"
    );
    let mut byte = [0u8; 1];
    check!(
        pipe.read(End::Read, &mut byte, true).map_err(io_err)? == 1,
        "the drain read did not return a byte"
    );
    check!(
        task::harness::state(me) == Some(task::TaskState::Runnable),
        "a read did not wake the parked writer: {:?}",
        task::harness::state(me)
    );
    check!(
        task::harness::take_wake_reason(me) == Some(task::WakeReason::Woken),
        "writer wake reason is not Woken"
    );
    check!(
        pipe.write(b"y", End::Write, true).map_err(io_err)? == 1,
        "space freed by the read did not accept a byte"
    );

    pipe.release(End::Read);
    pipe.release(End::Write);
    drop(pipe);
    check!(pipe::Pipe::live() == 0, "the pipe was not freed");
    Ok(())
}
