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

/// Where the peer task's program is mapped in its own address space.
const PEER_CODE: u64 = 0x0060_0000;
/// The XMM0 value the peer loads, distinct from every kernel-side pattern.
const PEER_XMM0: u64 = 0x5eed_0f_face_b00c;

/// Offset of the peer's progress counter inside its page.
const PEER_COUNTER: usize = 32;

/// A ring-3 program that, in a loop, loads [`PEER_XMM0`] into XMM0 and
/// [`MXCSR_ROUND_UP`] into MXCSR, then bumps a counter in its own page:
///
/// ```text
///  0: mov rax, PEER_XMM0        48 b8 imm64
/// 10: movq xmm0, rax            66 48 0f 6e c0
/// 15: ldmxcsr [rip + 0x12]      0f ae 15 12 00 00 00   (-> offset 40)
/// 22: inc qword [rip + 3]       48 ff 05 03 00 00 00   (-> offset 32)
/// 29: jmp 0                     eb e1
/// 32: dq counter
/// 40: dd MXCSR_ROUND_UP
/// ```
///
/// The counter is the proof the peer executed: a switch into it can take an
/// already-pending tick before its first instruction, so CPU ticks charged
/// to it prove nothing. Reloading the registers every iteration means a
/// missing restore clobbers whichever task runs next.
fn peer_program() -> [u8; 44] {
    let mut code = [0u8; 44];
    code[0..2].copy_from_slice(&[0x48, 0xb8]);
    code[2..10].copy_from_slice(&PEER_XMM0.to_le_bytes());
    code[10..15].copy_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xc0]);
    code[15..22].copy_from_slice(&[0x0f, 0xae, 0x15, 0x12, 0x00, 0x00, 0x00]);
    code[22..29].copy_from_slice(&[0x48, 0xff, 0x05, 0x03, 0x00, 0x00, 0x00]);
    code[29..31].copy_from_slice(&[0xeb, 0xe1]);
    code[40..44].copy_from_slice(&MXCSR_ROUND_UP.to_le_bytes());
    code
}

/// A forked peer whose saved frame resumes, in ring 3, at [`peer_program`]
/// in its own table, plus a kernel view of its progress counter.
struct Peer {
    slot: usize,
    counter: *const u64,
}

impl Peer {
    /// How many loop iterations the peer has completed.
    fn progress(&self) -> u64 {
        // SAFETY: `counter` points into the peer's code page through the
        // physical-memory map, 8-aligned (offset 32 of a page); the page
        // stays allocated until the peer is reaped, after the last read.
        unsafe { self.counter.read_volatile() }
    }
}

fn spawn_peer() -> Result<Peer, String> {
    use crate::mem::vma::{Kind, Prot};
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let pml4 = x86_64::PhysAddr::new(task::harness::pml4(slot).ok_or("peer has no table")?);
    let pages = process::map_range_kind(
        pml4,
        PEER_CODE,
        PEER_CODE + 4096,
        Prot::READ | Prot::WRITE | Prot::EXEC,
        Kind::Anon,
    )
    .map_err(|error| format!("map peer code: {error}"))?;
    let (_, phys) = *pages.first().ok_or("no peer code page")?;
    let code = peer_program();
    let page = mem::phys_to_virt(x86_64::PhysAddr::new(phys)).as_mut_ptr::<u8>();
    // SAFETY: `phys` is the frame `map_range_kind` just allocated for the
    // peer, reached through the physical-memory map; 44 bytes fit in it.
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), page, code.len()) };
    // The program never touches its stack; an interrupt from ring 3 switches
    // to the task's kernel stack, so any user `rsp` will do.
    check!(
        task::harness::set_user_frame(slot, PEER_CODE, PEER_CODE + 4096),
        "could not point the peer at its program"
    );
    Ok(Peer {
        slot,
        // SAFETY: offset 32 is inside the page `page` points at.
        counter: unsafe { page.add(PEER_COUNTER) } as *const u64,
    })
}

/// A real context switch to another task that clobbers XMM0 and MXCSR in
/// ring 3: the kernel task parks, the scheduler runs the peer until a tick
/// switches back, and each side keeps exactly its own state. The peer's
/// counter proves it executed in between; its saved image proves it was
/// saved on switch-out.
pub fn fpu_real_switch_to_a_user_task() -> Result<(), String> {
    with_kernel_state_kept(|| {
        let peer = spawn_peer()?;
        let outcome = (|| {
            for round in 0..20u64 {
                let mine = 0xfeed_face_0000_0000 | round;
                set_live(mine, MXCSR_ROUND_DOWN);
                // Park a tick at a time until the peer (the only other
                // runnable task) has made progress.
                let before = peer.progress();
                let mut parks = 0;
                while peer.progress() == before {
                    check!(parks < 100, "round {round}: the peer never ran");
                    task::wait_sleep(task::ticks() + 1);
                    parks += 1;
                }
                check!(
                    live() == (mine, MXCSR_ROUND_DOWN),
                    "round {round}: kernel resumed with {:x?} after the peer ran",
                    live()
                );
                check!(
                    fpu::saved_xmm0(peer.slot) == PEER_XMM0
                        && fpu::saved_mxcsr(peer.slot) == MXCSR_ROUND_UP,
                    "round {round}: peer image xmm0={:#x} mxcsr={:#x}",
                    fpu::saved_xmm0(peer.slot),
                    fpu::saved_mxcsr(peer.slot)
                );
            }
            Ok(())
        })();
        task::harness::finish(peer.slot, 0);
        while task::reap_child().is_some() {}
        outcome
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
