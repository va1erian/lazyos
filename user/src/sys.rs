//! Thin wrappers around the LazyOS `int 0x80` syscalls.
//!
//! Register convention: `rax` = syscall number, args in `rdi`, `rsi`, `rdx`,
//! result in `rax`.
//!
//! The kernel's `int 0x80` stub saves and restores
//! `rdi`/`rsi`/`rdx`/`r8`/`r9`/`r10` around the Rust dispatcher, and `rax`
//! carries the result. `rcx` and `r11` are *not* preserved, so every wrapper
//! declares `clobber_abi("sysv64")`; otherwise the compiler may keep a live
//! value in them across the gate (issue #91 hit exactly that in `bootstrap()`).

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
/// `messenger(op, args, result)` — the native Messenger surface (issue #69).
pub const SYS_MESSENGER: u64 = 5;

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
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
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
        asm!(
            "int 0x80",
            in("rax") SYS_READ_CHAR,
            lateout("rax") code,
            options(nostack),
            clobber_abi("sysv64"),
        );
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
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
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
        asm!(
            "int 0x80",
            in("rax") SYS_SBRK,
            in("rdi") increment,
            lateout("rax") previous,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    previous
}

/// Invoke the native Messenger syscall: `op` selects the operation, `args`
/// and `result` are user addresses of the fixed-size blocks defined in
/// [`crate::messenger`]. Returns 0 on success or a negative errno.
pub fn messenger(op: u64, args: u64, result: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 5; the kernel validates both pointers
    // against this task's address space before touching them.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_MESSENGER,
            in("rdi") op,
            in("rsi") args,
            in("rdx") result,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

/// Terminate the program; does not return.
pub fn exit(code: u32) -> ! {
    // Safety: `int 0x80` with syscall 0; does not return.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_EXIT,
            in("rdi") code as u64,
            options(noreturn, nostack),
            clobber_abi("sysv64"),
        );
    }
}
