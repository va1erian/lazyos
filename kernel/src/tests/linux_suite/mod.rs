//! Linux ABI round 2: mremap, epoll/eventfd, seqpacket, unix sockets,
//! and the nanosleep/clock_nanosleep family.

use super::*;
use crate::ipc::pipe;
use crate::ipc::unix;

const PAGE: u64 = 4096;

const PROT_RW: u64 = 3;

const MAP_PRIVATE: u64 = 0x02;

const MAP_FIXED: u64 = 0x10;

const MAP_ANONYMOUS: u64 = 0x20;

const MREMAP_MAYMOVE: u64 = 1;

const MREMAP_FIXED: u64 = 2;

const AF_UNIX: u64 = 1;

const SOCK_STREAM: u64 = 1;

const SOCK_SEQPACKET: u64 = 5;

const SOCK_CLOEXEC: u64 = 0o2000000;

const EPOLL_CTL_ADD: u64 = 1;

const EPOLL_CTL_DEL: u64 = 2;

const EPOLL_CTL_MOD: u64 = 3;

const EPOLLIN: u32 = 0x0001;

const EPOLLHUP: u32 = 0x0010;

const EPOLLET: u32 = 0x8000_0000;

const O_NONBLOCK: u64 = 0o4000;

const EFD_SEMAPHORE: u64 = 1;

const EAGAIN: u64 = (-11i64) as u64;

const EEXIST: u64 = (-17i64) as u64;

const ENOENT: u64 = (-2i64) as u64;

const EINVAL: u64 = (-22i64) as u64;

const EMSGSIZE: u64 = (-90i64) as u64;

const CLOCK_REALTIME: u64 = 0;

const CLOCK_MONOTONIC: u64 = 1;

const TIMER_ABSTIME: u64 = 1;

/// Register the kernel task with a bump region, close leftover
/// descriptors, and forget any bound socket names, so each test starts
/// from a clean ABI surface.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    // A fork left behind by an earlier test (e.g. an unreaped
    // `spawn_fork` peer) would otherwise sit in the table as a real
    // `Runnable` competitor: harmless while the kernel task itself never
    // blocks, but a genuine hijack risk for the tests below that call a
    // blocking syscall (`nanosleep`/`clock_nanosleep`) for real, since
    // `pick_next_best` would rather run any other `Runnable` slot than
    // let the CPU idle.
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    for fd in 3..task::FD_COUNT {
        let _ = task::fd_close(fd);
    }
    unix::clear_for_test();
    let table = crate::mem::kernel_table();
    task::register_bumps(
        table.as_u64(),
        process::linux::BRK_BASE,
        process::linux::MMAP_BASE,
    );
    Ok(())
}

fn fds_clean() -> bool {
    (3..task::FD_COUNT).all(|fd| task::fd_kind(fd) == task::FdKind::Closed)
}

fn mmap_fixed(base: u64, len: u64) -> u64 {
    process::linux::dispatch_args5_for_test(
        9,
        base,
        len,
        PROT_RW,
        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
        0,
    )
}

fn mremap(old: u64, old_size: u64, new_size: u64, flags: u64, new_addr: u64) -> u64 {
    process::linux::dispatch_args5_for_test(25, old, old_size, new_size, flags, new_addr)
}

fn munmap(base: u64, len: u64) -> u64 {
    process::linux::dispatch_for_test(11, base, len, 0)
}

/// Deterministic fill pattern, mirroring the pipe soak's.
fn fill(addr: u64, seed: u8, len: usize) {
    for offset in 0..len {
        let byte = seed ^ (offset as u8).wrapping_mul(31);
        // Safety: the test maps this page range into the kernel's user half.
        unsafe { (addr as *mut u8).add(offset).write_volatile(byte) };
    }
}

fn matches(addr: u64, seed: u8, len: usize) -> bool {
    (0..len).all(|offset| {
        let byte = seed ^ (offset as u8).wrapping_mul(31);
        // Safety: same mapped range as `fill`.
        unsafe { (addr as *const u8).add(offset).read_volatile() == byte }
    })
}

fn epoll_event(events: u32, data: u64) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[..4].copy_from_slice(&events.to_le_bytes());
    buf[4..].copy_from_slice(&data.to_le_bytes());
    buf
}

/// Unpack one packed `struct epoll_event` from a ready array.
fn unpack_event(out: &[u8]) -> (u32, u64) {
    let events = u32::from_le_bytes(out[..4].try_into().unwrap());
    let data = u64::from_le_bytes(out[4..12].try_into().unwrap());
    (events, data)
}

fn epoll_ctl(epfd: u64, op: u64, fd: u64, event: &[u8; 12]) -> u64 {
    process::linux::dispatch_args_for_test(233, epfd, op, fd, event.as_ptr() as u64)
}

fn epoll_wait0(epfd: u64, out: &mut [u8]) -> u64 {
    let max = (out.len() / 12) as u64;
    process::linux::dispatch_args_for_test(232, epfd, out.as_ptr() as u64, max, 0)
}

fn read_fd(fd: u64, buf: &mut [u8]) -> u64 {
    process::linux::dispatch_for_test(0, fd, buf.as_mut_ptr() as u64, buf.len() as u64)
}

fn write_fd(fd: u64, buf: &[u8]) -> u64 {
    process::linux::dispatch_for_test(1, fd, buf.as_ptr() as u64, buf.len() as u64)
}

fn socketpair(kind: u64) -> Result<(u64, u64), String> {
    let mut sv = [0i32; 2];
    let ret = process::linux::dispatch_args_for_test(53, AF_UNIX, kind, 0, sv.as_mut_ptr() as u64);
    check!(ret == 0, "socketpair({kind:#x}) returned {ret:#x}");
    Ok((sv[0] as u64, sv[1] as u64))
}

/// `clock_gettime(clock, ..)`, returning `(sec, nsec)`.
fn clock_now(clock: u64) -> (i64, i64) {
    let mut out = [0i64; 2];
    process::linux::dispatch_for_test(228, clock, out.as_mut_ptr() as u64, 0);
    (out[0], out[1])
}

/// `(sec, nsec)` normalized after adding `add_nsec` nanoseconds.
fn add_nanos(sec: i64, nsec: i64, add_nsec: i64) -> (i64, i64) {
    let mut sec = sec;
    let mut nsec = nsec + add_nsec;
    while nsec >= 1_000_000_000 {
        nsec -= 1_000_000_000;
        sec += 1;
    }
    (sec, nsec)
}

/// `nanosleep(req, rem)` (syscall 35): always relative, no clock argument.
fn nanosleep(req: &[i64; 2], rem: &mut [i64; 2]) -> u64 {
    process::linux::dispatch_for_test(35, req.as_ptr() as u64, rem.as_mut_ptr() as u64, 0)
}

/// `clock_nanosleep(clockid, flags, req, rem)` (syscall 230).
fn clock_nanosleep(clock: u64, flags: u64, req: &[i64; 2], rem: &mut [i64; 2]) -> u64 {
    process::linux::dispatch_args_for_test(
        230,
        clock,
        flags,
        req.as_ptr() as u64,
        rem.as_mut_ptr() as u64,
    )
}

mod creds;
mod epoll;
mod mmap_reuse;
mod mremap_eventfd;
mod nanosleep_clock;
mod random;
mod sendfile;
mod seqpacket_unix;

pub(super) use creds::*;
pub(super) use epoll::*;
pub(super) use mmap_reuse::*;
pub(super) use mremap_eventfd::*;
pub(super) use nanosleep_clock::*;
pub(super) use random::*;
pub(super) use sendfile::*;
pub(super) use seqpacket_unix::*;

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "linux_getuid_family_reports_credentials",
        getuid_family_reports_credentials,
    ),
    (
        "linux_setuid_unprivileged_refused",
        setuid_unprivileged_refused,
    ),
    (
        "linux_setuid_privileged_drop_is_irreversible",
        setuid_privileged_drop_is_irreversible,
    ),
    (
        "linux_setres_partial_and_unchanged",
        setres_partial_and_unchanged,
    ),
    ("linux_setuid_soak_never_widens", setuid_soak_never_widens),
    ("linux_chacha20_rfc8439_block", chacha20_rfc8439_block),
    (
        "linux_getrandom_unique_within_tick",
        getrandom_unique_within_tick,
    ),
    ("linux_getrandom_statistics", getrandom_statistics),
    ("linux_entropy_reseeds", entropy_reseeds),
    ("linux_getrandom_soak", getrandom_soak),
    ("linux_mmap_reuses_freed_range", mmap_reuses_freed_range),
    (
        "linux_mmap_skips_a_large_mapping",
        mmap_skips_a_large_mapping_in_one_step,
    ),
    (
        "linux_mmap_munmap_soak_does_not_exhaust_region",
        mmap_munmap_soak_does_not_exhaust_region,
    ),
    ("linux_mremap_grow_shrink_move", mremap_grow_shrink_move),
    ("linux_mremap_soak_churn", mremap_soak_churn),
    ("linux_eventfd_semantics", eventfd_semantics),
    ("linux_epoll_level_edge_hangup", epoll_level_edge_hangup),
    ("linux_epoll_edge_over_maxevents", epoll_edge_over_maxevents),
    (
        "linux_epoll_level_does_not_starve",
        epoll_level_does_not_starve,
    ),
    (
        "linux_epoll_soak_add_wait_cycles",
        epoll_soak_add_wait_cycles,
    ),
    ("linux_seqpacket_boundaries", seqpacket_boundaries),
    ("linux_seqpacket_soak_messages", seqpacket_soak_messages),
    ("linux_unix_pair_eof_shutdown", unix_pair_eof_shutdown),
    (
        "linux_unix_pathname_bind_connect_accept",
        unix_pathname_bind_connect_accept,
    ),
    ("linux_unix_write_before_accept", unix_write_before_accept),
    ("linux_unix_pathname_soak", unix_pathname_soak),
    (
        "linux_nanosleep_relative_duration",
        nanosleep_relative_duration,
    ),
    (
        "linux_clock_nanosleep_absolute_past_returns_immediately",
        clock_nanosleep_absolute_past_returns_immediately,
    ),
    (
        "linux_clock_nanosleep_absolute_future_waits_until_deadline",
        clock_nanosleep_absolute_future_waits_until_deadline,
    ),
    (
        "linux_clock_nanosleep_bad_clock_and_flags",
        clock_nanosleep_bad_clock_and_flags,
    ),
    (
        "linux_clock_nanosleep_soak_absolute",
        clock_nanosleep_soak_absolute,
    ),
    (
        "linux_sendfile_pipe_to_pipe_copies_and_accounts",
        sendfile_pipe_to_pipe_copies_and_accounts,
    ),
    (
        "linux_sendfile_rejects_positional_and_bad_descriptors",
        sendfile_rejects_positional_and_bad_descriptors,
    ),
    (
        "linux_sendfile_refuses_nonblocking_stream_destination",
        sendfile_refuses_nonblocking_stream_destination,
    ),
    (
        "linux_sendfile_soak_cycles_no_leaks",
        sendfile_soak_cycles_no_leaks,
    ),
];
