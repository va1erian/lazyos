//! CPU feature setup: the FPU, SSE and AVX state.
//!
//! On x86_64 `movups`/`movsd` and friends raise `#UD` unless the OS clears
//! `CR0.EM` and sets `CR4.OSFXSR`; the bootloader does not do this for us.
//!
//! AVX needs more (issue #822). `CPUID` reports the AVX bit whatever the OS
//! did, but a VEX-encoded instruction raises `#UD` until `CR4.OSXSAVE` is set
//! and `XCR0` enables the SSE and AVX state components, and the OS must then
//! save and restore that state with `XSAVE`/`XRSTOR` ([`crate::task::fpu`]).
//! [`init`] turns it on when the CPU has `XSAVE` and `AVX` and the area has
//! the standard layout the per-task buffers are sized for; otherwise the
//! kernel stays on `FXSAVE` and programs see no `OSXSAVE`, as before.
//! AVX-512 and the other components stay off: nothing saves their state.

use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::xcontrol::{XCr0, XCr0Flags};

/// `CPUID.1:ECX` bit 26: the `XSAVE` instruction family.
const CPUID_XSAVE: u32 = 1 << 26;
/// `CPUID.1:ECX` bit 28: AVX.
const CPUID_AVX: u32 = 1 << 28;

/// Bytes of the `XSAVE` area for x87 + SSE + AVX in the standard format:
/// 512 legacy, 64 header, 256 for `YMM_Hi128` at offset 576.
pub const XSAVE_AREA_SIZE: usize = 832;
/// Where the `YMM_Hi128` component sits in the standard-format area.
const AVX_OFFSET: u32 = 576;
/// The size of the `YMM_Hi128` component.
const AVX_SIZE: u32 = 256;

/// The `XCR0` value the kernel runs with (x87, SSE, AVX), or 0 when `XSAVE`
/// is not in use and the context switch uses `FXSAVE`. Set once by [`init`].
static XSAVE_MASK: AtomicU64 = AtomicU64::new(0);
/// The bits of `MXCSR` this CPU lets software set (`MXCSR_MASK`).
static MXCSR_MASK: AtomicU32 = AtomicU32::new(0xffbf);

/// The enabled `XCR0` state components, or 0 when the kernel uses `FXSAVE`.
pub fn xsave_mask() -> u64 {
    XSAVE_MASK.load(Ordering::Relaxed)
}

/// The `MXCSR` bits that may be set; `FXRSTOR`/`XRSTOR` raise `#GP` on others.
pub fn mxcsr_mask() -> u32 {
    MXCSR_MASK.load(Ordering::Relaxed)
}

/// Test hook: switch the context-switch path between `XSAVE` (`mask` as
/// returned by an earlier call) and `FXSAVE` (0). The caller keeps interrupts
/// off and restores the old value before they come back on.
#[cfg(lazyos_tests)]
pub fn swap_xsave_mask_for_test(mask: u64) -> u64 {
    XSAVE_MASK.swap(mask, Ordering::Relaxed)
}

/// Enable the x87 FPU and SSE/SSE2 for kernel and user code, and AVX state
/// when the CPU has it.
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
    MXCSR_MASK.store(read_mxcsr_mask(), Ordering::Relaxed);
    if !enable_avx() {
        disable_osxsave();
    }
}

/// Firmware may leave `CR4.OSXSAVE` (and an `XCR0` with AVX or AVX-512 bits)
/// set. When the kernel does not save that state, user code must not be told
/// it can use it: with `OSXSAVE` clear, `CPUID` reports it clear and VEX
/// instructions raise `#UD`, as before.
fn disable_osxsave() {
    if Cr4::read().contains(Cr4Flags::OSXSAVE) {
        // SAFETY: only clears the documented feature bit; no XSAVE-family
        // instruction runs before `enable_avx` sets it again or the kernel
        // keeps using `FXSAVE`.
        unsafe { Cr4::update(|cr4| cr4.remove(Cr4Flags::OSXSAVE)) };
    }
}

/// The `MXCSR_MASK` field of an `FXSAVE` image (0 means the architectural
/// default, bits 6 and 16..31 reserved).
fn read_mxcsr_mask() -> u32 {
    #[repr(C, align(16))]
    struct Image([u8; 512]);
    let mut image = Image([0; 512]);
    // SAFETY: `image` is 512 bytes and 16-byte aligned, what `fxsave64`
    // writes.
    unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) &mut image, options(nostack)) };
    let mask = u32::from_le_bytes([image.0[28], image.0[29], image.0[30], image.0[31]]);
    if mask == 0 {
        0xffbf
    } else {
        mask
    }
}

/// Set `CR4.OSXSAVE` and `XCR0 = x87 | SSE | AVX` if the CPU supports them
/// and the `XSAVE` area has the layout [`XSAVE_AREA_SIZE`] assumes. Returns
/// whether the kernel now saves AVX state.
fn enable_avx() -> bool {
    let leaf1 = __cpuid(1);
    if leaf1.ecx & CPUID_XSAVE == 0 || leaf1.ecx & CPUID_AVX == 0 || __cpuid(0).eax < 0xd {
        return false;
    }
    let want = XCr0Flags::X87 | XCr0Flags::SSE | XCr0Flags::AVX;
    let supported = __cpuid_count(0xd, 0).eax as u64;
    let avx = __cpuid_count(0xd, 2);
    if supported & want.bits() != want.bits() || avx.ebx != AVX_OFFSET || avx.eax != AVX_SIZE {
        return false;
    }
    // SAFETY: `XSAVE` is present, so `CR4.OSXSAVE` exists, and `XCR0` takes
    // only components `CPUID.(EAX=0DH,ECX=0)` lists as supported, with x87
    // set and SSE set whenever AVX is.
    unsafe {
        Cr4::update(|cr4| cr4.insert(Cr4Flags::OSXSAVE));
        // `write_raw`: `write` keeps the bits it does not know (AMX, CET, ...),
        // which firmware may have set and nothing here saves.
        XCr0::write_raw(want.bits());
    }
    // The size the CPU reports for exactly the enabled components.
    let size = __cpuid_count(0xd, 0).ebx as usize;
    if size > XSAVE_AREA_SIZE {
        // Cannot happen with a standard layout; back out rather than overrun
        // the per-task areas.
        // SAFETY: as above, with the components cut back to the SSE default.
        unsafe { XCr0::write_raw((XCr0Flags::X87 | XCr0Flags::SSE).bits()) };
        return false;
    }
    XSAVE_MASK.store(want.bits(), Ordering::Relaxed);
    true
}
