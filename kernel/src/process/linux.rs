//! Linux x86_64 ABI: loading static ELFs and the syscall dispatch.
//!
//! Only the subset `std`/musl need to reach `main` is implemented; everything
//! else is logged as `ENOSYS` (see `tools/abi/coverage.py`).

use alloc::string::String;
use alloc::vec::Vec;
use x86_64::PhysAddr;
use xmas_elf::program::Type as ProgramType;
use xmas_elf::ElfFile;

use super::{load_segments, map_range, page_phys};
use crate::task::{self, Fd, FdKind};

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
const EBADF: u64 = 9;
const ESPIPE: u64 = 29;

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
pub fn load(table: PhysAddr, elf_bytes: &[u8]) -> Result<(u64, u64), &'static str> {
    let entry = load_segments(table, elf_bytes)?;
    let stack = map_range(table, STACK_TOP - STACK_SIZE, STACK_TOP)?;

    let phdr = program_header_addr(elf_bytes);
    let (phent, phnum) = phdr_size(elf_bytes);
    let rsp = build_start_stack(&stack, entry, phdr, phent, phnum);
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
fn build_start_stack(stack: &[(u64, u64)], entry: u64, phdr: u64, phent: u16, phnum: u16) -> u64 {
    let mut cursor = STACK_TOP;

    // Helper: write bytes just below `cursor`.
    let push_bytes = |bytes: &[u8], cursor: &mut u64| -> u64 {
        *cursor -= bytes.len() as u64;
        write_user(&stack, *cursor, bytes);
        *cursor
    };

    // Strings.
    let execfn = push_bytes(b"init\0", &mut cursor);
    let mut random = [0u8; 16];
    fill_random(&mut random);
    let random_addr = push_bytes(&random, &mut cursor);
    let arg0 = push_bytes(b"init\0", &mut cursor);

    // Word arrays (low to high): argc, argv[], NULL, envp NULL, auxv, AT_NULL.
    let mut words: Vec<u64> = Vec::new();
    words.push(1); // argc
    words.push(arg0);
    words.push(0); // argv NULL
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
        write_user(&stack, cursor + (i as u64) * 8, &word.to_le_bytes());
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
extern "C" fn linux_dispatch(
    nr: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    _a5: u64,
    _a6: u64,
) -> u64 {
    match nr {
        0 => sys_read(a1, a2, a3),
        1 => sys_write(a1, a2, a3),
        2 => sys_openat(AT_FDCWD, a1, a2), // open
        3 => sys_close(a1),
        4 => sys_stat_path(a1, a2), // stat(path, buf)
        5 => sys_fstat(a1, a2),     // fstat(fd, buf)
        7 => sys_poll(a1, a2),      // poll
        8 => sys_lseek(a1, a2, a3), // lseek
        9 => sys_mmap(a1, a2, a3, a4),
        10 => 0, // mprotect (ignore)
        11 => 0, // munmap (ignore)
        12 => sys_brk(a1),
        13 | 14 | 131 => 0, // rt_sigaction/procmask, sigaltstack
        16 => sys_ioctl(a1),
        21 => sys_access(a1),               // access(path, mode)
        28 => 0,                            // madvise
        32 | 33 => sys_dup(nr, a1, a2),     // dup / dup2
        39 | 186 => task::current() as u64, // getpid/gettid (kernel task 0 is PID 0)
        60 | 231 => sys_exit(),
        63 => sys_uname(a1),
        72 => sys_fcntl(a1, a2), // fcntl
        79 => sys_getcwd(a1, a2),
        89 => err(EINVAL), // readlink (no links yet)
        158 => sys_arch_prctl(a1, a2),
        204 => sys_sched_getaffinity(a2, a3),
        217 => 0,                      // getdents64 (empty for now)
        218 => task::current() as u64, // set_tid_address
        228 => sys_clock_gettime(a1, a2),
        257 => sys_openat(a1, a2, a3), // openat
        262 => sys_newfstatat(a1, a2, a3, a4),
        273 => 0, // set_robust_list
        318 => sys_getrandom(a1, a2),
        334 => err(ENOSYS), // rseq (musl falls back)
        _ => {
            crate::serial_println!("ENOSYS {} syscall_{}", nr, nr);
            err(ENOSYS)
        }
    }
}

fn sys_poll(fds: u64, nfds: u64) -> u64 {
    // struct pollfd { i32 fd; i16 events; i16 revents; } — report nothing ready.
    for i in 0..nfds {
        // Safety: user array of `nfds` pollfd entries.
        unsafe {
            core::ptr::write_volatile((fds + i * 8 + 6) as *mut u16, 0);
        }
    }
    0
}

fn sys_write(fd: u64, ptr: u64, len: u64) -> u64 {
    if fd > 2 {
        return err(EBADF); // files are read-only for now
    }
    // Safety: the caller passes a valid user buffer.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    task::write_output(bytes);
    crate::serial::write_bytes(bytes);
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

/// Block until a line is typed on the task's terminal.
fn read_terminal(ptr: u64, len: u64) -> u64 {
    let mut written = 0u64;
    while written < len {
        if let Some(key) = task::take_key() {
            let byte = key_to_byte(key);
            // Safety: destination within the user buffer.
            unsafe { core::ptr::write_volatile((ptr + written) as *mut u8, byte) };
            written += 1;
            if byte == b'\n' {
                break;
            }
        } else {
            x86_64::instructions::interrupts::enable();
            x86_64::instructions::hlt();
        }
    }
    written
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

fn sys_ioctl(fd: u64) -> u64 {
    if fd <= 2 {
        0 // pretend tty; TCGETS etc. succeed
    } else {
        err(ENOTTY)
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

fn sys_clock_gettime(clock: u64, out: u64) -> u64 {
    let ticks = crate::arch::idt::TICKS.load(core::sync::atomic::Ordering::Relaxed);
    let sec = (ticks / 100) as i64;
    let nsec = ((ticks % 100) * 10_000_000) as i64;
    let _ = clock;
    // struct timespec { i64 tv_sec; i64 tv_nsec; }
    // Safety: user buffer.
    unsafe {
        core::ptr::write_volatile(out as *mut i64, sec);
        core::ptr::write_volatile((out + 8) as *mut i64, nsec);
    }
    0
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
        "/" | "/dev" | "/proc" | "/etc" | "/tmp" => fd_result(task::fd_open(Fd::File {
            data: Vec::new(),
            offset: 0,
        })),
        _ => {
            let name = path.trim_start_matches('/');
            match crate::fs::stat(name) {
                Some((_, true)) => fd_result(task::fd_open(Fd::File {
                    data: Vec::new(),
                    offset: 0,
                })),
                Some((_, false)) => match crate::fs::read(name) {
                    Some(data) => fd_result(task::fd_open(Fd::File { data, offset: 0 })),
                    None => err(ENOENT),
                },
                None => err(ENOENT),
            }
        }
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
    let known = matches!(
        path.as_str(),
        "/" | "/dev"
            | "/proc"
            | "/etc"
            | "/tmp"
            | "/dev/tty"
            | "/dev/console"
            | "/dev/tty0"
            | "/dev/tty1"
            | "/dev/null"
            | "/dev/zero"
    );
    if known || crate::fs::stat(path.trim_start_matches('/')).is_some() {
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
    if matches!(path, "/" | "/dev" | "/proc" | "/etc" | "/tmp") {
        fill_stat(buf, S_IFDIR | 0o755, 0, 1);
        return 0;
    }
    match crate::fs::stat(path.trim_start_matches('/')) {
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

fn sys_exit() -> u64 {
    task::finish_current();
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}
