//! Linux ABI round 3: what real CLI programs (dash, lua, sqlite3, jq, rg,
//! BusyBox) need beyond `std`: the full futex family, `wait4`'s pid argument
//! and signal statuses, `CLONE_FILES`/`CLONE_FS` sharing, `MSG_PEEK`/
//! `MSG_DONTWAIT`, `select`/`ppoll`, advisory locks, resource reporting,
//! `prctl`/`madvise`/robust lists, links and the fabricated `/etc` and
//! `/proc` files. Each area has correctness tests and a soak.

use super::*;

mod ctty;
mod fdshare;
mod files;
mod futex;
mod locks;
mod msg;
mod resources;
mod select;
mod termios;
mod wait;

use ctty::*;
use fdshare::*;
use files::*;
use futex::*;
use locks::*;
use msg::*;
use resources::*;
use select::*;
use termios::*;
use wait::*;

/// `-errno` as the dispatcher returns it.
const fn neg(errno: i64) -> u64 {
    (-errno) as u64
}

const EPERM: u64 = neg(1);
const ENOENT: u64 = neg(2);
const EBADF: u64 = neg(9);
const ECHILD: u64 = neg(10);
const EAGAIN: u64 = neg(11);
const EFAULT: u64 = neg(14);
const EEXIST: u64 = neg(17);
const EINVAL: u64 = neg(22);
const ENOSYS: u64 = neg(38);
const EOPNOTSUPP: u64 = neg(95);
const ETIMEDOUT: u64 = neg(110);

/// A clean ABI surface for one test: only the kernel task, no stray
/// descriptors.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    for fd in 3..task::harness::fd_table_len() {
        let _ = task::fd_close(fd);
    }
    let table = crate::mem::kernel_table();
    task::register_bumps(
        table.as_u64(),
        process::linux::BRK_BASE,
        process::linux::MMAP_BASE,
    );
    // A ramfs root and `/tmp`, with the account file `/etc/passwd` is built
    // from.
    crate::fs::install_abi_ramfs_for_test();
    let root = crate::fs::vfs::Id::ROOT;
    let fs_error = |error: crate::fs::vfs::FsError| format!("seed: {}", error.message());
    for dir in [fhs::SYSTEM, fhs::SYSTEM_ETC] {
        crate::fs::abi_mkdir(root, dir, 0o755).map_err(fs_error)?;
    }
    crate::fs::abi_create(root, fhs::etc::PASSWD, 0o600).map_err(fs_error)?;
    crate::fs::abi_write(
        root,
        fhs::etc::PASSWD,
        0,
        b"admin:0:0:secret:/home/admin:sh\n",
    )
    .map_err(fs_error)?;
    Ok(())
}

/// Syscall `nr` with up to six arguments.
fn sys(nr: u64, args: &[u64]) -> u64 {
    let mut all = [0u64; 6];
    all[..args.len()].copy_from_slice(args);
    process::linux::dispatch_args6_for_test(nr, all)
}

/// `sys` with user-pointer validation on, for the calls that must answer
/// `EFAULT` (the harness otherwise trusts kernel buffers as user memory).
fn sys_checked(nr: u64, args: &[u64]) -> u64 {
    let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
    let result = sys(nr, args);
    crate::user_ptr::set_trust_kernel_pointers(trusted);
    result
}

/// A pipe's `(read, write)` descriptors.
fn pipe() -> Result<(u64, u64), String> {
    let mut fds = [0i32; 2];
    let ret = sys(22, &[fds.as_mut_ptr() as u64]);
    check!(ret == 0, "pipe returned {ret:#x}");
    Ok((fds[0] as u64, fds[1] as u64))
}

/// A connected `AF_UNIX` stream pair.
fn socketpair() -> Result<(u64, u64), String> {
    let mut sv = [0i32; 2];
    let ret = sys(53, &[1, 1, 0, sv.as_mut_ptr() as u64]);
    check!(ret == 0, "socketpair returned {ret:#x}");
    Ok((sv[0] as u64, sv[1] as u64))
}

/// A NUL-terminated path for a syscall argument.
fn cpath(path: &str) -> Vec<u8> {
    let mut bytes = Vec::from(path.as_bytes());
    bytes.push(0);
    bytes
}

/// The fabricated file at `path`, as text.
fn fabricated(path: &str) -> Result<String, String> {
    let bytes =
        process::linux::proc_file_for_test(path).ok_or(format!("{path} is not fabricated"))?;
    String::from_utf8(bytes).map_err(|_| format!("{path} is not UTF-8"))
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "compat_futex_wake_counts_and_bitsets",
        futex_wake_counts_and_bitsets,
    ),
    (
        "compat_futex_requeue_and_cmp_requeue",
        futex_requeue_and_cmp_requeue,
    ),
    ("compat_futex_wake_op", futex_wake_op),
    (
        "compat_futex_keys_per_address_space",
        futex_keys_per_address_space,
    ),
    (
        "compat_futex_timeouts_and_errors",
        futex_timeouts_and_errors,
    ),
    ("compat_futex_unknown_ops_enosys", futex_unknown_ops_enosys),
    ("compat_futex_soak", futex_soak),
    ("compat_wait4_pid_selection", wait4_pid_selection),
    ("compat_wait4_signal_status", wait4_signal_status),
    ("compat_waitid_reports_exit", waitid_reports_exit),
    ("compat_wait4_soak", wait4_soak),
    ("compat_clone_files_shares_table", clone_files_shares_table),
    (
        "compat_clone_without_files_copies",
        clone_without_files_copies,
    ),
    ("compat_clone_fs_shares_cwd", clone_fs_shares_cwd),
    ("compat_fdshare_soak", fdshare_soak),
    ("compat_msg_peek_and_dontwait", msg_peek_and_dontwait),
    ("compat_msg_sendmsg_recvmsg", msg_sendmsg_recvmsg),
    ("compat_msg_bad_flags", msg_bad_flags),
    ("compat_msg_recvmsg_scatters", msg_recvmsg_scatters),
    ("compat_msg_soak", msg_soak),
    ("compat_msg_recvmsg_soak", msg_recvmsg_soak),
    ("compat_select_and_ppoll", select_and_ppoll),
    ("compat_select_timeout_and_ebadf", select_timeout_and_ebadf),
    ("compat_select_soak", select_soak),
    ("compat_flock_and_record_locks", flock_and_record_locks),
    ("compat_lock_range_overflow", lock_range_overflow),
    ("compat_locks_soak", locks_soak),
    ("compat_rlimits_honest", rlimits_honest),
    ("compat_sysinfo_times_rusage", sysinfo_times_rusage),
    ("compat_prctl_and_madvise", prctl_and_madvise),
    ("compat_robust_list_owner_died", robust_list_owner_died),
    ("compat_resources_soak", resources_soak),
    ("compat_links_and_readlink", links_and_readlink),
    ("compat_etc_files", etc_files),
    ("compat_proc_files", proc_files),
    ("compat_dup3_renameat2_faccessat", dup3_renameat2_faccessat),
    ("compat_files_soak", files_soak),
    ("compat_termios_roundtrip", termios_roundtrip),
    ("compat_termios_canonical_line", termios_canonical_line),
    ("compat_termios_soak", termios_soak),
    ("compat_pty_slave_owner", pty_slave_owner),
    ("compat_ctty_job_control", ctty_job_control),
    ("compat_ctty_soak", ctty_soak),
    (
        "compat_terminal_signal_stays_in_session",
        terminal_signal_stays_in_session,
    ),
    ("compat_terminal_signal_soak", terminal_signal_soak),
];
