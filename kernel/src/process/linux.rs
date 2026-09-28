//! Linux x86_64 ABI: loading static ELFs and the syscall dispatch.
//!
//! Only the subset `std`/musl need to reach `main` is implemented; everything
//! else is logged as `ENOSYS` (see `tools/abi/coverage.py`).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;
use xmas_elf::program::Type as ProgramType;
use xmas_elf::ElfFile;

use super::{load_segments, map_range_kind, page_phys};
use crate::fs::vfs::{self, FileKind, FsError, Id, Meta};
use crate::mem::vma::{Kind, Prot};
use crate::quota::{self, Resource};
use crate::task::process::GroupError;
use crate::task::signal::{self, Disposition, SignalError};
use crate::task::wait::WaitQueue;
use crate::task::{self, Fd, FdKind, WaitKind, WakeReason};
use crate::user_ptr;

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

const PAGE: u64 = 4096;

// errno values (returned as negative values).
const EPERM: u64 = 1;
const ESRCH: u64 = 3;
const ENOSYS: u64 = 38;
const ENOMEM: u64 = 12;
const EINVAL: u64 = 22;
const ENODEV: u64 = 19;
const ENOTTY: u64 = 25;
const ENOENT: u64 = 2;
const ECHILD: u64 = 10;
const EBADF: u64 = 9;
const EAGAIN: u64 = 11;
const EFAULT: u64 = 14;
const ENOEXEC: u64 = 8;
const ESPIPE: u64 = 29;
const EINTR: u64 = 4;
const ETIMEDOUT: u64 = 110;
// Filesystem errnos (mapped from `FsError` by `fs_err`).
const EACCES: u64 = 13;
const EEXIST: u64 = 17;
const ENOTDIR: u64 = 20;
const EISDIR: u64 = 21;
const ENOSPC: u64 = 28;
const EROFS: u64 = 30;
const ENAMETOOLONG: u64 = 36;
const ENOTEMPTY: u64 = 39;

// `clone` flags we honour (thread creation).
const CLONE_VM: u64 = 0x0000_0100;
const CLONE_SETTLS: u64 = 0x0008_0000;
const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;

/// Futex word address -> wait queue. A waiter parks on the queue keyed by its
/// word; `FUTEX_WAKE` notifies exactly that queue, so a wake cannot reach an
/// unrelated futex. Queue entries are pruned once empty and unreferenced.
static FUTEX_QUEUES: Mutex<Vec<(u64, Arc<WaitQueue>)>> = Mutex::new(Vec::new());

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
        43 => "accept",
        44 => "sendto",
        45 => "recvfrom",
        49 => "bind",
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
        234 => "tgkill",
        204 => "sched_getaffinity",
        217 => "getdents64",
        218 => "set_tid_address",
        228 => "clock_gettime",
        229 => "clock_getres",
        230 => "clock_nanosleep",
        231 => "exit_group",
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
#[cfg(laZYOS_TESTS)]
pub fn dispatch_for_test(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    linux_dispatch(nr, a1, a2, a3, 0, 0, 0)
}

/// `openat(AT_FDCWD, ...)` sentinel.
const AT_FDCWD: u64 = (-100i64) as u64;

// `struct stat` file-type bits (the VFS carries full modes; only the bits the
// synthetic entries need are spelled out here).
const S_IFREG: u32 = 0o100000;
const S_IFCHR: u32 = 0o020000;

fn err(e: u64) -> u64 {
    (e as i64).wrapping_neg() as u64
}

/// Map a VFS/filesystem failure to the errno the ABI returns.
fn fs_err(error: FsError) -> u64 {
    err(match error {
        FsError::NotFound => ENOENT,
        FsError::Exists => EEXIST,
        FsError::NotDir => ENOTDIR,
        FsError::IsDir => EISDIR,
        FsError::NotEmpty => ENOTEMPTY,
        FsError::Access => EACCES,
        FsError::ReadOnly => EROFS,
        FsError::Invalid => EINVAL,
        FsError::NoSpace => ENOSPC,
        FsError::NameTooLong => ENAMETOOLONG,
        FsError::NotSupported => ENOSYS,
    })
}

const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;

/// Load a Linux image into `table`, build its start stack, and return
/// `(entry, stack_pointer)`.
pub fn load(table: PhysAddr, elf_bytes: &[u8], argv0: &str) -> Result<(u64, u64), &'static str> {
    let entry = load_segments(table, elf_bytes)?;
    let stack = map_range_kind(
        table,
        STACK_TOP - STACK_SIZE,
        STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    )?;

    let phdr = program_header_addr(elf_bytes);
    let (phent, phnum) = phdr_size(elf_bytes);
    let mut arg0 = Vec::from(argv0.as_bytes());
    arg0.push(0);
    let rsp = build_start_stack(
        &stack,
        core::slice::from_ref(&arg0),
        &[],
        entry,
        phdr,
        phent,
        phnum,
    );
    Ok((entry, rsp))
}

/// Runtime address of the program headers (within a `PT_LOAD` segment).
fn program_header_addr(elf_bytes: &[u8]) -> u64 {
    let Ok(elf) = ElfFile::new(elf_bytes) else {
        return 0;
    };
    let phoff = u64::from(elf.header.pt2.ph_offset());
    for ph in elf.program_iter() {
        if ph.get_type() != Ok(ProgramType::Load) {
            continue;
        }
        let start = ph.offset();
        let end = start + ph.file_size();
        if phoff >= start && phoff < end {
            return ph.virtual_addr() + (phoff - start);
        }
    }
    0
}

fn phdr_size(elf_bytes: &[u8]) -> (u16, u16) {
    match ElfFile::new(elf_bytes) {
        Ok(elf) => (elf.header.pt2.ph_entry_size(), elf.header.pt2.ph_count()),
        Err(_) => (0, 0),
    }
}

/// Build the Linux process start stack: `argc/argv/envp/auxv` plus strings.
/// `argv`/`envp` are NUL-terminated byte strings.
fn build_start_stack(
    stack: &[(u64, u64)],
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
    entry: u64,
    phdr: u64,
    phent: u16,
    phnum: u16,
) -> u64 {
    let mut cursor = STACK_TOP;

    // Helper: write bytes just below `cursor`.
    let push_bytes = |bytes: &[u8], cursor: &mut u64| -> u64 {
        *cursor -= bytes.len() as u64;
        write_user(stack, *cursor, bytes);
        *cursor
    };

    // Strings (any order; the arrays below hold their addresses).
    let mut random = [0u8; 16];
    fill_random(&mut random);
    let random_addr = push_bytes(&random, &mut cursor);
    let execfn = argv
        .first()
        .map(|a| push_bytes(a, &mut cursor))
        .unwrap_or(0);
    let argv_ptrs: Vec<u64> = argv.iter().map(|a| push_bytes(a, &mut cursor)).collect();
    let envp_ptrs: Vec<u64> = envp.iter().map(|e| push_bytes(e, &mut cursor)).collect();

    // Word arrays (low to high): argc, argv[], NULL, envp[], NULL, auxv, AT_NULL.
    let mut words: Vec<u64> = Vec::new();
    words.push(argv.len() as u64);
    words.extend_from_slice(&argv_ptrs);
    words.push(0); // argv NULL
    words.extend_from_slice(&envp_ptrs);
    words.push(0); // envp NULL
    let auxv: [(u64, u64); 13] = [
        (AT_PHDR, phdr),
        (AT_PHENT, phent as u64),
        (AT_PHNUM, phnum as u64),
        (AT_PAGESZ, PAGE),
        (AT_BASE, 0),
        (AT_ENTRY, entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_CLKTCK, 100),
        (AT_RANDOM, random_addr),
        (AT_EXECFN, execfn),
    ];
    for (kind, value) in auxv {
        words.push(kind);
        words.push(value);
    }
    words.push(0); // AT_NULL
    words.push(0);

    cursor -= (words.len() as u64) * 8;
    cursor &= !0xF; // 16-byte aligned stack
    for (i, word) in words.iter().enumerate() {
        write_user(stack, cursor + (i as u64) * 8, &word.to_le_bytes());
    }
    cursor
}

const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_CLKTCK: u64 = 17;
const AT_RANDOM: u64 = 25;
const AT_EXECFN: u64 = 31;

/// Write bytes into a mapped user page set (via the kernel's phys map).
fn write_user(pages: &[(u64, u64)], va: u64, bytes: &[u8]) {
    let Some(phys) = page_phys(pages, va) else {
        return;
    };
    let dst = crate::mem::phys_to_virt(PhysAddr::new(phys)) + (va & 0xFFF);
    // Safety: within the freshly-mapped user page.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.as_mut_ptr::<u8>(), bytes.len());
    }
}

fn fill_random(buffer: &mut [u8]) {
    let mut state =
        crate::arch::idt::TICKS.load(core::sync::atomic::Ordering::Relaxed) ^ 0x9E37_79B9_7F4A_7C15;
    for byte in buffer.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
}

fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

/// Syscall dispatch, called from `arch::linux` (Linux ABI: nr in `rax`, args in
/// `rdi,rsi,rdx,r10,r8,r9`, result in `rax`).
#[no_mangle]
extern "C" fn linux_dispatch(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> u64 {
    trace_syscall(nr);
    let result = match nr {
        0 => sys_read(a1, a2, a3),
        1 => sys_write(a1, a2, a3),
        2 => sys_openat(AT_FDCWD, a1, a2), // open
        3 => sys_close(a1),
        4 => sys_stat_path(a1, a2), // stat(path, buf)
        5 => sys_fstat(a1, a2),     // fstat(fd, buf)
        6 => sys_stat_path(a1, a2), // lstat(path, buf)
        7 => sys_poll(a1, a2, a3),  // poll
        8 => sys_lseek(a1, a2, a3), // lseek
        9 => sys_mmap(a1, a2, a3, a4),
        10 => sys_mprotect(a1, a2, a3),
        11 => sys_munmap(a1, a2),
        12 => sys_brk(a1),
        13 => sys_rt_sigaction(a1, a2, a3, a4),
        14 => sys_rt_sigprocmask(a1, a2, a3, a4),
        15 => sys_rt_sigreturn(),
        16 => sys_ioctl(a1, a2, a3),
        19 => sys_readv(a1, a2, a3),
        20 => sys_writev(a1, a2, a3),
        21 => sys_access(a1, a2),            // access(path, mode)
        28 => 0,                             // madvise
        32 | 33 => sys_dup(nr, a1, a2),      // dup / dup2
        35 => sys_nanosleep(a1),             // nanosleep(req, rem)
        39 | 186 => task::current() as u64,  // getpid/gettid: pid == slot (#59)
        56 => sys_clone(a1, a2, a3, a4, a5), // clone(flags, stack, ptid, ctid, tls)
        57 => sys_fork(),
        59 => sys_execve(a1, a2, a3), // execve(path, argv, envp)
        60 => sys_exit(a1),           // exit: this task (a thread)
        61 => sys_wait4(a1, a2, a3),  // wait4(pid, status, options)
        62 => sys_kill(a1, a2),       // kill(pid, sig)
        63 => sys_uname(a1),
        72 => sys_fcntl(a1, a2), // fcntl
        79 => sys_getcwd(a1, a2),
        80 => 0,                        // chdir (root-only)
        89 => sys_readlink(a1, a2, a3), // readlink
        95 => sys_umask(a1),            // umask(mask)
        96 => sys_gettimeofday(a1),     // gettimeofday(tv, tz)
        102 | 103 | 104 | 105 => 0,     // getuid/getgid/geteuid/getegid
        106 | 107 | 108 | 113 => 0,     // set[re]uid/gid (root-only)
        109 => sys_setpgid(a1, a2),     // setpgid
        110 => task::ppid() as u64,     // getppid
        111 => task::pgid() as u64,     // getpgrp
        112 => sys_setsid(),            // setsid
        121 => sys_getpgid(a1),         // getpgid
        124 => sys_getsid(a1),          // getsid
        131 => sys_sigaltstack(a1, a2),
        157 => 0, // prctl (accept)
        158 => sys_arch_prctl(a1, a2),
        169 => 0,                     // reboot (accept)
        200 => sys_tkill(a1, a2),     // tkill(tid, sig)
        202 => sys_futex(a1, a2, a3), // futex(uaddr, op, val)
        204 => sys_sched_getaffinity(a2, a3),
        217 => sys_getdents64(a1, a2, a3), // getdents64
        218 => sys_set_tid_address(a1),
        228 => sys_clock_gettime(a1, a2),
        229 => sys_clock_getres(a2),
        230 => sys_nanosleep(a3), // clock_nanosleep(clockid, flags, req, rem)
        231 => sys_exit_group(a1),
        234 => sys_tgkill(a1, a2, a3), // tgkill(tgid, tid, sig)
        257 => sys_openat(a1, a2, a3), // openat
        262 => sys_newfstatat(a1, a2, a3, a4),
        273 => 0, // set_robust_list
        318 => sys_getrandom(a1, a2),
        334 => {
            crate::serial_println!("ENOSYS 334 rseq");
            err(ENOSYS) // musl falls back
        }
        _ => {
            let _ = a6;
            crate::serial_println!("ENOSYS {} {}", nr, syscall_name(nr));
            err(ENOSYS)
        }
    };
    // Deliver pending unblocked signals on the way back to ring 3. The result
    // recorded in the signal frame is `rax` after `rt_sigreturn`, so an
    // interrupted syscall resumes as `-EINTR`.
    signal::deliver_linux(result);
    result
}

/// `writev(fd, iov, iovcnt)`: `struct iovec { void *base; size_t len; }`.
fn sys_writev(fd: u64, iov: u64, count: u64) -> u64 {
    let mut total = 0u64;
    for i in 0..count {
        // Safety: user array of iovec entries (the syscall ABI's contract).
        let base = unsafe { user_ptr::read::<u64>(iov + i * 16) };
        // Safety: same iovec entry, adjacent field.
        let len = unsafe { user_ptr::read::<u64>(iov + i * 16 + 8) };
        let written = sys_write(fd, base, len);
        if written > len {
            return written; // error
        }
        total += written;
    }
    total
}

/// `readv(fd, iov, iovcnt)`.
fn sys_readv(fd: u64, iov: u64, count: u64) -> u64 {
    let mut total = 0u64;
    for i in 0..count {
        // Safety: user array of iovec entries (the syscall ABI's contract).
        let base = unsafe { user_ptr::read::<u64>(iov + i * 16) };
        // Safety: same iovec entry, adjacent field.
        let len = unsafe { user_ptr::read::<u64>(iov + i * 16 + 8) };
        let got = sys_read(fd, base, len);
        if got > len {
            return got; // error
        }
        total += got;
        if got < len {
            break; // short read: stop
        }
    }
    total
}

/// `poll(fds, nfds, timeout)`. Only stdin is pollable; park on the terminal
/// wait queue until it has input or the deadline passes (no busy loop).
fn sys_poll(fds: u64, nfds: u64, timeout: u64) -> u64 {
    if timeout == 0 {
        // Zero means "report readiness now" — never block.
        return scan_poll(fds, nfds);
    }
    // The timeout is an i32 count of milliseconds; negative blocks forever.
    let deadline = if (timeout as i64) < 0 {
        None
    } else {
        Some(task::ticks() + millis_to_ticks(timeout))
    };
    loop {
        let ready = scan_poll(fds, nfds);
        if ready > 0 {
            return ready;
        }
        match task::wait_poll(deadline) {
            WakeReason::Woken => {} // input arrived: rescan
            WakeReason::TimedOut => return 0,
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}

/// One non-blocking poll pass over the user's `pollfd` array.
fn scan_poll(fds: u64, nfds: u64) -> u64 {
    const POLLIN: u16 = 0x0001;
    let mut ready = 0u64;
    for i in 0..nfds {
        // struct pollfd { i32 fd; i16 events; i16 revents; }
        // Safety: user array of pollfd entries (the syscall ABI's contract).
        let fd = unsafe { user_ptr::read::<i32>(fds + i * 8) };
        // Safety: same pollfd entry, adjacent field.
        let events = unsafe { user_ptr::read::<u16>(fds + i * 8 + 4) };
        let revents = if fd == 0 && events & POLLIN != 0 && task::input_available() {
            ready += 1;
            POLLIN
        } else {
            0
        };
        // Safety: user array of pollfd entries (the syscall ABI's contract).
        unsafe { user_ptr::write::<u16>(fds + i * 8 + 6, revents) };
    }
    ready
}

/// Milliseconds to 100 Hz PIT ticks, rounding up so a positive timeout never
/// fires early.
fn millis_to_ticks(millis: u64) -> u64 {
    millis.div_ceil(10).max(1)
}

fn sys_write(fd: u64, ptr: u64, len: u64) -> u64 {
    if fd > 2 {
        return err(EBADF); // files are read-only for now
    }
    if len == 0 {
        return 0;
    }
    // Safety: the caller passes a valid user buffer (the syscall ABI's contract).
    let bytes = unsafe { user_ptr::bytes(ptr, len as usize) };
    task::write_output(bytes);
    crate::serial::write_bytes(bytes);
    // Answer a cursor-position report request (busybox line editing asks for it).
    if bytes.windows(4).any(|w| w == b"\x1b[6n") {
        task::inject_input(b"\x1b[1;1R");
    }
    len
}

fn sys_read(fd: u64, ptr: u64, len: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Terminal => read_terminal(ptr, len),
        FdKind::File => task::fd_read(fd as usize, ptr as *mut u8, len as usize)
            .map(|n| n as u64)
            .unwrap_or(0),
        FdKind::Closed => err(EBADF),
    }
}

/// Read terminal input as a byte stream: return once at least one key is
/// available (raw-mode programs read a byte at a time). Between checks the
/// task parks on the terminal wait queue, so an idle shell costs no CPU.
fn read_terminal(ptr: u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    loop {
        if let Some(key) = task::take_key() {
            // Safety: destination within the user buffer (the syscall ABI's contract).
            unsafe { user_ptr::write::<u8>(ptr, key_to_byte(key)) };
            return 1;
        }
        // A key may have gone to another task's window; spurious wakeups just
        // loop. `read` has no timeout, so only an interrupt can end the wait.
        match task::wait_terminal() {
            WakeReason::Woken | WakeReason::TimedOut => {}
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}

fn key_to_byte(key: crate::input::keyboard::Key) -> u8 {
    use crate::input::keyboard::Key;
    match key {
        Key::Char(c) => c as u8,
        Key::Enter => b'\n',
        Key::Space => b' ',
        Key::Tab => b'\t',
        Key::Backspace => 8,
        Key::Escape => 27,
        _ => 0,
    }
}

/// `mmap(addr, len, prot, flags)`: anonymous private memory only.
///
/// The range is recorded as an `Anon` VMA and populated lazily (demand-zero):
/// no frames are spent until a page is first touched, which matches Linux for
/// a buffer that is allocated and never (fully) used. `MAP_FIXED` replaces
/// whatever was mapped there before, VMAs and page tables alike.
fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64) -> u64 {
    if flags & MAP_ANONYMOUS == 0 {
        return err(ENODEV); // file-backed mmap not supported yet
    }
    if len == 0 {
        return err(EINVAL);
    }
    let len = align_up(len, PAGE);
    let base = if flags & MAP_FIXED != 0 {
        addr & !0xFFF
    } else {
        task::mmap_next().max(MMAP_BASE)
    };
    let Some(end) = base.checked_add(len) else {
        return err(ENOMEM);
    };
    if end > MMAP_LIMIT {
        return err(ENOMEM);
    }
    let prot = Prot((prot & 0x7) as u8);
    let table = crate::mem::kernel_table();
    // Per-uid user-memory quota (issue #103), charged before any VMA or page
    // table changes so a refusal leaves the address space untouched. The
    // closest Linux errno for "over the user's memory quota" is ENOMEM.
    if quota::charge_for_slot(task::current(), Resource::UserMemory, len).is_err() {
        return err(ENOMEM);
    }
    let mut replaced = 0u64;
    if flags & MAP_FIXED != 0 {
        replaced = crate::mem::vma::remove(table, base, end)
            .iter()
            .map(|vma| vma.len())
            .sum();
        crate::mem::unmap_range(table, base, end);
    }
    crate::mem::vma::insert(table, base, end, prot, Kind::Anon);
    if flags & MAP_FIXED == 0 {
        task::set_mmap_next(end);
    }
    // A fixed mapping that replaced live ranges gives their charge back.
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    base
}

/// `munmap(addr, len)`: drop the VMAs and release any resident pages. Linux
/// accepts unmapping an unmapped range, so this always returns 0 for valid
/// arguments. COW frames only lose this address space's reference.
fn sys_munmap(addr: u64, len: u64) -> u64 {
    if len == 0 {
        return err(EINVAL);
    }
    let start = addr & !0xFFF;
    let Some(end) = addr.checked_add(len).map(|end| align_up(end, PAGE)) else {
        return err(EINVAL);
    };
    let table = crate::mem::kernel_table();
    let removed = crate::mem::vma::remove(table, start, end);
    if !removed.is_empty() {
        let bytes: u64 = removed.iter().map(|vma| vma.len()).sum();
        crate::mem::unmap_range(table, start, end);
        // Unmapping gives the user's quota back (issue #103).
        quota::release_for_slot(task::current(), Resource::UserMemory, bytes);
    }
    0
}

/// `mprotect(addr, len, prot)`: update the PTE flags for resident pages and
/// the VMA for the whole range, so pages faulted in later honor the new access
/// too. Resident COW pages are privatized first (their protection is per
/// address space). A range with no VMA is a no-op: the old shim ignored it, and
/// we have no signal to deliver an ENOMEM against anyway.
fn sys_mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    if len == 0 {
        return err(EINVAL);
    }
    let start = addr & !0xFFF;
    let Some(end) = addr.checked_add(len).map(|end| align_up(end, PAGE)) else {
        return err(EINVAL);
    };
    let prot = Prot((prot & 0x7) as u8);
    let table = crate::mem::kernel_table();
    if crate::mem::vma::find_range(table, start, end).is_empty() {
        return 0;
    }
    if !crate::mem::protect_range(table, start, end, prot) {
        return err(ENOMEM);
    }
    crate::mem::vma::protect(table, start, end, prot);
    0
}

/// `brk(addr)`: move the heap break. Growth records a `Heap` VMA and defers
/// the frames to first touch; shrinking unmaps what lies above the new break.
fn sys_brk(addr: u64) -> u64 {
    let current = task::brk();
    if addr == 0 || addr < BRK_BASE {
        return current;
    }
    let new = align_up(addr, PAGE);
    if new > BRK_LIMIT {
        return current;
    }
    let table = crate::mem::kernel_table();
    if new > current {
        // Per-uid user-memory quota (issue #103): charge the growth before the
        // VMA exists. A refusal reports the unchanged break, which is how a
        // caller detects a failed brk.
        if quota::charge_for_slot(task::current(), Resource::UserMemory, new - current).is_err() {
            return current;
        }
        crate::mem::vma::insert(table, current, new, Prot::READ | Prot::WRITE, Kind::Heap);
    } else if new < current {
        crate::mem::vma::remove(table, new, current);
        crate::mem::unmap_range(table, new, current);
        quota::release_for_slot(task::current(), Resource::UserMemory, current - new);
    }
    task::set_brk(new);
    new
}

fn sys_arch_prctl(code: u64, addr: u64) -> u64 {
    match code {
        0x1001 | 0x1002 => {
            // SET_GS / SET_FS. Only FS is used by musl.
            if code == 0x1002 {
                task::set_fs_base(addr);
            }
            0
        }
        0x1003 | 0x1004 => {
            // GET_FS / GET_GS: write the base to *addr.
            // Safety: user pointer (the syscall ABI's contract).
            unsafe { user_ptr::write::<u64>(addr, 0) };
            0
        }
        _ => err(EINVAL),
    }
}

fn sys_ioctl(fd: u64, request: u64, arg: u64) -> u64 {
    match request {
        0x5401 => 0, // TCGETS: report a default (zeroed) termios
        0x540F => {
            // TIOCGPGRP: report the foreground process group. There is no
            // separate controlling-terminal group yet, so it is the caller's
            // own group (which `getpgrp` reports too).
            // Safety: user `pid_t *` (the syscall ABI's contract).
            unsafe { user_ptr::write::<u32>(arg, task::pgid() as u32) };
            0
        }
        0x5410 => 0, // TIOCSPGRP
        0x5413 => {
            // TIOCGWINSZ: 24 rows x 80 columns.
            // Safety: user `struct winsize` (the syscall ABI's contract).
            unsafe {
                user_ptr::write::<u16>(arg, 24);
                user_ptr::write::<u16>(arg + 2, 80);
                user_ptr::write::<u16>(arg + 4, 0);
                user_ptr::write::<u16>(arg + 6, 0);
            }
            0
        }
        _ if fd <= 2 => 0,
        _ => err(ENOTTY),
    }
}

fn sys_sched_getaffinity(mask: u64, len: u64) -> u64 {
    if len >= 8 {
        // Safety: user buffer (the syscall ABI's contract).
        unsafe { user_ptr::write::<u64>(mask, 1) };
        8
    } else if len > 0 {
        // Safety: user buffer (the syscall ABI's contract).
        unsafe { user_ptr::write::<u8>(mask, 1) };
        1
    } else {
        0
    }
}

fn sys_uname(buf: u64) -> u64 {
    // struct utsname: six 65-byte fields.
    let mut data = [0u8; 6 * 65];
    let fields = ["LazyOS", "lazyos", "0.1.0", "0.1.0", "x86_64", "unknown"];
    for (i, field) in fields.iter().enumerate() {
        let bytes = field.as_bytes();
        data[i * 65..i * 65 + bytes.len()].copy_from_slice(bytes);
    }
    // Safety: user buffer of at least 390 bytes (musl's utsname, the
    // syscall ABI's contract).
    unsafe { user_ptr::copy_to(buf, &data) };
    0
}

/// `readlink(path, buf, size)`: the only link we have is `/proc/self/exe`,
/// which resolves to the BusyBox binary (so the shell can re-exec its applets).
fn sys_readlink(path: u64, buf: u64, size: u64) -> u64 {
    let target: &[u8] = match read_cstr(path).as_deref() {
        Some("/proc/self/exe") => b"/busybox",
        Some("/proc/self/cwd") => b"/",
        _ => return err(ENOENT),
    };
    if size == 0 {
        return err(EINVAL);
    }
    let n = (size as usize).min(target.len());
    // Safety: user buffer of at least `n` bytes (the syscall ABI's contract).
    unsafe { user_ptr::copy_to(buf, &target[..n]) };
    n as u64
}

fn sys_getcwd(buf: u64, size: u64) -> u64 {
    if size < 2 {
        return err(EINVAL);
    }
    // Safety: user buffer (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<u8>(buf, b'/');
        user_ptr::write::<u8>(buf + 1, 0);
    }
    buf
}

/// Fixed realtime epoch (2026-01-01T00:00:00Z); the PIT provides monotonicity.
const REALTIME_BASE: u64 = 1_767_225_600;

/// Monotonic tick count from the PIT (100 Hz).
fn now_ticks() -> u64 {
    crate::arch::idt::TICKS.load(core::sync::atomic::Ordering::Relaxed)
}

fn sys_clock_gettime(clock: u64, out: u64) -> u64 {
    // CLOCK_MONOTONIC(1) counts from boot; everything else is anchored to epoch.
    let ticks = now_ticks();
    let seconds = if clock == 1 {
        ticks / 100
    } else {
        REALTIME_BASE + ticks / 100
    };
    write_timespec(out, seconds, (ticks % 100) * 10_000_000);
    0
}

fn write_timespec(out: u64, sec: u64, nsec: u64) {
    // Safety: user buffer holds a `struct timespec` (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<i64>(out, sec as i64);
        user_ptr::write::<i64>(out + 8, nsec as i64);
    }
}

fn sys_clock_getres(out: u64) -> u64 {
    // 100 Hz PIT => 10 ms resolution.
    write_timespec(out, 0, 10_000_000);
    0
}

fn sys_gettimeofday(tv: u64) -> u64 {
    let ticks = now_ticks();
    // Safety: user buffer holds a `struct timeval` (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<i64>(tv, (REALTIME_BASE + ticks / 100) as i64);
        user_ptr::write::<i64>(tv + 8, ((ticks % 100) * 10_000) as i64);
    }
    0
}

/// Sleep for the `struct timespec` duration at `req` (nanosleep/clock_nanosleep).
fn sys_nanosleep(req: u64) -> u64 {
    // Safety: user buffer holds a `struct timespec` (the syscall ABI's contract).
    let (sec, nsec) = unsafe { (user_ptr::read::<i64>(req), user_ptr::read::<i64>(req + 8)) };
    if sec < 0 || nsec < 0 {
        return err(EINVAL);
    }
    let millis = sec as u64 * 1000 + (nsec as u64).div_ceil(1_000_000);
    // 100 Hz timer: round up to whole ticks, at least one so time advances.
    // The sleep queue is never notified; the timer's deadline sweep is what
    // makes this return, exactly like a timeout.
    let deadline = now_ticks() + millis_to_ticks(millis);
    match task::wait_sleep(deadline) {
        WakeReason::TimedOut => 0,
        WakeReason::Interrupted => err(EINTR),
        WakeReason::Woken => 0, // nothing notifies the sleep queue
    }
}

fn sys_getrandom(buf: u64, len: u64) -> u64 {
    let mut chunk = [0u8; 256];
    let mut written = 0u64;
    while written < len {
        let n = ((len - written) as usize).min(chunk.len());
        fill_random(&mut chunk[..n]);
        // Safety: user buffer (the syscall ABI's contract).
        unsafe { user_ptr::copy_to(buf + written, &chunk[..n]) };
        written += n as u64;
    }
    written
}

/// Read a NUL-terminated user string (bounded).
fn read_cstr(ptr: u64) -> Option<String> {
    if ptr == 0 {
        return None;
    }
    let mut out = String::new();
    for i in 0..4096u64 {
        // Safety: user memory up to the NUL terminator (the syscall ABI's contract).
        let byte = unsafe { user_ptr::read::<u8>(ptr + i) };
        if byte == 0 {
            break;
        }
        out.push(byte as char);
    }
    Some(out)
}

/// Allocate a descriptor, mapping failure to `-ENOMEM`.
fn fd_result(slot: Option<usize>) -> u64 {
    match slot {
        Some(fd) => fd as u64,
        None => err(ENOMEM),
    }
}

/// A bare applet name in a `bin` directory (or with no directory) that isn't a
/// real FAT file aliases to the BusyBox binary.
fn applet_name(path: &str) -> Option<&str> {
    let trimmed = path.trim_start_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let dir = &trimmed[..trimmed.len() - base.len()];
    let plain = !base.is_empty()
        && base.len() <= 12
        && !base.contains('.')
        && base
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if plain && (dir.is_empty() || dir.contains("bin")) {
        Some(base)
    } else {
        None
    }
}

/// The synthetic root directories that have no filesystem behind them yet
/// (`/tmp` is a real ramfs mount and resolves through the VFS).
fn synthetic_dir(path: &str) -> bool {
    matches!(
        path,
        "/" | "/bin" | "/sbin" | "/usr" | "/dev" | "/proc" | "/etc"
    )
}

/// Metadata for a kernel-fabricated entry: a synthetic directory or a BusyBox
/// applet alias. Only used when the VFS has no node at `path`.
fn synthetic_meta(path: &str) -> Option<Meta> {
    if synthetic_dir(path) {
        return Some(Meta {
            ino: 1,
            mode: vfs::S_IFDIR | 0o755,
            uid: 0,
            gid: 0,
            size: 0,
            kind: FileKind::Dir,
        });
    }
    if applet_name(path).is_some() {
        return crate::fs::stat("busybox").map(|(size, _)| Meta {
            ino: 0,
            mode: vfs::S_IFREG | 0o555,
            uid: 0,
            gid: 0,
            size: size as u64,
            kind: FileKind::File,
        });
    }
    None
}

/// Where a Linux path leads: a node on a mount, or a kernel-fabricated entry.
enum Target {
    Node(Meta),
    Synthetic(Meta),
}

/// Resolve a path the way the Linux ABI sees it: VFS mounts first (so `/tmp`
/// and future mounts work), then the synthetic directories and applet aliases.
/// A permission error from the VFS is returned as-is; only a miss falls
/// through to the fabricated entries.
fn resolve(path: &str) -> Result<Target, FsError> {
    match crate::fs::vfs_stat(Id::current(), path) {
        Ok(meta) => return Ok(Target::Node(meta)),
        Err(FsError::NotFound) => {}
        Err(error) => return Err(error),
    }
    synthetic_meta(path)
        .map(Target::Synthetic)
        .ok_or(FsError::NotFound)
}

/// Load a file's bytes through the VFS, with the BusyBox applet alias.
fn load_file(path: &str) -> Result<Vec<u8>, FsError> {
    match crate::fs::vfs_read(Id::current(), path) {
        Ok(data) => Ok(data),
        Err(FsError::NotFound) if applet_name(path).is_some() => {
            crate::fs::vfs_read(Id::current(), "/busybox").map_err(|_| FsError::NotFound)
        }
        Err(error) => Err(error),
    }
}

fn push_dirent(out: &mut Vec<u8>, ino: u64, d_type: u8, name: &str) {
    let start = out.len();
    out.extend_from_slice(&ino.to_le_bytes()); // d_ino
    out.extend_from_slice(&0u64.to_le_bytes()); // d_off
    out.extend_from_slice(&0u16.to_le_bytes()); // d_reclen (patched below)
    out.push(d_type);
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while (out.len() - start) % 8 != 0 {
        out.push(0);
    }
    let reclen = (out.len() - start) as u16;
    out[start + 16..start + 18].copy_from_slice(&reclen.to_le_bytes());
}

fn sys_getdents64(fd: u64, buf: u64, count: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::File => task::fd_read(fd as usize, buf as *mut u8, count as usize)
            .map(|n| n as u64)
            .unwrap_or(0),
        _ => err(EBADF),
    }
}

/// `openat(2)` access mode mask.
const O_ACCMODE: u64 = 0o3;

/// File descriptor metadata captured at open time. The task fd table
/// (`task::Fd::File`) carries only bytes, so `fstat` reads mode/ino/size from
/// this side table, keyed by `(task slot, fd)`; `close` clears the slot and
/// `dup` copies it. Fds inherited by `fork` fall back to the plain 0o444
/// answer until the fd table itself carries VFS handles.
#[derive(Clone, Copy)]
struct FdMeta {
    mode: u32,
    ino: u64,
    size: u64,
    /// Byte length of the snapshot, checked on read so a slot reused by a
    /// different file (fork-inherited or a recycled task slot) reports no
    /// metadata instead of a stale mode.
    data_len: usize,
}

const FD_META_SLOTS: usize = task::MAX_TASKS * task::FD_COUNT;
static FD_META: Mutex<[Option<FdMeta>; FD_META_SLOTS]> = Mutex::new([None; FD_META_SLOTS]);

/// This task's side-table slot for `fd`, if both are in range.
fn fd_meta_slot(fd: usize) -> Option<usize> {
    let slot = task::current();
    if fd >= task::FD_COUNT || slot >= task::MAX_TASKS {
        return None;
    }
    Some(slot * task::FD_COUNT + fd)
}

fn fd_meta_set(fd: usize, meta: FdMeta) {
    if let Some(slot) = fd_meta_slot(fd) {
        FD_META.lock()[slot] = Some(meta);
    }
}

fn fd_meta_get(fd: usize) -> Option<FdMeta> {
    let slot = fd_meta_slot(fd)?;
    let mut table = FD_META.lock();
    let meta = table[slot]?;
    if task::fd_size(fd) != Some(meta.data_len as u64) {
        table[slot] = None; // the slot now holds a different file
        return None;
    }
    Some(meta)
}

fn fd_meta_clear(fd: usize) {
    if let Some(slot) = fd_meta_slot(fd) {
        FD_META.lock()[slot] = None;
    }
}

fn fd_meta_copy(from: usize, to: usize) {
    let (Some(from), Some(to)) = (fd_meta_slot(from), fd_meta_slot(to)) else {
        return;
    };
    let mut table = FD_META.lock();
    table[to] = table[from];
}

/// Open a read-only file snapshot and record its VFS metadata for `fstat`.
fn open_file_fd(data: Vec<u8>, mode: u32, ino: u64, size: u64) -> u64 {
    let data_len = data.len();
    match task::fd_open(Fd::File { data, offset: 0 }) {
        Some(fd) => {
            fd_meta_set(
                fd,
                FdMeta {
                    mode,
                    ino,
                    size,
                    data_len,
                },
            );
            fd as u64
        }
        None => err(ENOMEM),
    }
}

/// The `linux_dirent64` type byte for a VFS node kind.
fn dtype_of(kind: FileKind) -> u8 {
    const DT_DIR: u8 = 4;
    const DT_REG: u8 = 8;
    match kind {
        FileKind::Dir => DT_DIR,
        FileKind::File => DT_REG,
    }
}

/// The `.`/`..` prefix every directory stream starts with.
fn empty_dir_stream() -> Vec<u8> {
    const DT_DIR: u8 = 4;
    let mut out = Vec::new();
    push_dirent(&mut out, 1, DT_DIR, ".");
    push_dirent(&mut out, 1, DT_DIR, "..");
    out
}

/// Build a `linux_dirent64` stream for a directory, so `getdents64` can read
/// it like a file (the fd table stores byte snapshots, not directory handles).
/// The VFS supplies the real entries; `.`/`..` are added here.
fn dir_stream(path: &str) -> Result<Vec<u8>, FsError> {
    let mut out = empty_dir_stream();
    for entry in crate::fs::vfs_readdir(Id::current(), path)? {
        push_dirent(&mut out, entry.ino, dtype_of(entry.kind), &entry.name);
    }
    Ok(out)
}

/// Refuse a write-mode open: check the mount's write permission first (so a
/// denial is `EACCES`), then answer `EROFS` because the Linux fd table stores
/// read-only snapshots until it carries VFS handles.
fn write_open_denied(path: &str) -> u64 {
    match crate::fs::vfs_check(Id::current(), path, vfs::WRITE) {
        Ok(_) | Err(FsError::NotFound) => {}
        Err(error) => return fs_err(error),
    }
    crate::serial_println!("fs: {path}: {}", FsError::ReadOnly.message());
    err(EROFS)
}

/// Open a path: mounts (the FAT root and `/tmp` ramfs today) through the VFS,
/// plus the synthetic device nodes and BusyBox applet aliases.
fn open_path(path: &str, flags: u64) -> u64 {
    match path {
        "/dev/tty" | "/dev/console" | "/dev/tty0" | "/dev/tty1" => {
            return fd_result(task::fd_open(Fd::Terminal));
        }
        "/dev/null" | "/dev/zero" | "/dev/full" => {
            return fd_result(task::fd_open(Fd::File {
                data: Vec::new(),
                offset: 0,
            }));
        }
        _ => {}
    }

    let write_access = flags & O_ACCMODE != 0;
    match resolve(path) {
        Ok(Target::Node(meta)) if meta.kind == FileKind::Dir => match dir_stream(path) {
            Ok(data) => open_file_fd(data, meta.mode as u32, meta.ino, meta.size),
            Err(error) => fs_err(error),
        },
        Ok(Target::Node(meta)) => {
            if write_access {
                return write_open_denied(path);
            }
            match load_file(path) {
                Ok(data) => open_file_fd(data, meta.mode as u32, meta.ino, meta.size),
                Err(error) => fs_err(error),
            }
        }
        Ok(Target::Synthetic(meta)) if meta.kind == FileKind::Dir => {
            open_file_fd(empty_dir_stream(), meta.mode as u32, meta.ino, meta.size)
        }
        Ok(Target::Synthetic(meta)) => {
            if write_access {
                return write_open_denied(path);
            }
            match load_file(path) {
                Ok(data) => open_file_fd(data, meta.mode as u32, meta.ino, meta.size),
                Err(error) => fs_err(error),
            }
        }
        Err(error) => fs_err(error),
    }
}

fn sys_openat(_dirfd: u64, path: u64, flags: u64) -> u64 {
    // Only AT_FDCWD ("cwd" is always `/`) is supported; a real dirfd is
    // ignored, exactly as before the VFS.
    match read_cstr(path) {
        Some(path) => open_path(&path, flags),
        None => err(EINVAL),
    }
}

fn sys_close(fd: u64) -> u64 {
    if task::fd_close(fd as usize) {
        fd_meta_clear(fd as usize);
        0
    } else {
        err(EBADF)
    }
}

fn sys_lseek(fd: u64, offset: u64, whence: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::File => match task::fd_seek(fd as usize, offset as i64, whence) {
            Some(pos) => pos,
            None => err(EINVAL),
        },
        FdKind::Terminal => err(ESPIPE),
        FdKind::Closed => err(EBADF),
    }
}
/// `access(path, mode)`: POSIX mode bits (`F_OK`=0, `X_OK`=1, `W_OK`=2,
/// `R_OK`=4) line up with the VFS masks, so they pass straight through.
fn sys_access(path: u64, mode: u64) -> u64 {
    let Some(path) = read_cstr(path) else {
        return err(EINVAL);
    };
    let id = Id::current();
    let mask = (mode & 0o7) as u8;
    match crate::fs::vfs_check(id, &path, mask) {
        Ok(_) => 0,
        Err(FsError::NotFound) => match synthetic_meta(&path) {
            Some(meta) => match vfs::check_access(&meta, id, mask) {
                Ok(()) => 0,
                Err(error) => fs_err(error),
            },
            None => err(ENOENT),
        },
        Err(error) => fs_err(error),
    }
}

/// `umask(mask)`: set the global creation mask, return the previous one.
fn sys_umask(mask: u64) -> u64 {
    crate::fs::vfs_set_umask((mask & 0o777) as u16) as u64
}

fn sys_dup(nr: u64, a1: u64, a2: u64) -> u64 {
    let slot = if nr == 32 {
        task::fd_dup(a1 as usize)
    } else {
        task::fd_dup2(a1 as usize, a2 as usize)
    };
    match slot {
        Some(fd) => {
            fd_meta_copy(a1 as usize, fd);
            fd as u64
        }
        None => err(EBADF),
    }
}

fn sys_fcntl(fd: u64, cmd: u64) -> u64 {
    match cmd {
        0 => match task::fd_dup(fd as usize) {
            // F_DUPFD
            Some(new) => {
                fd_meta_copy(fd as usize, new);
                new as u64
            }
            None => err(EBADF),
        },
        _ => 0, // F_GETFD/SETFD/GETFL/SETFL: report defaults
    }
}

/// Fill a `struct stat` (x86_64 layout) at `buf`.
fn fill_stat(buf: u64, mode: u32, size: u64, ino: u64) {
    if buf == 0 {
        return;
    }
    // Safety: the caller passes a valid 144-byte stat buffer.
    unsafe {
        core::ptr::write_bytes(buf as *mut u8, 0, 144);
    }
    write_u64(buf + 8, ino);
    write_u64(buf + 16, 1); // st_nlink
    write_u32(buf + 24, mode); // st_mode
    write_u64(buf + 48, size); // st_size
    write_u64(buf + 56, 4096); // st_blksize
    write_u64(buf + 64, size.div_ceil(512)); // st_blocks
}

fn write_u64(addr: u64, value: u64) {
    // Safety: caller ensures the address is valid user memory (the syscall
    // ABI's contract).
    unsafe { user_ptr::write::<u64>(addr, value) };
}

fn write_u32(addr: u64, value: u32) {
    // Safety: caller ensures the address is valid user memory (the syscall
    // ABI's contract).
    unsafe { user_ptr::write::<u32>(addr, value) };
}

fn sys_fstat(fd: u64, buf: u64) -> u64 {
    if fd <= 2 {
        fill_stat(buf, S_IFCHR | 0o620, 0, 0);
        return 0;
    }
    match task::fd_kind(fd as usize) {
        FdKind::File => {
            match fd_meta_get(fd as usize) {
                // The open recorded the VFS mode/ino/size; report those.
                Some(meta) => fill_stat(buf, meta.mode, meta.size, meta.ino),
                // Inherited fds (fork/exec) have no side-table entry yet.
                None => {
                    let size = task::fd_size(fd as usize).unwrap_or(0);
                    fill_stat(buf, S_IFREG | 0o444, size, fd);
                }
            }
            0
        }
        FdKind::Terminal => {
            fill_stat(buf, S_IFCHR | 0o620, 0, 0);
            0
        }
        FdKind::Closed => err(EBADF),
    }
}

fn sys_stat_path(path: u64, buf: u64) -> u64 {
    match read_cstr(path) {
        Some(path) => stat_path(&path, buf),
        None => err(EINVAL),
    }
}

fn stat_path(path: &str, buf: u64) -> u64 {
    match resolve(path) {
        Ok(Target::Node(meta)) | Ok(Target::Synthetic(meta)) => {
            fill_stat(buf, meta.mode as u32, meta.size, meta.ino);
            0
        }
        Err(error) => fs_err(error),
    }
}

fn sys_newfstatat(_dirfd: u64, path: u64, buf: u64, _flags: u64) -> u64 {
    match read_cstr(path) {
        Some(path) if !path.is_empty() => stat_path(&path, buf),
        _ => err(ENOENT),
    }
}

/// `clone(flags, stack, parent_tid, child_tid, tls)` — thread creation only.
fn sys_clone(flags: u64, stack: u64, parent_tid: u64, child_tid: u64, tls: u64) -> u64 {
    if flags & CLONE_VM == 0 {
        return err(ENOSYS); // fork/process creation is a later phase
    }
    let fs_base = if flags & CLONE_SETTLS != 0 { tls } else { 0 };
    let clear = if flags & CLONE_CHILD_CLEARTID != 0 {
        child_tid
    } else {
        0
    };
    match task::spawn_thread("thread", stack, fs_base, clear) {
        Ok(index) => {
            if flags & CLONE_PARENT_SETTID != 0 && parent_tid != 0 {
                write_u64(parent_tid, index as u64);
            }
            index as u64
        }
        Err(_) => err(ENOMEM),
    }
}

fn sys_set_tid_address(tidptr: u64) -> u64 {
    task::set_clear_child_tid(tidptr);
    task::current() as u64
}

/// Read a NUL-terminated user string as raw bytes (including the terminator).
fn read_cstr_bytes(ptr: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..4096u64 {
        // Safety: user memory up to the NUL terminator (the syscall ABI's contract).
        let byte = unsafe { user_ptr::read::<u8>(ptr + i) };
        out.push(byte);
        if byte == 0 {
            break;
        }
    }
    out
}

/// Read a NULL-terminated array of user string pointers.
fn read_str_ptr_array(arr: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if arr == 0 {
        return out;
    }
    for i in 0..256u64 {
        // Safety: user array of `char *` (the syscall ABI's contract).
        let ptr = unsafe { user_ptr::read_at::<u64>(arr, i as usize) };
        if ptr == 0 {
            break;
        }
        out.push(read_cstr_bytes(ptr));
    }
    out
}

/// Map special process paths to a real FAT entry (`/proc/self/exe` -> busybox).
fn resolve_exe(path: &str) -> &str {
    match path {
        "/proc/self/exe" => "busybox",
        other => other.trim_start_matches('/'),
    }
}

fn sys_fork() -> u64 {
    match task::spawn_fork() {
        Ok(index) => index as u64,
        Err(_) => err(ENOMEM),
    }
}

fn sys_wait4(_pid: u64, status: u64, options: u64) -> u64 {
    const WNOHANG: u64 = 1;
    if !task::has_children() {
        return err(ECHILD);
    }
    loop {
        if let Some((slot, code)) = task::reap_child() {
            if status != 0 {
                write_u32(status, (code as u32) << 8);
            }
            return slot as u64;
        }
        if options & WNOHANG != 0 {
            return 0; // no child has exited, don't wait
        }
        // No child is reapable yet: park until one exits. `sys_exit` (via
        // `finish_current`) notifies the queue, and the recheck above happens
        // with interrupts disabled, so an exit cannot slip in between.
        match task::wait_child_exit() {
            WakeReason::Woken | WakeReason::TimedOut => {}
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}

/// `execve(path, argv, envp)`: replace the current image with `path`'s ELF and
/// resume at its entry point.
fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> u64 {
    let Some(path) = read_cstr(path_ptr) else {
        return err(EFAULT);
    };
    let mut argv = read_str_ptr_array(argv_ptr);
    if argv.is_empty() {
        argv.push(read_cstr_bytes(path_ptr));
    }
    let envp = read_str_ptr_array(envp_ptr);

    let target = resolve_exe(&path);
    // Executables need the execute bit. Applet aliases and paths with no VFS
    // node fall through to `load_file`; root bypasses the check as usual.
    match crate::fs::vfs_check(Id::current(), target, vfs::EXECUTE) {
        Ok(_) | Err(FsError::NotFound) => {}
        Err(error) => return fs_err(error),
    }
    let elf = match load_file(target) {
        Ok(elf) => elf,
        Err(FsError::NotFound) => return err(ENOENT),
        Err(error) => return fs_err(error),
    };
    let Some(table) = crate::mem::new_user_table() else {
        return err(ENOMEM);
    };
    let entry = match load_segments(table, &elf) {
        Ok(entry) => entry,
        Err(_) => return err(ENOEXEC),
    };
    let stack = match map_range_kind(
        table,
        STACK_TOP - STACK_SIZE,
        STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    ) {
        Ok(stack) => stack,
        Err(_) => return err(ENOMEM),
    };
    let phdr = program_header_addr(&elf);
    let (phent, phnum) = phdr_size(&elf);
    let rsp = build_start_stack(&stack, &argv, &envp, entry, phdr, phent, phnum);

    // Replace the process image: switch to the new table and make `sysretq`
    // resume at the new entry.
    crate::mem::switch_to(table);
    task::set_pml4(table.as_u64());
    task::set_fs_base(0);
    task::register_bumps(table.as_u64(), BRK_BASE, MMAP_BASE);
    crate::arch::linux::set_user_return(entry, rsp, 0x202);
    0
}

/// `futex(uaddr, op, val)` — only WAIT/WAKE (the mutex/join primitives).
fn sys_futex(uaddr: u64, op: u64, val: u64) -> u64 {
    match op & 0x7f {
        0 | 9 => futex_wait(uaddr, val),  // FUTEX_WAIT / FUTEX_WAIT_BITSET
        1 | 10 => futex_wake(uaddr, val), // FUTEX_WAKE / FUTEX_WAKE_BITSET
        _ => 0,
    }
}

/// The wait queue for a futex word, created on first use.
fn futex_queue(uaddr: u64) -> Arc<WaitQueue> {
    let mut queues = FUTEX_QUEUES.lock();
    if let Some((_, queue)) = queues.iter().find(|(address, _)| *address == uaddr) {
        return Arc::clone(queue);
    }
    let queue = Arc::new(WaitQueue::new(WaitKind::Futex));
    queues.push((uaddr, Arc::clone(&queue)));
    queue
}

fn futex_wait(uaddr: u64, val: u64) -> u64 {
    // Safety: the futex word is a user 32-bit value (the syscall ABI's contract).
    let current = unsafe { user_ptr::read::<u32>(uaddr) };
    if current != val as u32 {
        return err(EAGAIN); // value changed: nothing to wait for
    }
    // The value check above and the park below are atomic with respect to
    // wakers: interrupts are masked in the syscall gate, and the single CPU
    // cannot run a `FUTEX_WAKE` between them.
    let queue = futex_queue(uaddr);
    match queue.wait(task::current(), None) {
        WakeReason::Woken => 0,
        WakeReason::TimedOut => err(ETIMEDOUT),
        WakeReason::Interrupted => err(EINTR),
    }
}

fn futex_wake(uaddr: u64, count: u64) -> u64 {
    let mut queues = FUTEX_QUEUES.lock();
    let Some(position) = queues.iter().position(|(address, _)| *address == uaddr) else {
        return 0;
    };
    let woken = if count == 1 {
        queues[position].1.notify_one()
    } else {
        queues[position]
            .1
            .notify(count.min(task::MAX_TASKS as u64) as usize)
    };
    // Prune empty queues so dead word addresses do not accumulate. Waiters hold
    // an `Arc` clone for as long as they are parked (and until their wait
    // returns), so the strong count guard keeps a queue alive while a lookup or
    // wake is still in flight.
    if queues[position].1.is_empty() && Arc::strong_count(&queues[position].1) == 1 {
        queues.remove(position);
    }
    woken as u64
}

// ---------------------------------------------------------------------------
// Signals (#60)
// ---------------------------------------------------------------------------

fn read_u64(addr: u64) -> u64 {
    // Safety: caller ensures the address is valid user memory (the syscall
    // ABI's contract).
    unsafe { user_ptr::read::<u64>(addr) }
}

fn read_u32(addr: u64) -> u32 {
    // Safety: caller ensures the address is valid user memory (the syscall
    // ABI's contract).
    unsafe { user_ptr::read::<u32>(addr) }
}

/// Map a signal-layer failure to its Linux errno.
fn signal_err(error: SignalError) -> u64 {
    match error {
        SignalError::NoSuchProcess => err(ESRCH),
        SignalError::NotPermitted => err(EPERM),
        SignalError::Invalid => err(EINVAL),
    }
}

/// Kernel `struct sigaction` <-> [`Disposition`]. The layout musl passes is
/// `{ handler, flags, restorer, mask }` (32 bytes on x86_64).
fn disposition_to_kernel(disposition: Disposition) -> (u64, u64, u64, u64) {
    match disposition {
        Disposition::Default => (signal::SIG_DFL, 0, 0, 0),
        Disposition::Ignore => (signal::SIG_IGN, 0, 0, 0),
        Disposition::Handler {
            handler,
            flags,
            restorer,
            mask,
        } => (handler, flags, restorer, mask),
    }
}

/// `rt_sigaction(sig, act, oldact, sigsetsize)`. `SIGKILL`/`SIGSTOP` are
/// refused with `EINVAL`, like Linux: they cannot be caught or ignored.
fn sys_rt_sigaction(sig: u64, act: u64, oldact: u64, sigsetsize: u64) -> u64 {
    if sig == 0 || sig as usize >= signal::NSIG {
        return err(EINVAL);
    }
    if sigsetsize != 0 && sigsetsize != 8 {
        return err(EINVAL);
    }
    let me = task::current();
    let sig = sig as u8;
    if oldact != 0 {
        let (handler, flags, restorer, mask) = disposition_to_kernel(signal::action(me, sig));
        write_u64(oldact, handler);
        write_u64(oldact + 8, flags);
        write_u64(oldact + 16, restorer);
        // The kernel stores `sa_mask` in its internal bit order.
        write_u64(oldact + 24, signal::kernel_to_linux_sigset(mask));
    }
    if act != 0 {
        let handler = read_u64(act);
        let flags = read_u64(act + 8);
        let restorer = read_u64(act + 16);
        // User space passes a Linux `sigset_t` (bit `sig - 1`).
        let mask = signal::linux_sigset_to_kernel(read_u64(act + 24));
        let disposition = match handler {
            signal::SIG_DFL => Disposition::Default,
            signal::SIG_IGN => Disposition::Ignore,
            handler => Disposition::Handler {
                handler,
                flags,
                restorer,
                // `SIGKILL`/`SIGSTOP` are never blockable, so they can never
                // be part of a handler mask either.
                mask: mask & !(1 << signal::SIGKILL | 1 << signal::SIGSTOP),
            },
        };
        if let Err(error) = signal::set_action(me, sig, disposition) {
            return signal_err(error);
        }
    }
    0
}

/// `rt_sigprocmask(how, set, oldset, sigsetsize)`. `SIGKILL`/`SIGSTOP` bits are
/// silently discarded: POSIX says attempts to block them are ignored.
fn sys_rt_sigprocmask(how: u64, set: u64, oldset: u64, sigsetsize: u64) -> u64 {
    if sigsetsize != 0 && sigsetsize != 8 {
        return err(EINVAL);
    }
    let me = task::current();
    // The kernel keeps its blocked set in internal bit order; user space
    // passes and reads Linux `sigset_t` (bit `sig - 1`).
    let current = signal::blocked(me);
    if set == 0 {
        if oldset != 0 {
            write_u64(oldset, signal::kernel_to_linux_sigset(current));
        }
        return 0;
    }
    if how != signal::SIG_BLOCK && how != signal::SIG_UNBLOCK && how != signal::SIG_SETMASK {
        return err(EINVAL);
    }
    if oldset != 0 {
        write_u64(oldset, signal::kernel_to_linux_sigset(current));
    }
    let requested = signal::linux_sigset_to_kernel(read_u64(set));
    let next = match how {
        signal::SIG_BLOCK => current | requested,
        signal::SIG_UNBLOCK => current & !requested,
        signal::SIG_SETMASK => requested,
        _ => return err(EINVAL),
    };
    signal::set_blocked(me, next);
    0
}

/// `rt_sigreturn()`: the restorer (`__restore_rt`) issues this syscall with RSP
/// just past the `pretcode` slot of the frame `deliver_linux` built. Restore
/// the interrupted registers from the `ucontext_t` and make `sysretq` land
/// there, returning the frame's `rax`.
fn sys_rt_sigreturn() -> u64 {
    let user_rsp = crate::arch::linux::saved_user_rsp();
    let (regs, mask) = signal::parse_linux_frame(user_rsp);
    signal::set_blocked(task::current(), mask);
    crate::arch::linux::set_user_return(regs.rip, regs.rsp, regs.rflags);
    let saved = [
        (0usize, regs.r15),
        (1, regs.r14),
        (2, regs.r13),
        (3, regs.r12),
        (4, regs.rbp),
        (5, regs.rbx),
        (6, regs.rdi),
        (7, regs.rsi),
        (8, regs.rdx),
        (9, regs.r8),
        (10, regs.r9),
        (11, regs.r10),
    ];
    for (slot, value) in saved {
        crate::arch::linux::set_saved_register(slot, value);
    }
    // Keep the captured context in step so a nested delivery builds on the
    // restored registers rather than the `rt_sigreturn` entry state.
    let context = crate::arch::linux::UserContext {
        rip: regs.rip,
        rflags: regs.rflags,
        rsp: regs.rsp,
        rbx: regs.rbx,
        rbp: regs.rbp,
        r12: regs.r12,
        r13: regs.r13,
        r14: regs.r14,
        r15: regs.r15,
        rdi: regs.rdi,
        rsi: regs.rsi,
        rdx: regs.rdx,
        r8: regs.r8,
        r9: regs.r9,
        r10: regs.r10,
    };
    crate::arch::linux::set_user_context(context);
    regs.rax
}

/// `sigaltstack(ss, old_ss)`: install/disable/report the alternate signal
/// stack. The frame lands there when the action carries `SA_ONSTACK`.
fn sys_sigaltstack(ss: u64, old_ss: u64) -> u64 {
    let me = task::current();
    if old_ss != 0 {
        let current = signal::altstack(me);
        write_u64(old_ss, current.sp);
        write_u32(
            old_ss + 8,
            if current.enabled {
                0
            } else {
                signal::SS_DISABLE
            },
        );
        write_u64(old_ss + 16, current.size);
    }
    if ss != 0 {
        let sp = read_u64(ss);
        let flags = read_u32(ss + 8);
        let size = read_u64(ss + 16);
        if flags & signal::SS_ONSTACK != 0 {
            return err(EINVAL); // cannot set a stack marked as in use
        }
        let stack = if flags & signal::SS_DISABLE != 0 {
            signal::AltStack::DISABLED
        } else {
            signal::AltStack {
                sp,
                size,
                enabled: true,
            }
        };
        if let Err(error) = signal::set_altstack(me, stack) {
            return signal_err(error);
        }
    }
    0
}

/// `kill(pid, sig)`: pid > 0 targets one process, 0 the caller's process
/// group, -1 everyone but init, and pid < -1 the group `-pid`.
fn sys_kill(pid: u64, sig: u64) -> u64 {
    let me = task::current();
    let info = signal::SigInfo::user(me, signal::SI_USER);
    match signal::kill(me, pid as i64, sig as u8, info) {
        Ok(()) => 0,
        Err(error) => signal_err(error),
    }
}

/// `tkill(tid, sig)`: send to one thread of the caller's process.
fn sys_tkill(tid: u64, sig: u64) -> u64 {
    let me = task::current();
    let info = signal::SigInfo::user(me, signal::SI_TKILL);
    match signal::send_tid(me, tid as usize, sig as u8, info) {
        Ok(()) => 0,
        Err(error) => signal_err(error),
    }
}

/// `tgkill(tgid, tid, sig)`: like `tkill`, but the thread group must match.
fn sys_tgkill(tgid: u64, tid: u64, sig: u64) -> u64 {
    if signal::tgid_of(tid as usize) != tgid as usize {
        return err(ESRCH);
    }
    sys_tkill(tid, sig)
}

/// Map a group/session error to its Linux errno.
fn group_err(error: GroupError) -> u64 {
    match error {
        GroupError::NoSuchProcess => err(ESRCH),
        GroupError::NotPermitted => err(EPERM),
        GroupError::Invalid => err(EINVAL),
    }
}

/// Unwrap a group/session syscall result, mapping errors to errno.
fn group_result(result: Result<usize, GroupError>) -> u64 {
    match result {
        Ok(value) => value as u64,
        Err(error) => group_err(error),
    }
}

/// `setpgid(pid, pgid)`: change the caller's group, or a child's.
fn sys_setpgid(pid: u64, pgid: u64) -> u64 {
    match task::process::setpgid(task::current(), pid as i64, pgid as i64) {
        Ok(()) => 0,
        Err(error) => group_err(error),
    }
}

/// `setsid()`: start a new session and group with the caller as leader.
fn sys_setsid() -> u64 {
    group_result(task::process::setsid(task::current()))
}

/// `getpgid(pid)`: the process group of `pid` (0 = the caller).
fn sys_getpgid(pid: u64) -> u64 {
    group_result(task::process::getpgid(task::current(), pid as i64))
}

/// `getsid(pid)`: the session of `pid` (0 = the caller).
fn sys_getsid(pid: u64) -> u64 {
    group_result(task::process::getsid(task::current(), pid as i64))
}

/// `exit_group(code)`: terminate the caller's *thread group* — every task that
/// shares its address space — each with `code`.
///
/// This fixes the #59 deviation, where `exit_group` killed the process group
/// (which would take forked children down with it): a process group is the job
/// control unit, the thread group is the process. Every thread's
/// `clear_child_tid` word is zeroed and futex-woken so joiners wake.
fn sys_exit_group(code: u64) -> u64 {
    let tids = task::exit_thread_group(code & 0xff);
    for tid in tids {
        if tid != 0 {
            write_u32(tid, 0);
            futex_wake(tid, 1);
        }
    }
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

fn sys_exit(code: u64) -> u64 {
    // Thread exit: clear the TID word and wake anyone joining on it.
    let tid = task::clear_child_tid();
    if tid != 0 {
        write_u32(tid, 0);
        futex_wake(tid, 1);
    }
    task::finish_current(code & 0xff);
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}
