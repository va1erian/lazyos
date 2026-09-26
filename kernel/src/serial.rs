//! Minimal COM1 serial output for logging.

use core::fmt;
use spin::Mutex;
use uart_16550::SerialPort;

static SERIAL1: Mutex<Option<SerialPort>> = Mutex::new(None);

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
        let _ = port.write_fmt(args);
    }
}

/// Write a string to the serial port (used by the unified logging sink).
pub fn _write_str(s: &str) {
    use core::fmt::Write;
    if let Some(port) = SERIAL1.lock().as_mut() {
        let _ = port.write_str(s);
    }
}

/// Write raw bytes to the serial port (mirrors user-program output).
pub fn write_bytes(bytes: &[u8]) {
    if let Some(port) = SERIAL1.lock().as_mut() {
        for &byte in bytes {
            port.send(byte);
        }
    }
}
