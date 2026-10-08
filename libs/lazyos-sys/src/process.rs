//! The native runtime's basics: the console (syscalls 1-3), the heap break
//! (4), exiting (0), reaping children (7) and reading this program's own
//! `argv`/`envp` blocks (9). A static-musl program has `std` for these.

use crate::nr;

pub use crate::raw::exit;

/// Value the console and child-wait syscalls return on failure or timeout.
pub const SERVICE_ERROR: u64 = u64::MAX;

/// syscall 9 selector: the `argv` block.
pub const ARGS_ARGV: u64 = 0;
/// syscall 9 selector: the `envp` block.
pub const ARGS_ENVP: u64 = 1;

/// Write raw bytes to the console.
pub fn write(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // SAFETY: the kernel reads `bytes.len()` bytes from `bytes`.
    unsafe { crate::raw::syscall2(nr::WRITE, bytes.as_ptr() as u64, bytes.len() as u64) };
}

/// Write a string to the console.
pub fn write_str(text: &str) {
    write(text.as_bytes());
}

/// Block until a key is pressed; returns its character code.
pub fn read_char() -> u64 {
    crate::raw::syscall0(nr::READ_CHAR) as u64
}

/// Read the file named by the **NUL-terminated** `name_z` into `buf`.
/// Returns the number of bytes read, or `None` if the file was not found.
pub fn read_file(name_z: &[u8], buf: &mut [u8]) -> Option<usize> {
    if !name_z.contains(&0) {
        return None;
    }
    // SAFETY: the kernel reads the NUL-terminated name (checked above to hold
    // its NUL) and writes at most `buf.len()` bytes into `buf`.
    let count = unsafe {
        crate::raw::syscall3(
            nr::READ_FILE,
            name_z.as_ptr() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    } as u64;
    (count != SERVICE_ERROR).then_some(count as usize)
}

/// Grow the heap by `increment` bytes; returns the previous break, or
/// [`SERVICE_ERROR`] on failure. `0` reports the current break.
pub fn sbrk(increment: u64) -> u64 {
    // SAFETY: syscall 4 takes no pointer; the kernel maps the new pages.
    unsafe { crate::raw::syscall1(nr::SBRK, increment) as u64 }
}

/// Wait for a child exit and reap it, up to the absolute PIT `deadline`
/// (`0` waits forever). Returns `Some((pid, status))`, or `None` on timeout.
pub fn wait(deadline: u64) -> Option<(u64, u64)> {
    // SAFETY: syscall 7 takes no pointer.
    let packed = unsafe { crate::raw::syscall1(nr::WAIT, deadline) } as u64;
    (packed != SERVICE_ERROR).then_some((packed >> 32, packed & 0xffff_ffff))
}

/// Copy this program's block `which` ([`ARGS_ARGV`] or [`ARGS_ENVP`], each
/// a run of NUL-terminated strings) into `buf`; returns its full length (an
/// empty `buf` just measures), or a negative errno.
pub fn args_block(buf: &mut [u8], which: u64) -> i64 {
    // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`.
    unsafe { crate::raw::syscall3(nr::ARGS, buf.as_mut_ptr() as u64, buf.len() as u64, which) }
}
