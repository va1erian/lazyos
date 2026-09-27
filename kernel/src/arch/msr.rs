//! Model-specific register access (needed for the Linux `syscall`/`sysret` path).

use x86_64::registers::model_specific::Msr;

/// `IA32_EFER` — enable `SCE` (System Call Extensions).
pub const IA32_EFER: u32 = 0xC000_0080;
/// `IA32_STAR` — syscall/sysret segment selectors.
pub const IA32_STAR: u32 = 0xC000_0081;
/// `IA32_LSTAR` — syscall entry RIP.
pub const IA32_LSTAR: u32 = 0xC000_0082;
/// `IA32_FMASK` — RFLAGS bits cleared on syscall entry.
pub const IA32_FMASK: u32 = 0xC000_0084;
/// `IA32_FS_BASE` — user thread pointer (`%fs`).
pub const IA32_FS_BASE: u32 = 0xC000_0100;
/// `IA32_GS_BASE` — kernel GS base.
pub const IA32_GS_BASE: u32 = 0xC000_0101;
/// `IA32_KERNEL_GS_BASE` — the GS base `swapgs` loads on entry.
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

/// Write an MSR.
pub fn write(msr: u32, value: u64) {
    // Safety: ring 0; writing the MSRs we own.
    unsafe {
        Msr::new(msr).write(value);
    }
}

/// Read an MSR.
pub fn read(msr: u32) -> u64 {
    // Safety: ring 0; reading a readable MSR.
    unsafe { Msr::new(msr).read() }
}
