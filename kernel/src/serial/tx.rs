//! COM1's transmit side: a FIFO ring that writers fill and drain
//! (docs/performance-plan.md P5).
//!
//! Under a hypervisor every UART register access is a VM exit. The port used
//! to be written a byte at a time, each byte after a poll of the line status:
//! two exits per byte, all with the port lock held and interrupts off for the
//! whole write. Now a write is copied into the ring whole (so it can never be
//! interleaved with another writer's bytes, and the order of everything the
//! kernel and programs print is the order it was queued in), and the ring is
//! drained 16 bytes per line-status poll: the 16550's transmit FIFO is empty
//! whenever its holding-register-empty bit is set. A program's output is
//! drained in [`CHUNK`]s with interrupts let in between (`serial::mirror`).
//! A kernel line drains as many bytes as it queued: at once when nothing else
//! is waiting (as before), and without paying for a program's backlog with
//! interrupts off when something is (the program's own write drains that,
//! and the kernel task's loop is the backstop, `serial::service`). Nothing is
//! dropped: a writer that finds the ring full drains it first. Only a UART
//! that never empties (a million status polls) loses bytes.

/// Bytes the kernel task's loop drains per pass, in chunks, when a backlog
/// was left (`serial::service`).
pub(super) const SERVICE_BUDGET: usize = 1024;

use crate::arch::io::{inb, outb};

/// Bytes the ring holds.
const RING: usize = 16 * 1024;
/// Bytes the transmit FIFO takes after one "empty" status.
const FIFO: usize = 16;
/// Bytes a program's output drains between two breaths.
pub(super) const CHUNK: usize = 32;
/// Status polls before a drain gives up on a UART that never empties (the
/// bytes stay queued for the next drain).
const MAX_POLLS: u32 = 1_000_000;

const DATA: u16 = 0;
const LINE_STATUS: u16 = 5;
/// Line status: the transmit holding register (and FIFO) is empty.
const THR_EMPTY: u8 = 1 << 5;

/// The queued bytes and the port they go to.
pub(super) struct Tx {
    base: u16,
    ring: [u8; RING],
    head: usize,
    len: usize,
    /// Bytes ever queued: a writer's own count is the difference.
    queued: usize,
}

impl Tx {
    pub(super) const fn new(base: u16) -> Tx {
        Tx {
            base,
            ring: [0; RING],
            head: 0,
            len: 0,
            queued: 0,
        }
    }

    /// Bytes ever queued (wrapping).
    pub(super) fn queued(&self) -> usize {
        self.queued
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Queue `byte`, draining first when the ring is full.
    pub(super) fn push(&mut self, byte: u8) {
        if self.len == RING {
            self.drain(FIFO);
            if self.len == RING {
                return; // the UART is wedged: nothing can be queued or sent
            }
        }
        self.ring[(self.head + self.len) % RING] = byte;
        self.len += 1;
        self.queued = self.queued.wrapping_add(1);
    }

    /// Send up to `budget` queued bytes, a FIFO's worth per status poll.
    pub(super) fn drain(&mut self, budget: usize) {
        let mut sent = 0;
        while sent < budget && self.len > 0 {
            if !self.wait_empty() {
                return;
            }
            let burst = FIFO.min(budget - sent).min(self.len);
            for _ in 0..burst {
                let byte = self.ring[self.head];
                self.head = (self.head + 1) % RING;
                self.len -= 1;
                // SAFETY: writing COM1's data register sends one byte; the
                // port was probed and initialised, and the FIFO has room.
                unsafe { outb(self.base + DATA, byte) };
            }
            sent += burst;
        }
    }

    /// Wait until the transmitter is empty; false if it never gets there.
    fn wait_empty(&self) -> bool {
        for _ in 0..MAX_POLLS {
            // SAFETY: reading the line status register has no side effect.
            if unsafe { inb(self.base + LINE_STATUS) } & THR_EMPTY != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}

/// The ring on a port that is not there (COM4: its status reads `0xFF`, so
/// it always looks empty, and writes go nowhere): more than a ring's worth
/// queued drains on the way, nothing is counted twice or lost, and a drain
/// budget is honoured.
#[cfg(lazyos_tests)]
pub(crate) fn selftest() -> Result<(), &'static str> {
    static TX: spin::Mutex<Tx> = spin::Mutex::new(Tx::new(0x2E8));
    let mut tx = TX.lock();
    tx.drain(usize::MAX);
    let before = tx.queued();
    for index in 0..(RING + RING / 2) {
        tx.push(index as u8);
    }
    if tx.queued().wrapping_sub(before) != RING + RING / 2 {
        return Err("queued count");
    }
    if tx.len != RING {
        return Err("a full ring did not stay full after draining for room");
    }
    // The oldest byte left is the first one not drained for room.
    if tx.ring[tx.head] != (RING / 2) as u8 {
        return Err("the ring lost its order");
    }
    tx.drain(CHUNK);
    if tx.len != RING - CHUNK {
        return Err("a drain ignored its budget");
    }
    tx.drain(usize::MAX);
    if !tx.is_empty() {
        return Err("a full drain left bytes");
    }
    Ok(())
}
