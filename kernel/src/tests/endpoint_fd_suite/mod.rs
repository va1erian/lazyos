//! Messenger endpoints as pollable Linux descriptors (issue #667,
//! `ipc::endpointfd`, docs/architecture/endpoint-fd.md).
//!
//! Readiness is checked from the kernel task with `poll` and `epoll_wait`
//! (timeout 0) through the real Linux syscall dispatcher; the wakeups with a
//! waiter parked on the keyed poll queue and with a kernel thread blocked in
//! `epoll_wait`; the op itself through the native gate in a scratch address
//! space, as a user task would issue it.

use super::*;
use crate::ipc::channels::{self, harness as chan};
use crate::ipc::endpointfd;
use crate::ipc::handles;
use crate::ipc::pipe::{POLLERR, POLLHUP, POLLIN, POLLOUT};

mod abi;
mod readiness;
mod wake;

/// Linux syscall numbers the suite drives.
const SYS_CLOSE: u64 = 3;
const SYS_EPOLL_WAIT: u64 = 232;
const SYS_EPOLL_CTL: u64 = 233;
const SYS_EPOLL_CREATE1: u64 = 291;
const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const EPOLLET: u32 = 0x8000_0000;

/// A Linux syscall with up to four arguments, as the current task.
fn sys(nr: u64, args: [u64; 4]) -> u64 {
    process::linux::dispatch_args6_for_test(nr, [args[0], args[1], args[2], args[3], 0, 0])
}

/// The bring-up state: kernel task current, no channels, nothing watched.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    handles::reset_for_task(task::KERNEL_TASK);
    channels::reset();
    check!(
        endpointfd::live() == 0,
        "{} endpoint watches left from an earlier test",
        endpointfd::live()
    );
    Ok(())
}

fn pair() -> Result<(u64, u64), String> {
    channels::create().map_err(|e| format!("create: {e:?}"))
}

/// A one-way message, the waitset suite's.
fn message() -> Result<Vec<u8>, String> {
    super::waitset_suite::parcel_bytes()
}

fn send(handle: u64, bytes: &[u8]) -> Result<(), String> {
    channels::send(handle, bytes).map_err(|e| format!("send: {e:?}"))
}

/// Take one message from `handle`; whether there was one.
fn take(handle: u64) -> Result<bool, String> {
    channels::try_recv(handle)
        .map(|taken| taken.is_some())
        .map_err(|e| format!("try_recv: {e:?}"))
}

/// A descriptor watching `handle` in the current task's table.
fn watch(handle: u64) -> Result<u64, String> {
    endpointfd::open_fd(handle, false)
        .map(|fd| fd as u64)
        .map_err(|e| format!("open_fd: {e:?}"))
}

/// `poll` revents of `fd` for `events`.
fn revents(fd: u64, events: u16) -> u16 {
    task::fd_poll(fd as usize, events).unwrap_or(u16::MAX)
}

fn close(fd: u64) -> Result<(), String> {
    let ret = sys(SYS_CLOSE, [fd, 0, 0, 0]);
    check!(ret == 0, "close({fd}) returned {ret:#x}");
    Ok(())
}

fn epoll_create() -> Result<u64, String> {
    let ret = sys(SYS_EPOLL_CREATE1, [0, 0, 0, 0]);
    check!((ret as i64) >= 0, "epoll_create1 returned {ret:#x}");
    Ok(ret)
}

/// The packed 12-byte `struct epoll_event`.
fn event(events: u32, data: u64) -> [u8; 12] {
    let mut bytes = [0u8; 12];
    bytes[..4].copy_from_slice(&events.to_le_bytes());
    bytes[4..].copy_from_slice(&data.to_le_bytes());
    bytes
}

fn epoll_add(epfd: u64, fd: u64, events: u32) -> Result<(), String> {
    let request = event(events, fd);
    let ret = sys(
        SYS_EPOLL_CTL,
        [epfd, EPOLL_CTL_ADD, fd, request.as_ptr() as u64],
    );
    check!(ret == 0, "epoll_ctl(ADD {fd}) returned {ret:#x}");
    Ok(())
}

/// `epoll_wait` with `timeout` (ms; 0 polls): the `(events, data)` pairs.
fn epoll_wait(epfd: u64, timeout: i64) -> Result<Vec<(u32, u64)>, String> {
    const MAX: usize = 32;
    let mut buf = [0u8; 12 * MAX];
    let ret = sys(
        SYS_EPOLL_WAIT,
        [epfd, buf.as_mut_ptr() as u64, MAX as u64, timeout as u64],
    );
    check!((ret as i64) >= 0, "epoll_wait returned {ret:#x}");
    Ok(buf
        .chunks_exact(12)
        .take(ret as usize)
        .map(|chunk| {
            let events = u32::from_le_bytes(chunk[..4].try_into().unwrap_or_default());
            let data = u64::from_le_bytes(chunk[4..].try_into().unwrap_or_default());
            (events, data)
        })
        .collect())
}

/// Nothing leaked: no watch, no endpoint registration.
fn nothing_left(context: &str) -> Result<(), String> {
    check!(
        endpointfd::live() == 0,
        "{context}: {} watches leaked",
        endpointfd::live()
    );
    check!(
        chan::total_waiters() == 0,
        "{context}: {} endpoint registrations leaked",
        chan::total_waiters()
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_endpoint_fd_op_abi", abi::op_abi),
    (
        "ipc_endpoint_fd_refuses_bad_handles",
        abi::refuses_bad_handles,
    ),
    ("ipc_endpoint_fd_readiness", readiness::readiness),
    (
        "ipc_endpoint_fd_hangs_up_with_its_handle",
        readiness::hangs_up_with_its_handle,
    ),
    ("ipc_endpoint_fd_close_ordering", readiness::close_ordering),
    (
        "ipc_endpoint_fd_epoll_edge_rearm",
        readiness::epoll_edge_rearm,
    ),
    ("ipc_endpoint_fd_rejects_io", readiness::rejects_io),
    ("ipc_endpoint_fd_keyed_wakeups", wake::keyed_wakeups),
    ("ipc_endpoint_fd_epoll_wait_wakes", wake::epoll_wait_wakes),
    ("ipc_endpoint_fd_epoll_soak", wake::epoll_soak),
    ("ipc_endpoint_fd_blocking_soak", wake::blocking_soak),
];
