//! Lock-free COM1 output for reports printed from NMI context (issue #382).
//!
//! `serial_println!` takes the `SERIAL1` spin lock. A hang report exists to
//! describe a machine that may be spinning on exactly such a lock, so it must
//! write the UART directly: the interrupted context cannot run while the NMI
//! handler does, and the worst a torn line costs is a garbled byte in a log.

use core::fmt;

use super::io::{inb, outb};

/// COM1 transmit holding register.
const COM1: u16 = 0x3F8;
/// COM1 line status register.
const LINE_STATUS: u16 = COM1 + 5;
/// Line status bit: the transmit holding register is empty.
const THR_EMPTY: u8 = 0x20;
/// Polls before a byte is sent regardless: a wedged UART must not turn the
/// report itself into a second hang.
const MAX_POLLS: u32 = 100_000;

/// A `fmt::Write` sink straight to the COM1 registers.
pub struct RawSerial;

impl RawSerial {
    fn put(byte: u8) {
        // No UART (the boot probe said so): every poll would read a floating
        // bus and the byte would go nowhere.
        if !crate::serial::present() {
            return;
        }
        for _ in 0..MAX_POLLS {
            // SAFETY: reading COM1's line status register has no side effect;
            // `serial::init` configured the port at boot.
            if unsafe { inb(LINE_STATUS) } & THR_EMPTY != 0 {
                break;
            }
        }
        // SAFETY: writing the transmit register queues one byte on the
        // initialised COM1 port, the same operation `uart_16550` performs.
        unsafe { outb(COM1, byte) };
    }
}

impl fmt::Write for RawSerial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.bytes().for_each(Self::put);
        Ok(())
    }
}
