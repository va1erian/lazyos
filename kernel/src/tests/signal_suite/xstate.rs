//! AVX state in the Linux signal frame (issue #822): `uc_mcontext.fpstate`
//! points at the whole `XSAVE` area with Linux's markers
//! (`signal::FP_XSTATE_MAGIC1` in the software bytes, `signal::FP_XSTATE_MAGIC2` after the
//! area), `rt_sigreturn` puts the YMM halves back, and a forged area cannot
//! fault the kernel or smuggle components `XCR0` has off. The kernel is
//! soft-float, so the tests write YMM0 directly.

use super::fpstate::{build, fpstate_slot, read_u64, write_u64};
use super::*;
use crate::arch::cpu;
use crate::task::fpu;
use crate::task::signal::harden;

const MXCSR_DEFAULT: u32 = 0x1f80;
/// Offsets inside the frame's `XSAVE` image.
const XMM0_OFFSET: u64 = 160;
const SW_BYTES: u64 = 464;
const HEADER: u64 = 512;
const YMM_HI: u64 = 576;

fn avx_on() -> bool {
    cpu::xsave_mask() & fpu::XFEATURE_AVX != 0
}

fn set_ymm0(value: &[u64; 4]) {
    // SAFETY: loads YMM0 from 32 readable bytes; the kernel is soft-float and
    // `settle` puts the registers back.
    unsafe { core::arch::asm!("vmovdqu ymm0, [{}]", in(reg) value.as_ptr(), options(nostack)) };
}

fn ymm0() -> [u64; 4] {
    let mut value = [0u64; 4];
    // SAFETY: stores YMM0 into 32 writable bytes.
    unsafe { core::arch::asm!("vmovdqu [{}], ymm0", in(reg) value.as_mut_ptr(), options(nostack)) };
    value
}

/// Put the kernel task's registers back to the power-on state.
fn settle() {
    let mxcsr = MXCSR_DEFAULT;
    // SAFETY: `vzeroall` has no operands; `ldmxcsr` loads a valid MXCSR.
    unsafe {
        if avx_on() {
            core::arch::asm!("vzeroall", options(nostack, nomem));
        }
        core::arch::asm!("xorps xmm0, xmm0", options(nostack, nomem));
        core::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
    }
}

fn read_u32(addr: u64) -> u32 {
    // SAFETY: the caller passes an address inside the test's `UserStack`.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn write_u32(addr: u64, value: u32) {
    // SAFETY: as `read_u32`.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) };
}

fn pattern(seed: u64) -> [u64; 4] {
    let base = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    [base, base.rotate_left(13), base.rotate_left(29), !base]
}

/// The frame's x87/SSE/AVX image carries Linux's markers and the interrupted
/// XMM0 and upper YMM0.
pub fn xstate_frame_layout() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    fresh()?;
    let stack = UserStack::new()?;
    let top = stack.top();
    let p = pattern(11);
    set_ymm0(&p);
    let frame = build(top);
    settle();
    let frame = frame?;
    let fp = read_u64(fpstate_slot(&frame));
    check!(fp % 64 == 0, "fpstate {fp:#x} is not 64-byte aligned");
    check!(
        fp + fpu::AREA_SIZE as u64 + 4 <= top - 128 && fp >= frame.rsp + 512 - 8,
        "fpstate {fp:#x} overlaps the frame {:#x} or the red zone below {top:#x}",
        frame.rsp
    );
    check!(
        read_u32(fp + SW_BYTES) == signal::FP_XSTATE_MAGIC1,
        "magic1 is {:#x}",
        read_u32(fp + SW_BYTES)
    );
    check!(
        read_u32(fp + SW_BYTES + 4) == fpu::AREA_SIZE as u32 + 4,
        "extended_size is {}",
        read_u32(fp + SW_BYTES + 4)
    );
    check!(
        read_u64(fp + SW_BYTES + 8) == 7,
        "xfeatures is {:#x}",
        read_u64(fp + SW_BYTES + 8)
    );
    check!(
        read_u32(fp + SW_BYTES + 16) == fpu::AREA_SIZE as u32,
        "xstate_size is {}",
        read_u32(fp + SW_BYTES + 16)
    );
    check!(
        read_u32(fp + fpu::AREA_SIZE as u64) == signal::FP_XSTATE_MAGIC2,
        "magic2 is {:#x}",
        read_u32(fp + fpu::AREA_SIZE as u64)
    );
    check!(
        read_u64(fp + HEADER) & fpu::XFEATURE_AVX != 0,
        "XSTATE_BV {:#x} does not mark AVX in use",
        read_u64(fp + HEADER)
    );
    let saved = [
        read_u64(fp + XMM0_OFFSET),
        read_u64(fp + XMM0_OFFSET + 8),
        read_u64(fp + YMM_HI),
        read_u64(fp + YMM_HI + 8),
    ];
    check!(saved == p, "frame holds YMM0 {saved:x?}, expected {p:x?}");
    Ok(())
}

/// `rt_sigreturn` puts the YMM halves back over whatever the handler left,
/// and a handler's edit of the saved image is what comes back.
pub fn xstate_roundtrip_and_edit() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    fresh()?;
    let stack = UserStack::new()?;
    let p = pattern(12);
    set_ymm0(&p);
    let frame = build(stack.top());
    set_ymm0(&pattern(13));
    let frame = frame?;
    let ok = harden::restore_fpstate(frame.rsp + 8).is_some();
    let got = ymm0();
    check!(ok, "a genuine frame was refused");
    check!(got == p, "restored {got:x?}, expected {p:x?}");

    // The handler changes the interrupted YMM0 (an unwinder, a debugger).
    let fp = read_u64(fpstate_slot(&frame));
    let edited = pattern(14);
    write_u64(fp + YMM_HI, edited[2]);
    write_u64(fp + YMM_HI + 8, edited[3]);
    set_ymm0(&pattern(15));
    let ok = harden::restore_fpstate(frame.rsp + 8).is_some();
    let got = ymm0();
    settle();
    check!(ok, "an edited frame was refused");
    check!(
        got[..2] == p[..2] && got[2..] == edited[2..],
        "restored {got:x?}, expected low {:x?} and edited high {:x?}",
        &p[..2],
        &edited[2..]
    );
    Ok(())
}

/// Forged areas: a header naming components `XCR0` has off, the compaction
/// bit and reserved words, a lying size, a missing trailing magic, and a
/// forged `MXCSR_MASK` never fault the kernel (`#GP` in ring 0 would be a
/// panic), and a frame whose markers do not add up restores the legacy
/// registers with the AVX state reset.
pub fn xstate_forged_areas() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    fresh()?;
    let stack = UserStack::new()?;
    let p = pattern(16);
    set_ymm0(&p);
    let frame = build(stack.top());
    settle();
    let frame = frame?;
    let fp = read_u64(fpstate_slot(&frame));
    let area = fpu::AREA_SIZE as u64;
    let rsp = frame.rsp + 8;

    // Restore the frame and report the registers it left, or why not.
    let restore = |what: &str| -> Result<[u64; 4], String> {
        set_ymm0(&pattern(99));
        let ok = harden::restore_fpstate(rsp).is_some();
        let got = ymm0();
        settle();
        check!(ok, "{what}: the frame was refused");
        Ok(got)
    };

    // A hostile header: every component, compaction on, reserved words set.
    write_u64(fp + HEADER, !0);
    write_u64(fp + HEADER + 8, 1 << 63);
    for word in 2..8 {
        write_u64(fp + HEADER + 8 * word, !0);
    }
    let got = restore("hostile header")?;
    check!(got == p, "hostile header: sanitised restore gave {got:x?}");

    // Every MXCSR bit set and a forged MXCSR_MASK that allows them.
    write_u64(fp + 24, 0xffff_ffff_ffff_ffff);
    let got = restore("forged MXCSR_MASK")?;
    check!(got == p, "forged MXCSR: {got:x?}");

    // Each of these breaks the markers: legacy registers back, AVX reset.
    let genuine_magic2 = read_u32(fp + area);
    let size_word = read_u64(fp + SW_BYTES + 16);
    let magic1 = read_u32(fp + SW_BYTES);
    type Forge = (&'static str, fn(u64, u64), fn(u64, u64));
    let breaks: [Forge; 5] = [
        (
            "no trailing magic",
            |fp, area| write_u32(fp + area, 0),
            |fp, area| write_u32(fp + area, signal::FP_XSTATE_MAGIC2),
        ),
        (
            "no leading magic",
            |fp, _| write_u32(fp + SW_BYTES, 0),
            |fp, _| write_u32(fp + SW_BYTES, signal::FP_XSTATE_MAGIC1),
        ),
        (
            "oversized xstate_size",
            |fp, _| write_u32(fp + SW_BYTES + 16, 0xffff_ffff),
            |fp, area| write_u32(fp + SW_BYTES + 16, area as u32),
        ),
        (
            "undersized xstate_size",
            |fp, _| write_u32(fp + SW_BYTES + 16, 100),
            |fp, area| write_u32(fp + SW_BYTES + 16, area as u32),
        ),
        (
            "unknown xfeatures",
            |fp, _| write_u64(fp + SW_BYTES + 8, 0xff),
            |fp, _| write_u64(fp + SW_BYTES + 8, 7),
        ),
    ];
    for (what, forge, undo) in breaks {
        forge(fp, area);
        let got = restore(what)?;
        undo(fp, area);
        check!(
            got[..2] == p[..2] && got[2..] == [0, 0],
            "{what}: restored {got:x?}, expected the legacy half and a reset upper half"
        );
    }
    check!(
        read_u32(fp + area) == genuine_magic2
            && read_u64(fp + SW_BYTES + 16) == size_word
            && read_u32(fp + SW_BYTES) == magic1,
        "the test left the frame forged"
    );

    // A shorter, self-consistent area has no AVX part: the upper half resets.
    write_u32(fp + SW_BYTES + 4, fpu::EXTENDED_OFFSET as u32 + 4);
    write_u32(fp + SW_BYTES + 16, fpu::EXTENDED_OFFSET as u32);
    write_u32(fp + fpu::EXTENDED_OFFSET as u64, signal::FP_XSTATE_MAGIC2);
    let got = restore("short area")?;
    check!(
        got[..2] == p[..2] && got[2..] == [0, 0],
        "short area: restored {got:x?}"
    );
    Ok(())
}

/// On a CPU run with `FXSAVE` alone the frame is the 512-byte image with
/// zero software bytes (no `FP_XSTATE_MAGIC1`), and it round-trips.
pub fn xstate_fxsave_frame() -> Result<(), String> {
    fresh()?;
    let stack = UserStack::new()?;
    x86_64::instructions::interrupts::without_interrupts(|| {
        let enabled = cpu::swap_xsave_mask_for_test(0);
        let outcome = (|| {
            let xmm = [0x0123_4567_89ab_cdefu64, 0xfedc_ba98_7654_3210];
            // SAFETY: loads XMM0 from 16 readable bytes.
            unsafe {
                core::arch::asm!("movups xmm0, [{}]", in(reg) xmm.as_ptr(), options(nostack))
            };
            let frame = build(stack.top())?;
            let fp = read_u64(fpstate_slot(&frame));
            check!(
                (0..6).all(|word| read_u64(fp + SW_BYTES + 8 * word) == 0),
                "the software bytes of an FXSAVE frame are not zero: magic1 {:#x}",
                read_u32(fp + SW_BYTES)
            );
            check!(
                read_u64(fp + XMM0_OFFSET) == xmm[0],
                "frame XMM0 {:#x}",
                read_u64(fp + XMM0_OFFSET)
            );
            // SAFETY: loads XMM0 from 16 readable bytes.
            unsafe { core::arch::asm!("xorps xmm0, xmm0", options(nostack, nomem)) };
            check!(
                harden::restore_fpstate(frame.rsp + 8).is_some(),
                "a genuine FXSAVE frame was refused"
            );
            let mut got = [0u64; 2];
            // SAFETY: stores XMM0 into 16 writable bytes.
            unsafe {
                core::arch::asm!("movups [{}], xmm0", in(reg) got.as_mut_ptr(), options(nostack))
            };
            check!(got == xmm, "restored {got:x?}");
            Ok(())
        })();
        cpu::swap_xsave_mask_for_test(enabled);
        settle();
        outcome
    })
}

/// Soak: thousands of frames at every stack offset, each with its own YMM
/// pattern (every fifth with the AVX state initial) surviving a clobber.
pub fn xstate_soak() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    fresh()?;
    let stack = UserStack::new()?;
    let top = stack.top();
    for round in 0..4096u64 {
        let initial = round % 5 == 0;
        let p = if initial {
            [pattern(round)[0], pattern(round)[1], 0, 0]
        } else {
            pattern(round)
        };
        set_ymm0(&p);
        if initial {
            // Only the SSE half in use: the upper halves go back to initial.
            // SAFETY: `vzeroupper` has no operands.
            unsafe { core::arch::asm!("vzeroupper", options(nostack, nomem)) };
        }
        let frame = build(top - (round & 0x3f) * 8);
        set_ymm0(&pattern(!round));
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                settle();
                return Err(format!("round {round}: {error}"));
            }
        };
        let ok = harden::restore_fpstate(frame.rsp + 8).is_some();
        let got = ymm0();
        if !ok || got != p {
            settle();
            return Err(format!(
                "round {round}: restored {got:x?} (accepted: {ok}), expected {p:x?}"
            ));
        }
    }
    settle();
    Ok(())
}
