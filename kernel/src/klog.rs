//! The kernel boot-log ring (H1 of `docs/real-pc-boot-plan.md`).
//!
//! A real PC usually has no serial port, so everything `serial_println!`
//! writes also lands here: a fixed RAM ring of the most recent [`CAPACITY`]
//! bytes, kept whether or not COM1 exists. The panic screen draws its tail
//! (`panic_screen`), and userspace reads it with the Linux `syslog(2)` call
//! (BusyBox `dmesg`, `process::linux::misc::sys_syslog`).
//!
//! The ring is written with interrupts off, so an interrupt handler that
//! logs cannot spin on a lock its own interrupted code holds; the panic path
//! reads it with `try_lock` only, never waiting.

use core::fmt;

use spin::Mutex;

/// Bytes the ring keeps (the oldest are overwritten first).
pub const CAPACITY: usize = 64 * 1024;

/// The ring proper; generic over its size so the kernel suite can exercise
/// wrap-around on a small one.
pub struct Ring<const N: usize> {
    bytes: [u8; N],
    /// Total bytes ever written; `written % N` is the next write position.
    written: u64,
}

impl<const N: usize> Ring<N> {
    pub const fn new() -> Self {
        Ring {
            bytes: [0; N],
            written: 0,
        }
    }

    /// Append `data`, overwriting the oldest bytes once full.
    pub fn push(&mut self, data: &[u8]) {
        // Only the last N bytes of an oversized write can survive anyway.
        let data = &data[data.len().saturating_sub(N)..];
        for &byte in data {
            self.bytes[(self.written % N as u64) as usize] = byte;
            self.written += 1;
        }
    }

    /// Bytes currently held (at most `N`).
    fn held(&self) -> usize {
        self.written.min(N as u64) as usize
    }

    /// Total bytes ever pushed, including those since overwritten.
    pub fn total(&self) -> u64 {
        self.written
    }

    /// Copy the held bytes, oldest first, into `out`; returns how many were
    /// copied (the *newest* `out.len()` when `out` is smaller than the ring).
    pub fn copy_to(&self, out: &mut [u8]) -> usize {
        let held = self.held();
        let count = held.min(out.len());
        let first = self.written - count as u64;
        for (i, slot) in out.iter_mut().take(count).enumerate() {
            *slot = self.bytes[((first + i as u64) % N as u64) as usize];
        }
        count
    }
}

impl<const N: usize> Default for Ring<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for Ring<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}

static RING: Mutex<Ring<CAPACITY>> = Mutex::new(Ring::new());

/// Append formatted text (the `serial_print!` path).
pub fn write_fmt(args: fmt::Arguments) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _ = fmt::Write::write_fmt(&mut *RING.lock(), args);
    });
}

/// Copy the newest bytes into `out` (oldest first). Never waits: when the
/// ring is locked (a panic inside a log write) it returns 0.
pub fn snapshot_nowait(out: &mut [u8]) -> usize {
    match RING.try_lock() {
        Some(ring) => ring.copy_to(out),
        None => 0,
    }
}

/// Copy the newest bytes into `out` (oldest first).
pub fn snapshot(out: &mut [u8]) -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| RING.lock().copy_to(out))
}

/// Total bytes logged since boot.
#[allow(dead_code)] // Reported by `dmesg`-style tools; the suite reads it.
pub fn total() -> u64 {
    x86_64::instructions::interrupts::without_interrupts(|| RING.lock().total())
}

/// The tail of `text` (a ring snapshot) holding its last `max` lines, with
/// no allocation (the panic path uses it). A trailing newline does not count
/// as an empty last line.
pub fn tail_lines(text: &[u8], max: usize) -> &[u8] {
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    if max == 0 {
        return &body[body.len()..];
    }
    let mut seen = 0;
    for (index, &byte) in body.iter().enumerate().rev() {
        if byte == b'\n' {
            seen += 1;
            if seen == max {
                return &body[index + 1..];
            }
        }
    }
    body
}
