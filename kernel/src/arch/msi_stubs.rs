//! IDT vector stubs for the MSI vectors (issue #616).
//!
//! MSI vector `i` arrives on `dev::msi::VECTOR_BASE + i` through the local
//! APIC. Each stub calls [`crate::dev::msi::dispatch`], which masks the
//! vector in software, records the raise and sends the APIC EOI; the rest is
//! the same task-context bottom half as a legacy line (`irq_stubs`).

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

use crate::dev::msi::{VECTORS, VECTOR_BASE};

macro_rules! msi_stub {
    ($($name:ident => $index:literal),* $(,)?) => {
        $(
            extern "x86-interrupt" fn $name(stack: InterruptStackFrame) {
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
            let stubs: &[extern "x86-interrupt" fn(InterruptStackFrame)] = &[$($name),*];
            assert_eq!(stubs.len(), usize::from(VECTORS), "one stub per MSI vector");
            for (index, stub) in stubs.iter().enumerate() {
                idt[VECTOR_BASE + index as u8].set_handler_fn(*stub);
            }
        }
    };
}

msi_stub! {
    msi0 => 0, msi1 => 1, msi2 => 2, msi3 => 3,
    msi4 => 4, msi5 => 5, msi6 => 6, msi7 => 7,
    msi8 => 8, msi9 => 9, msi10 => 10, msi11 => 11,
    msi12 => 12, msi13 => 13, msi14 => 14, msi15 => 15,
    msi16 => 16, msi17 => 17, msi18 => 18, msi19 => 19,
    msi20 => 20, msi21 => 21, msi22 => 22, msi23 => 23,
    msi24 => 24, msi25 => 25, msi26 => 26, msi27 => 27,
    msi28 => 28, msi29 => 29, msi30 => 30, msi31 => 31,
}
