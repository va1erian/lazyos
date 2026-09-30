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
/// `spawn(cmdline)` — start an ELF as a child of the caller (issue #93).
pub const SYS_SPAWN: u64 = 6;
/// `wait(deadline)` — reap a child exit, packing `(pid << 32) | status`.
pub const SYS_WAIT: u64 = 7;
/// `clock()` — the PIT tick counter (100 Hz), absolute deadlines.
pub const SYS_CLOCK: u64 = 8;
/// `args(buf, len)` — copy this service's manifest argument string.
pub const SYS_ARGS: u64 = 9;
/// `creds(op, a1, a2)` — the audited credential gate (issue #101).
pub const SYS_CREDS: u64 = 10;
/// `display(op, a1, a2)` — the display device grant (issue #113).
pub const SYS_DISPLAY: u64 = 12;
/// `tasks(buf)` — scheduler task-list introspection (MCP debug bridge Phase 2).
pub const SYS_TASKS: u64 = 13;
/// `system_stats(op, a1, a2)` — the read-only system monitor surface
/// (syscall 14). See [`crate::sysinfo`] for the typed client.
pub const SYS_SYSTEM_STATS: u64 = 14;

/// Value returned by the service syscalls on failure/timeout.
pub const SERVICE_ERROR: u64 = u64::MAX;

mod cred;
mod display;
mod input;
mod introspect;
mod wall;

pub use cred::*;
pub use display::*;
pub use input::*;
pub use introspect::*;
pub use wall::*;

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

/// Start the program named by a **NUL-terminated** command line
/// (`"PATH.ELF [args...]"`) as a child of the calling task. Returns the child's
/// pid (its task slot), or `None` when the file is missing or no resource is
/// free. The kernel remembers the argument string for [`service_args`].
pub fn spawn(cmdline_z: &[u8]) -> Option<u64> {
    let pid: u64;
    // Safety: `int 0x80` with syscall 6 and a valid NUL-terminated buffer.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_SPAWN,
            in("rdi") cmdline_z.as_ptr() as u64,
            lateout("rax") pid,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    (pid != SERVICE_ERROR).then_some(pid)
}

/// Wait for a child exit and reap it, up to the absolute PIT `deadline`
/// (`0` waits forever). Returns `Some((pid, status))`, or `None` on timeout.
pub fn wait(deadline: u64) -> Option<(u64, u64)> {
    let packed: u64;
    // Safety: `int 0x80` with syscall 7; no pointers cross the gate.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_WAIT,
            in("rdi") deadline,
            lateout("rax") packed,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    (packed != SERVICE_ERROR).then_some((packed >> 32, packed & 0xffff_ffff))
}

/// The PIT tick counter (100 Hz), the supervisor's clock. Deadlines passed to
/// [`wait`] and to the Messenger API are absolute values of this clock.
pub fn clock() -> u64 {
    let ticks: u64;
    // Safety: `int 0x80` with syscall 8; no arguments.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_CLOCK,
            lateout("rax") ticks,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    ticks
}

/// Copy this service's manifest argument string into `buf`; returns its full
/// length. A zero-length `buf` reports the length without copying.
pub fn service_args(buf: &mut [u8]) -> usize {
    let length: u64;
    // Safety: `int 0x80` with syscall 9; the buffer is valid for its length.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_ARGS,
            in("rdi") buf.as_mut_ptr() as u64,
            in("rsi") buf.len() as u64,
            lateout("rax") length,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    length as usize
}
