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

/// Size in bytes of one `FXSAVE` image.
pub const IMAGE_SIZE: usize = 512;

/// One `FXSAVE`/`FXRSTOR` image. The instructions require 16-byte alignment;
/// 64 keeps the type usable for an `XSAVE` area later.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct FxImage([u8; IMAGE_SIZE]);

impl FxImage {
    /// An all-zero image (not a valid register state until filled).
    pub const ZERO: FxImage = FxImage([0; IMAGE_SIZE]);

    /// Quadword `index` (0..64) of the image.
    pub fn word(&self, index: usize) -> u64 {
        let mut word = [0u8; 8];
        word.copy_from_slice(&self.0[index * 8..index * 8 + 8]);
        u64::from_le_bytes(word)
    }

    /// Replace quadword `index` (0..64).
    pub fn set_word(&mut self, index: usize, value: u64) {
        self.0[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }

    /// Clear the MXCSR bits the CPU reserves, which `FXRSTOR` would fault on.
    /// The image's own `MXCSR_MASK` field (offset 28) says which are valid;
    /// zero means the architectural default.
    fn sanitize(&mut self) {
        let mut mask = [0u8; 4];
        mask.copy_from_slice(&self.0[28..32]);
        let mask = match u32::from_le_bytes(mask) {
            0 => 0x0000_ffbf,
            valid => valid,
        };
        let mut mxcsr = [0u8; 4];
        mxcsr.copy_from_slice(&self.0[24..28]);
        let mxcsr = u32::from_le_bytes(mxcsr) & mask;
        self.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    }
}

type FxArea = FxImage;

/// The state `FNINIT` plus the default MXCSR leave: x87 control word 0x037F
/// (all exceptions masked, extended precision), empty tag word, MXCSR 0x1F80
/// (all SIMD exceptions masked, round to nearest), every register zero.
const DEFAULT: FxArea = {
    let mut bytes = [0u8; IMAGE_SIZE];
    // FCW at offset 0.
    bytes[0] = 0x7f;
    bytes[1] = 0x03;
    // MXCSR at offset 24.
    bytes[24] = 0x80;
    bytes[25] = 0x1f;
    FxImage(bytes)
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

/// `slot`'s current x87/SSE state: the live registers when `slot` is the task
/// on the CPU (the kernel is soft-float, so they are its user state), else its
/// saved area. Signal delivery reads it to build a frame, and the timer sweep
/// delivers to threads that are not running.
pub fn capture(slot: usize) -> FxImage {
    if slot == crate::task::current() {
        let mut image = FxImage::ZERO;
        // SAFETY: `image` is 512 bytes and 64-byte aligned; `fxsave64` writes
        // nothing else.
        unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) &mut image, options(nostack)) };
        image
    } else {
        // SAFETY: in bounds; the sweep and callers run with interrupts off.
        unsafe { *area(slot) }
    }
}

/// Make `image` `slot`'s state (live registers for the running task, else its
/// saved area). The image comes from user memory, so reserved MXCSR bits are
/// cleared first: `FXRSTOR` would raise `#GP` in ring 0 on them.
pub fn load(slot: usize, image: &FxImage) {
    let mut image = *image;
    image.sanitize();
    // SAFETY: as `capture`; the sanitised MXCSR cannot fault `fxrstor64`.
    unsafe {
        if slot == crate::task::current() {
            core::arch::asm!("fxrstor64 [{}]", in(reg) &image, options(nostack));
        } else {
            area(slot).write(image);
        }
    }
}

/// Give `slot` the power-on state, as a signal handler starts with (Linux
/// does the same): the handler must not see, or depend on, the interrupted
/// code's registers, which the frame holds.
pub fn reset_for(slot: usize) {
    load(slot, &DEFAULT);
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
