//! CPU feature setup: the FPU and SSE.
//!
//! On x86_64 `movups`/`movsd` and friends raise `#UD` unless the OS clears
//! `CR0.EM` and sets `CR4.OSFXSR`; the bootloader does not do this for us.

use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

/// Enable the x87 FPU and SSE/SSE2 for kernel and user code.
pub fn init() {
    // Safety: only toggling the documented control-register feature bits.
    unsafe {
        let mut cr0 = Cr0::read();
        cr0.remove(Cr0Flags::EMULATE_COPROCESSOR);
        cr0.insert(Cr0Flags::MONITOR_COPROCESSOR);
        cr0.remove(Cr0Flags::TASK_SWITCHED);
        Cr0::write(cr0);

        let mut cr4 = Cr4::read();
        cr4.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
        Cr4::write(cr4);
    }

    // Reset the x87/SSE state (FNINIT sets the default control/status words).
    // Safety: no memory operands; only resets the FPU.
    unsafe {
        core::arch::asm!("fninit");
    }
}
