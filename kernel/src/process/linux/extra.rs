//! The second half of the syscall table: the calls added for real-world CLI
//! programs (shells, interpreters, `sqlite3`, `rg`, ...) beyond what `std`
//! and BusyBox first needed. [`super::linux_dispatch`] tries its own table
//! first and falls through to [`dispatch`] before answering `ENOSYS`.

use crate::task;

use super::cwd::AT_FDCWD;
use super::errno::{err, EFAULT, EINTR, EPERM};
use super::{epoll, fd, links, locks, msgio, pathops, procattr, procctl, resources, select, wait};

/// Handle syscall `nr` with arguments `a`, or `None` when it is unknown.
pub(super) fn dispatch(nr: u64, a: [u64; 6]) -> Option<u64> {
    let [a1, a2, a3, a4, a5, a6] = a;
    Some(match nr {
        23 => select::sys_select(a1, a2, a3, a4, a5),
        24 => procattr::sys_sched_yield(),
        28 => procattr::sys_madvise(a1, a2, a3),
        34 => sys_pause(),
        39 => task::linuxstate::tgid() as u64, // getpid: the thread group
        186 => task::current() as u64,         // gettid
        44 => msgio::sys_sendto(a1, a2, a3, a4, a5, a6),
        45 => msgio::sys_recvfrom(a1, a2, a3, a4, a5, a6),
        46 => msgio::sys_sendmsg(a1, a2, a3),
        47 => msgio::sys_recvmsg(a1, a2, a3),
        58 => procctl::sys_fork(), // vfork: a copy-on-write fork is a valid vfork
        61 => wait::sys_wait4(a1, a2, a3, a4),
        73 => locks::sys_flock(a1, a2),
        86 | 88 => links::sys_make_link(a1, AT_FDCWD, a2), // link / symlink
        89 => links::sys_readlink(a1, a2, a3),
        97 => resources::sys_getrlimit(a1, a2),
        98 => resources::sys_getrusage(a1, a2),
        99 => resources::sys_sysinfo(a1),
        100 => resources::sys_times(a1),
        115 => procattr::sys_getgroups(a1, a2),
        116 => procattr::sys_setgroups(a1, a2),
        143 => procattr::sys_sched_getparam(a1, a2),
        145 => procattr::sys_sched_getscheduler(a1),
        146 | 147 => procattr::sys_sched_priority_bound(a1),
        157 => procattr::sys_prctl(a1, a2, a3),
        160 => resources::sys_setrlimit(a1, a2),
        // reboot: only `init` stops the machine (docs/shutdown.md).
        169 => err(EPERM),
        213 => epoll::sys_epoll_create(a1),
        247 => wait::sys_waitid(a1, a2, a3, a4, a5),
        265 => links::sys_linkat(a1, a2, a3, a4),
        266 => links::sys_make_link(a1, a2, a3), // symlinkat(target, dirfd, path)
        267 => links::sys_readlinkat(a1, a2, a3, a4),
        269 | 439 => pathops::sys_faccessat(a1, a2, a3),
        270 => select::sys_pselect6(a1, [a2, a3, a4], a5, a6),
        271 => select::sys_ppoll(a1, a2, a3, a4, a5),
        273 => procattr::sys_set_robust_list(a1, a2),
        274 => procattr::sys_get_robust_list(a1, a2, a3),
        281 => epoll::sys_epoll_pwait(a1, a2, a3, a4, (a5, a6)),
        284 => epoll::sys_eventfd2(a1, 0), // eventfd(initval)
        292 => fd::sys_dup3(a1, a2, a3),
        302 => resources::sys_prlimit64(a1, a2, a3, a4),
        309 => sys_getcpu(a1, a2),
        316 => pathops::sys_renameat2(a1, a2, a3, a4, a5),
        _ => return None,
    })
}

/// `pause()`: sleep until a signal is delivered.
fn sys_pause() -> u64 {
    while task::wait_signal() != task::WakeReason::Interrupted {}
    err(EINTR)
}

/// `getcpu(cpu, node)`: there is one CPU and one node.
fn sys_getcpu(cpu: u64, node: u64) -> u64 {
    for ptr in [cpu, node] {
        if ptr != 0 && crate::user_ptr::try_write::<u32>(ptr, 0).is_err() {
            return err(EFAULT);
        }
    }
    0
}

/// `SIGPIPE` for a write that found no reader (`-EPIPE`), as Linux raises it,
/// unless a send asked for `MSG_NOSIGNAL`. The signal is delivered on the way
/// out of this same syscall: its default ends the process, a program that
/// ignores it (Rust's `std` does) just sees `EPIPE`.
pub(super) fn raise_sigpipe(nr: u64, a: [u64; 6], result: u64) {
    const EPIPE: u64 = (-32i64) as u64;
    const MSG_NOSIGNAL: u64 = 0x4000;
    if result != EPIPE {
        return;
    }
    let flags = match nr {
        1 | 20 | 40 => 0,
        44 => a[3],
        46 => a[2],
        _ => return,
    };
    if flags & MSG_NOSIGNAL != 0 {
        return;
    }
    let me = task::current();
    let _ = task::signal::send_tid(
        me,
        me,
        task::signal::SIGPIPE,
        task::signal::SigInfo::kernel(),
    );
}
