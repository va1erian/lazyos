//! Unified logging sink: mirror text to the serial port *and* the framebuffer
//! console, so `println!` output is visible both on screen and over COM1.

use core::fmt;

struct Mirror;

impl fmt::Write for Mirror {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        crate::serial::_write_str(s);
        crate::console::_write_str(s);
        Ok(())
    }
}

/// Write a formatted message to every active sink (used by `print!`/`println!`).
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    let _ = Mirror.write_fmt(args);
}
