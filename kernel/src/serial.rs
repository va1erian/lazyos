//! Minimal COM1 serial output for logging.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use uart_16550::SerialPort;

static SERIAL1: Mutex<Option<SerialPort>> = Mutex::new(None);

/// Whether lines get an uptime prefix. Only optimized boots stamp: the debug
/// and `LAZYOS_TESTS` boots feed line-anchored evidence parsers in `tools/`.
const TIMESTAMPS: bool = cfg!(all(not(debug_assertions), not(lazyos_tests)));

/// Whether the next byte written starts a new line (guarded by `SERIAL1`).
static AT_LINE_START: AtomicBool = AtomicBool::new(true);

/// Writes to the port, prefixing each line with `[secs.millis]` uptime from
/// the 100 Hz PIT tick counter (0 until the timer starts).
struct Stamped<'a>(&'a mut SerialPort);

impl Stamped<'_> {
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
            self.0.send(b'[');
            for i in (0..len).rev() {
                self.0.send(digits[i]);
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
                self.0.send(b);
            }
        }
        if byte == b'\n' {
            AT_LINE_START.store(true, Ordering::Relaxed);
        }
        self.0.send(byte);
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

/// Initialise COM1.
pub fn init() {
    // Safety: 0x3F8 is the standard COM1 base port.
    let mut port = unsafe { SerialPort::new(0x3F8) };
    port.init();
    *SERIAL1.lock() = Some(port);
}

/// Write a formatted message to the serial port (used by the `serial_print!` macros).
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    if let Some(port) = SERIAL1.lock().as_mut() {
        let _ = Stamped(port).write_fmt(args);
    }
}

/// Write a string to the serial port (used by the unified logging sink).
pub fn _write_str(s: &str) {
    use core::fmt::Write;
    if let Some(port) = SERIAL1.lock().as_mut() {
        let _ = Stamped(port).write_str(s);
    }
}

/// Write raw bytes to the serial port (mirrors user-program output).
pub fn write_bytes(bytes: &[u8]) {
    if let Some(port) = SERIAL1.lock().as_mut() {
        let mut out = Stamped(port);
        bytes.iter().for_each(|&byte| out.put(byte));
    }
}
