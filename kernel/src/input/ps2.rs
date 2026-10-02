//! i8042 intake: every byte the controller holds reaches a bounded software
//! FIFO before the controller's own tiny queue can overflow.
//!
//! The PS/2 keyboard behind the i8042 buffers only a handful of bytes (QEMU's
//! queue is 16: four shifted characters) and silently discards what arrives
//! while it is full. Syscalls run with interrupts off (`arch::clock`, issue
//! #344), and on a fresh image's first boot `pkgd` installs the core packages
//! with `write_file`/`append_file`/`fsync` calls that keep them off for 50 to
//! 120 ms, so IRQ1 came too late and keys typed meanwhile were lost before the
//! kernel ever saw them, with no trace anywhere.
//!
//! So the bytes are *collected* wherever the kernel can be busy for long —
//! [`service`] is called from the block drivers' completion waits and the
//! ext2 block I/O path as well as from IRQ1/IRQ12 and the timer tick — and
//! *decoded* only in interrupt context ([`dispatch`]), exactly where the
//! keyboard and mouse drivers always ran, so no lock is ever taken from a new
//! context. Collection preserves the controller's order and tags each byte
//! with the port it came from.
//!
//! Loss is never silent: the FIFO holds [`FIFO_CAP`] bytes (a minute of fast
//! typing with nothing decoding it); past that new bytes are counted, the next
//! dispatch logs `PS2:DROP bytes=<n> total=<n> capacity=<n>` and releases every key the
//! keyboard tap thinks is held, so a lost release cannot leave a key stuck.
//! `PS2:GAP` reports each new longest stretch (over [`GAP_REPORT_MS`]) between
//! two services, which is the worst input latency the controller saw.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use crate::arch::io::inb;

/// Bytes the software FIFO retains. A power of two so the wrap is a mask.
pub const FIFO_CAP: usize = 1024;

/// Most bytes one [`service`] call reads: the controller cannot hold more, so
/// a status register stuck "full" cannot spin the caller forever.
const MAX_READS: usize = 64;

/// Service gaps shorter than this are never reported.
const GAP_REPORT_MS: u64 = 40;

/// i8042 status bits.
const STATUS_OUTPUT_FULL: u8 = 0x01;
const STATUS_AUX: u8 = 0x20;

/// Which i8042 port a byte came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    Keyboard,
    Mouse,
}

/// One collected byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Byte {
    pub port: Port,
    pub value: u8,
}

/// The bounded byte FIFO and its loss accounting.
pub struct Fifo {
    buf: [Byte; FIFO_CAP],
    head: usize,
    len: usize,
    /// Bytes refused since the last [`Fifo::take_dropped`].
    dropped: u64,
    /// Most bytes ever waiting at once.
    peak: usize,
}

impl Fifo {
    pub const fn new() -> Fifo {
        Fifo {
            buf: [Byte {
                port: Port::Keyboard,
                value: 0,
            }; FIFO_CAP],
            head: 0,
            len: 0,
            dropped: 0,
            peak: 0,
        }
    }

    /// Append `byte`; a full FIFO refuses it and counts it. The *new* byte is
    /// the one refused: the queued ones are older input and stay in order.
    pub fn push(&mut self, byte: Byte) -> bool {
        if self.len == FIFO_CAP {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        self.buf[(self.head + self.len) & (FIFO_CAP - 1)] = byte;
        self.len += 1;
        self.peak = self.peak.max(self.len);
        true
    }

    pub fn pop(&mut self) -> Option<Byte> {
        if self.len == 0 {
            return None;
        }
        let byte = self.buf[self.head];
        self.head = (self.head + 1) & (FIFO_CAP - 1);
        self.len -= 1;
        Some(byte)
    }

    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.len
    }

    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn peak(&self) -> usize {
        self.peak
    }

    /// Bytes refused since the previous call, resetting the count.
    pub fn take_dropped(&mut self) -> u64 {
        core::mem::take(&mut self.dropped)
    }

    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.dropped = 0;
        self.peak = 0;
    }
}

impl Default for Fifo {
    fn default() -> Self {
        Fifo::new()
    }
}

/// Taken only with interrupts off (every caller below), so the IRQ handlers
/// can never find it held by the code they interrupted.
static FIFO: Mutex<Fifo> = Mutex::new(Fifo::new());
/// Set once the i8042 is initialised: before that, the mouse setup reads the
/// controller's replies itself and nothing else may consume them.
static READY: AtomicBool = AtomicBool::new(false);
/// Whether the FIFO may hold bytes (a cheap check for the timer tick).
static PENDING: AtomicBool = AtomicBool::new(false);
/// Bytes lost to a full FIFO over the whole boot.
static DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Lost bytes not yet reported on serial.
static UNREPORTED: AtomicU64 = AtomicU64::new(0);
/// TSC at the previous [`service`], and the longest gap seen (cycles).
static LAST_SERVICE: AtomicU64 = AtomicU64::new(0);
static MAX_GAP: AtomicU64 = AtomicU64::new(0);
/// Largest gap already reported, in milliseconds.
static REPORTED_GAP_MS: AtomicU64 = AtomicU64::new(0);

/// Start collecting (called once the controller and the mouse are set up).
pub fn enable() {
    LAST_SERVICE.store(rdtsc(), Ordering::Relaxed);
    READY.store(true, Ordering::Release);
}

/// Move every byte the controller holds into the FIFO. Cheap (one status
/// read) when it holds none; safe from any context.
pub fn service() {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    x86_64::instructions::interrupts::without_interrupts(|| {
        note_gap();
        let mut fifo = FIFO.lock();
        let mut queued = false;
        for _ in 0..MAX_READS {
            // SAFETY: reading the i8042 status port has no side effect.
            let status = unsafe { inb(0x64) };
            if status & STATUS_OUTPUT_FULL == 0 {
                break;
            }
            // SAFETY: the status just reported a byte in the output buffer;
            // reading it is how the controller is meant to be drained.
            let value = unsafe { inb(0x60) };
            let port = if status & STATUS_AUX != 0 {
                Port::Mouse
            } else {
                Port::Keyboard
            };
            // A refusal is counted by the FIFO and reported by `dispatch`.
            fifo.push(Byte { port, value });
            queued = true;
        }
        if queued {
            PENDING.store(true, Ordering::Release);
        }
    });
}

/// Decode everything collected. Interrupt context only (IRQ0, IRQ1, IRQ12):
/// the keyboard and mouse drivers take locks that code running with
/// interrupts off may hold.
pub fn dispatch() {
    if !PENDING.swap(false, Ordering::AcqRel) && UNREPORTED.load(Ordering::Relaxed) == 0 {
        return;
    }
    // One byte at a time, the lock dropped before decoding: the drivers never
    // run under the FIFO lock.
    loop {
        let next = FIFO.lock().pop();
        let Some(byte) = next else { break };
        deliver(byte);
    }
    report_loss();
}

/// The IRQ1/IRQ12 handlers: drain the controller, then decode.
pub fn on_irq() {
    service();
    dispatch();
}

fn deliver(byte: Byte) {
    match byte.port {
        Port::Keyboard => super::keyboard::push_scancode(byte.value),
        Port::Mouse => super::mouse::push_byte(byte.value),
    }
}

/// After a loss, release every key the keyboard believes held and say so.
fn report_loss() {
    let lost = FIFO.lock().take_dropped();
    if lost > 0 {
        DROPPED_TOTAL.fetch_add(lost, Ordering::Relaxed);
        UNREPORTED.fetch_add(lost, Ordering::Relaxed);
        super::keyboard::release_all_after_loss();
    }
    let unreported = UNREPORTED.load(Ordering::Relaxed);
    if unreported == 0 {
        return;
    }
    let total = DROPPED_TOTAL.load(Ordering::Relaxed);
    // The serial lock may be held by the code this interrupt stopped; the
    // count then waits for a later dispatch rather than risk a deadlock.
    if crate::serial::try_print(format_args!(
        "PS2:DROP bytes={unreported} total={total} capacity={FIFO_CAP}\n"
    )) {
        UNREPORTED.fetch_sub(unreported, Ordering::Relaxed);
    }
}

/// Track the longest gap between two services; report each new record.
fn note_gap() {
    let now = rdtsc();
    let last = LAST_SERVICE.swap(now, Ordering::Relaxed);
    let gap = now.wrapping_sub(last);
    if gap <= MAX_GAP.load(Ordering::Relaxed) {
        return;
    }
    MAX_GAP.store(gap, Ordering::Relaxed);
    let per_tick = crate::arch::clock::cycles_per_tick();
    if per_tick == 0 {
        return;
    }
    // A PIT tick is 10 ms.
    let ms = gap.saturating_mul(10) / per_tick;
    if ms >= GAP_REPORT_MS && ms > REPORTED_GAP_MS.load(Ordering::Relaxed) {
        REPORTED_GAP_MS.store(ms, Ordering::Relaxed);
        let _ = crate::serial::try_print(format_args!(
            "PS2:GAP max_ms={ms} last_syscall={:#x}\n",
            crate::process::gate::LAST_SYSCALL.load(Ordering::Relaxed)
        ));
    }
}

fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Bytes lost to a full FIFO since boot.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn dropped_total() -> u64 {
    DROPPED_TOTAL.load(Ordering::Relaxed)
}

/// The FIFO itself, for tests that drive it without the controller.
#[cfg(lazyos_tests)]
pub fn with_fifo<R>(body: impl FnOnce(&mut Fifo) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| body(&mut FIFO.lock()))
}

/// Test hook: mark the FIFO as holding bytes (after [`with_fifo`] filled it).
#[cfg(lazyos_tests)]
pub fn mark_pending() {
    PENDING.store(true, Ordering::Release);
}

/// Test hook: forget queued bytes and counters.
#[cfg(lazyos_tests)]
pub fn reset() {
    with_fifo(Fifo::clear);
    PENDING.store(false, Ordering::Release);
    UNREPORTED.store(0, Ordering::Relaxed);
}
