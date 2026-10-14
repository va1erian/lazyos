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
//! interrupts off when something is (the kernel task's loop drains that,
//! `serial::service`).
//!
//! A writer never waits for room. A full ring drops the byte and counts it
//! (issue #857): the port can be slower than the producers for as long as the
//! host's consumer stalls, and a writer that drained synchronously, or a
//! program write that waited for the ring to empty, turned that into a frozen
//! machine (writers livelocked in the kernel, each demotion printing more).
//! The count is reported as `SERIAL:DROPPED` once the ring has room again
//! (`serial::service`); only a panic or a power-off drains without limit
//! (`serial::flush`). `drained` counts the bytes sent, so a writer can tell
//! when its own last byte has gone without asking for an empty ring.

use crate::arch::io::{inb, outb};

/// Bytes the ring holds.
const RING: usize = 16 * 1024;
/// Bytes the transmit FIFO takes after one "empty" status.
const FIFO: usize = 16;
/// Bytes a program's output drains between two breaths.
pub(super) const CHUNK: usize = 32;
/// Milliseconds a drain waits for the transmitter to empty before it gives up
/// on a UART that is not emptying (the bytes stay queued for the next drain).
/// Time, not polls: a port read is a VM exit of a microsecond or a hundred,
/// and a host that stopped reading the port (a paused console) must cost a
/// writer about a millisecond, not tens. A 16-byte FIFO empties within ~17 ms
/// at 9600 baud, so a slow line costs a few gave-up drains, not data.
const WAIT_MS: u64 = 1;
/// The same for [`Tx::drain_all`], used when the machine is about to stop.
const WAIT_FLUSH_MS: u64 = 100;
/// TSC cycles in a millisecond when the timer is not calibrated yet.
const FALLBACK_CYCLES_PER_MS: u64 = 3_000_000;
/// Timer periods a drain that gave up leaves the port alone the first time;
/// each further give-up in a row doubles it, up to [`BACKOFF_MAX_TICKS`].
const BACKOFF_TICKS: u64 = 2;
const BACKOFF_MAX_TICKS: u64 = 100;

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
    /// Bytes ever sent to the UART (wrapping), in queue order.
    drained: usize,
    /// Bytes refused for lack of room since the last report.
    dropped: usize,
    /// TSC value before which an ordinary drain does not touch the UART: the
    /// last one found it not emptying ([`Tx::drain`]).
    retry_at: u64,
    /// Longest single port access seen since the last report (TSC cycles):
    /// what a host that is slow to take serial bytes looks like from here.
    max_io: u64,
    /// Drains in a row that gave up (the back-off doubles with it).
    stalls: u32,
    /// Test builds: the most one drain call may send (a slow consumer).
    #[cfg(lazyos_tests)]
    cap: usize,
    /// Test builds: the transmitter never reports empty.
    #[cfg(lazyos_tests)]
    never_empty: bool,
}

impl Tx {
    pub(super) const fn new(base: u16) -> Tx {
        Tx {
            base,
            ring: [0; RING],
            head: 0,
            len: 0,
            queued: 0,
            drained: 0,
            dropped: 0,
            retry_at: 0,
            max_io: 0,
            stalls: 0,
            #[cfg(lazyos_tests)]
            cap: usize::MAX,
            #[cfg(lazyos_tests)]
            never_empty: false,
        }
    }

    /// Ring capacity in bytes.
    pub(super) const CAPACITY: usize = RING;

    /// Bytes waiting to be sent.
    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// Bytes ever sent (wrapping); see [`Tx::passed`].
    pub(super) fn drained(&self) -> usize {
        self.drained
    }

    /// Whether everything queued up to the sequence number `mark` (a value
    /// of [`Tx::queued`]) has been sent.
    pub(super) fn passed(&self, mark: usize) -> bool {
        self.drained.wrapping_sub(mark) as isize >= 0
    }

    /// Count `bytes` refused for lack of room.
    pub(super) fn note_dropped(&mut self, bytes: usize) {
        self.dropped = self.dropped.saturating_add(bytes);
    }

    /// Bytes dropped since the last call (resets the count).
    pub(super) fn take_dropped(&mut self) -> usize {
        core::mem::take(&mut self.dropped)
    }

    /// Bytes dropped and not yet reported.
    pub(super) fn pending_drops(&self) -> usize {
        self.dropped
    }

    /// Bytes ever queued (wrapping).
    pub(super) fn queued(&self) -> usize {
        self.queued
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Queue `byte`; a full ring drops it (and counts it).
    pub(super) fn push(&mut self, byte: u8) {
        if self.len == RING {
            self.note_dropped(1);
            return;
        }
        self.ring[(self.head + self.len) % RING] = byte;
        self.len += 1;
        self.queued = self.queued.wrapping_add(1);
    }

    /// Send up to `budget` queued bytes, a FIFO's worth per status poll.
    ///
    /// The UART sends at its baud rate (QEMU paces the "empty" bit too), so a
    /// long drain takes milliseconds with interrupts off; it takes pending
    /// interrupts at poll points (`arch::irq_window`) while it waits. Measured
    /// in issue #400: a 128-byte program write held them off for up to 49 ms,
    /// long enough for QEMU's 16-byte PS/2 queue to overflow and lose keys.
    ///
    /// A UART that stops emptying (the host's consumer is paused or slow,
    /// issue #857) makes the drain give up after [`WAIT_MS`], and the next
    /// ones return at once for a while that doubles with every give-up in a
    /// row, instead of each paying the wait again.
    pub(super) fn drain(&mut self, budget: usize) {
        self.drain_with(budget, WAIT_MS, true);
    }

    /// Send everything, however long the UART takes: a panic or a power-off
    /// is about to stop the machine and the last lines must not be lost.
    pub(super) fn drain_all(&mut self) {
        self.drain_with(usize::MAX, WAIT_FLUSH_MS, false);
    }

    fn drain_with(&mut self, budget: usize, wait_ms: u64, back_off: bool) {
        #[cfg(lazyos_tests)]
        let budget = budget.min(self.cap);
        if back_off && crate::perf::rdtsc() < self.retry_at {
            return;
        }
        let mut sent = 0;
        while sent < budget && self.len > 0 {
            crate::arch::irq_window::poll_point();
            if !self.wait_empty(wait_ms * cycles_per_ms()) {
                self.stalls = (self.stalls + 1).min(16);
                self.retry_at = crate::perf::rdtsc().saturating_add(back_off_cycles(self.stalls));
                return;
            }
            self.stalls = 0;
            let burst = FIFO.min(budget - sent).min(self.len);
            for _ in 0..burst {
                let byte = self.ring[self.head];
                self.head = (self.head + 1) % RING;
                self.len -= 1;
                self.drained = self.drained.wrapping_add(1);
                let began = crate::perf::rdtsc();
                // SAFETY: writing COM1's data register sends one byte; the
                // port was probed and initialised, and the FIFO has room.
                unsafe { outb(self.base + DATA, byte) };
                self.note_io(began);
            }
            sent += burst;
        }
    }

    /// A port access that began at TSC `began` just finished.
    fn note_io(&mut self, began: u64) {
        self.max_io = self.max_io.max(crate::perf::rdtsc().wrapping_sub(began));
    }

    /// The longest port access since the last call, in TSC cycles.
    pub(super) fn take_max_io(&mut self) -> u64 {
        core::mem::take(&mut self.max_io)
    }

    /// Wait until the transmitter is empty, for at most `cycles` TSC cycles;
    /// false if it never gets there.
    fn wait_empty(&mut self, cycles: u64) -> bool {
        let began = crate::perf::rdtsc();
        loop {
            let access = crate::perf::rdtsc();
            // SAFETY: reading the line status register has no side effect.
            let status = unsafe { inb(self.base + LINE_STATUS) };
            self.note_io(access);
            #[cfg(lazyos_tests)]
            let status = if self.never_empty { 0 } else { status };
            if status & THR_EMPTY != 0 {
                return true;
            }
            if crate::perf::rdtsc().wrapping_sub(began) > cycles {
                return false;
            }
            // Safe under the port lock: a window's handlers take no lock but
            // the i8042 FIFO's, which no serial writer holds.
            crate::arch::irq_window::poll_point();
            core::hint::spin_loop();
        }
    }

    /// Test builds: make the transmitter never report empty, or normal again.
    #[cfg(lazyos_tests)]
    pub(super) fn set_never_empty(&mut self, on: bool) {
        self.never_empty = on;
        self.retry_at = 0;
        self.stalls = 0;
    }

    /// Test builds: the queued bytes, oldest first.
    #[cfg(lazyos_tests)]
    pub(super) fn contents(&self) -> alloc::vec::Vec<u8> {
        (0..self.len)
            .map(|i| self.ring[(self.head + i) % RING])
            .collect()
    }

    /// Test builds: drains in a row that gave up.
    #[cfg(lazyos_tests)]
    pub(super) fn stall_level(&self) -> u32 {
        self.stalls
    }

    /// Test builds: change the per-call cap of a diverted port.
    #[cfg(lazyos_tests)]
    pub(super) fn set_cap(&mut self, cap: usize) {
        self.cap = cap;
        self.retry_at = 0;
        self.stalls = 0;
    }

    /// Test builds: send to `base` (a port nothing answers on swallows the
    /// bytes at full speed), at most `cap` bytes per drain call; returns the
    /// previous settings. The ring is emptied and the drop count cleared.
    #[cfg(lazyos_tests)]
    pub(super) fn divert(&mut self, base: u16, cap: usize) -> (u16, usize) {
        let old = (self.base, self.cap);
        self.base = base;
        self.cap = cap;
        self.head = 0;
        self.len = 0;
        self.drained = self.queued;
        self.dropped = 0;
        self.retry_at = 0;
        self.stalls = 0;
        old
    }
}

/// TSC cycles in a millisecond.
fn cycles_per_ms() -> u64 {
    match crate::arch::clock::cycles_per_tick() {
        0 => FALLBACK_CYCLES_PER_MS,
        per_tick => (per_tick / 10).max(1),
    }
}

/// How long the `stalls`-th give-up in a row leaves the UART alone.
fn back_off_cycles(stalls: u32) -> u64 {
    let ticks = (BACKOFF_TICKS << stalls.saturating_sub(1).min(8)).min(BACKOFF_MAX_TICKS);
    ticks * 10 * cycles_per_ms()
}

/// The ring on a port that is not there (COM4: its status reads `0xFF`, so
/// it always looks empty, and writes go nowhere): bytes past a ring's worth
/// are dropped and counted, never drained for room; the order of what was
/// kept, the sent-byte sequence and a drain budget are honoured.
#[cfg(lazyos_tests)]
pub(crate) fn selftest() -> Result<(), &'static str> {
    static TX: spin::Mutex<Tx> = spin::Mutex::new(Tx::new(0x2E8));
    let mut tx = TX.lock();
    tx.drain(usize::MAX);
    tx.take_dropped();
    let before = tx.queued();
    for index in 0..(RING + RING / 2) {
        tx.push(index as u8);
    }
    if tx.queued().wrapping_sub(before) != RING {
        return Err("queued count");
    }
    if tx.len != RING {
        return Err("a full ring did not stay full");
    }
    if tx.pending_drops() != RING / 2 {
        return Err("the bytes refused were not counted");
    }
    // The oldest byte is the first one queued: nothing was drained for room.
    if tx.ring[tx.head] != 0 {
        return Err("the ring lost its order");
    }
    if tx.passed(tx.queued()) {
        return Err("queued bytes counted as sent");
    }
    tx.drain(CHUNK);
    if tx.len != RING - CHUNK {
        return Err("a drain ignored its budget");
    }
    if !tx.passed(before.wrapping_add(CHUNK)) || tx.passed(before.wrapping_add(CHUNK + 1)) {
        return Err("the sent sequence is off");
    }
    tx.drain(usize::MAX);
    if !tx.is_empty() || !tx.passed(tx.queued()) {
        return Err("a full drain left bytes");
    }
    if tx.take_dropped() != RING / 2 || tx.take_dropped() != 0 {
        return Err("the drop count was not taken once");
    }
    Ok(())
}
