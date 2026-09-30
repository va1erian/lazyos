//! Architecture-specific setup: interrupts, PIC, PIT.

pub mod cpu;
pub mod fault;
pub mod fault_report;
pub mod gdt;
pub mod idt;
pub mod io;
pub mod irq_stubs;
pub mod linux;
pub mod msr;
pub mod pic;
pub mod rtc;

/// Initialise interrupt hardware and load the IDT.
pub fn init() {
    cpu::init();
    gdt::init();
    idt::init_hardware();
    linux::init();
    crate::input::mouse::init();
}
