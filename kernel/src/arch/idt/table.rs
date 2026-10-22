//! The machine-wide IDT's one copy: the BSP builds it in [`super::init`],
//! every AP loads the same table in its bring-up (docs/smp-plan.md S8.2:
//! "the IDT stays one shared table").

use alloc::boxed::Box;
use core::sync::atomic::{AtomicPtr, Ordering};
use x86_64::structures::idt::InterruptDescriptorTable;

/// The published table (null before [`publish_and_load`]).
static TABLE: AtomicPtr<InterruptDescriptorTable> = AtomicPtr::new(core::ptr::null_mut());

/// Leak `idt` (it must outlive every CPU's `IDTR`), publish it for the APs
/// and load it on this CPU.
pub(super) fn publish_and_load(idt: Box<InterruptDescriptorTable>) {
    TABLE.store(Box::into_raw(idt), Ordering::Release);
    load_on_this_cpu();
}

/// Load the published IDT on this CPU (an AP starts with `IDTR` = 0, so any
/// interrupt or fault before this triple-faults). `false` before the BSP
/// published it.
pub fn load_on_this_cpu() -> bool {
    let table = TABLE.load(Ordering::Acquire);
    if table.is_null() {
        return false;
    }
    // SAFETY: the table was leaked by `publish_and_load` (`'static`, never
    // freed, never written after publication).
    unsafe { (*table).load() };
    true
}
