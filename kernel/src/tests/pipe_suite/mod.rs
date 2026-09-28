//! Pipes, pipe2 and socketpair (issue #135).

use super::*;
use crate::ipc::pipe::{self, End};

const O_NONBLOCK: u64 = 0o4000;

const O_CLOEXEC: u64 = 0o2000000;

const SOCK_STREAM: u64 = 1;

const SOCK_CLOEXEC: u64 = 0o2000000;

const F_GETFD: u64 = 1;

const F_GETFL: u64 = 3;

const F_SETFL: u64 = 4;

const F_DUPFD_CLOEXEC: u64 = 1030;

const EAGAIN: u64 = (-11i64) as u64;

/// Register the kernel task and close any descriptor an earlier test left
/// behind, so pipe-object accounting starts from a clean slate.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    for fd in 3..task::FD_COUNT {
        let _ = task::fd_close(fd);
    }
    check!(
        pipe::Pipe::live() == 0,
        "{} pipes leaked into this test",
        pipe::Pipe::live()
    );
    Ok(())
}

/// Whether every descriptor of the current task from 3 up is closed.
fn fds_clean() -> bool {
    (3..task::FD_COUNT).all(|fd| task::fd_kind(fd) == task::FdKind::Closed)
}

fn io_err(error: pipe::Error) -> String {
    format!("pipe I/O: {error:?}")
}

mod basic;
mod lifecycle;

pub(super) use basic::*;
pub(super) use lifecycle::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("pipe_syscalls_create_and_io", syscalls_create_and_io),
    ("pipe_ring_wrap_roundtrip", ring_wrap_roundtrip),
    ("pipe_blocking_read_write_wake", blocking_read_write_wake),
    ("pipe_eof_epipe_nonblock", eof_epipe_nonblock),
    ("pipe_dup_fork_cloexec", dup_fork_cloexec),
    ("pipe_vfork_clone_child", vfork_clone_child),
    (
        "pipe_soak_throughput_and_lifecycle",
        soak_throughput_and_lifecycle,
    ),
];
