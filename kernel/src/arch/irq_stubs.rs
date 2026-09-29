//! IDT vector stubs for the device IRQ lines (issue #240, driver-plan D2).
//!
//! PIC IRQ n arrives on vector 32 + n. The timer (0), keyboard (1) and mouse
//! (12) keep their own handlers in `arch::idt`; every other line gets a stub
//! that calls [`crate::dev::irq::dispatch`], which masks the line and leaves
//! the rest to the task-context bottom half. The cascade (2) is included so a
//! glitch on it is at least acknowledged and masked instead of hitting an
//! empty gate.

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

/// First vector of the remapped PIC.
const VECTOR_BASE: u8 = 32;

macro_rules! irq_stub {
    ($($name:ident => $line:literal),* $(,)?) => {
        $(
            extern "x86-interrupt" fn $name(_stack: InterruptStackFrame) {
                crate::dev::irq::dispatch($line);
            }
        )*
        /// Install the stub for every line that has no built-in handler.
        pub fn install(idt: &mut InterruptDescriptorTable) {
            $(
                idt[VECTOR_BASE + $line].set_handler_fn($name);
            )*
        }
    };
}

irq_stub! {
    irq2 => 2,
    irq3 => 3,
    irq4 => 4,
    irq5 => 5,
    irq6 => 6,
    irq7 => 7,
    irq8 => 8,
    irq9 => 9,
    irq10 => 10,
    irq11 => 11,
    irq13 => 13,
    irq14 => 14,
    irq15 => 15,
}
