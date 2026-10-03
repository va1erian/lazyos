//! Architecture-specific setup: interrupts, PIC, PIT.

pub mod clock;
pub mod cpu;
pub mod fault;
pub mod fault_report;
pub mod fault_storm;
pub mod gdt;
pub mod idt;
pub mod io;
pub mod irq_stubs;
pub mod kernel_fault_report;
pub mod linux;
pub mod msr;
pub mod nmi;
pub mod pagewalk;
pub mod pic;
pub mod raw_serial;
pub mod rtc;
pub mod spurious_fault;
pub mod string_io;

/// Initialise interrupt hardware and load the IDT.
pub fn init() {
    cpu::init();
    gdt::init();
    idt::init_hardware();
    linux::init();
    crate::input::mouse::init();
    // From here on the controller's bytes are collected wherever the kernel
    // can be busy for long, not only in IRQ1/IRQ12 (`input::ps2`).
    crate::input::ps2::enable();
}
