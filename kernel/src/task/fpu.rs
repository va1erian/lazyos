//! Per-task x87/SSE/AVX register state (issues #373, #822).
//!
//! `arch::cpu::init` enables SSE (and AVX, when the CPU has it) for user code,
//! and every Rust or C program keeps floats and `memcpy` temporaries in XMM
//! registers. The kernel itself is built soft-float (`x86_64-unknown-none`)
//! and never touches them, so the live registers always belong to the task
//! that last ran in user mode. Before this module they were never switched: a
//! tick that preempted one app mid-computation resumed it with another app's
//! XMM values and MXCSR, which showed up as glyph rasterizers looping over
//! garbage coordinates and desktop apps that never finished their first frame.
//!
//! Each task slot owns one [`AREA_SIZE`]-byte area (static, like the kernel
//! stacks, so the scheduler never allocates), 64-byte aligned as `XSAVE`
//! requires. With `XSAVE` enabled (`arch::cpu::xsave_mask() != 0`) it holds the
//! standard-format image of x87, SSE and the upper halves of the YMM
//! registers: 512 legacy bytes (the `FXSAVE` layout), a 64-byte header whose
//! first word is `XSTATE_BV` (which components are not in their initial state),
//! and `YMM_Hi128` at offset 576. Without it, only the first 512 bytes are
//! used, saved with `FXSAVE`. The scheduler saves the outgoing task's
//! registers and restores the incoming one's on every switch; new tasks start
//! from the power-on default, and `fork`/threads inherit their creator's live
//! registers, as on Linux.
//!
//! The code never uses `XSAVEOPT`/`XSAVEC`: the header's reserved words stay
//! zero in every area, which is what keeps `XRSTOR` from faulting, and a
//! plain `XSAVE` image is what the Linux signal frame carries.
//!
//! Every access runs with interrupts off on the single CPU (the scheduler
//! gates, spawn and exec syscalls), which is what makes the unsynchronised
//! per-slot areas sound.

use super::MAX_TASKS;
use crate::arch::cpu;

/// Size in bytes of the legacy `FXSAVE` image, the start of every area.
pub const LEGACY_SIZE: usize = 512;
/// Size in bytes of a full x87 + SSE + AVX `XSAVE` image.
pub const AREA_SIZE: usize = cpu::XSAVE_AREA_SIZE;
/// Offset of the `XSAVE` header (`XSTATE_BV` is its first word).
pub const HEADER_OFFSET: usize = LEGACY_SIZE;
/// First byte after the header, where the extended components start.
pub const EXTENDED_OFFSET: usize = HEADER_OFFSET + 64;
/// `XSTATE_BV`/`XCR0` bit of the x87 state.
pub const XFEATURE_X87: u64 = 1;
/// `XSTATE_BV`/`XCR0` bit of the SSE state (XMM registers and MXCSR).
pub const XFEATURE_SSE: u64 = 1 << 1;
/// `XSTATE_BV`/`XCR0` bit of the AVX state (`YMM_Hi128`).
pub const XFEATURE_AVX: u64 = 1 << 2;

/// One register image: the `FXSAVE` layout, followed by the `XSAVE` header
/// and `YMM_Hi128` when `XSAVE` is in use. `FXSAVE` needs 16-byte alignment
/// and `XSAVE` 64.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct FpuImage([u8; AREA_SIZE]);

impl FpuImage {
    /// An all-zero image (not a valid register state until filled).
    pub const ZERO: FpuImage = FpuImage([0; AREA_SIZE]);

    /// Quadword `index` (0..`AREA_SIZE / 8`) of the image.
    pub fn word(&self, index: usize) -> u64 {
        let mut word = [0u8; 8];
        word.copy_from_slice(&self.0[index * 8..index * 8 + 8]);
        u64::from_le_bytes(word)
    }

    /// Replace quadword `index` (0..`AREA_SIZE / 8`).
    pub fn set_word(&mut self, index: usize, value: u64) {
        self.0[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }

    /// Which components the image says are not in their initial state.
    pub fn xstate_bv(&self) -> u64 {
        self.word(HEADER_OFFSET / 8)
    }

    /// Make the image carry only the components in `features`: the others
    /// revert to their initial state when it is restored.
    pub fn set_xstate_bv(&mut self, features: u64) {
        self.set_word(HEADER_OFFSET / 8, features);
    }

    /// Make the image safe to hand to `FXRSTOR`/`XRSTOR`, which raise `#GP`
    /// in ring 0 on a reserved MXCSR bit and (`XRSTOR`) on a header that
    /// names a component `XCR0` has off or sets the compaction bits. The
    /// image comes from user memory, so the CPU's own `MXCSR_MASK` decides,
    /// never the image's.
    pub(crate) fn sanitize(&mut self) {
        let mut mxcsr = [0u8; 4];
        mxcsr.copy_from_slice(&self.0[24..28]);
        let mxcsr = u32::from_le_bytes(mxcsr) & cpu::mxcsr_mask();
        self.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        // The CPU does not read the image's MXCSR_MASK field (offset 28),
        // but a frame handed back to user space should not show a forged one.
        self.0[28..32].copy_from_slice(&cpu::mxcsr_mask().to_le_bytes());
        let features = self.xstate_bv() & cpu::xsave_mask();
        self.0[HEADER_OFFSET..EXTENDED_OFFSET].fill(0);
        self.set_xstate_bv(features);
    }
}

/// The state `FNINIT` plus the default MXCSR leave: x87 control word 0x037F
/// (all exceptions masked, extended precision), empty tag word, MXCSR 0x1F80
/// (all SIMD exceptions masked, round to nearest), every register zero. The
/// header names x87 and SSE as in use, so `XRSTOR` loads the two words above
/// from the image, and leaves the upper YMM halves in their initial (zero)
/// state.
const DEFAULT: FpuImage = {
    let mut bytes = [0u8; AREA_SIZE];
    // FCW at offset 0.
    bytes[0] = 0x7f;
    bytes[1] = 0x03;
    // MXCSR at offset 24.
    bytes[24] = 0x80;
    bytes[25] = 0x1f;
    // XSTATE_BV: x87 | SSE.
    bytes[HEADER_OFFSET] = (XFEATURE_X87 | XFEATURE_SSE) as u8;
    FpuImage(bytes)
};

/// The saved state of each task slot while it is off the CPU.
static mut AREAS: [FpuImage; MAX_TASKS] = [DEFAULT; MAX_TASKS];

/// A raw pointer to `slot`'s area (never a reference: the areas are only
/// touched through `XSAVE`/`XRSTOR` and plain copies).
fn area(slot: usize) -> *mut FpuImage {
    assert!(slot < MAX_TASKS, "fpu: slot {slot} out of range");
    // SAFETY: `slot` is in bounds; `addr_of_mut!` takes the address without
    // creating a reference to the `static mut`.
    unsafe { core::ptr::addr_of_mut!(AREAS[slot]) }
}

/// Bytes of the image a signal frame carries and a switch writes: the whole
/// `XSAVE` area, or the 512 legacy bytes on a CPU run with `FXSAVE`.
pub fn state_size() -> usize {
    if cpu::xsave_mask() != 0 {
        AREA_SIZE
    } else {
        LEGACY_SIZE
    }
}

/// The `XCR0` components user space may use: what the frame's `xfeatures`
/// reports.
pub fn xfeatures() -> u64 {
    match cpu::xsave_mask() {
        0 => XFEATURE_X87 | XFEATURE_SSE,
        mask => mask,
    }
}

/// Write the live registers to `*image`.
///
/// # Safety
/// `image` must be valid for [`AREA_SIZE`] bytes, 64-byte aligned, and
/// nothing else may access it for the duration (interrupts off).
unsafe fn save_to(image: *mut FpuImage) {
    let mask = cpu::xsave_mask();
    // SAFETY: the caller guarantees the image; `xsave64` writes at most
    // `AREA_SIZE` bytes for the enabled components, `fxsave64` 512.
    unsafe {
        if mask != 0 {
            core::arch::asm!(
                "xsave64 [{}]",
                in(reg) image,
                in("eax") mask as u32,
                in("edx") (mask >> 32) as u32,
                options(nostack)
            );
        } else {
            core::arch::asm!("fxsave64 [{}]", in(reg) image, options(nostack));
        }
    }
}

/// Load the live registers from `*image`.
///
/// # Safety
/// As [`save_to`], and the image must be one `XRSTOR`/`FXRSTOR` accepts: an
/// area written by [`save_to`], [`DEFAULT`], or an image that went through
/// [`FpuImage::sanitize`].
unsafe fn restore_from(image: *const FpuImage) {
    let mask = cpu::xsave_mask();
    // SAFETY: the caller guarantees a valid image, so neither instruction
    // faults.
    unsafe {
        if mask != 0 {
            core::arch::asm!(
                "xrstor64 [{}]",
                in(reg) image,
                in("eax") mask as u32,
                in("edx") (mask >> 32) as u32,
                options(nostack)
            );
        } else {
            core::arch::asm!("fxrstor64 [{}]", in(reg) image, options(nostack));
        }
    }
}

/// Save the live registers into `slot`'s area.
pub(crate) fn save(slot: usize) {
    // SAFETY: the area is `AREA_SIZE` bytes, 64-byte aligned, and only this
    // CPU touches it with interrupts off.
    unsafe { save_to(area(slot)) };
}

/// Load `slot`'s saved state into the registers.
pub(crate) fn restore(slot: usize) {
    // SAFETY: as `save`; the area only ever holds an image written by
    // `save_to`, `DEFAULT` or `load` (sanitised), so the restore cannot fault.
    unsafe { restore_from(area(slot)) };
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

/// `slot`'s current register state: the live registers when `slot` is the
/// task on the CPU (the kernel is soft-float, so they are its user state),
/// else its saved area. Signal delivery reads it to build a frame, and the
/// timer sweep delivers to threads that are not running.
pub fn capture(slot: usize) -> FpuImage {
    if slot == crate::task::current() {
        // Start from the default so a component `XSAVE` leaves unwritten
        // (it is in its initial state, `XSTATE_BV` says so) still reads as
        // the initial value.
        let mut image = DEFAULT;
        // SAFETY: `image` is `AREA_SIZE` bytes and 64-byte aligned.
        unsafe { save_to(&mut image) };
        image
    } else {
        // SAFETY: in bounds; the sweep and callers run with interrupts off.
        unsafe { *area(slot) }
    }
}

/// Make `image` `slot`'s state (live registers for the running task, else its
/// saved area). The image comes from user memory, so it is sanitised first:
/// `FXRSTOR`/`XRSTOR` would raise `#GP` in ring 0 on reserved MXCSR bits or a
/// malformed `XSAVE` header.
pub fn load(slot: usize, image: &FpuImage) {
    let mut image = *image;
    image.sanitize();
    // SAFETY: as `capture`; the sanitised image cannot fault the restore.
    unsafe {
        if slot == crate::task::current() {
            restore_from(&image);
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

/// Test view: `slot`'s saved XMM0 low quadword (offset 160 in the image); zero
/// when the image says the SSE state is in its initial state.
#[cfg(lazyos_tests)]
pub fn saved_xmm0(slot: usize) -> u64 {
    // SAFETY: in bounds; a plain read of the XMM0 slot.
    let image = unsafe { *area(slot) };
    if cpu::xsave_mask() != 0 && image.xstate_bv() & XFEATURE_SSE == 0 {
        return 0;
    }
    image.word(160 / 8)
}
