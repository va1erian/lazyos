//! Linux x86_64 ABI: loading static ELFs and the syscall dispatch.
//!
//! Only the subset `std`/musl need to reach `main` is implemented; everything
//! else is logged as `ENOSYS` (see `tools/abi/coverage.py`).
//!
//! The syscall surface is split by family into sibling modules: [`elf`] loads
//! the initial image and start stack, [`io`] is `read`/`write`/`poll`,
//! [`mem`] the `mmap` family, [`fd`] the descriptor table and its metadata,
//! [`path`] and [`pathops`] path resolution and the naming syscalls (`open`,
//! `mkdir`, `rename`, ...), [`stat`] the `stat` family, [`time`] clocks and
//! sleeping, [`misc`] odds and ends (`ioctl`, `arch_prctl`, `uname`, ...),
//! [`pipes`] and [`socket`] IPC endpoints, [`epoll`] readiness polling and
//! `eventfd`, [`futex`] the wait/wake primitive, [`sig`] signals, and
//! [`procctl`] process/thread lifecycle and process-group syscalls.
//! [`errno`], [`flags`] and [`uaccess`] hold the small pieces shared across
//! several of them. This file itself only wires a syscall number to its
//! handler ([`linux_dispatch`]) and re-exports the few items other kernel
//! code needs by name.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::task;

mod creds;
mod elf;
mod epoll;
mod errno;
mod fd;
mod flags;
mod futex;
mod io;
mod mem;
mod misc;
mod path;
mod pathops;
mod pipes;
mod procctl;
mod sig;
mod socket;
mod stat;
mod time;
mod uaccess;

pub use elf::load;
// Only the `#[cfg(lazyos_tests)]` harness (`kernel/src/tests.rs`) reaches this
// through the `process::linux::` path; a normal build never does, hence the
// otherwise-unused-import warning this silences.
#[allow(unused_imports)]
pub use fd::close_cloexec_fds;

/// Resolve an executable for a native `spawn` of a Linux program: the named FAT
/// file, the FAT-root basename, or a BusyBox applet alias. `None` when no such
/// entry exists. `process::spawn_line` uses this so a `linux:sh` command reaches
/// the BusyBox multiplexer as `argv[0] = "sh"` (issue #254).
pub fn load_executable(path: &str) -> Option<alloc::vec::Vec<u8>> {
    path::load_executable(path).ok()
}

// User memory layout for Linux tasks (kept clear of code and each other).
/// `brk` region (grows up).
pub const BRK_BASE: u64 = 0x0100_0000;
/// Upper bound of the `brk` region.
pub const BRK_LIMIT: u64 = 0x1f00_0000;
/// Anonymous `mmap` region (bumps up).
pub const MMAP_BASE: u64 = 0x4000_0000;
/// Upper bound of the `mmap` region.
pub const MMAP_LIMIT: u64 = 0x7000_0000;
/// User stack top (grows down from here).
pub const STACK_TOP: u64 = 0x0200_0000;
/// User stack size.
pub const STACK_SIZE: u64 = 0x0010_0000;

/// Page size shared by every syscall that rounds an address or length to it.
const PAGE: u64 = 4096;

/// Syscall numbers we have dispatched at least once (for the coverage report).
static SYSCALL_SEEN: [AtomicBool; 512] = [const { AtomicBool::new(false) }; 512];

/// x86_64 syscall names for the ones the shim is likely to meet.
fn syscall_name(nr: u64) -> &'static str {
    match nr {
        0 => "read",
        1 => "write",
        2 => "open",
        3 => "close",
        4 => "stat",
        5 => "fstat",
        6 => "lstat",
        7 => "poll",
        8 => "lseek",
        9 => "mmap",
        10 => "mprotect",
        11 => "munmap",
        12 => "brk",
        13 => "rt_sigaction",
        14 => "rt_sigprocmask",
        15 => "rt_sigreturn",
        16 => "ioctl",
        19 => "readv",
        20 => "writev",
        21 => "access",
        22 => "pipe",
        23 => "select",
        24 => "sched_yield",
        25 => "mremap",
        28 => "madvise",
        32 => "dup",
        33 => "dup2",
        35 => "nanosleep",
        39 => "getpid",
        40 => "sendfile",
        41 => "socket",
        42 => "connect",
        43 => "accept",
        44 => "sendto",
        45 => "recvfrom",
        48 => "shutdown",
        49 => "bind",
        50 => "listen",
        51 => "getsockname",
        52 => "getpeername",
        53 => "socketpair",
        56 => "clone",
        57 => "fork",
        58 => "vfork",
        59 => "execve",
        60 => "exit",
        61 => "wait4",
        62 => "kill",
        63 => "uname",
        72 => "fcntl",
        73 => "flock",
        78 => "getdents",
        79 => "getcwd",
        80 => "chdir",
        82 => "rename",
        83 => "mkdir",
        84 => "rmdir",
        86 => "link",
        87 => "unlink",
        89 => "readlink",
        90 => "chmod",
        92 => "chown",
        95 => "umask",
        96 => "gettimeofday",
        97 => "getrlimit",
        99 => "sysinfo",
        102 => "getuid",
        103 => "getgid",
        104 => "geteuid",
        105 => "getegid",
        106 => "setuid",
        107 => "setgid",
        109 => "setpgid",
        110 => "getppid",
        111 => "getpgrp",
        112 => "setsid",
        113 => "setreuid",
        114 => "setregid",
        115 => "getgroups",
        116 => "setgroups",
        121 => "getpgid",
        124 => "getsid",
        131 => "sigaltstack",
        157 => "prctl",
        158 => "arch_prctl",
        186 => "gettid",
        200 => "tkill",
        202 => "futex",
        204 => "sched_getaffinity",
        217 => "getdents64",
        218 => "set_tid_address",
        228 => "clock_gettime",
        229 => "clock_getres",
        230 => "clock_nanosleep",
        231 => "exit_group",
        232 => "epoll_wait",
        233 => "epoll_ctl",
        234 => "tgkill",
        257 => "openat",
        258 => "mkdirat",
        259 => "mknodat",
        260 => "fchownat",
        262 => "newfstatat",
        263 => "unlinkat",
        264 => "renameat",
        271 => "ppoll",
        273 => "set_robust_list",
        275 => "splice",
        288 => "accept4",
        290 => "eventfd2",
        291 => "epoll_create1",
        293 => "pipe2",
        302 => "prlimit64",
        318 => "getrandom",
        332 => "statx",
        334 => "rseq",
        _ => "unknown",
    }
}

/// Announce each syscall number the first time it is dispatched, and any that
/// are unimplemented, so `tools/abi/coverage.py` can report what was used.
fn trace_syscall(nr: u64) {
    let index = nr as usize;
    if index < SYSCALL_SEEN.len() && !SYSCALL_SEEN[index].swap(true, Ordering::Relaxed) {
        crate::serial_println!("SYSCALL_USED {} {}", nr, syscall_name(nr));
    }
}

/// Test-harness entry into the syscall dispatcher (issue #62), compiled only
/// with `LAZYOS_TESTS=1`.
#[cfg(lazyos_tests)]
pub fn dispatch_for_test(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    dispatch_args_for_test(nr, a1, a2, a3, 0)
}

/// [`dispatch_for_test`] with a fourth argument (`socketpair`'s `sv`).
#[cfg(lazyos_tests)]
pub fn dispatch_args_for_test(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    dispatch_args5_for_test(nr, a1, a2, a3, a4, 0)
}

/// [`dispatch_for_test`] with five arguments (`mremap`'s `new_address`).
///
/// The real `int 0x80` gate is an interrupt gate, so a syscall body always
/// starts with interrupts disabled (`sys_read_char`'s and `exit`'s own
/// `enable()` calls exist precisely because of that guarantee); a blocking
/// body such as `sys_clock_nanosleep` relies on it too, to keep `park`'s
/// register-then-block sequence atomic against the timer (see
/// `task::wait::WaitQueue::wait`), and `enable_and_hlt` leaves interrupts
/// enabled once it returns (real hardware restores the caller's flags via
/// `iretq`, which this harness entry has no equivalent of). The rest of the
/// kernel test suite runs with interrupts off between the narrow windows
/// that explicitly enable them, so calling `linux_dispatch` as a plain
/// function - without a gate to save and restore that state - would leave
/// every later test preempted by a real, unexpected timer tick if we did not
/// force it back off here.
#[cfg(lazyos_tests)]
pub fn dispatch_args5_for_test(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    x86_64::instructions::interrupts::disable();
    let result = linux_dispatch(nr, a1, a2, a3, a4, a5, 0);
    x86_64::instructions::interrupts::disable();
    result
}

/// Syscall dispatch, called from `arch::linux` (Linux ABI: nr in `rax`, args in
/// `rdi,rsi,rdx,r10,r8,r9`, result in `rax`).
#[no_mangle]
extern "C" fn linux_dispatch(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> u64 {
    // Finished parentless tasks (threads) were flagged by the scheduler and are
    // reclaimed here, on entry to a syscall: the current task holds no heap
    // lock, so dropping their buffers cannot deadlock (issue #133). Interrupts
    // are off inside the gate.
    task::reclaim_pending();
    trace_syscall(nr);
    let result = match nr {
        0 => io::sys_read(a1, a2, a3),
        1 => io::sys_write(a1, a2, a3),
        2 => path::sys_openat(path::AT_FDCWD, a1, a2, a3), // open
        3 => fd::sys_close(a1),
        4 => stat::sys_stat_path(a1, a2), // stat(path, buf)
        5 => stat::sys_fstat(a1, a2),     // fstat(fd, buf)
        6 => stat::sys_stat_path(a1, a2), // lstat(path, buf)
        7 => io::sys_poll(a1, a2, a3),    // poll
        8 => fd::sys_lseek(a1, a2, a3),   // lseek
        9 => mem::sys_mmap(a1, a2, a3, a4),
        10 => mem::sys_mprotect(a1, a2, a3),
        11 => mem::sys_munmap(a1, a2),
        12 => mem::sys_brk(a1),
        13 => sig::sys_rt_sigaction(a1, a2, a3, a4),
        14 => sig::sys_rt_sigprocmask(a1, a2, a3, a4),
        15 => sig::sys_rt_sigreturn(),
        16 => misc::sys_ioctl(a1, a2, a3),
        19 => io::sys_readv(a1, a2, a3),
        20 => io::sys_writev(a1, a2, a3),
        21 => pathops::sys_access(a1, a2), // access(path, mode)
        22 => pipes::sys_pipe(a1, 0),      // pipe(fds)
        25 => mem::sys_mremap(a1, a2, a3, a4, a5), // mremap(old, old_size, new_size, flags, new)
        28 => 0,                           // madvise
        32 | 33 => fd::sys_dup(nr, a1, a2), // dup / dup2
        // nanosleep(req, rem) is always relative, so the clock is irrelevant.
        35 => time::sys_clock_nanosleep(time::CLOCK_MONOTONIC, 0, a1, a2),
        39 | 186 => task::current() as u64, // getpid/gettid: pid == slot (#59)
        40 => io::sys_sendfile(a1, a2, a3, a4), // sendfile(out, in, offset, count)
        41 => socket::sys_socket(a1, a2, a3), // socket(domain, type, protocol)
        42 => socket::sys_connect(a1, a2, a3), // connect(fd, addr, len)
        43 => socket::sys_accept(a1, a2, a3, 0), // accept(fd, addr, addrlen)
        44 => socket::sys_sendto(a1, a2, a3), // sendto (musl's send)
        45 => socket::sys_recvfrom(a1, a2, a3), // recvfrom (musl's recv)
        48 => socket::sys_shutdown(a1, a2), // shutdown(fd, how)
        49 => socket::sys_bind(a1, a2, a3), // bind(fd, addr, len)
        50 => socket::sys_listen(a1, a2),   // listen(fd, backlog)
        51 => socket::sys_get_sockname(a1, a2, a3), // getsockname
        52 => socket::sys_get_sockname(a1, a2, a3), // getpeername (connected pair: same answer)
        53 => pipes::sys_socketpair(a1, a2, a3, a4), // socketpair(domain, type, proto, sv)
        56 => procctl::sys_clone(a1, a2, a3, a4, a5), // clone(flags, stack, ptid, ctid, tls)
        57 => procctl::sys_fork(),
        59 => procctl::sys_execve(a1, a2, a3), // execve(path, argv, envp)
        60 => procctl::sys_exit(a1),           // exit: this task (a thread)
        61 => procctl::sys_wait4(a1, a2, a3),  // wait4(pid, status, options)
        62 => sig::sys_kill(a1, a2),           // kill(pid, sig)
        63 => misc::sys_uname(a1),
        72 => fd::sys_fcntl(a1, a2, a3), // fcntl(fd, cmd, arg)
        79 => pathops::sys_getcwd(a1, a2),
        80 => 0,                                        // chdir (root-only)
        82 => pathops::sys_rename(a1, a2),              // rename
        83 => pathops::sys_mkdir(a1, a2),               // mkdir
        84 => pathops::sys_rmdir(a1),                   // rmdir
        87 => pathops::sys_unlink(a1),                  // unlink
        89 => pathops::sys_readlink(a1, a2, a3),        // readlink
        95 => pathops::sys_umask(a1),                   // umask(mask)
        96 => time::sys_gettimeofday(a1),               // gettimeofday(tv, tz)
        102 | 107 => creds::sys_getuid(),               // getuid/geteuid
        104 | 108 => creds::sys_getgid(),               // getgid/getegid
        105 => creds::sys_setuid(a1),                   // setuid
        106 => creds::sys_setgid(a1),                   // setgid
        113 => creds::sys_setres(&[a1, a2], false),     // setreuid
        114 => creds::sys_setres(&[a1, a2], true),      // setregid
        117 => creds::sys_setres(&[a1, a2, a3], false), // setresuid
        119 => creds::sys_setres(&[a1, a2, a3], true),  // setresgid
        109 => procctl::sys_setpgid(a1, a2),            // setpgid
        110 => task::ppid() as u64,                     // getppid
        111 => task::pgid() as u64,                     // getpgrp
        112 => procctl::sys_setsid(),                   // setsid
        121 => procctl::sys_getpgid(a1),                // getpgid
        124 => procctl::sys_getsid(a1),                 // getsid
        131 => sig::sys_sigaltstack(a1, a2),
        157 => 0, // prctl (accept)
        158 => misc::sys_arch_prctl(a1, a2),
        169 => 0,                            // reboot (accept)
        200 => sig::sys_tkill(a1, a2),       // tkill(tid, sig)
        202 => futex::sys_futex(a1, a2, a3), // futex(uaddr, op, val)
        204 => misc::sys_sched_getaffinity(a2, a3),
        217 => fd::sys_getdents64(a1, a2, a3), // getdents64
        218 => procctl::sys_set_tid_address(a1),
        228 => time::sys_clock_gettime(a1, a2),
        229 => time::sys_clock_getres(a2),
        230 => time::sys_clock_nanosleep(a1, a2, a3, a4), // clock_nanosleep(clockid, flags, req, rem)
        231 => procctl::sys_exit_group(a1),
        232 => epoll::sys_epoll_wait(a1, a2, a3, a4), // epoll_wait(epfd, events, maxevents, timeout)
        233 => epoll::sys_epoll_ctl(a1, a2, a3, a4),  // epoll_ctl(epfd, op, fd, event)
        234 => sig::sys_tgkill(a1, a2, a3),           // tgkill(tgid, tid, sig)
        257 => path::sys_openat(a1, a2, a3, a4),      // openat
        258 => pathops::sys_mkdirat(a1, a2, a3),      // mkdirat
        262 => stat::sys_newfstatat(a1, a2, a3, a4),
        263 => pathops::sys_unlinkat(a1, a2, a3), // unlinkat
        264 => pathops::sys_renameat(a1, a2, a3, a4), // renameat
        273 => 0,                                 // set_robust_list
        288 => socket::sys_accept(a1, a2, a3, a4), // accept4(fd, addr, addrlen, flags)
        290 => epoll::sys_eventfd2(a1, a2),       // eventfd2(initval, flags)
        291 => epoll::sys_epoll_create1(a1),      // epoll_create1(flags)
        293 => pipes::sys_pipe(a1, a2),           // pipe2(fds, flags)
        318 => time::sys_getrandom(a1, a2),
        334 => {
            crate::serial_println!("ENOSYS 334 rseq");
            errno::err(errno::ENOSYS) // musl falls back
        }
        _ => {
            let _ = a6;
            crate::serial_println!("ENOSYS {} {}", nr, syscall_name(nr));
            errno::err(errno::ENOSYS)
        }
    };
    // Deliver pending unblocked signals on the way back to ring 3. The result
    // recorded in the signal frame is `rax` after `rt_sigreturn`, so an
    // interrupted syscall resumes as `-EINTR`.
    task::signal::deliver_linux(result);
    result
}
