//! `pipe`/`pipe2` and `socketpair`: the two syscalls that hand back a pair of
//! connected descriptors in one call. Unix sockets that go through
//! `socket`/`bind`/`connect`/`accept` are [`super::socket`]; this is just the
//! shortcut that skips naming a socket at all.

use alloc::sync::Arc;

use crate::ipc::pipe::{self, End, Side};
use crate::task::{self, Fd};
use crate::user_ptr;

use super::errno::{err, EFAULT, EINVAL, EMFILE};
use super::flags::{
    AF_UNIX, O_CLOEXEC, O_NONBLOCK, SOCK_CLOEXEC, SOCK_NONBLOCK, SOCK_SEQPACKET, SOCK_STREAM,
};

/// `pipe(fds)` and `pipe2(fds, flags)`: a pair of descriptors onto one bounded
/// byte pipe. `O_CLOEXEC` is set on both ends; `O_NONBLOCK` at creation is
/// honored through the pipe's per-end status state.
pub(super) fn sys_pipe(fds: u64, flags: u64) -> u64 {
    if fds == 0 {
        return err(EFAULT);
    }
    let Some(pipe) = pipe::Pipe::new() else {
        return err(EMFILE);
    };
    let Some(read_fd) = task::fd_open(Fd::pipe_end(Arc::clone(&pipe), End::Read)) else {
        return err(EMFILE);
    };
    let Some(write_fd) = task::fd_open(Fd::pipe_end(Arc::clone(&pipe), End::Write)) else {
        task::fd_close(read_fd);
        return err(EMFILE);
    };
    if flags & O_CLOEXEC != 0 {
        task::fd_set_cloexec(read_fd, true);
        task::fd_set_cloexec(write_fd, true);
    }
    if flags & O_NONBLOCK != 0 {
        pipe.set_nonblock(End::Read, true);
        pipe.set_nonblock(End::Write, true);
    }
    // Safety: user array of two `int` descriptors (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<i32>(fds, read_fd as i32);
        user_ptr::write::<i32>(fds + 4, write_fd as i32);
    }
    0
}

/// `socketpair(AF_UNIX, SOCK_STREAM|SOCK_SEQPACKET, 0, sv)`: a new pair of
/// byte-stream endpoints. `SOCK_SEQPACKET` is accepted but message boundaries
/// are not preserved (std sends one fixed 8-byte record; see
/// `crate::ipc::pipe::SocketPair`). `SOCK_CLOEXEC`/`SOCK_NONBLOCK` are honored.
pub(super) fn sys_socketpair(domain: u64, kind: u64, protocol: u64, sv: u64) -> u64 {
    let base = kind & 0xf;
    if domain != AF_UNIX || (base != SOCK_STREAM && base != SOCK_SEQPACKET) || protocol != 0 {
        return err(EINVAL);
    }
    if sv == 0 {
        return err(EFAULT);
    }
    let pair = if base == SOCK_SEQPACKET {
        pipe::SocketPair::new_seqpacket()
    } else {
        pipe::SocketPair::new()
    };
    let Some(pair) = pair else {
        return err(EMFILE);
    };
    let Some(a) = task::fd_open(Fd::socket_side(Arc::clone(&pair), Side::A)) else {
        return err(EMFILE);
    };
    let Some(b) = task::fd_open(Fd::socket_side(Arc::clone(&pair), Side::B)) else {
        task::fd_close(a);
        return err(EMFILE);
    };
    if kind & SOCK_CLOEXEC != 0 {
        task::fd_set_cloexec(a, true);
        task::fd_set_cloexec(b, true);
    }
    if kind & SOCK_NONBLOCK != 0 {
        pair.set_nonblock(Side::A, true);
        pair.set_nonblock(Side::B, true);
    }
    // Safety: user array of two `int` descriptors (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<i32>(sv, a as i32);
        user_ptr::write::<i32>(sv + 4, b as i32);
    }
    0
}
