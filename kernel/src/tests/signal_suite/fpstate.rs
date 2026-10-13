//! x87/SSE state in the Linux signal frame (issue #809): the frame carries the
//! interrupted registers in `uc_mcontext.fpstate`, `rt_sigreturn` puts them
//! back, and a forged image cannot fault the kernel. The kernel is
//! soft-float, so the tests write XMM0 and MXCSR directly.

use super::*;
use crate::task::fpu;
use crate::task::signal::harden;

const MXCSR_DEFAULT: u32 = 0x1f80;
const MXCSR_ROUND_DOWN: u32 = 0x3f80;
/// Offsets inside the FXSAVE image.
const MXCSR_OFFSET: u64 = 24;
const XMM0_OFFSET: u64 = 160;

fn set_live(xmm0: u64, mxcsr: u32) {
    // SAFETY: writes XMM0 and MXCSR only; the kernel is soft-float.
    unsafe {
        core::arch::asm!("movq xmm0, {}", in(reg) xmm0, options(nostack));
        core::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
    }
}

fn live() -> (u64, u32) {
    let xmm0: u64;
    let mut mxcsr: u32 = 0;
    // SAFETY: reads XMM0 and stores MXCSR into a local.
    unsafe {
        core::arch::asm!("movq {}, xmm0", out(reg) xmm0, options(nostack));
        core::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack));
    }
    (xmm0, mxcsr)
}

pub(super) fn read_u64(addr: u64) -> u64 {
    // SAFETY: the caller passes an address inside the test's `UserStack`.
    unsafe { core::ptr::read_volatile(addr as *const u64) }
}

pub(super) fn write_u64(addr: u64, value: u64) {
    // SAFETY: as `read_u64`.
    unsafe { core::ptr::write_volatile(addr as *mut u64, value) };
}

/// A frame built from `top`; the interrupted FP state is whatever is live.
pub(super) fn build(top: u64) -> Result<signal::FrameResult, String> {
    let regs = signal::UserRegs {
        rip: 0x0040_1000,
        rsp: top - 0x80,
        rflags: 0x202,
        ..signal::UserRegs::default()
    };
    let info = SigInfo::user(0, signal::SI_USER);
    signal::build_linux_frame(
        top,
        &regs,
        signal::SIGUSR1,
        0x0040_2000,
        0,
        0x0040_3000,
        0,
        0,
        &info,
    )
    .ok_or_else(|| String::from("frame does not fit the stack"))
}

pub(super) fn fpstate_slot(frame: &signal::FrameResult) -> u64 {
    frame.rsp + signal::lf::MCONTEXT + signal::lf::FPSTATE
}

/// Put the kernel task's registers back to the defaults after a test.
fn settle() {
    set_live(0, MXCSR_DEFAULT);
}

/// The frame holds the interrupted XMM0 and MXCSR in a 64-byte aligned image
/// above the `rt_sigframe` and below the red zone, and the restore puts them
/// back over whatever the handler left.
pub fn fpstate_frame_roundtrip() -> Result<(), String> {
    fresh()?;
    let stack = UserStack::new()?;
    let top = stack.top();
    set_live(0x1122_3344_5566_7788, MXCSR_ROUND_DOWN);
    let frame = build(top)?;
    let fp = read_u64(fpstate_slot(&frame));
    check!(fp % 64 == 0, "fpstate {fp:#x} is not 64-byte aligned");
    check!(
        fp >= frame.rsp + 512 - 8 && fp + fpu::state_size() as u64 <= top - 128,
        "fpstate {fp:#x} overlaps the frame {:#x} or the red zone below {top:#x}",
        frame.rsp
    );
    check!(
        read_u64(fp + XMM0_OFFSET) == 0x1122_3344_5566_7788,
        "frame XMM0 is {:#x}",
        read_u64(fp + XMM0_OFFSET)
    );
    check!(
        read_u64(fp + MXCSR_OFFSET) & 0xffff_ffff == u64::from(MXCSR_ROUND_DOWN),
        "frame MXCSR is {:#x}",
        read_u64(fp + MXCSR_OFFSET)
    );

    // The handler clobbers the registers; sigreturn brings them back.
    set_live(0xdead_beef_dead_beef, MXCSR_DEFAULT);
    check!(
        harden::restore_fpstate(frame.rsp + 8).is_some(),
        "a genuine frame was refused"
    );
    let got = live();
    settle();
    check!(
        got == (0x1122_3344_5566_7788, MXCSR_ROUND_DOWN),
        "restored {got:x?}"
    );
    Ok(())
}

/// A null `fpstate` resets to the power-on state, as Linux does.
pub fn fpstate_null_resets() -> Result<(), String> {
    fresh()?;
    let stack = UserStack::new()?;
    let frame = build(stack.top())?;
    write_u64(fpstate_slot(&frame), 0);
    set_live(0xabcd, MXCSR_ROUND_DOWN);
    check!(
        harden::restore_fpstate(frame.rsp + 8).is_some(),
        "null fpstate refused"
    );
    let got = live();
    settle();
    check!(got == (0, MXCSR_DEFAULT), "null fpstate left {got:x?}");
    Ok(())
}

/// A forged image never reaches `fxrstor` unsanitised: a misaligned pointer
/// and ones outside user space are refused, and reserved MXCSR bits are
/// masked rather than raising `#GP` in ring 0.
pub fn fpstate_forged_images() -> Result<(), String> {
    fresh()?;
    let stack = UserStack::new()?;
    let frame = build(stack.top())?;
    let slot = fpstate_slot(&frame);
    let genuine = read_u64(slot);
    for bad in [genuine + 8, 0x10, harden::USER_MAX - 8, harden::USER_MAX] {
        write_u64(slot, bad);
        check!(
            harden::restore_fpstate(frame.rsp + 8).is_none(),
            "forged fpstate {bad:#x} accepted"
        );
    }
    write_u64(slot, genuine);
    // Every MXCSR bit set, including all the reserved ones.
    write_u64(genuine + MXCSR_OFFSET, 0xffff_ffff);
    set_live(0, MXCSR_DEFAULT);
    check!(
        harden::restore_fpstate(frame.rsp + 8).is_some(),
        "an all-ones MXCSR was refused"
    );
    let (_, mxcsr) = live();
    settle();
    // The kernel masks with the CPU's own `MXCSR_MASK`, which is 0xffff on
    // Intel and 0x2ffff on AMD (bit 17, the misaligned-exception mask): the
    // valid set is the CPU's to define, so assert against it, not a constant.
    let mask = crate::arch::cpu::mxcsr_mask();
    check!(
        mask & 0xffff_0000 & !(1 << 17) == 0,
        "unexpected MXCSR_MASK {mask:#x}"
    );
    check!(
        mxcsr & !mask == 0,
        "reserved MXCSR bits survived: {mxcsr:#x} (mask {mask:#x})"
    );
    Ok(())
}

/// A task that is not the one on the CPU (the timer sweep delivers to
/// sibling threads) is read and written through its saved area.
pub fn fpstate_other_slot_uses_saved_area() -> Result<(), String> {
    fresh()?;
    let other = task::MAX_TASKS - 3;
    check!(other != task::current(), "slot collision");
    let mut image = fpu::capture(other);
    image.set_word(XMM0_OFFSET as usize / 8, 0x7777);
    fpu::load(other, &image);
    check!(
        fpu::saved_xmm0(other) == 0x7777,
        "load did not reach the saved area"
    );
    check!(
        fpu::capture(other).word(XMM0_OFFSET as usize / 8) == 0x7777,
        "capture did not read the saved area"
    );
    fpu::reset_for(other);
    check!(
        fpu::saved_xmm0(other) == 0 && fpu::saved_mxcsr(other) == MXCSR_DEFAULT,
        "reset_for left {:#x}/{:#x}",
        fpu::saved_xmm0(other),
        fpu::saved_mxcsr(other)
    );
    Ok(())
}

/// Soak: thousands of frames at every stack offset, each carrying a distinct
/// pattern that survives a clobber and comes back exact.
pub fn fpstate_soak() -> Result<(), String> {
    fresh()?;
    let stack = UserStack::new()?;
    let top = stack.top();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for round in 0..4096u32 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let mxcsr = MXCSR_DEFAULT | (((seed >> 60) as u32) & 3) << 13;
        set_live(seed, mxcsr);
        let frame = build(top - (u64::from(round) & 0x3f) * 8)?;
        set_live(!seed, MXCSR_DEFAULT);
        check!(
            harden::restore_fpstate(frame.rsp + 8).is_some(),
            "round {round}: genuine frame refused"
        );
        let got = live();
        if got != (seed, mxcsr) {
            settle();
            return Err(format!(
                "round {round}: restored {got:x?}, expected {:x?}",
                (seed, mxcsr)
            ));
        }
    }
    settle();
    Ok(())
}
