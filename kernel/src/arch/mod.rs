//! Architecture-specific setup: interrupts, PIC, PIT.

pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod linux;
pub mod msr;
pub mod pic;

/// Initialise interrupt hardware and load the IDT.
pub fn init() {
    cpu::init();
    gdt::init();
    idt::init_hardware();
    linux::init();
    crate::input::mouse::init();
}
