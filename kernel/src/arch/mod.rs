//! Architecture-specific setup: interrupts, PIC, PIT.

pub mod idt;
pub mod pic;

/// Initialise interrupt hardware and load the IDT.
pub fn init() {
    idt::init_hardware();
}

/// Timer ticks (100 Hz) since boot.
pub fn ticks() -> u64 {
    idt::TICKS.load(core::sync::atomic::Ordering::Relaxed)
}
