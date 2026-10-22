//! IDT vector stubs for the device IRQ lines (issue #240, driver-plan D2).
//!
//! PIC IRQ n arrives on vector 32 + n. The timer (0), keyboard (1) and mouse
//! (12) keep their own handlers in `arch::idt`; every other line gets a stub
//! that calls [`crate::dev::irq::dispatch`], which masks the line and leaves
//! the rest to the task-context bottom half. The cascade (2) is included so a
//! glitch on it is at least acknowledged and masked instead of hitting an
//! empty gate.

use crate::arch::gsgate::irq_gate;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrameValue};

/// First vector of the remapped PIC.
const VECTOR_BASE: u8 = 32;

macro_rules! irq_stub {
    ($($name:ident / $gate:ident => $line:literal),* $(,)?) => {
        $(
            irq_gate!($gate => $name);

            extern "C" fn $name(stack: &InterruptStackFrameValue) {
                let quiet = crate::task::interrupted_quiet_context(stack.code_segment.0 as u64);
                crate::dev::irq::dispatch($line);
                // Tell a userspace claimant now rather than at the next
                // syscall or mux pass (P1.2), when what this interrupt
                // stopped holds no lock the bottom half takes.
                if quiet {
                    crate::dev::intx::service_in_interrupt();
                }
                // The claimant (or a kernel driver's waiter) may outrank
                // what was running (P1.1).
                crate::task::preempt_point();
            }
        )*
        /// Install the stub for every line that has no built-in handler.
        pub fn install(idt: &mut InterruptDescriptorTable) {
            $(
                // SAFETY: an `irq_gate!` stub (`arch::gsgate`): no error
                // code, ends in `iretq`.
                unsafe {
                    idt[VECTOR_BASE + $line].set_handler_addr(super::idt::gate($gate));
                }
            )*
        }
    };
}

irq_stub! {
    irq2 / irq2_gate => 2,
    irq3 / irq3_gate => 3,
    irq4 / irq4_gate => 4,
    irq5 / irq5_gate => 5,
    irq6 / irq6_gate => 6,
    irq7 / irq7_gate => 7,
    irq8 / irq8_gate => 8,
    irq9 / irq9_gate => 9,
    irq10 / irq10_gate => 10,
    irq11 / irq11_gate => 11,
    irq13 / irq13_gate => 13,
    irq14 / irq14_gate => 14,
    irq15 / irq15_gate => 15,
}
