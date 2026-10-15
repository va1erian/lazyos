//! Minimal COM1 serial output for logging.
//!
//! Every line also goes to the boot-log ring (`klog`), which is what a PC
//! with no serial port has instead. COM1 is probed once at boot (H1 of
//! `docs/real-pc-boot-plan.md`): an absent port is never initialised or
//! written, and the verdict is logged as `HW:COM1:PRESENT|ABSENT`.
//!
//! Output is queued in a ring and drained a transmit FIFO at a time
//! ([`tx`]). No writer ever waits for the ring to *empty* (issue #857): a full
//! ring drops whole lines and counts them, a program's terminal output
//! ([`mirror`]) waits for room, and then for its own last byte to go out, in
//! chunks with interrupts let in between and for a bounded *time*, and the
//! kernel task's loop ([`service`]) drains the rest while [`report`] says what
//! was dropped.

mod report;
mod stamp;
#[cfg(lazyos_tests)]
mod testhooks;
mod tx;
#[cfg(lazyos_tests)]
pub use testhooks::*;

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
pub use report::report;
#[cfg(lazyos_tests)]
pub use report::{dropped_total, report_dropped};
use spin::Mutex;
use stamp::Stamped;
use uart_16550::SerialPort;

/// COM1, initialised, with its transmit ring.
struct Port {
    /// Owns the UART's configuration (set up by `init`).
    _uart: SerialPort,
    tx: tx::Tx,
}

static SERIAL1: Mutex<Option<Port>> = Mutex::new(None);

/// The port, locked. Every lock goes through here so the kernel suite can
/// prove that code which must not hold it with interrupts on does not.
fn port() -> spin::MutexGuard<'static, Option<Port>> {
    #[cfg(lazyos_tests)]
    testhooks::note_lock();
    SERIAL1.lock()
}
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

/// Most bytes a writer drains for its own line, however much it queued: the
/// rest goes out from the kernel task, with interrupts on.
const DRAIN_OWN_MAX: usize = 1024;
/// Most rounds the kernel task's drain runs per pass (a ring's worth of
/// chunks), so even a caller that cannot let a tick in ends.
const MIRROR_MAX_ROUNDS: usize = tx::Tx::CAPACITY / tx::CHUNK + 8;
/// Ticks (10 ms, real time on the TSC) a program write waits for its own bytes.
const MIRROR_WAIT_TICKS: u64 = 2;
/// Ticks of real time a program write waits for room in the ring before it
/// drops lines.
const MIRROR_ROOM_TICKS: u64 = 5;
/// Most bytes of one program write considered at all (the rest is dropped,
/// counted): at 38400 baud a megabyte takes four minutes to send.
const WRITE_MAX: usize = 1024 * 1024;
/// Most bytes set aside at once while waiting for room: what the ring can
/// hold for a program, so a huge write never costs a huge copy.
const ASIDE_MAX: usize = tx::Tx::CAPACITY - stamp::PROGRAM_RESERVE;
/// Ticks one pass of the kernel task's drain may take.
const SERVICE_TICKS: u64 = 2;
/// Bytes of backlog above which the kernel task asks for another pass soon
/// ([`backlog`]).
const BACKLOG_SOON: usize = 1024;

/// Whether the port lock is held right now (the NMI hang report, issue #382).
pub fn locked() -> bool {
    SERIAL1.is_locked()
}

/// Test hook: run `f` with the port locked, as code in the middle of a log
/// line would be (the interrupt-window suite). `f` must not print.
#[cfg(lazyos_tests)]
pub fn with_port_locked<R>(f: impl FnOnce() -> R) -> R {
    let _port = port();
    f()
}

/// Probe and initialise COM1, and log the verdict.
pub fn init() {
    let present = probe_on(&mut Com1);
    PRESENT.store(present, Ordering::Relaxed);
    if present {
        // Safety: 0x3F8 is the standard COM1 base port, and a UART answers there.
        let mut uart = unsafe { SerialPort::new(COM1) };
        uart.init();
        *port() = Some(Port {
            _uart: uart,
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
    if let Some(port) = port().as_mut() {
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
    if let Some(port) = port().as_mut() {
        let before = port.tx.queued();
        let _ = Stamped::kernel(&mut port.tx).write_str(s);
        port.drain_own(before);
    }
}

/// Write raw bytes to the serial port at once (terminal echo). Not copied
/// into the boot log: program output would push the boot out of it.
pub fn write_bytes(bytes: &[u8]) {
    if let Some(port) = port().as_mut() {
        let before = port.tx.queued();
        let _ = Stamped::program(&mut port.tx).lines(bytes);
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
/// interrupts let in between, for at most [`SERVICE_TICKS`]. Call with
/// interrupts on. It prints nothing: a trace takes the port's lock, which a
/// task preempted while holding it would keep from every syscall that logs
/// with interrupts off (the #382 rule); the reports are [`report`]'s.
pub fn service() {
    let start = crate::task::ticks();
    for _ in 0..MIRROR_MAX_ROUNDS {
        let empty =
            x86_64::instructions::interrupts::without_interrupts(|| match port().as_mut() {
                Some(port) => {
                    port.tx.drain(tx::CHUNK);
                    port.tx.is_empty()
                }
                None => true,
            });
        if empty || crate::task::ticks().wrapping_sub(start) >= SERVICE_TICKS {
            break;
        }
    }
}

/// Whether bytes wait in the ring beyond what the next writer will take
/// (the kernel task's loop sleeps one tick, not many, while this holds).
pub fn backlog() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        port()
            .as_ref()
            .is_some_and(|port| port.tx.len() > BACKLOG_SOON)
    })
}

/// Bytes waiting in the ring (tests).
#[cfg(lazyos_tests)]
pub fn queued_len() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| {
        port().as_ref().map_or(0, |port| port.tx.len())
    })
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
    if let Some(port) = port().as_mut() {
        port.tx.drain_all();
    }
}

/// The last line of a panic: the ring is emptied first, so a kernel that
/// filled it does not drop the line that says why it stopped (issue #857),
/// and again after, so the line is out before the machine halts.
///
/// A panic can come from code that held the port: the panicking context is
/// the only one that runs (one CPU), so a lock still held is never released
/// by its holder, and waiting for it would hang the report. It is broken.
pub fn final_line(args: fmt::Arguments) {
    if SERIAL1.is_locked() {
        // SAFETY: the holder is the context that panicked (or one it
        // interrupted), which never resumes: nobody else uses the guard.
        unsafe { SERIAL1.force_unlock() };
    }
    flush_final();
    _print(format_args!("{args}\n"));
    flush_final();
}

/// [`flush`] for the last lines the machine prints (a panic, a power-off):
/// then, if the ring dropped bytes since the last report, say so on the spot
/// (`SERIAL:DROPPED`, no budget applies to the end of a run) and flush that.
pub fn flush_final() {
    flush();
    if let Some((bytes, total)) = report::take_for_final() {
        _print(format_args!("SERIAL:DROPPED bytes={bytes} total={total}\n"));
        flush();
    }
}

/// Write one finished trace line (`trace.rs`): the serial port and, in a
/// `LAZYOS_DBGD=1` kernel, the program-output ring. Not the boot log: a chatty
/// trace must not push the boot diagnostics out of it. The line is queued
/// whole under the port lock, so no other task's output lands inside it, or
/// not at all: a ring without room for it drops it and counts it
/// (`SERIAL:DROPPED`), it is never cut.
pub fn write_trace(line: &[u8]) {
    #[cfg(lazyos_dbgd)]
    crate::klog::push_program(line);
    if let Some(port) = port().as_mut() {
        let before = port.tx.queued();
        let mut out = Stamped::kernel(&mut port.tx);
        let taken = out.own_lines(line);
        if taken < line.len() {
            out.drop_lines(&line[taken..]);
        }
        port.drain_own(before);
    }
}

/// Mirror a program's terminal output (`write` to the terminal, `sendfile`).
///
/// The whole lines that fit below the kernel's reserve are queued first, so
/// `bytes` (user memory) is read before any window opens. What does not fit
/// is copied aside, a ring's worth of whole lines at a time, and the writer
/// waits for room, helping to drain the ring in [`tx::CHUNK`]s with
/// interrupts let in between when the caller may sleep
/// (`block::iowait::breathe`), for [`MIRROR_ROOM_TICKS`] of real time; lines
/// still without room then are dropped, whole, and counted
/// (`SERIAL:DROPPED`). A caller with a fatal signal pending stops waiting at
/// once. At most [`WRITE_MAX`] bytes of one write are considered: at 38400
/// baud more would take minutes to send, and a huge buffer is a linear scan
/// with interrupts off.
///
/// Then it drains until its *own last byte* has gone out, so output keeps its
/// order with the kernel's own lines and a program that writes and then exits
/// has been heard. It never waits for the ring to empty (issue #857: while
/// anything else kept logging it never did, and the writer spun in the
/// kernel, immune to `SIGKILL`). It gives up after [`MIRROR_WAIT_TICKS`] of
/// real time, or at once when the caller has a fatal signal pending; its
/// bytes stay queued and the kernel task drains them. A write that queued
/// nothing (all of it dropped) returns at once.
pub fn mirror(bytes: &[u8]) {
    #[cfg(lazyos_dbgd)]
    crate::klog::push_program(bytes);
    let (bytes, excess) = cap_write(bytes);
    if excess > 0 {
        count_dropped(excess);
    }
    let Some((taken, start, mut end)) = queue_program(bytes) else {
        return;
    };
    let rest = &bytes[taken..];
    if !rest.is_empty() {
        end = queue_waiting(rest, end);
    }
    if end != start {
        wait_for_own_bytes(end);
    }
}

/// The part of a write that is considered: at most [`WRITE_MAX`] bytes, cut
/// at a line end, and how many bytes were left out.
fn cap_write(bytes: &[u8]) -> (&[u8], usize) {
    if bytes.len() <= WRITE_MAX {
        return (bytes, 0);
    }
    let cut = bytes[..WRITE_MAX]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(WRITE_MAX, |end| end + 1);
    (&bytes[..cut], bytes.len() - cut)
}

/// The next piece of `rest` to set aside: whole lines, at most
/// [`ASIDE_MAX`] bytes. `(len, skip)`: take `len` bytes, or (when the first
/// line is longer than the ring may hold) drop the first `skip` bytes.
fn next_chunk(rest: &[u8]) -> (usize, usize) {
    if rest.len() <= ASIDE_MAX {
        return (rest.len(), 0);
    }
    match rest[..ASIDE_MAX].iter().rposition(|&b| b == b'\n') {
        Some(end) => (end + 1, 0),
        None => (0, ASIDE_MAX),
    }
}

/// Queue what fits: the bytes taken, the queued mark before and the mark
/// after them; `None` without a port.
fn queue_program(bytes: &[u8]) -> Option<(usize, usize, usize)> {
    let mut guard = port();
    let port = guard.as_mut()?;
    let start = port.tx.queued();
    let taken = Stamped::program(&mut port.tx).lines(bytes);
    Some((taken, start, port.tx.queued()))
}

/// Count `bytes` as dropped.
fn count_dropped(bytes: usize) {
    if let Some(port) = port().as_mut() {
        port.tx.note_dropped(bytes);
    }
}

/// TSC cycles in `ticks` timer ticks (a guess before the timer is calibrated).
fn cycles_in_ticks(ticks: u64) -> u64 {
    #[cfg(lazyos_tests)]
    let ticks = ticks * testhooks::WAIT_SCALE.load(Ordering::Relaxed);
    match crate::arch::clock::cycles_per_tick() {
        0 => ticks * 30_000_000,
        per_tick => per_tick * ticks,
    }
}

/// Queue the lines of `rest` that did not fit, a chunk at a time as room
/// appears, until the room deadline; what is left then is dropped. Returns
/// the queued mark after the last line taken (`end` if none was).
fn queue_waiting(mut rest: &[u8], mut end: usize) -> usize {
    let deadline = crate::perf::rdtsc().saturating_add(cycles_in_ticks(MIRROR_ROOM_TICKS));
    while !rest.is_empty() {
        let (len, skip) = next_chunk(rest);
        if len == 0 {
            count_dropped(skip);
            rest = &rest[skip..];
            continue;
        }
        // Waiting lets other tasks run: keep our own copy of the chunk.
        let mut aside: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
        if aside.try_reserve_exact(len).is_err() {
            break;
        }
        aside.extend_from_slice(&rest[..len]);
        rest = &rest[len..];
        match wait_for_room(&aside, end, deadline) {
            (mark, true) => end = mark,
            (mark, false) => {
                end = mark;
                break;
            }
        }
    }
    if !rest.is_empty() {
        count_dropped(rest.len());
    }
    end
}

/// Queue `chunk` line by line as room appears, until `deadline` (TSC) or a
/// fatal signal. Returns the queued mark after the last line taken and
/// whether the whole chunk went in; the lines left are counted as dropped.
fn wait_for_room(chunk: &[u8], mut end: usize, deadline: u64) -> (usize, bool) {
    let mut rest = chunk;
    loop {
        if crate::task::signal::must_unwind(crate::task::current()) {
            break;
        }
        #[cfg(lazyos_tests)]
        MIRROR_ROUNDS.fetch_add(1, Ordering::Relaxed);
        if let Some(port) = port().as_mut() {
            port.tx.drain(tx::CHUNK);
            let taken = Stamped::program(&mut port.tx).lines(rest);
            rest = &rest[taken..];
            if taken > 0 {
                end = port.tx.queued();
            }
        }
        if rest.is_empty() {
            return (end, true);
        }
        if crate::perf::rdtsc() >= deadline {
            break;
        }
        crate::block::iowait::breathe(crate::block::Wait::MaySleep);
    }
    count_dropped(rest.len());
    (end, false)
}

/// Help drain until the bytes queued up to `mark` have gone out, for at most
/// [`MIRROR_WAIT_TICKS`] of real time (a port in back-off sends nothing, and
/// the time, not a count of rounds, bounds the wait).
fn wait_for_own_bytes(mark: usize) {
    let deadline = crate::perf::rdtsc().saturating_add(cycles_in_ticks(MIRROR_WAIT_TICKS));
    loop {
        if crate::task::signal::must_unwind(crate::task::current()) {
            return;
        }
        #[cfg(lazyos_tests)]
        MIRROR_ROUNDS.fetch_add(1, Ordering::Relaxed);
        let sent = match port().as_mut() {
            Some(port) => {
                port.tx.drain(tx::CHUNK);
                port.tx.passed(mark)
            }
            None => true,
        };
        if sent || crate::perf::rdtsc() >= deadline {
            return;
        }
        crate::block::iowait::breathe(crate::block::Wait::MaySleep);
    }
}
