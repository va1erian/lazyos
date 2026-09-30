//! Per-task x87/SSE state (issue #373): the scheduler parks the outgoing
//! task's registers and loads the incoming one's, so one app's floats never
//! leak into another's. The kernel is soft-float, so these tests write the
//! XMM registers and MXCSR directly and put the kernel task's state back.

use super::*;
use crate::task::fpu;

/// MXCSR with every exception masked and the given rounding mode bits.
const MXCSR_DEFAULT: u32 = 0x1f80;
const MXCSR_ROUND_DOWN: u32 = 0x3f80;
const MXCSR_ROUND_UP: u32 = 0x5f80;

/// Two slots no test leaves occupied after `harness::reset`.
const SLOT_A: usize = task::MAX_TASKS - 1;
const SLOT_B: usize = task::MAX_TASKS - 2;

fn set_live(xmm0: u64, mxcsr: u32) {
    // SAFETY: writes XMM0 and MXCSR only; the kernel is soft-float and keeps
    // no values there, and `with_kernel_state_kept` restores both.
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

/// Run `body` on a fresh task table (only the kernel task, current and
/// runnable, so no live task's saved state is overwritten), with interrupts
/// off as every real caller is; then put the kernel task's own state back so
/// later tests start clean.
fn with_kernel_state_kept(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    x86_64::instructions::interrupts::without_interrupts(|| {
        fpu::save(task::KERNEL_TASK);
        let outcome = body();
        fpu::restore(task::KERNEL_TASK);
        outcome
    })
}

/// A reset slot holds the power-on default: MXCSR 0x1F80, zero registers.
pub fn fpu_reset_is_default() -> Result<(), String> {
    with_kernel_state_kept(|| {
        fpu::reset(SLOT_A);
        check!(
            fpu::saved_mxcsr(SLOT_A) == MXCSR_DEFAULT,
            "reset MXCSR {:#x}",
            fpu::saved_mxcsr(SLOT_A)
        );
        check!(fpu::saved_xmm0(SLOT_A) == 0, "reset XMM0 not zero");
        fpu::restore(SLOT_A);
        check!(
            live() == (0, MXCSR_DEFAULT),
            "restored default {:x?}",
            live()
        );
        Ok(())
    })
}

/// The scheduler's sequence (save outgoing, restore incoming) hands each task
/// back exactly its own XMM0 and MXCSR.
pub fn fpu_switch_keeps_each_tasks_state() -> Result<(), String> {
    with_kernel_state_kept(|| {
        set_live(0x1111_2222_3333_4444, MXCSR_ROUND_DOWN);
        fpu::save(SLOT_A);
        // Task B runs and changes everything.
        set_live(0xaaaa_bbbb_cccc_dddd, MXCSR_ROUND_UP);
        fpu::save(SLOT_B);
        fpu::restore(SLOT_A);
        check!(
            live() == (0x1111_2222_3333_4444, MXCSR_ROUND_DOWN),
            "A resumed with {:x?}",
            live()
        );
        fpu::restore(SLOT_B);
        check!(
            live() == (0xaaaa_bbbb_cccc_dddd, MXCSR_ROUND_UP),
            "B resumed with {:x?}",
            live()
        );
        Ok(())
    })
}

/// A fork/thread inherits the live registers; `execve` resets them.
pub fn fpu_inherit_then_exec_reset() -> Result<(), String> {
    with_kernel_state_kept(|| {
        set_live(0x0123_4567_89ab_cdef, MXCSR_ROUND_UP);
        fpu::inherit_live(SLOT_A);
        check!(
            fpu::saved_xmm0(SLOT_A) == 0x0123_4567_89ab_cdef
                && fpu::saved_mxcsr(SLOT_A) == MXCSR_ROUND_UP,
            "child did not inherit the parent's registers"
        );
        fpu::reset_live(SLOT_A);
        check!(live() == (0, MXCSR_DEFAULT), "exec left {:x?}", live());
        Ok(())
    })
}

/// Real entries through the voluntary scheduler gate resume the caller with
/// its registers intact.
pub fn fpu_survives_real_yields() -> Result<(), String> {
    with_kernel_state_kept(|| {
        set_live(0xfeed_face_cafe_beef, MXCSR_ROUND_DOWN);
        for round in 0..5_000 {
            task::switch::yield_now();
            check!(
                live() == (0xfeed_face_cafe_beef, MXCSR_ROUND_DOWN),
                "yield {round} resumed with {:x?}",
                live()
            );
        }
        Ok(())
    })
}

/// Soak: many switch rounds across every slot, each with its own pattern,
/// never cross-contaminate.
pub fn fpu_switch_soak_all_slots() -> Result<(), String> {
    const MODES: [u32; 3] = [MXCSR_DEFAULT, MXCSR_ROUND_DOWN, MXCSR_ROUND_UP];
    let pattern = |slot: usize, round: u64| {
        (slot as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ round.rotate_left(17)
    };
    with_kernel_state_kept(|| {
        for round in 0..2_000u64 {
            for slot in 1..task::MAX_TASKS {
                set_live(pattern(slot, round), MODES[(slot + round as usize) % 3]);
                fpu::save(slot);
            }
            for slot in (1..task::MAX_TASKS).rev() {
                fpu::restore(slot);
                let expected = (pattern(slot, round), MODES[(slot + round as usize) % 3]);
                check!(
                    live() == expected,
                    "round {round} slot {slot}: {:x?} != {expected:x?}",
                    live()
                );
            }
        }
        for slot in 1..task::MAX_TASKS {
            fpu::reset(slot);
        }
        Ok(())
    })
}
