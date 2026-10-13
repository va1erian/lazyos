//! The assembly of the `avx` fixture: register patterns, setting and
//! checking the upper YMM halves, the spin loop (issue #822).

use std::sync::atomic::AtomicU32;

/// Set by the main thread to stop the spinners.
pub static STOP: AtomicU32 = AtomicU32::new(0);

/// Eight 256-bit register values.
#[repr(C, align(32))]
pub struct Block(pub [u8; 256]);

/// Eight 128-bit values: the upper halves of YMM0-YMM7.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct Hi(pub [u8; 128]);

pub fn block(seed: u8) -> Block {
    let mut block = Block([0; 256]);
    for (index, byte) in block.0.iter_mut().enumerate() {
        *byte = seed.wrapping_mul(37).wrapping_add(index as u8 ^ 0xa5) | 1;
    }
    block
}

pub fn hi(seed: u8) -> Hi {
    let mut hi = Hi([0; 128]);
    for (index, byte) in hi.0.iter_mut().enumerate() {
        *byte = seed.wrapping_mul(53).wrapping_add(index as u8 ^ 0x3c) | 1;
    }
    hi
}

/// Put `hi` into the upper halves of YMM0-YMM7.
pub fn set_ymm_hi(hi: &Hi) {
    // SAFETY: reads 128 bytes from `hi`; only YMM0-YMM7 are written, and they
    // are declared clobbered.
    unsafe {
        core::arch::asm!(
            "vinsertf128 ymm0, ymm0, xmmword ptr [{p}], 1",
            "vinsertf128 ymm1, ymm1, xmmword ptr [{p} + 16], 1",
            "vinsertf128 ymm2, ymm2, xmmword ptr [{p} + 32], 1",
            "vinsertf128 ymm3, ymm3, xmmword ptr [{p} + 48], 1",
            "vinsertf128 ymm4, ymm4, xmmword ptr [{p} + 64], 1",
            "vinsertf128 ymm5, ymm5, xmmword ptr [{p} + 80], 1",
            "vinsertf128 ymm6, ymm6, xmmword ptr [{p} + 96], 1",
            "vinsertf128 ymm7, ymm7, xmmword ptr [{p} + 112], 1",
            p = in(reg) hi,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            options(nostack)
        );
    }
}

/// 0 when the upper halves of YMM0-YMM7 equal `hi`, else the 1-based register.
pub fn check_ymm_hi(hi: &Hi) -> u64 {
    let status: u64;
    // SAFETY: reads 128 bytes from `hi`; only YMM8 and `rdx` are written.
    unsafe {
        core::arch::asm!(
            "mov edx, 1", "vextractf128 xmm8, ymm0, 1",
            "vpxor xmm8, xmm8, xmmword ptr [{p}]", "vptest xmm8, xmm8", "jnz 3f",
            "mov edx, 2", "vextractf128 xmm8, ymm1, 1",
            "vpxor xmm8, xmm8, xmmword ptr [{p} + 16]", "vptest xmm8, xmm8", "jnz 3f",
            "mov edx, 3", "vextractf128 xmm8, ymm2, 1",
            "vpxor xmm8, xmm8, xmmword ptr [{p} + 32]", "vptest xmm8, xmm8", "jnz 3f",
            "mov edx, 4", "vextractf128 xmm8, ymm3, 1",
            "vpxor xmm8, xmm8, xmmword ptr [{p} + 48]", "vptest xmm8, xmm8", "jnz 3f",
            "mov edx, 5", "vextractf128 xmm8, ymm4, 1",
            "vpxor xmm8, xmm8, xmmword ptr [{p} + 64]", "vptest xmm8, xmm8", "jnz 3f",
            "mov edx, 6", "vextractf128 xmm8, ymm5, 1",
            "vpxor xmm8, xmm8, xmmword ptr [{p} + 80]", "vptest xmm8, xmm8", "jnz 3f",
            "mov edx, 7", "vextractf128 xmm8, ymm6, 1",
            "vpxor xmm8, xmm8, xmmword ptr [{p} + 96]", "vptest xmm8, xmm8", "jnz 3f",
            "mov edx, 8", "vextractf128 xmm8, ymm7, 1",
            "vpxor xmm8, xmm8, xmmword ptr [{p} + 112]", "vptest xmm8, xmm8", "jnz 3f",
            "xor edx, edx",
            "3:",
            p = in(reg) hi,
            out("rdx") status,
            out("xmm8") _,
            options(nostack)
        );
    }
    status
}

/// Fill YMM0-YMM15 with ones (`vcmpps` with the always-true predicate).
pub fn dirty_all_ymm() {
    // SAFETY: only vector registers are written, all declared clobbered.
    unsafe {
        core::arch::asm!(
            "vcmpps ymm0, ymm0, ymm0, 15", "vcmpps ymm1, ymm1, ymm1, 15",
            "vcmpps ymm2, ymm2, ymm2, 15", "vcmpps ymm3, ymm3, ymm3, 15",
            "vcmpps ymm4, ymm4, ymm4, 15", "vcmpps ymm5, ymm5, ymm5, 15",
            "vcmpps ymm6, ymm6, ymm6, 15", "vcmpps ymm7, ymm7, ymm7, 15",
            "vcmpps ymm8, ymm8, ymm8, 15", "vcmpps ymm9, ymm9, ymm9, 15",
            "vcmpps ymm10, ymm10, ymm10, 15", "vcmpps ymm11, ymm11, ymm11, 15",
            "vcmpps ymm12, ymm12, ymm12, 15", "vcmpps ymm13, ymm13, ymm13, 15",
            "vcmpps ymm14, ymm14, ymm14, 15", "vcmpps ymm15, ymm15, ymm15, 15",
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
            options(nostack)
        );
    }
}

/// Hold `block` in YMM0-YMM7 until `STOP`, checking all 256 bits on every
/// pass. Returns 0, or the 1-based register that changed.
pub fn spin(block: &Block) -> u64 {
    let status: u64;
    // SAFETY: reads 256 bytes from `block` and the `STOP` word; touches only
    // YMM0-YMM8, `eax`, `edx`, and calls nothing.
    unsafe {
        core::arch::asm!(
            "vmovups ymm0, [rsi]", "vmovups ymm1, [rsi + 32]", "vmovups ymm2, [rsi + 64]",
            "vmovups ymm3, [rsi + 96]", "vmovups ymm4, [rsi + 128]",
            "vmovups ymm5, [rsi + 160]", "vmovups ymm6, [rsi + 192]",
            "vmovups ymm7, [rsi + 224]",
            "2:",
            "mov edx, 1", "vmovups ymm8, [rsi]", "vxorps ymm8, ymm8, ymm0", "vptest ymm8, ymm8", "jnz 3f",
            "mov edx, 2", "vmovups ymm8, [rsi + 32]", "vxorps ymm8, ymm8, ymm1", "vptest ymm8, ymm8", "jnz 3f",
            "mov edx, 3", "vmovups ymm8, [rsi + 64]", "vxorps ymm8, ymm8, ymm2", "vptest ymm8, ymm8", "jnz 3f",
            "mov edx, 4", "vmovups ymm8, [rsi + 96]", "vxorps ymm8, ymm8, ymm3", "vptest ymm8, ymm8", "jnz 3f",
            "mov edx, 5", "vmovups ymm8, [rsi + 128]", "vxorps ymm8, ymm8, ymm4", "vptest ymm8, ymm8", "jnz 3f",
            "mov edx, 6", "vmovups ymm8, [rsi + 160]", "vxorps ymm8, ymm8, ymm5", "vptest ymm8, ymm8", "jnz 3f",
            "mov edx, 7", "vmovups ymm8, [rsi + 192]", "vxorps ymm8, ymm8, ymm6", "vptest ymm8, ymm8", "jnz 3f",
            "mov edx, 8", "vmovups ymm8, [rsi + 224]", "vxorps ymm8, ymm8, ymm7", "vptest ymm8, ymm8", "jnz 3f",
            "xor edx, edx",
            "cmp dword ptr [rdi], 0",
            "je 2b",
            "3:",
            in("rsi") block,
            in("rdi") STOP.as_ptr(),
            out("rdx") status,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _,
            options(nostack)
        );
    }
    status
}
