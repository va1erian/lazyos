//! Minimal COM1 serial output for logging.
//!
//! Every line also goes to the boot-log ring (`klog`), which is what a PC
//! with no serial port has instead. COM1 is probed once at boot (H1 of
//! `docs/real-pc-boot-plan.md`): an absent port is never initialised or
//! written, and the verdict is logged as `HW:COM1:PRESENT|ABSENT`.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use uart_16550::SerialPort;

static SERIAL1: Mutex<Option<SerialPort>> = Mutex::new(None);
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

/// Writes to the port, prefixing each line with `[secs.millis]` uptime from
/// the 100 Hz PIT tick counter (0 until the timer starts).
struct Stamped<'a>(&'a mut SerialPort);

impl Stamped<'_> {
    /// One byte to the UART, a poll point first (`arch::irq_window`): at the
    /// port's 38400 baud a byte takes about 260 µs on hardware (and a VM exit
    /// per port access under a hypervisor), all with interrupts off, so a
    /// line or even a timestamp prefix must not go out in one stretch.
    fn send(&mut self, byte: u8) {
        crate::arch::irq_window::poll_point();
        self.0.send(byte);
    }

    fn put(&mut self, byte: u8) {
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
        *SERIAL1.lock() = Some(port);
    }
    crate::serial_println!("HW:COM1:{}", if present { "PRESENT" } else { "ABSENT" });
}

/// Write a formatted message to the boot log and the serial port (used by
/// the `serial_print!` macros).
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    crate::klog::write_fmt(args);
    if let Some(port) = SERIAL1.lock().as_mut() {
        let _ = Stamped(port).write_fmt(args);
    }
}

/// Print unless the port is busy; returns whether it printed. For interrupt
/// context, where the code just interrupted may hold the port's lock and
/// waiting for it would deadlock.
pub fn try_print(args: fmt::Arguments) -> bool {
    use core::fmt::Write;
    if let Some(mut guard) = SERIAL1.try_lock() {
        if let Some(port) = guard.as_mut() {
            let _ = Stamped(port).write_fmt(args);
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
        let _ = Stamped(port).write_str(s);
    }
}

/// Write raw bytes to the serial port (mirrors user-program output). Not
/// copied into the boot log: program output would push the boot out of it.
pub fn write_bytes(bytes: &[u8]) {
    if let Some(port) = SERIAL1.lock().as_mut() {
        let mut out = Stamped(port);
        bytes.iter().for_each(|&byte| out.put(byte));
    }
}
