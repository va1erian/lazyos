//! Queuing text into the transmit ring: uptime stamps, the program/kernel
//! split of the ring, and what happens when it is full (issue #857).
//!
//! Evidence lines (`UP:PASS`, `TEST:...`, a trace) are parsed by whole line, so
//! a full ring drops **whole lines**, never the tail of one: a marker cut at
//! `...:1234` would still parse as the wrong value. [`Stamped::lines`] queues
//! the segments of a buffer that fit and stops at the first that does not.
//! Only formatted kernel text (`serial_println!`), whose length is not known
//! before it is written, is dropped byte by byte, and a line it cut short is
//! ended before the next one starts (`CUT`).

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use super::tx::Tx;

/// Whether lines get an uptime prefix. Only optimized boots stamp: the debug
/// and `LAZYOS_TESTS` boots feed line-anchored evidence parsers in `tools/`.
pub(super) const TIMESTAMPS: bool = cfg!(all(not(debug_assertions), not(lazyos_tests)));

/// Bytes a stamp (`[secs.millis] `) takes at most.
pub(super) const STAMP_MAX: usize = 32;

/// Ring bytes a program's output leaves to the kernel's own lines: terminal
/// output fills the ring only up to `CAPACITY - PROGRAM_RESERVE`, so a
/// program printing in a loop cannot make the kernel's diagnostics (a panic
/// report, a trace) the lines that are dropped.
pub(super) const PROGRAM_RESERVE: usize = 2048;

/// Whether the next byte written starts a new line (guarded by `SERIAL1`).
static AT_LINE_START: AtomicBool = AtomicBool::new(true);
/// Whether a byte was dropped since the last byte queued (guarded by `SERIAL1`).
static CUT: AtomicBool = AtomicBool::new(false);

/// Queues for the port, prefixing each line with `[secs.millis]` uptime
/// from the 100 Hz PIT tick counter (0 until the timer starts). Bytes past
/// `limit` (the ring's fill) are dropped and counted, except a line's
/// newline, so a cut line still ends.
pub(super) struct Stamped<'a> {
    tx: &'a mut Tx,
    limit: usize,
}

impl<'a> Stamped<'a> {
    /// A kernel line: the whole ring is open to it.
    pub(super) fn kernel(tx: &'a mut Tx) -> Self {
        Stamped {
            tx,
            limit: Tx::CAPACITY,
        }
    }

    /// A program's output: it stops short of the kernel's reserve.
    pub(super) fn program(tx: &'a mut Tx) -> Self {
        Stamped {
            tx,
            limit: Tx::CAPACITY - PROGRAM_RESERVE,
        }
    }

    /// Queue the whole lines of `bytes` that fit, in order, and stop at the
    /// first that does not. Returns how many bytes it took; a segment that
    /// can never fit (longer than the ring may hold) is counted as dropped
    /// and skipped, so the caller does not wait for it.
    pub(super) fn lines(&mut self, bytes: &[u8]) -> usize {
        let mut taken = 0;
        for segment in bytes.split_inclusive(|&b| b == b'\n') {
            // A long buffer is a long scan with interrupts off: let them in.
            crate::arch::irq_window::poll_point();
            let need = segment.len() + STAMP_MAX;
            if need > self.limit {
                self.tx.note_dropped(segment.len());
            } else if self.tx.len() + need > self.limit {
                break;
            } else {
                segment.iter().for_each(|&byte| self.put(byte));
            }
            taken += segment.len();
        }
        taken
    }

    /// Count `bytes` as dropped: the caller gave up on them.
    pub(super) fn drop_lines(&mut self, bytes: &[u8]) {
        self.tx.note_dropped(bytes.len());
    }

    /// One byte to the ring, a poll point first (`arch::irq_window`): at the
    /// port's 38400 baud a byte takes about 260 µs on hardware (and a VM exit
    /// per port access under a hypervisor), all with interrupts off, so a
    /// line or even a timestamp prefix must not go out in one stretch.
    fn send(&mut self, byte: u8) {
        let limit = if byte == b'\n' {
            Tx::CAPACITY
        } else {
            self.limit
        };
        if self.tx.len() >= limit {
            self.tx.note_dropped(1);
            CUT.store(true, Ordering::Relaxed);
            return;
        }
        crate::arch::irq_window::poll_point();
        self.tx.push(byte);
    }

    pub(super) fn put(&mut self, byte: u8) {
        // A line was cut short by a full ring (its newline too, perhaps):
        // end it before this one starts, so two lines never run together.
        if CUT.load(Ordering::Relaxed) && self.tx.len() < self.limit {
            CUT.store(false, Ordering::Relaxed);
            AT_LINE_START.store(true, Ordering::Relaxed);
            self.send(b'\n');
        }
        if TIMESTAMPS && AT_LINE_START.swap(false, Ordering::Relaxed) {
            let ms = crate::task::ticks() * 10;
            let mut digits = [0u8; 20];
            let mut n = ms / 1000;
            let mut len = 0;
            loop {
                digits[len] = b'0' + (n % 10) as u8;
                len += 1;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            self.send(b'[');
            for i in (0..len).rev() {
                self.send(digits[i]);
            }
            let frac = ms % 1000;
            for b in [
                b'.',
                b'0' + (frac / 100) as u8,
                b'0' + (frac / 10 % 10) as u8,
                b'0' + (frac % 10) as u8,
                b']',
                b' ',
            ] {
                self.send(b);
            }
        }
        if byte == b'\n' {
            AT_LINE_START.store(true, Ordering::Relaxed);
        }
        self.send(byte);
    }
}

impl fmt::Write for Stamped<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.bytes().for_each(|byte| self.put(byte));
        Ok(())
    }
}
