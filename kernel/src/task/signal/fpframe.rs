//! The floating-point state in the Linux signal frame (issues #809, #822).
//!
//! `uc_mcontext.fpstate` points at a `struct _fpstate` in the frame: the
//! 512-byte `FXSAVE` image. With `XSAVE` in use the image is the whole
//! standard-format `XSAVE` area (legacy region, 64-byte header, `YMM_Hi128`),
//! and Linux marks that in the legacy region's software-reserved bytes
//! (offset 464, `struct _fpx_sw_bytes`) so that glibc, musl, libgcc and
//! debuggers can find the extended part:
//!
//! ```text
//! 464  u32 magic1 = FP_XSTATE_MAGIC1 ("FPXS")
//! 468  u32 extended_size = xstate_size + 4
//! 472  u64 xfeatures     = the XCR0 components the area holds
//! 480  u32 xstate_size   = bytes of the XSAVE area
//! 484  u32 padding[7]
//! ...  the XSAVE area, xstate_size bytes
//!      u32 magic2 = FP_XSTATE_MAGIC2 ("FPXE"), right after the area
//! ```
//!
//! On a CPU run with `FXSAVE` alone the frame holds the 512 bytes and the
//! same software bytes with `xstate_size = extended_size = 512` and no
//! trailing magic, as Linux writes them.
//!
//! `rt_sigreturn` reads the image back from user memory, which a handler (or
//! an attacker) may have rewritten: a missing or inconsistent marker falls
//! back to the legacy region with the AVX state reset, and
//! [`crate::task::fpu::load`] sanitises MXCSR and the header either way.

use super::frames::{write_u32, write_u64};
use super::harden::{read_word, MIN_FRAME_ADDR, USER_MAX};
use crate::arch::cpu;
use crate::task::fpu::{self, FpuImage};

/// `FP_XSTATE_MAGIC1`: starts the software bytes of an `XSAVE` frame.
pub const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
/// `FP_XSTATE_MAGIC2`: follows the `XSAVE` area in the frame.
pub const FP_XSTATE_MAGIC2: u32 = 0x4650_5845;
/// Bytes `FP_XSTATE_MAGIC2` takes after the area.
const MAGIC2_SIZE: u64 = 4;
/// Offset of `struct _fpx_sw_bytes` in the legacy region.
pub const SW_BYTES_OFFSET: u64 = 464;

/// Whether the frame carries an `XSAVE` area.
fn xsave() -> bool {
    cpu::xsave_mask() != 0
}

/// Bytes the frame's FP state occupies: the image, plus the trailing magic
/// when it is an `XSAVE` area.
pub(super) fn frame_size() -> u64 {
    fpu::state_size() as u64 + if xsave() { MAGIC2_SIZE } else { 0 }
}

/// Write `fp` and its markers at `addr` (64-byte aligned, `frame_size()`
/// bytes of the user stack).
pub(super) fn write(addr: u64, fp: &FpuImage) {
    let size = fpu::state_size() as u64;
    for index in 0..size / 8 {
        write_u64(addr + 8 * index, fp.word(index as usize));
    }
    write_u32(addr + SW_BYTES_OFFSET, FP_XSTATE_MAGIC1);
    write_u32(addr + SW_BYTES_OFFSET + 4, frame_size() as u32);
    write_u64(addr + SW_BYTES_OFFSET + 8, fpu::xfeatures());
    write_u32(addr + SW_BYTES_OFFSET + 16, size as u32);
    for pad in 0..7 {
        write_u32(addr + SW_BYTES_OFFSET + 20 + 4 * pad, 0);
    }
    if xsave() {
        write_u32(addr + size, FP_XSTATE_MAGIC2);
    }
}

/// The `XSAVE` area size the frame at `ptr` claims, when its markers are all
/// consistent with what this kernel can restore; `None` means "treat it as a
/// plain `FXSAVE` image". `legacy` is the image's first 512 bytes.
fn claimed_size(ptr: u64, legacy: &FpuImage) -> Option<usize> {
    let word = |offset: u64| legacy.word(((SW_BYTES_OFFSET + offset) / 8) as usize);
    let magic1 = word(0) as u32;
    let extended = (word(0) >> 32) as u32;
    let xfeatures = word(8);
    let size = word(16) as u32 as usize;
    let consistent = magic1 == FP_XSTATE_MAGIC1
        && (fpu::EXTENDED_OFFSET..=fpu::AREA_SIZE).contains(&size)
        && size.is_multiple_of(8)
        && u64::from(extended) == size as u64 + MAGIC2_SIZE
        && xfeatures & !fpu::xfeatures() == 0
        && ptr.is_multiple_of(64);
    if !consistent {
        return None;
    }
    let magic2 = crate::user_ptr::try_read::<u32>(ptr + size as u64).ok()?;
    (magic2 == FP_XSTATE_MAGIC2).then_some(size)
}

/// Read the FP state of the frame at `ptr` (the frame's `fpstate`), or `None`
/// for a pointer that cannot be one (misaligned, outside user space) or
/// memory that cannot be read. The image is not yet trusted: pass it to
/// [`fpu::load`].
pub(super) fn read(ptr: u64) -> Option<FpuImage> {
    let legacy = fpu::LEGACY_SIZE as u64;
    if !ptr.is_multiple_of(16)
        || ptr < MIN_FRAME_ADDR
        || ptr.checked_add(legacy + MAGIC2_SIZE)? > USER_MAX
    {
        return None;
    }
    let mut image = FpuImage::ZERO;
    for index in 0..fpu::LEGACY_SIZE / 8 {
        image.set_word(index, read_word(ptr + 8 * index as u64)?);
    }
    // Whatever the legacy region says, the extended components start out in
    // their initial state unless a consistent `XSAVE` area follows.
    image.set_xstate_bv(fpu::XFEATURE_X87 | fpu::XFEATURE_SSE);
    if !xsave() {
        return Some(image);
    }
    let Some(size) = claimed_size(ptr, &image) else {
        return Some(image);
    };
    if ptr.checked_add(size as u64 + MAGIC2_SIZE)? > USER_MAX {
        return None;
    }
    for index in fpu::LEGACY_SIZE / 8..size / 8 {
        image.set_word(index, read_word(ptr + 8 * index as u64)?);
    }
    // Components whose data the area is too short to hold revert to initial.
    let mut features = image.xstate_bv();
    if size < fpu::AREA_SIZE {
        features &= !fpu::XFEATURE_AVX;
    }
    image.set_xstate_bv(features);
    Some(image)
}
