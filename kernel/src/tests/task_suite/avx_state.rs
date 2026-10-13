//! AVX state (issue #822): the kernel enables `CR4.OSXSAVE` and `XCR0`, VEX
//! instructions run, and the context switch keeps the upper halves of the YMM
//! registers per task. The kernel is soft-float, so the tests write YMM0
//! directly and `with_kernel_state_kept` puts the kernel task's state back.
//!
//! On a CPU without `XSAVE` and `AVX` (QEMU's default `qemu64` model) the
//! kernel stays on `FXSAVE` and the AVX cases pass vacuously; the suite
//! runner boots with `-cpu max` so they run.

use super::fpu_state::{spawn_peer_running, with_kernel_state_kept};
use super::*;
use crate::arch::cpu;
use crate::task::fpu;

const MXCSR_DEFAULT: u32 = 0x1f80;
const MXCSR_ROUND_DOWN: u32 = 0x3f80;

/// Two slots no test leaves occupied after `harness::reset`.
const SLOT_A: usize = task::MAX_TASKS - 1;
const SLOT_B: usize = task::MAX_TASKS - 2;

/// Image words of XMM0's two halves and of its upper YMM half.
const XMM0_LOW_WORD: usize = 160 / 8;
const YMM0_HI_WORD: usize = 576 / 8;

/// Whether the kernel runs with AVX state on.
fn avx_on() -> bool {
    cpu::xsave_mask() & fpu::XFEATURE_AVX != 0
}

fn set_ymm0(value: &[u64; 4]) {
    // SAFETY: loads YMM0 from 32 readable bytes; the kernel is soft-float and
    // `with_kernel_state_kept` restores the kernel task's registers.
    unsafe { core::arch::asm!("vmovdqu ymm0, [{}]", in(reg) value.as_ptr(), options(nostack)) };
}

fn ymm0() -> [u64; 4] {
    let mut value = [0u64; 4];
    // SAFETY: stores YMM0 into 32 writable bytes.
    unsafe { core::arch::asm!("vmovdqu [{}], ymm0", in(reg) value.as_mut_ptr(), options(nostack)) };
    value
}

fn set_xmm0(value: &[u64; 2]) {
    // SAFETY: loads XMM0 from 16 readable bytes (legacy SSE, no AVX needed).
    unsafe { core::arch::asm!("movups xmm0, [{}]", in(reg) value.as_ptr(), options(nostack)) };
}

fn xmm0() -> [u64; 2] {
    let mut value = [0u64; 2];
    // SAFETY: stores XMM0 into 16 writable bytes.
    unsafe { core::arch::asm!("movups [{}], xmm0", in(reg) value.as_mut_ptr(), options(nostack)) };
    value
}

fn set_mxcsr(mxcsr: u32) {
    // SAFETY: loads a valid MXCSR (no reserved bits).
    unsafe { core::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack)) };
}

fn mxcsr() -> u32 {
    let mut value: u32 = 0;
    // SAFETY: stores MXCSR into a local.
    unsafe { core::arch::asm!("stmxcsr [{}]", in(reg) &mut value, options(nostack)) };
    value
}

/// A pattern whose four words differ per `seed`, so a mix-up of halves or
/// tasks shows.
fn pattern(seed: u64) -> [u64; 4] {
    let base = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    [base, base.rotate_left(13), base.rotate_left(29), !base]
}

fn xgetbv0() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: `xgetbv` with ECX=0 reads XCR0; it raises `#UD` only without
    // `CR4.OSXSAVE`, which the caller has checked.
    unsafe {
        core::arch::asm!("xgetbv", in("ecx") 0, out("eax") low, out("edx") high, options(nomem, nostack))
    };
    u64::from(high) << 32 | u64::from(low)
}

/// The boot enabled what the CPU can give: `OSXSAVE`, `XCR0` = x87 | SSE | AVX
/// and nothing else, visible to user code through `CPUID` and `XGETBV`.
pub fn avx_enabled_at_boot() -> Result<(), String> {
    let leaf1 = core::arch::x86_64::__cpuid(1);
    let cpu_has = leaf1.ecx & (1 << 26) != 0 && leaf1.ecx & (1 << 28) != 0;
    if !cpu_has {
        check!(
            cpu::xsave_mask() == 0,
            "XSAVE mask {:#x} on a CPU without XSAVE and AVX",
            cpu::xsave_mask()
        );
        return Ok(());
    }
    check!(
        cpu::xsave_mask() == 7,
        "the CPU has XSAVE and AVX but the kernel enabled mask {:#x}",
        cpu::xsave_mask()
    );
    check!(
        x86_64::registers::control::Cr4::read()
            .contains(x86_64::registers::control::Cr4Flags::OSXSAVE),
        "CR4.OSXSAVE is clear"
    );
    check!(
        core::arch::x86_64::__cpuid(1).ecx & (1 << 27) != 0,
        "CPUID.1:ECX.OSXSAVE is clear: user code cannot tell AVX is usable"
    );
    let xcr0 = xgetbv0();
    check!(xcr0 == 7, "XCR0 is {xcr0:#x}, expected x87|SSE|AVX = 0x7");
    check!(
        core::arch::x86_64::__cpuid_count(0xd, 0).ebx as usize <= fpu::AREA_SIZE,
        "the CPU needs a {} byte XSAVE area, the per-task buffers hold {}",
        core::arch::x86_64::__cpuid_count(0xd, 0).ebx,
        fpu::AREA_SIZE
    );
    Ok(())
}

/// VEX-encoded instructions execute: 256-bit arithmetic, a broadcast, the
/// `vmovsd [mem+0x50], xmm0` that killed the JIT shell (`c4 c1 7b 11 43 50`),
/// and `vzeroupper`.
pub fn avx_vex_instructions_execute() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    with_kernel_state_kept(|| {
        let a = [
            1.5f64.to_bits(),
            2.5f64.to_bits(),
            (-4.0f64).to_bits(),
            0.25f64.to_bits(),
        ];
        set_ymm0(&a);
        // SAFETY: AVX is enabled (checked); registers YMM0/YMM1 are the
        // kernel task's, restored by `with_kernel_state_kept`.
        unsafe { core::arch::asm!("vaddpd ymm1, ymm0, ymm0", options(nostack, nomem)) };
        let mut sum = [0u64; 4];
        // SAFETY: stores YMM1 into 32 writable bytes.
        unsafe {
            core::arch::asm!("vmovupd [{}], ymm1", in(reg) sum.as_mut_ptr(), options(nostack))
        };
        let want = [
            3.0f64.to_bits(),
            5.0f64.to_bits(),
            (-8.0f64).to_bits(),
            0.5f64.to_bits(),
        ];
        check!(sum == want, "vaddpd gave {sum:x?}, expected {want:x?}");

        let mut cell = [0u64; 2];
        // SAFETY: stores XMM0's low double at `base + 0x50`; the base is
        // moved back so the displacement lands on `cell[0]`.
        unsafe {
            core::arch::asm!(
                "vmovsd [{0} + 0x50], xmm0",
                in(reg) (cell.as_mut_ptr() as usize).wrapping_sub(0x50),
                options(nostack)
            )
        };
        check!(cell[0] == a[0], "vmovsd stored {:#x}", cell[0]);

        // SAFETY: `vzeroupper` has no operands; it zeroes the upper YMM halves.
        unsafe { core::arch::asm!("vzeroupper", options(nostack, nomem)) };
        let after = ymm0();
        check!(
            after[2] == 0 && after[3] == 0 && after[0] == a[0],
            "vzeroupper left {after:x?}"
        );
        Ok(())
    })
}

/// The scheduler's sequence (save outgoing, restore incoming) hands each task
/// back its own YMM halves, and a component in its initial state comes back
/// zero rather than the previous task's.
pub fn avx_switch_keeps_each_tasks_ymm() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    with_kernel_state_kept(|| {
        let (pa, pb) = (pattern(1), pattern(2));
        set_ymm0(&pa);
        set_mxcsr(MXCSR_ROUND_DOWN);
        fpu::save(SLOT_A);
        set_ymm0(&pb);
        set_mxcsr(MXCSR_DEFAULT);
        fpu::save(SLOT_B);
        fpu::restore(SLOT_A);
        check!(
            ymm0() == pa && mxcsr() == MXCSR_ROUND_DOWN,
            "A resumed with {:x?}",
            ymm0()
        );
        fpu::restore(SLOT_B);
        check!(ymm0() == pb, "B resumed with {:x?}", ymm0());

        // A task whose AVX state is initial: the upper halves must be zeroed
        // on restore even though the registers hold another task's values.
        // SAFETY: `vzeroall` has no operands.
        unsafe { core::arch::asm!("vzeroall", options(nostack, nomem)) };
        fpu::save(SLOT_A);
        set_ymm0(&pb);
        fpu::restore(SLOT_A);
        check!(
            ymm0() == [0; 4],
            "initial AVX state restored as {:x?}",
            ymm0()
        );
        Ok(())
    })
}

/// A fork/thread inherits the live YMM halves; `execve` zeroes them.
pub fn avx_inherit_then_exec_reset() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    with_kernel_state_kept(|| {
        let p = pattern(3);
        set_ymm0(&p);
        fpu::inherit_live(SLOT_A);
        let image = fpu::capture(SLOT_A);
        check!(
            [
                image.word(XMM0_LOW_WORD),
                image.word(XMM0_LOW_WORD + 1),
                image.word(YMM0_HI_WORD),
                image.word(YMM0_HI_WORD + 1)
            ] == p,
            "child did not inherit the parent's YMM0"
        );
        fpu::reset_live(SLOT_A);
        check!(
            ymm0() == [0; 4] && mxcsr() == MXCSR_DEFAULT,
            "exec left {:x?} / {:#x}",
            ymm0(),
            mxcsr()
        );
        Ok(())
    })
}

/// The peer's program: forever load YMM0 from its own page and bump a counter.
///
/// ```text
///  0: vmovdqu ymm0, [rip + 0x38]   c5 fe 6f 05 38 00 00 00   (-> offset 64)
///  8: inc qword [rip + 0x11]       48 ff 05 11 00 00 00      (-> offset 32)
/// 15: jmp 0                        eb ef
/// 32: dq counter
/// 64: 32 bytes of YMM0 data
/// ```
const PEER_YMM0: [u64; 4] = [
    0x005e_ed0f_face_b00c,
    0x1357_9bdf_0246_8ace,
    0xa5a5_5a5a_c3c3_3c3c,
    0xfedc_ba98_7654_3210,
];

fn peer_program() -> [u8; 96] {
    let mut code = [0u8; 96];
    code[0..8].copy_from_slice(&[0xc5, 0xfe, 0x6f, 0x05, 0x38, 0x00, 0x00, 0x00]);
    code[8..15].copy_from_slice(&[0x48, 0xff, 0x05, 0x11, 0x00, 0x00, 0x00]);
    code[15..17].copy_from_slice(&[0xeb, 0xef]);
    for (i, word) in PEER_YMM0.iter().enumerate() {
        code[64 + 8 * i..72 + 8 * i].copy_from_slice(&word.to_le_bytes());
    }
    code
}

/// A real context switch to a ring-3 task running VEX code: the kernel task
/// parks, the scheduler runs the peer until a tick switches back, and each
/// side keeps exactly its own YMM0. The counter proves the peer ran; its saved
/// image proves its upper half was saved on switch-out.
pub fn avx_real_switch_to_a_user_task() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    with_kernel_state_kept(|| {
        let peer = spawn_peer_running(&peer_program())?;
        let outcome = (|| {
            for round in 0..20u64 {
                let mine = pattern(0x100 + round);
                set_ymm0(&mine);
                let before = peer.progress();
                let mut parks = 0;
                while peer.progress() == before {
                    check!(parks < 100, "round {round}: the peer never ran");
                    task::wait_sleep(task::ticks() + 1);
                    parks += 1;
                }
                check!(
                    ymm0() == mine,
                    "round {round}: kernel resumed with {:x?} after the peer ran",
                    ymm0()
                );
                let image = fpu::capture(peer.slot);
                let saved = [
                    image.word(XMM0_LOW_WORD),
                    image.word(XMM0_LOW_WORD + 1),
                    image.word(YMM0_HI_WORD),
                    image.word(YMM0_HI_WORD + 1),
                ];
                check!(
                    saved == PEER_YMM0 && image.xstate_bv() & fpu::XFEATURE_AVX != 0,
                    "round {round}: peer image holds {saved:x?} (XSTATE_BV {:#x})",
                    image.xstate_bv()
                );
            }
            Ok(())
        })();
        task::harness::finish(peer.slot, 0);
        while task::reap_child().is_some() {}
        outcome
    })
}

/// Soak: many rounds across every slot, each with its own YMM pattern, never
/// cross-contaminate; every fourth slot is saved with the AVX state initial.
pub fn avx_switch_soak_all_slots() -> Result<(), String> {
    if !avx_on() {
        return Ok(());
    }
    let seed = |slot: usize, round: u64| (slot as u64) << 20 ^ round;
    with_kernel_state_kept(|| {
        for round in 0..500u64 {
            for slot in 1..task::MAX_TASKS {
                if slot % 4 == 0 {
                    // SAFETY: `vzeroall` has no operands.
                    unsafe { core::arch::asm!("vzeroall", options(nostack, nomem)) };
                } else {
                    set_ymm0(&pattern(seed(slot, round)));
                }
                fpu::save(slot);
            }
            for slot in (1..task::MAX_TASKS).rev() {
                set_ymm0(&pattern(!0));
                fpu::restore(slot);
                let expected = if slot % 4 == 0 {
                    [0; 4]
                } else {
                    pattern(seed(slot, round))
                };
                check!(
                    ymm0() == expected,
                    "round {round} slot {slot}: {:x?} != {expected:x?}",
                    ymm0()
                );
            }
        }
        for slot in 1..task::MAX_TASKS {
            fpu::reset(slot);
        }
        Ok(())
    })
}

/// The `FXSAVE` path a CPU without AVX runs (and the kernel falls back to)
/// still works: force it, switch SSE state between two slots, and check XMM0
/// and MXCSR come back. Interrupts stay off, so the scheduler never sees the
/// switched-off mask.
pub fn avx_fxsave_fallback_path() -> Result<(), String> {
    with_kernel_state_kept(|| {
        let enabled = cpu::swap_xsave_mask_for_test(0);
        let outcome = (|| {
            check!(
                fpu::state_size() == 512,
                "FXSAVE state is {}",
                fpu::state_size()
            );
            fpu::reset(SLOT_A);
            fpu::reset(SLOT_B);
            let first = [0x1122_3344_5566_7788, 0x99aa_bbcc_ddee_ff00];
            set_xmm0(&first);
            set_mxcsr(MXCSR_ROUND_DOWN);
            fpu::save(SLOT_A);
            set_xmm0(&[8, 9]);
            set_mxcsr(MXCSR_DEFAULT);
            fpu::save(SLOT_B);
            fpu::restore(SLOT_A);
            check!(
                xmm0() == first && mxcsr() == MXCSR_ROUND_DOWN,
                "FXSAVE round trip gave {:x?} / {:#x}",
                xmm0(),
                mxcsr()
            );
            fpu::restore(SLOT_B);
            check!(
                xmm0() == [8, 9] && mxcsr() == MXCSR_DEFAULT,
                "FXSAVE restore gave {:x?} / {:#x}",
                xmm0(),
                mxcsr()
            );
            Ok(())
        })();
        cpu::swap_xsave_mask_for_test(enabled);
        // Leave the slots as a later test expects them.
        fpu::reset(SLOT_A);
        fpu::reset(SLOT_B);
        outcome
    })
}

/// Cycles for `rounds` save + restore pairs on one slot.
fn pair_cycles(rounds: u64) -> u64 {
    let begin = crate::boot_trace::tsc();
    for _ in 0..rounds {
        fpu::save(SLOT_A);
        fpu::restore(SLOT_A);
    }
    crate::boot_trace::tsc() - begin
}

/// What a context switch pays for the FP state: a save and a restore with
/// `XSAVE`/`XRSTOR` (the whole x87/SSE/AVX area) against `FXSAVE`/`FXRSTOR`,
/// reported as a `PERF:fpu_switch` line. The bound only catches a pathological
/// cost (a trapping instruction, a hypervisor exit per switch).
pub fn avx_switch_cost() -> Result<(), String> {
    with_kernel_state_kept(|| {
        const ROUNDS: u64 = 20_000;
        pair_cycles(100);
        let xsave = pair_cycles(ROUNDS) / ROUNDS;
        let enabled = cpu::swap_xsave_mask_for_test(0);
        let fxsave = pair_cycles(ROUNDS) / ROUNDS;
        cpu::swap_xsave_mask_for_test(enabled);
        crate::trace::line!(
            tracefmt::Channel::Perf,
            "fpu_switch:save_restore_cycles xsave={xsave} fxsave={fxsave} area={}",
            fpu::state_size()
        );
        check!(
            xsave < 100_000 && fxsave < 100_000,
            "a save + restore pair costs {xsave} cycles with XSAVE, {fxsave} with FXSAVE"
        );
        fpu::reset(SLOT_A);
        Ok(())
    })
}
