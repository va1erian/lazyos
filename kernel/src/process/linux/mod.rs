//! Linux x86_64 ABI: loading static ELFs and the syscall dispatch.
//!
//! Only the subset `std`/musl need to reach `main` is implemented; everything
//! else is logged as `ENOSYS` (see `tools/abi/coverage.py`).
//!
//! The syscall surface is split by family into sibling modules: [`elf`] loads
//! the initial image and start stack, [`io`] is `read`/`write`/`poll`,
//! [`mem`] the `mmap` family, [`fd`] the descriptor table and its metadata,
//! [`cwd`] the working directory and the one path resolver, [`path`] and
//! [`pathops`] the VFS lookup and the naming syscalls (`open`,
//! `mkdir`, `rename`, ...), [`attr`] `chmod`/`chown`/`utimensat` and their
//! variants, [`stat`] the `stat` family, [`time`] clocks and
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

use names::syscall_name;

mod attr;
mod creds;
mod cwd;
mod dents;
mod dirstream;
mod elf;
mod epoll;
mod errno;
mod etcfs;
mod etcmap;
mod extra;
mod fd;
mod filerw;
mod filesys;
mod flags;
pub(crate) mod futex;
mod futex_queue;
mod inet;
mod io;
mod iov;
mod jobctl;
mod links;
mod locks;
mod mem;
mod misc;
mod msgio;
mod names;
pub(crate) mod native;
mod path;
mod pathops;
mod pipes;
mod procattr;
mod procctl;
mod procfs;
mod procinfo;
mod resources;
mod select;
pub(crate) mod shebang;

/// `/etc/passwd` rendered from a LazyOS account file (the compat suite).
#[cfg(lazyos_tests)]
pub fn render_passwd_for_test(source: &str) -> alloc::string::String {
    etcfs::render_passwd(source)
}

/// `/etc/group` rendered from a LazyOS account file and group view (the
/// compat suite).
#[cfg(lazyos_tests)]
pub fn render_group_for_test(source: &str, groups: &str) -> alloc::string::String {
    etcfs::render_group(source, groups)
}

/// The robust-list walk a thread's exit runs (the compat suite).
#[cfg(lazyos_tests)]
pub fn robust_exit_for_test(tid: usize) {
    procattr::exit_robust_list(tid);
}

/// Advisory locks still held (the compat suite's leak check).
#[cfg(lazyos_tests)]
pub fn locks_held_for_test() -> usize {
    locks::held_for_test()
}

/// The errno `execve` reports for a loader `reason`, for the loader suite.
#[cfg(lazyos_tests)]
pub fn load_errno_for_test(reason: &'static str) -> u64 {
    elf::classify(reason).errno()
}

/// A fabricated `/proc` file's bytes, for the mount suite.
#[cfg(lazyos_tests)]
pub fn proc_file_for_test(path: &str) -> Option<alloc::vec::Vec<u8>> {
    procfs::contents(path)
}
mod scatter;
mod sendfile;
mod sig;
mod slow;
mod socket;
mod sockopt;
mod stat;
mod statx;
mod time;
mod tty;
mod uaccess;
mod vfsfd;
mod wait;

pub use elf::{load_image, nul_terminated};
pub(crate) use native::{read_redirected, write_redirected};
// Only the `#[cfg(lazyos_tests)]` harness (`kernel/src/tests.rs`) reaches this
// through the `process::linux::` path; a normal build never does, hence the
// otherwise-unused-import warning this silences.
#[allow(unused_imports)]
pub use fd::close_cloexec_fds;

/// Resolve an executable for a `spawnv` of a Linux-personality program: the
/// named file, `/system/bin/<base>`, or a BusyBox applet alias, opened for
/// streaming. `None` when no such entry exists. `process::spawnv` uses this so
/// a Linux spawn of `sh` reaches the BusyBox multiplexer as `argv[0] = "sh"`
/// (issue #254).
pub fn open_executable(path: &str) -> Option<crate::process::image::VfsFile> {
    path::open_executable(path).ok()
}

/// [`open_executable`]'s whole file, for the suite's resolution tests.
#[cfg(lazyos_tests)]
pub fn load_executable(path: &str) -> Option<alloc::vec::Vec<u8>> {
    use crate::process::image::Image;
    let file = open_executable(path)?;
    let mut bytes = alloc::vec![0u8; file.len() as usize];
    file.read_exact_at(0, &mut bytes).ok()?;
    Some(bytes)
}

// User memory layout for Linux tasks: see `crate::process::layout`.
#[cfg_attr(not(lazyos_tests), allow(unused_imports))]
pub use crate::process::layout::STACK_TOP;
pub use crate::process::layout::{MMAP_BASE, MMAP_LIMIT};
/// Upper bound of the `brk` region: the start of the mmap area.
pub const BRK_LIMIT: u64 = MMAP_BASE;
/// The break an address space starts with when nothing was loaded into it
/// (the test suite's scratch spaces); a loaded image's break starts right
/// after its highest segment instead (`elf::load_image`).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub const BRK_BASE: u64 = 0x0100_0000;
#[cfg_attr(not(lazyos_tests), allow(unused_imports))]
pub use elf::stack_size;

/// Page size shared by every syscall that rounds an address or length to it.
const PAGE: u64 = 4096;

/// Syscall numbers we have dispatched at least once (for the coverage report).
static SYSCALL_SEEN: [AtomicBool; 512] = [const { AtomicBool::new(false) }; 512];

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
    dispatch_args6_for_test(nr, [a1, a2, a3, a4, a5, 0])
}

/// [`dispatch_for_test`] with all six argument registers (`preadv2`'s flags).
#[cfg(lazyos_tests)]
pub fn dispatch_args6_for_test(nr: u64, args: [u64; 6]) -> u64 {
    x86_64::instructions::interrupts::disable();
    let [a1, a2, a3, a4, a5, a6] = args;
    let result = linux_dispatch(nr, a1, a2, a3, a4, a5, a6);
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
    crate::arch::irqoff::enter_linux(nr);
    task::reclaim_pending();
    super::gate::LAST_SYSCALL.store(nr, core::sync::atomic::Ordering::Relaxed);
    crate::perf::syscall_entry(nr);
    trace_syscall(nr);
    let probe = slow::begin();
    let result = match nr {
        0 => io::sys_read(a1, a2, a3),
        1 => io::sys_write(a1, a2, a3),
        2 => path::sys_openat(cwd::AT_FDCWD, a1, a2, a3), // open
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
        17 => filerw::sys_pread64(a1, a2, a3, a4), // pread64(fd, buf, count, offset)
        18 => filerw::sys_pwrite64(a1, a2, a3, a4), // pwrite64(fd, buf, count, offset)
        19 => iov::sys_readv(a1, a2, a3),
        20 => iov::sys_writev(a1, a2, a3),
        21 => pathops::sys_access(a1, a2), // access(path, mode)
        22 => pipes::sys_pipe(a1, 0),      // pipe(fds)
        25 => mem::sys_mremap(a1, a2, a3, a4, a5), // mremap(old, old_size, new_size, flags, new)
        32 | 33 => fd::sys_dup(nr, a1, a2), // dup / dup2
        // nanosleep(req, rem) is always relative, so the clock is irrelevant.
        35 => time::sys_clock_nanosleep(time::CLOCK_MONOTONIC, 0, a1, a2),
        40 => sendfile::sys_sendfile(a1, a2, a3, a4), // sendfile(out, in, offset, count)
        41 => socket::sys_socket(a1, a2, a3),         // socket(domain, type, protocol)
        42 => socket::sys_connect(a1, a2, a3),        // connect(fd, addr, len)
        43 => socket::sys_accept(a1, a2, a3, 0),      // accept(fd, addr, addrlen)
        48 => socket::sys_shutdown(a1, a2),           // shutdown(fd, how)
        49 => socket::sys_bind(a1, a2, a3),           // bind(fd, addr, len)
        50 => socket::sys_listen(a1, a2),             // listen(fd, backlog)
        51 => socket::sys_get_sockname(a1, a2, a3, false), // getsockname
        52 => socket::sys_get_sockname(a1, a2, a3, true), // getpeername
        54 => socket::sys_setsockopt(a1, a2, a3, a4, a5), // setsockopt(fd, level, name, val, len)
        55 => socket::sys_getsockopt(a1, a2, a3, a4, a5), // getsockopt(fd, level, name, val, lenp)
        53 => pipes::sys_socketpair(a1, a2, a3, a4),  // socketpair(domain, type, proto, sv)
        56 => procctl::sys_clone(a1, a2, a3, a4, a5), // clone(flags, stack, ptid, ctid, tls)
        57 => procctl::sys_fork(),
        59 => procctl::sys_execve(a1, a2, a3), // execve(path, argv, envp)
        60 => procctl::sys_exit(a1),           // exit: this task (a thread)
        62 => sig::sys_kill(a1, a2),           // kill(pid, sig)
        63 => misc::sys_uname(a1),
        72 => fd::sys_fcntl(a1, a2, a3),       // fcntl(fd, cmd, arg)
        74 | 75 => filesys::sys_fsync(a1),     // fsync / fdatasync
        76 => filesys::sys_truncate(a1, a2),   // truncate(path, length)
        77 => filesys::sys_ftruncate(a1, a2),  // ftruncate(fd, length)
        78 => dents::sys_getdents(a1, a2, a3), // getdents
        79 => cwd::sys_getcwd(a1, a2),
        80 => cwd::sys_chdir(a1),                       // chdir
        81 => cwd::sys_fchdir(a1),                      // fchdir
        82 => pathops::sys_rename(a1, a2),              // rename
        83 => pathops::sys_mkdir(a1, a2),               // mkdir
        84 => pathops::sys_rmdir(a1),                   // rmdir
        87 => pathops::sys_unlink(a1),                  // unlink
        90 => attr::sys_chmod(a1, a2),                  // chmod(path, mode)
        91 => attr::sys_fchmod(a1, a2),                 // fchmod(fd, mode)
        92 | 94 => attr::sys_chown(a1, a2, a3),         // chown/lchown(path, uid, gid)
        93 => attr::sys_fchown(a1, a2, a3),             // fchown(fd, uid, gid)
        95 => pathops::sys_umask(a1),                   // umask(mask)
        96 => time::sys_gettimeofday(a1),               // gettimeofday(tv, tz)
        102 | 107 => creds::sys_getuid(),               // getuid/geteuid
        103 => misc::sys_syslog(a1, a2, a3),            // syslog (dmesg)
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
        130 => sig::sys_rt_sigsuspend(a1, a2),          // rt_sigsuspend(mask, size)
        131 => sig::sys_sigaltstack(a1, a2),
        132 => attr::sys_utime(a1, a2),      // utime(path, times)
        137 => filesys::sys_statfs(a1, a2),  // statfs(path, buf)
        138 => filesys::sys_fstatfs(a1, a2), // fstatfs(fd, buf)
        162 => filesys::sys_sync(),
        158 => misc::sys_arch_prctl(a1, a2),
        200 => sig::sys_tkill(a1, a2), // tkill(tid, sig)
        202 => futex::sys_futex(futex::Args {
            uaddr: a1,
            op: a2,
            val: a3,
            timeout: a4,
            uaddr2: a5,
            val3: a6,
        }),
        204 => misc::sys_sched_getaffinity(a3, a2), // sched_getaffinity(pid, len, mask)
        217 => dents::sys_getdents64(a1, a2, a3),   // getdents64
        218 => procctl::sys_set_tid_address(a1),
        227 => time::sys_clock_settime(a1, a2),
        228 => time::sys_clock_gettime(a1, a2),
        229 => time::sys_clock_getres(a2),
        230 => time::sys_clock_nanosleep(a1, a2, a3, a4), // clock_nanosleep(clockid, flags, req, rem)
        231 => procctl::sys_exit_group(a1),
        232 => epoll::sys_epoll_wait(a1, a2, a3, a4), // epoll_wait(epfd, events, maxevents, timeout)
        233 => epoll::sys_epoll_ctl(a1, a2, a3, a4),  // epoll_ctl(epfd, op, fd, event)
        234 => sig::sys_tgkill(a1, a2, a3),           // tgkill(tgid, tid, sig)
        235 => attr::sys_utimes(a1, a2),              // utimes(path, times)
        257 => path::sys_openat(a1, a2, a3, a4),      // openat
        258 => pathops::sys_mkdirat(a1, a2, a3),      // mkdirat
        260 => attr::sys_fchownat(a1, a2, a3, a4, a5), // fchownat(dirfd, path, uid, gid, flags)
        261 => attr::sys_futimesat(a1, a2, a3),       // futimesat(dirfd, path, times)
        262 => stat::sys_newfstatat(a1, a2, a3, a4),
        263 => pathops::sys_unlinkat(a1, a2, a3), // unlinkat
        264 => pathops::sys_renameat(a1, a2, a3, a4), // renameat
        268 => attr::sys_fchmodat(a1, a2, a3),    // fchmodat(dirfd, path, mode)
        280 => attr::sys_utimensat(a1, a2, a3, a4), // utimensat(dirfd, path, times, flags)
        288 => socket::sys_accept(a1, a2, a3, a4), // accept4(fd, addr, addrlen, flags)
        290 => epoll::sys_eventfd2(a1, a2),       // eventfd2(initval, flags)
        291 => epoll::sys_epoll_create1(a1),      // epoll_create1(flags)
        293 => pipes::sys_pipe(a1, a2),           // pipe2(fds, flags)
        295 => iov::sys_preadv(a1, a2, a3, a4),   // preadv(fd, iov, cnt, offset)
        296 => iov::sys_pwritev(a1, a2, a3, a4),  // pwritev(fd, iov, cnt, offset)
        306 => filesys::sys_syncfs(a1),           // syncfs(fd)
        318 => time::sys_getrandom(a1, a2),
        327 => iov::sys_preadv2(a1, a2, a3, a4, a6), // preadv2(fd, iov, cnt, offset, flags)
        328 => iov::sys_pwritev2(a1, a2, a3, a4, a6), // pwritev2
        332 => statx::sys_statx(a1, a2, a3, a4, a5), // statx(dirfd, path, flags, mask, buf)
        334 => {
            crate::serial_println!("ENOSYS 334 rseq");
            errno::err(errno::ENOSYS) // musl falls back
        }
        _ => match extra::dispatch(nr, [a1, a2, a3, a4, a5, a6]) {
            Some(result) => result,
            None => {
                crate::serial_println!("ENOSYS {} {}", nr, syscall_name(nr));
                errno::err(errno::ENOSYS)
            }
        },
    };
    // Kept for the fatal-fault report (issue #375): the last few syscalls tell
    // a wild jump's story better than the faulting `rip` alone.
    task::trace::record_syscall(task::current(), nr, a1, result);
    // Deliver pending unblocked signals on the way back to ring 3. The result
    // recorded in the signal frame is `rax` after `rt_sigreturn`, so an
    // interrupted syscall resumes as `-EINTR`, or is issued again when the
    // handler asked for `SA_RESTART` and the call is one Linux restarts.
    extra::raise_sigpipe(nr, [a1, a2, a3, a4, a5, a6], result);
    let restart = restartable(nr).then_some(nr);
    let result = task::signal::deliver_linux_restartable(result, restart);
    crate::arch::irqoff::exit();
    crate::perf::syscall_exit();
    slow::end(probe, nr, [a1, a2, a3]);
    result
}

/// [`restartable`], for the console-read signal tests.
#[cfg(lazyos_tests)]
pub fn restartable_for_test(nr: u64) -> bool {
    restartable(nr)
}

/// Whether an interrupted `nr` is re-issued after an `SA_RESTART` handler:
/// the transfers, waits and lock calls Linux restarts. The sleeps and the
/// readiness waits (`poll`, `select`, `epoll_wait`, `nanosleep`,
/// `sigsuspend`) are never restarted with a handler; they return `EINTR`.
fn restartable(nr: u64) -> bool {
    matches!(
        nr,
        0 | 1 | 2 | 16..=20 | 42..=47 | 61 | 72 | 73 | 202 | 247 | 257 | 288 | 295 | 296 | 327 | 328
    )
}
