//! Architecture-specific setup: interrupts, PIC, PIT.

pub mod gdt;
pub mod idt;
pub mod pic;

/// Initialise interrupt hardware and load the IDT.
pub fn init() {
    gdt::init();
    idt::init_hardware();
    crate::input::mouse::init();
}
