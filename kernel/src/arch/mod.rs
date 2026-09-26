//! Architecture-specific setup: interrupts, PIC, PIT.

pub mod idt;
pub mod pic;

pub use idt::TICKS;

/// Initialise interrupt hardware and load the IDT.
pub fn init() {
    idt::init_hardware();
}
