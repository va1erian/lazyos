//! Architecture-specific setup: interrupts, PIC, PIT.

pub mod idt;
pub mod pic;

/// Initialise interrupt hardware and load the IDT.
pub fn init() {
    idt::init_hardware();
}
