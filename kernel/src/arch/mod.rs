//! Architecture-specific setup: interrupts, PIC, the tick (PIT or local APIC).

pub mod acpi_tables;
pub mod clock;
pub mod cpu;
pub mod event_timer;
pub mod fault;
pub mod fault_report;
pub mod fault_storm;
pub mod gdt;
pub mod idt;
pub mod io;
pub mod irq_stubs;
pub mod irq_window;
pub mod irqoff;
pub mod kernel_fault_report;
pub mod lapic;
pub mod linux;
pub mod msr;
pub mod nmi;
pub mod pagewalk;
pub mod pic;
pub mod raw_serial;
pub mod refclock;
pub mod rtc;
pub mod spurious_fault;
pub mod string_io;
pub mod timer;
pub mod timer_cal;

/// Initialise interrupt hardware and load the IDT.
pub fn init() {
    cpu::init();
    gdt::init();
    idt::init_hardware();
    linux::init();
    // Probe the controller first (`HW:I8042:PRESENT`/`ABSENT`); it enables
    // the mouse when an auxiliary port answers. Only a controller that is
    // there gets its bytes collected outside IRQ1/IRQ12 (`input::ps2`): a
    // floating bus reads 0xFF forever.
    crate::input::i8042::init();
    if crate::input::i8042::present() {
        crate::input::ps2::enable();
    }
}
