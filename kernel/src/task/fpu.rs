//! Per-task x87/SSE register state (issue #373).
//!
//! `arch::cpu::init` enables SSE for user code, and every Rust or C program
//! keeps floats and `memcpy` temporaries in XMM registers. The kernel itself
//! is built soft-float (`x86_64-unknown-none`) and never touches them, so the
//! live x87/SSE registers always belong to the task that last ran in user
//! mode. Before this module they were never switched: a tick that preempted
//! one app mid-computation resumed it with another app's XMM values and
//! MXCSR, which showed up as glyph rasterizers looping over garbage
//! coordinates and desktop apps that never finished their first frame.
//!
//! Each task slot owns one 512-byte `FXSAVE` area (static, like the kernel
//! stacks, so the scheduler never allocates). The scheduler saves the
//! outgoing task's registers and restores the incoming one's on every
//! switch; new tasks start from the power-on default, and `fork`/threads
//! inherit their creator's live registers, as on Linux.
//!
//! Every access runs with interrupts off on the single CPU (the scheduler
//! gates, spawn and exec syscalls), which is what makes the unsynchronised
//! per-slot areas sound.

use super::MAX_TASKS;

/// One `FXSAVE`/`FXRSTOR` image. The instructions require 16-byte alignment.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct FxArea([u8; 512]);

/// The state `FNINIT` plus the default MXCSR leave: x87 control word 0x037F
/// (all exceptions masked, extended precision), empty tag word, MXCSR 0x1F80
/// (all SIMD exceptions masked, round to nearest), every register zero.
const DEFAULT: FxArea = {
    let mut bytes = [0u8; 512];
    // FCW at offset 0.
    bytes[0] = 0x7f;
    bytes[1] = 0x03;
    // MXCSR at offset 24.
    bytes[24] = 0x80;
    bytes[25] = 0x1f;
    FxArea(bytes)
};

/// The saved state of each task slot while it is off the CPU.
static mut AREAS: [FxArea; MAX_TASKS] = [DEFAULT; MAX_TASKS];

/// A raw pointer to `slot`'s area (never a reference: the areas are only
/// touched through `FXSAVE`/`FXRSTOR` and plain copies).
fn area(slot: usize) -> *mut FxArea {
    assert!(slot < MAX_TASKS, "fpu: slot {slot} out of range");
    // SAFETY: `slot` is in bounds; `addr_of_mut!` takes the address without
    // creating a reference to the `static mut`.
    unsafe { core::ptr::addr_of_mut!(AREAS[slot]) }
}

/// Save the live x87/SSE registers into `slot`'s area.
pub(crate) fn save(slot: usize) {
    // SAFETY: the area is 512 bytes, 16-byte aligned (`FxArea`), and only
    // this CPU touches it with interrupts off. `fxsave64` writes nothing else.
    unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) area(slot), options(nostack)) };
}

/// Load `slot`'s saved state into the x87/SSE registers.
pub(crate) fn restore(slot: usize) {
    // SAFETY: as in `save`; the area only ever holds an image written by
    // `fxsave64` or `DEFAULT`, so its MXCSR has no reserved bits set and
    // `fxrstor64` cannot fault.
    unsafe { core::arch::asm!("fxrstor64 [{}]", in(reg) area(slot), options(nostack)) };
}

/// A new program in `slot` starts from the default state.
pub(crate) fn reset(slot: usize) {
    // SAFETY: in bounds (checked by `area`); interrupts are off, so nothing
    // else reads or writes the area concurrently.
    unsafe { area(slot).write(DEFAULT) };
}

/// `slot` (a fork child or a new thread) starts with the calling task's live
/// registers, the way Linux copies them into the child.
pub(crate) fn inherit_live(slot: usize) {
    save(slot);
}

/// Replace the calling task's live registers with the default state:
/// `execve` must not leak the old image's floats into the new one.
pub(crate) fn reset_live(slot: usize) {
    reset(slot);
    restore(slot);
}

/// Test view: `slot`'s saved MXCSR.
#[cfg(lazyos_tests)]
pub fn saved_mxcsr(slot: usize) -> u32 {
    // SAFETY: in bounds; a plain read of the 4 MXCSR bytes at offset 24.
    let bytes = unsafe { (*area(slot)).0 };
    u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]])
}

/// Test view: `slot`'s saved XMM0 low quadword (offset 160 in the image).
#[cfg(lazyos_tests)]
pub fn saved_xmm0(slot: usize) -> u64 {
    // SAFETY: in bounds; a plain read of the XMM0 slot.
    let bytes = unsafe { (*area(slot)).0 };
    let mut low = [0u8; 8];
    low.copy_from_slice(&bytes[160..168]);
    u64::from_le_bytes(low)
}
