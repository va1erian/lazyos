//! Minimal COM1 serial output for logging.
//!
//! Every line also goes to the boot-log ring (`klog`), which is what a PC
//! with no serial port has instead. COM1 is probed once at boot (H1 of
//! `docs/real-pc-boot-plan.md`): an absent port is never initialised or
//! written, and the verdict is logged as `HW:COM1:PRESENT|ABSENT`.
//!
//! Output is queued in a ring and drained a transmit FIFO at a time
//! ([`tx`]). No writer waits for room or for the ring to empty (issue #857):
//! a full ring drops what does not fit and counts it, a program's terminal
//! output ([`mirror`]) waits at most for its own last byte to go out, in
//! chunks with interrupts let in between and for a bounded time, and the
//! kernel task's loop ([`service`]) drains the rest and reports the drops.

mod tx;
#[cfg(lazyos_tests)]
mod testhooks;
#[cfg(lazyos_tests)]
pub use testhooks::*;

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;
use uart_16550::SerialPort;

/// COM1, initialised, with its transmit ring.
struct Port {
    /// Owns the UART's configuration (set up by `init`).
    _uart: SerialPort,
    tx: tx::Tx,
}

static SERIAL1: Mutex<Option<Port>> = Mutex::new(None);
/// Whether the COM1 probe found a UART (read lock-free by the NMI writer).
static PRESENT: AtomicBool = AtomicBool::new(false);

/// COM1's base port.
const COM1: u16 = 0x3F8;
/// Register offsets the probe uses.
const LINE_STATUS: u16 = 5;
const SCRATCH: u16 = 7;

/// A UART's registers by offset, so the probe can run against fakes.
pub trait Uart {
    fn read(&mut self, register: u16) -> u8;
    fn write(&mut self, register: u16, value: u8);
}

/// The real COM1.
pub struct Com1;

impl Uart for Com1 {
    fn read(&mut self, register: u16) -> u8 {
        // SAFETY: reading a COM1 register (line status, scratch) has no side
        // effect; nothing is read from the receive buffer here.
        unsafe { crate::arch::io::inb(COM1 + register) }
    }

    fn write(&mut self, register: u16, value: u8) {
        // SAFETY: the probe only writes the scratch register, which exists
        // for software and changes nothing about the line.
        unsafe { crate::arch::io::outb(COM1 + register, value) };
    }
}

/// Whether a 16550-compatible UART answers: the line status register is not
/// a floating `0xFF`, and the scratch register holds two complementary
/// patterns (an undecoded port reads `0xFF` whatever was written). The
/// scratch register's old value is put back.
pub fn probe_on(uart: &mut impl Uart) -> bool {
    if uart.read(LINE_STATUS) == 0xFF {
        return false;
    }
    let saved = uart.read(SCRATCH);
    let answers = [0x5Au8, 0xA5].into_iter().all(|pattern| {
        uart.write(SCRATCH, pattern);
        uart.read(SCRATCH) == pattern
    });
    uart.write(SCRATCH, saved);
    answers
}

/// Whether COM1 exists (decided once by [`init`]).
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// Whether lines get an uptime prefix. Only optimized boots stamp: the debug
/// and `LAZYOS_TESTS` boots feed line-anchored evidence parsers in `tools/`.
const TIMESTAMPS: bool = cfg!(all(not(debug_assertions), not(lazyos_tests)));

/// Whether the next byte written starts a new line (guarded by `SERIAL1`).
static AT_LINE_START: AtomicBool = AtomicBool::new(true);

/// Ring bytes a program's output leaves to the kernel's own lines: terminal
/// output fills the ring only up to `CAPACITY - PROGRAM_RESERVE`, so a
/// program printing in a loop cannot make the kernel's diagnostics (a panic
/// report, a trace) the lines that are dropped.
const PROGRAM_RESERVE: usize = 2048;
/// Most bytes a writer drains for its own line, however much it queued: the
/// rest goes out from the kernel task, with interrupts on.
const DRAIN_OWN_MAX: usize = 1024;
/// Most rounds a program write drains for its own bytes: a ring's worth of
/// chunks, so even a caller that cannot let a tick in ends.
const MIRROR_MAX_ROUNDS: usize = tx::Tx::CAPACITY / tx::CHUNK + 8;
/// Ticks (10 ms) a program write waits for its own bytes to go out.
const MIRROR_WAIT_TICKS: u64 = 2;
/// Ticks one pass of the kernel task's drain may take.
const SERVICE_TICKS: u64 = 2;
/// Bytes of backlog above which the kernel task asks for another pass soon
/// ([`backlog`]).
const BACKLOG_SOON: usize = 1024;

/// Whether a byte was dropped since the last byte queued (guarded by `SERIAL1`).
static CUT: AtomicBool = AtomicBool::new(false);

/// Queues for the port, prefixing each line with `[secs.millis]` uptime
/// from the 100 Hz PIT tick counter (0 until the timer starts). Bytes past
/// `limit` (the ring's fill) are dropped and counted, except a line's
/// newline, so a cut line still ends.
struct Stamped<'a> {
    tx: &'a mut tx::Tx,
    limit: usize,
}

impl<'a> Stamped<'a> {
    /// A kernel line: the whole ring is open to it.
    fn kernel(tx: &'a mut tx::Tx) -> Self {
        Stamped {
            tx,
            limit: tx::Tx::CAPACITY,
        }
    }

    /// A program's output: it stops short of the kernel's reserve.
    fn program(tx: &'a mut tx::Tx) -> Self {
        Stamped {
            tx,
            limit: tx::Tx::CAPACITY - PROGRAM_RESERVE,
        }
    }

    /// One byte to the ring, a poll point first (`arch::irq_window`): at the
    /// port's 38400 baud a byte takes about 260 µs on hardware (and a VM exit
    /// per port access under a hypervisor), all with interrupts off, so a
    /// line or even a timestamp prefix must not go out in one stretch.
    fn send(&mut self, byte: u8) {
        let limit = if byte == b'\n' {
            tx::Tx::CAPACITY
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

    fn put(&mut self, byte: u8) {
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

/// Whether the port lock is held right now (the NMI hang report, issue #382).
pub fn locked() -> bool {
    SERIAL1.is_locked()
}

/// Test hook: run `f` with the port locked, as code in the middle of a log
/// line would be (the interrupt-window suite). `f` must not print.
#[cfg(lazyos_tests)]
pub fn with_port_locked<R>(f: impl FnOnce() -> R) -> R {
    let _port = SERIAL1.lock();
    f()
}

/// Probe and initialise COM1, and log the verdict.
pub fn init() {
    let present = probe_on(&mut Com1);
    PRESENT.store(present, Ordering::Relaxed);
    if present {
        // Safety: 0x3F8 is the standard COM1 base port, and a UART answers there.
        let mut port = unsafe { SerialPort::new(COM1) };
        port.init();
        *SERIAL1.lock() = Some(Port {
            _uart: port,
            tx: tx::Tx::new(COM1),
        });
    }
    crate::serial_println!("HW:COM1:{}", if present { "PRESENT" } else { "ABSENT" });
}

/// Write a formatted message to the boot log and the serial port (used by
/// the `serial_print!` macros).
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    crate::klog::write_fmt(args);
    if let Some(port) = SERIAL1.lock().as_mut() {
        let before = port.tx.queued();
        let _ = Stamped::kernel(&mut port.tx).write_fmt(args);
        port.drain_own(before);
    }
}

/// Print unless the port is busy; returns whether it printed. For interrupt
/// context, where the code just interrupted may hold the port's lock and
/// waiting for it would deadlock.
pub fn try_print(args: fmt::Arguments) -> bool {
    use core::fmt::Write;
    if let Some(mut guard) = SERIAL1.try_lock() {
        if let Some(port) = guard.as_mut() {
            let before = port.tx.queued();
            let _ = Stamped::kernel(&mut port.tx).write_fmt(args);
            port.drain_own(before);
        }
        return true;
    }
    false
}

/// Write a string to the boot log and the serial port (used by the unified
/// logging sink).
pub fn _write_str(s: &str) {
    use core::fmt::Write;
    crate::klog::write_fmt(format_args!("{s}"));
    if let Some(port) = SERIAL1.lock().as_mut() {
        let before = port.tx.queued();
        let _ = Stamped::kernel(&mut port.tx).write_str(s);
        port.drain_own(before);
    }
}

/// Write raw bytes to the serial port at once (terminal echo). Not copied
/// into the boot log: program output would push the boot out of it.
pub fn write_bytes(bytes: &[u8]) {
    if let Some(port) = SERIAL1.lock().as_mut() {
        let before = port.tx.queued();
        queue_program(port, bytes);
        port.drain_own(before);
    }
}

impl Port {
    /// Drain as many bytes as the caller queued since `before` (plus a FIFO's
    /// worth, at most [`DRAIN_OWN_MAX`]): all of them when nothing else
    /// waits, never someone else's whole backlog with interrupts off.
    fn drain_own(&mut self, before: usize) {
        let own = self.tx.queued().wrapping_sub(before);
        self.tx.drain((own + 16).min(DRAIN_OWN_MAX));
    }
}

/// The kernel task's drain: what writers left queued, a chunk at a time with
/// interrupts let in between, for at most [`SERVICE_TICKS`]; then the bytes
/// dropped while the ring was full are reported (`SERIAL:DROPPED`). Call with
/// interrupts on.
pub fn service() {
    let start = crate::task::ticks();
    loop {
        let empty = x86_64::instructions::interrupts::without_interrupts(|| {
            match SERIAL1.lock().as_mut() {
                Some(port) => {
                    port.tx.drain(tx::CHUNK);
                    port.tx.is_empty()
                }
                None => true,
            }
        });
        if empty || crate::task::ticks().wrapping_sub(start) >= SERVICE_TICKS {
            break;
        }
    }
    report_dropped();
    report_backlog();
}

/// Ring fill above which `SERIAL:BACKLOG` speaks.
const BACKLOG_REPORT_BYTES: usize = 4096;
/// Ticks between two `SERIAL:BACKLOG` lines.
const BACKLOG_REPORT_TICKS: u64 = 500;

/// While the ring holds more than 4 KiB, every 5 s:
/// `SERIAL:BACKLOG bytes=<n> dropped=<total> max_io_us=<n>`: how far behind
/// the port is, what it has refused, and the longest single access to it
/// (a host that is slow to take bytes shows here before anything else does).
fn report_backlog() {
    report_backlog_at(crate::task::ticks());
}

/// Tick of the last `SERIAL:BACKLOG` line.
static BACKLOG_LAST: AtomicU64 = AtomicU64::new(0);

fn report_backlog_at(now: u64) {
    if now.wrapping_sub(BACKLOG_LAST.load(Ordering::Relaxed)) < BACKLOG_REPORT_TICKS {
        return;
    }
    let (bytes, cycles) = x86_64::instructions::interrupts::without_interrupts(|| {
        match SERIAL1.lock().as_mut() {
            Some(port) if port.tx.len() > BACKLOG_REPORT_BYTES => {
                (port.tx.len(), port.tx.take_max_io())
            }
            _ => (0, 0),
        }
    });
    if bytes == 0 {
        return;
    }
    BACKLOG_LAST.store(now, Ordering::Relaxed);
    crate::trace::event!(
        tracefmt::Channel::Serial,
        "BACKLOG",
        bytes = bytes,
        dropped = dropped_total(),
        max_io_us = crate::arch::irqoff::to_us(cycles)
    );
}

/// Whether bytes wait in the ring beyond what the next writer will take
/// (the kernel task's loop sleeps one tick, not many, while this holds).
pub fn backlog() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        SERIAL1
            .lock()
            .as_ref()
            .is_some_and(|port| port.tx.len() > BACKLOG_SOON)
    })
}

/// Bytes waiting in the ring (`SERIAL:BACKLOG`, tests).
pub fn queued_len() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| {
        SERIAL1.lock().as_ref().map_or(0, |port| port.tx.len())
    })
}

/// Bytes ever dropped for lack of room (counted when reported).
static DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Bytes dropped for lack of room since boot, reported or not.
pub fn dropped_total() -> u64 {
    let pending = x86_64::instructions::interrupts::without_interrupts(|| {
        SERIAL1.lock().as_ref().map_or(0, |port| port.tx.pending_drops())
    });
    DROPPED_TOTAL.load(Ordering::Relaxed) + pending as u64
}

/// Report the bytes dropped while the ring was full, once it has room again:
/// `SERIAL:DROPPED bytes=<n> total=<n>`. Task context, port lock not held
/// (the line is a trace, and a trace takes the port's lock).
pub fn report_dropped() {
    let taken = x86_64::instructions::interrupts::without_interrupts(|| {
        match SERIAL1.lock().as_mut() {
            Some(port) if port.tx.len() < tx::Tx::CAPACITY / 2 => port.tx.take_dropped(),
            _ => 0,
        }
    });
    if taken == 0 {
        return;
    }
    let total = DROPPED_TOTAL.fetch_add(taken as u64, Ordering::Relaxed) + taken as u64;
    crate::trace::event!(
        tracefmt::Channel::Serial,
        "DROPPED",
        bytes = taken,
        total = total
    );
}

/// The transmit ring's own checks (kernel test suite).
#[cfg(lazyos_tests)]
pub fn ring_selftest() -> Result<(), &'static str> {
    tx::selftest()
}

/// Drain everything queued, whatever it costs: before the machine stops
/// (power off, reboot, a panic), so the last lines are never lost. The only
/// caller that waits for the UART without a bound.
pub fn flush() {
    if let Some(port) = SERIAL1.lock().as_mut() {
        port.tx.drain_all();
    }
}

/// Write one finished trace line (`trace.rs`): the serial port and, in a
/// `LAZYOS_DBGD=1` kernel, the program-output ring. Not the boot log: a chatty
/// trace must not push the boot diagnostics out of it. The line is queued
/// whole under the port lock, so no other task's output lands inside it.
pub fn write_trace(line: &[u8]) {
    #[cfg(lazyos_dbgd)]
    crate::klog::push_program(line);
    if let Some(port) = SERIAL1.lock().as_mut() {
        let before = port.tx.queued();
        let mut out = Stamped::kernel(&mut port.tx);
        line.iter().for_each(|&byte| out.put(byte));
        port.drain_own(before);
    }
}

/// Mirror a program's terminal output (`write` to the terminal, `sendfile`).
/// The bytes are queued whole first, so `bytes` may be user memory: it is
/// read only here, before any window opens. What does not fit below the
/// kernel's reserve is dropped and counted (`SERIAL:DROPPED`).
///
/// Then the write helps drain the ring in [`tx::CHUNK`]s, letting interrupts
/// in between when the caller may sleep (a syscall that holds no lock;
/// `block::iowait::breathe`), until its *own last byte* has gone out, so
/// output keeps its order with the kernel's own lines and a program that
/// writes and then exits has been heard. It never waits for the ring to
/// empty (issue #857: while anything else kept logging it never did, and the
/// writer spun in the kernel, immune to `SIGKILL`). It gives up after
/// [`MIRROR_WAIT_TICKS`], after [`MIRROR_MAX_ROUNDS`] chunks, or at once when
/// the caller has a fatal signal pending; its bytes stay queued and the
/// kernel task drains them.
pub fn mirror(bytes: &[u8]) {
    #[cfg(lazyos_dbgd)]
    crate::klog::push_program(bytes);
    let mark = match SERIAL1.lock().as_mut() {
        Some(port) => {
            queue_program(port, bytes);
            port.tx.queued()
        }
        None => return,
    };
    let deadline = crate::task::ticks() + MIRROR_WAIT_TICKS;
    for _ in 0..MIRROR_MAX_ROUNDS {
        if crate::task::signal::must_unwind(crate::task::current()) {
            return;
        }
        #[cfg(lazyos_tests)]
        MIRROR_ROUNDS.fetch_add(1, Ordering::Relaxed);
        let sent = match SERIAL1.lock().as_mut() {
            Some(port) => {
                port.tx.drain(tx::CHUNK);
                port.tx.passed(mark)
            }
            None => true,
        };
        if sent || crate::task::ticks() >= deadline {
            return;
        }
        crate::block::iowait::breathe(crate::block::Wait::MaySleep);
    }
}

/// Queue a program's `bytes` with their line stamps; past the reserve they
/// are dropped and counted.
fn queue_program(port: &mut Port, bytes: &[u8]) {
    let mut out = Stamped::program(&mut port.tx);
    bytes.iter().for_each(|&byte| out.put(byte));
}
