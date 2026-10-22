//! IDT vector stubs for the MSI vectors (issue #616).
//!
//! MSI vector `i` arrives on `dev::msi::VECTOR_BASE + i` through the local
//! APIC. Each stub calls [`crate::dev::msi::dispatch`], which masks the
//! vector in software, records the raise and sends the APIC EOI; the rest is
//! the same task-context bottom half as a legacy line (`irq_stubs`).

use crate::arch::gsgate::irq_gate;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrameValue};

use crate::dev::msi::{VECTORS, VECTOR_BASE};

macro_rules! msi_stub {
    ($($name:ident / $gate:ident => $index:literal),* $(,)?) => {
        $(
            irq_gate!($gate => $name);

            extern "C" fn $name(stack: &InterruptStackFrameValue) {
                let quiet = crate::task::interrupted_quiet_context(stack.code_segment.0 as u64);
                crate::dev::msi::dispatch($index);
                if quiet {
                    crate::dev::intx::service_in_interrupt();
                }
                crate::task::preempt_point();
            }
        )*
        /// Install a stub on every MSI vector.
        pub fn install(idt: &mut InterruptDescriptorTable) {
            let gates: &[unsafe extern "C" fn()] = &[$($gate),*];
            assert_eq!(gates.len(), usize::from(VECTORS), "one stub per MSI vector");
            for (index, gate) in gates.iter().enumerate() {
                // SAFETY: an `irq_gate!` stub (`arch::gsgate`): no error
                // code, ends in `iretq`.
                unsafe {
                    idt[VECTOR_BASE + index as u8].set_handler_addr(super::idt::gate(*gate));
                }
            }
        }
    };
}

msi_stub! {
    msi0 / msi0_gate => 0, msi1 / msi1_gate => 1, msi2 / msi2_gate => 2, msi3 / msi3_gate => 3,
    msi4 / msi4_gate => 4, msi5 / msi5_gate => 5, msi6 / msi6_gate => 6, msi7 / msi7_gate => 7,
    msi8 / msi8_gate => 8, msi9 / msi9_gate => 9, msi10 / msi10_gate => 10, msi11 / msi11_gate => 11,
    msi12 / msi12_gate => 12, msi13 / msi13_gate => 13, msi14 / msi14_gate => 14, msi15 / msi15_gate => 15,
    msi16 / msi16_gate => 16, msi17 / msi17_gate => 17, msi18 / msi18_gate => 18, msi19 / msi19_gate => 19,
    msi20 / msi20_gate => 20, msi21 / msi21_gate => 21, msi22 / msi22_gate => 22, msi23 / msi23_gate => 23,
    msi24 / msi24_gate => 24, msi25 / msi25_gate => 25, msi26 / msi26_gate => 26, msi27 / msi27_gate => 27,
    msi28 / msi28_gate => 28, msi29 / msi29_gate => 29, msi30 / msi30_gate => 30, msi31 / msi31_gate => 31,
}
