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

use super::{load_segments, map_range, page_phys};
use crate::task::wait::WaitQueue;
use crate::task::{self, Fd, FdKind, WaitKind, WakeReason};

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
        202 => "futex",
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

// `struct stat` file-type bits.
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFCHR: u32 = 0o020000;

fn err(e: u64) -> u64 {
    (e as i64).wrapping_neg() as u64
}

const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;

/// Load a Linux image into `table`, build its start stack, and return
/// `(entry, stack_pointer)`.
pub fn load(table: PhysAddr, elf_bytes: &[u8], argv0: &str) -> Result<(u64, u64), &'static str> {
    let entry = load_segments(table, elf_bytes)?;
    let stack = map_range(table, STACK_TOP - STACK_SIZE, STACK_TOP)?;

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
    match nr {
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
        10 => 0, // mprotect (ignore)
        11 => 0, // munmap (ignore)
        12 => sys_brk(a1),
        13 | 14 | 131 => 0, // rt_sigaction/procmask, sigaltstack
        16 => sys_ioctl(a1, a2, a3),
        19 => sys_readv(a1, a2, a3),
        20 => sys_writev(a1, a2, a3),
        21 => sys_access(a1),                // access(path, mode)
        28 => 0,                             // madvise
        32 | 33 => sys_dup(nr, a1, a2),      // dup / dup2
        35 => sys_nanosleep(a1),             // nanosleep(req, rem)
        39 | 186 => task::current() as u64,  // getpid/gettid (kernel task 0 is PID 0)
        56 => sys_clone(a1, a2, a3, a4, a5), // clone(flags, stack, ptid, ctid, tls)
        57 => sys_fork(),
        59 => sys_execve(a1, a2, a3), // execve(path, argv, envp)
        60 | 231 => sys_exit(a1),
        61 => sys_wait4(a1, a2, a3), // wait4(pid, status, options)
        62 => 0,                     // kill (signals accepted, not delivered yet)
        63 => sys_uname(a1),
        72 => sys_fcntl(a1, a2), // fcntl
        79 => sys_getcwd(a1, a2),
        80 => 0,                                         // chdir (root-only)
        89 => sys_readlink(a1, a2, a3),                  // readlink
        95 => 0,                                         // umask
        96 => sys_gettimeofday(a1),                      // gettimeofday(tv, tz)
        102 | 103 | 104 | 105 => 0,                      // getuid/getgid/geteuid/getegid
        106 | 107 | 108 | 109 | 112 | 113 => 0,          // set[re]uid/gid, setpgid/setsid
        110 | 111 | 121 | 124 => task::current() as u64, // getppid/pgrp/pgid/sid
        157 => 0,                                        // prctl (accept)
        158 => sys_arch_prctl(a1, a2),
        169 => 0,                     // reboot (accept)
        202 => sys_futex(a1, a2, a3), // futex(uaddr, op, val)
        204 => sys_sched_getaffinity(a2, a3),
        217 => sys_getdents64(a1, a2, a3), // getdents64
        218 => sys_set_tid_address(a1),
        228 => sys_clock_gettime(a1, a2),
        229 => sys_clock_getres(a2),
        230 => sys_nanosleep(a3), // clock_nanosleep(clockid, flags, req, rem)
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
    }
}

/// `writev(fd, iov, iovcnt)`: `struct iovec { void *base; size_t len; }`.
fn sys_writev(fd: u64, iov: u64, count: u64) -> u64 {
    let mut total = 0u64;
    for i in 0..count {
        // Safety: user array of iovec entries.
        let base = unsafe { core::ptr::read_volatile((iov + i * 16) as *const u64) };
        let len = unsafe { core::ptr::read_volatile((iov + i * 16 + 8) as *const u64) };
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
        // Safety: user array of iovec entries.
        let base = unsafe { core::ptr::read_volatile((iov + i * 16) as *const u64) };
        let len = unsafe { core::ptr::read_volatile((iov + i * 16 + 8) as *const u64) };
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
        // Safety: user array of pollfd entries.
        let fd = unsafe { core::ptr::read_volatile((fds + i * 8) as *const i32) };
        let events = unsafe { core::ptr::read_volatile((fds + i * 8 + 4) as *const u16) };
        let revents = if fd == 0 && events & POLLIN != 0 && task::input_available() {
            ready += 1;
            POLLIN
        } else {
            0
        };
        // Safety: user array of pollfd entries.
        unsafe { core::ptr::write_volatile((fds + i * 8 + 6) as *mut u16, revents) };
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
    // Safety: the caller passes a valid user buffer.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
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
            // Safety: destination within the user buffer.
            unsafe { core::ptr::write_volatile(ptr as *mut u8, key_to_byte(key)) };
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

fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64) -> u64 {
    if flags & MAP_ANONYMOUS == 0 {
        return err(ENODEV); // file-backed mmap not supported yet
    }
    let len = align_up(len, PAGE);
    let base = if flags & MAP_FIXED != 0 {
        addr & !0xFFF
    } else {
        task::mmap_next().max(MMAP_BASE)
    };
    let end = base + len;
    if end > MMAP_LIMIT {
        return err(ENOMEM);
    }
    let table = crate::mem::kernel_table();
    match map_range(table, base, end) {
        Ok(_) => {
            if flags & MAP_FIXED == 0 {
                task::set_mmap_next(end);
            }
            let _ = prot;
            base
        }
        Err(_) => err(ENOMEM),
    }
}

fn sys_brk(addr: u64) -> u64 {
    let current = task::brk();
    if addr == 0 || addr < BRK_BASE {
        return current;
    }
    let new = align_up(addr, PAGE);
    if new > BRK_LIMIT {
        return current;
    }
    if new > current {
        let table = crate::mem::kernel_table();
        if map_range(table, current, new).is_err() {
            return current;
        }
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
            // Safety: user pointer.
            unsafe { core::ptr::write_volatile(addr as *mut u64, 0) };
            0
        }
        _ => err(EINVAL),
    }
}

fn sys_ioctl(fd: u64, request: u64, arg: u64) -> u64 {
    match request {
        0x5401 => 0, // TCGETS: report a default (zeroed) termios
        0x540F => {
            // TIOCGPGRP: report the foreground process group.
            // Safety: user `pid_t *`.
            unsafe { core::ptr::write_volatile(arg as *mut u32, task::current() as u32) };
            0
        }
        0x5410 => 0, // TIOCSPGRP
        0x5413 => {
            // TIOCGWINSZ: 24 rows x 80 columns.
            // Safety: user `struct winsize`.
            unsafe {
                core::ptr::write_volatile(arg as *mut u16, 24);
                core::ptr::write_volatile((arg + 2) as *mut u16, 80);
                core::ptr::write_volatile((arg + 4) as *mut u16, 0);
                core::ptr::write_volatile((arg + 6) as *mut u16, 0);
            }
            0
        }
        _ if fd <= 2 => 0,
        _ => err(ENOTTY),
    }
}

fn sys_sched_getaffinity(mask: u64, len: u64) -> u64 {
    if len >= 8 {
        // Safety: user buffer.
        unsafe { core::ptr::write_volatile(mask as *mut u64, 1) };
        8
    } else if len > 0 {
        // Safety: user buffer.
        unsafe { core::ptr::write_volatile(mask as *mut u8, 1) };
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
    // Safety: user buffer of at least 390 bytes (musl's utsname).
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), buf as *mut u8, data.len());
    }
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
    // Safety: user buffer of at least `n` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(target.as_ptr(), buf as *mut u8, n);
    }
    n as u64
}

fn sys_getcwd(buf: u64, size: u64) -> u64 {
    if size < 2 {
        return err(EINVAL);
    }
    // Safety: user buffer.
    unsafe {
        core::ptr::write_volatile(buf as *mut u8, b'/');
        core::ptr::write_volatile((buf + 1) as *mut u8, 0);
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
    // Safety: user buffer holds a `struct timespec`.
    unsafe {
        core::ptr::write_volatile(out as *mut i64, sec as i64);
        core::ptr::write_volatile((out + 8) as *mut i64, nsec as i64);
    }
}

fn sys_clock_getres(out: u64) -> u64 {
    // 100 Hz PIT => 10 ms resolution.
    write_timespec(out, 0, 10_000_000);
    0
}

fn sys_gettimeofday(tv: u64) -> u64 {
    let ticks = now_ticks();
    // Safety: user buffer holds a `struct timeval`.
    unsafe {
        core::ptr::write_volatile(tv as *mut i64, (REALTIME_BASE + ticks / 100) as i64);
        core::ptr::write_volatile((tv + 8) as *mut i64, ((ticks % 100) * 10_000) as i64);
    }
    0
}

/// Sleep for the `struct timespec` duration at `req` (nanosleep/clock_nanosleep).
fn sys_nanosleep(req: u64) -> u64 {
    // Safety: user buffer holds a `struct timespec`.
    let (sec, nsec) = unsafe {
        (
            core::ptr::read_volatile(req as *const i64),
            core::ptr::read_volatile((req + 8) as *const i64),
        )
    };
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
        // Safety: user buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(chunk.as_ptr(), (buf + written) as *mut u8, n);
        }
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
        // Safety: user memory up to the NUL terminator.
        let byte = unsafe { core::ptr::read_volatile((ptr + i) as *const u8) };
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

/// Look up a path (FAT root, synthetic dirs, or a BusyBox applet alias).
fn lookup(path: &str) -> Option<(u32, bool)> {
    if matches!(
        path,
        "/" | "/bin" | "/sbin" | "/usr" | "/dev" | "/proc" | "/etc" | "/tmp"
    ) {
        return Some((0, true));
    }
    if let Some(entry) = crate::fs::stat(path.trim_start_matches('/')) {
        return Some(entry);
    }
    if applet_name(path).is_some() {
        return crate::fs::stat("busybox").map(|(size, _)| (size, false));
    }
    None
}

/// Load a file's bytes (with the BusyBox applet alias).
fn load_file(path: &str) -> Option<Vec<u8>> {
    if let Some(data) = crate::fs::read(path.trim_start_matches('/')) {
        return Some(data);
    }
    if applet_name(path).is_some() {
        return crate::fs::read("busybox");
    }
    None
}

/// Build a `linux_dirent64` byte stream for the FAT root, so `getdents64` can
/// read it like a file (we have no real directory fd yet).
fn dir_data() -> Vec<u8> {
    const DT_DIR: u8 = 4;
    const DT_REG: u8 = 8;
    let mut out = Vec::new();
    push_dirent(&mut out, 1, DT_DIR, ".");
    push_dirent(&mut out, 1, DT_DIR, "..");
    for (name, is_dir, _) in crate::fs::list() {
        let ino = name
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31) + b as u64)
            | 2;
        push_dirent(&mut out, ino, if is_dir { DT_DIR } else { DT_REG }, &name);
    }
    out
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

/// Open a path: the root-only FAT volume plus a few synthetic device nodes.
fn open_path(path: &str) -> u64 {
    match path {
        "/dev/tty" | "/dev/console" | "/dev/tty0" | "/dev/tty1" => {
            fd_result(task::fd_open(Fd::Terminal))
        }
        "/dev/null" | "/dev/zero" | "/dev/full" => fd_result(task::fd_open(Fd::File {
            data: Vec::new(),
            offset: 0,
        })),
        // Synthetic directories (empty until `getdents64` exists).
        "/" | "/bin" | "/sbin" | "/usr" | "/dev" | "/proc" | "/etc" | "/tmp" => {
            fd_result(task::fd_open(Fd::File {
                data: dir_data(),
                offset: 0,
            }))
        }
        _ => match lookup(path) {
            Some((_, true)) => fd_result(task::fd_open(Fd::File {
                data: dir_data(),
                offset: 0,
            })),
            Some((_, false)) => match load_file(path) {
                Some(data) => fd_result(task::fd_open(Fd::File { data, offset: 0 })),
                None => err(ENOENT),
            },
            None => err(ENOENT),
        },
    }
}

fn sys_openat(_dirfd: u64, path: u64, _flags: u64) -> u64 {
    match read_cstr(path) {
        Some(path) => open_path(&path),
        None => err(EINVAL),
    }
}

fn sys_close(fd: u64) -> u64 {
    if task::fd_close(fd as usize) {
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
fn sys_access(path: u64) -> u64 {
    let Some(path) = read_cstr(path) else {
        return err(EINVAL);
    };
    if lookup(&path).is_some() {
        0
    } else {
        err(ENOENT)
    }
}

fn sys_dup(nr: u64, a1: u64, a2: u64) -> u64 {
    let slot = if nr == 32 {
        task::fd_dup(a1 as usize)
    } else {
        task::fd_dup2(a1 as usize, a2 as usize)
    };
    match slot {
        Some(fd) => fd as u64,
        None => err(EBADF),
    }
}

fn sys_fcntl(fd: u64, cmd: u64) -> u64 {
    match cmd {
        0 => match task::fd_dup(fd as usize) {
            // F_DUPFD
            Some(new) => new as u64,
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
    // Safety: caller ensures the address is valid user memory.
    unsafe { core::ptr::write_volatile(addr as *mut u64, value) };
}

fn write_u32(addr: u64, value: u32) {
    // Safety: caller ensures the address is valid user memory.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) };
}

fn sys_fstat(fd: u64, buf: u64) -> u64 {
    if fd <= 2 {
        fill_stat(buf, S_IFCHR | 0o620, 0, 0);
        return 0;
    }
    match task::fd_kind(fd as usize) {
        FdKind::File => {
            let size = task::fd_size(fd as usize).unwrap_or(0);
            fill_stat(buf, S_IFREG | 0o444, size, fd);
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
    match lookup(path) {
        Some((size, true)) => {
            fill_stat(buf, S_IFDIR | 0o755, size as u64, 1);
            0
        }
        Some((size, false)) => {
            fill_stat(buf, S_IFREG | 0o444, size as u64, 2);
            0
        }
        None => err(ENOENT),
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
        // Safety: user memory up to the NUL terminator.
        let byte = unsafe { core::ptr::read_volatile((ptr + i) as *const u8) };
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
        // Safety: user array of `char *`.
        let ptr = unsafe { core::ptr::read_volatile((arr + i * 8) as *const u64) };
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

    let Some(elf) = load_file(resolve_exe(&path)) else {
        return err(ENOENT);
    };
    let Some(table) = crate::mem::new_user_table() else {
        return err(ENOMEM);
    };
    let entry = match load_segments(table, &elf) {
        Ok(entry) => entry,
        Err(_) => return err(ENOEXEC),
    };
    let stack = match map_range(table, STACK_TOP - STACK_SIZE, STACK_TOP) {
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
    // Safety: the futex word is a user 32-bit value.
    let current = unsafe { core::ptr::read_volatile(uaddr as *const u32) };
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
