//! Thin wrappers around the LazyOS `int 0x80` syscalls.
//!
//! Register convention: `rax` = syscall number, args in `rdi`, `rsi`, `rdx`,
//! result in `rax`.

use core::arch::asm;

/// `exit(code)` — terminate the program.
pub const SYS_EXIT: u64 = 0;
/// `write(ptr, len)` — write bytes to the console.
pub const SYS_WRITE: u64 = 1;
/// `read_char()` — block until a key is pressed, return its code.
pub const SYS_READ_CHAR: u64 = 2;
/// `read_file(name, buf, len)` — read a file into a buffer.
pub const SYS_READ_FILE: u64 = 3;
/// `sbrk(incr)` — grow the heap, returning the previous break.
pub const SYS_SBRK: u64 = 4;

/// Write raw bytes to the console.
pub fn write(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // Safety: `int 0x80` with syscall 1 and a valid buffer.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_WRITE,
            in("rdi") bytes.as_ptr() as u64,
            in("rsi") bytes.len() as u64,
            lateout("rax") _,
            options(nostack),
        );
    }
}

/// Write a string to the console.
pub fn write_str(text: &str) {
    write(text.as_bytes());
}

/// Block until a key is pressed; returns its character code.
pub fn read_char() -> u64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 2; result in rax.
    unsafe {
        asm!("int 0x80", in("rax") SYS_READ_CHAR, lateout("rax") code, options(nostack));
    }
    code
}

/// Read a file named by a **NUL-terminated** byte slice into `buf`.
/// Returns the number of bytes read, or `None` if the file was not found.
pub fn read_file(name_z: &[u8], buf: &mut [u8]) -> Option<usize> {
    let count: u64;
    // Safety: `int 0x80` with syscall 3; pointers are valid for their lengths.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_READ_FILE,
            in("rdi") name_z.as_ptr() as u64,
            in("rsi") buf.as_mut_ptr() as u64,
            in("rdx") buf.len() as u64,
            lateout("rax") count,
            options(nostack),
        );
    }
    if count == u64::MAX {
        None
    } else {
        Some(count as usize)
    }
}

/// Grow the heap by `increment` bytes; returns the previous break, or
/// `u64::MAX` on failure. Calling with 0 reports the current break.
pub fn sbrk(increment: u64) -> u64 {
    let previous: u64;
    // Safety: `int 0x80` with syscall 4.
    unsafe {
        asm!("int 0x80", in("rax") SYS_SBRK, in("rdi") increment, lateout("rax") previous, options(nostack));
    }
    previous
}

/// Terminate the program; does not return.
pub fn exit(code: u32) -> ! {
    // Safety: `int 0x80` with syscall 0; does not return.
    unsafe {
        asm!("int 0x80", in("rax") SYS_EXIT, in("rdi") code as u64, options(noreturn, nostack));
    }
}
