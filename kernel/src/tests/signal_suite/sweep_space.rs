//! The timer sweep and the address space a handler frame is written through
//! (issue #375).
//!
//! The sweep runs on the scheduler's lock with whatever page table the tick
//! interrupted. A handler frame it lays out for task A is a write to A's user
//! stack, so it only reaches A if it goes through A's own table. Written
//! through another task's table it lands in that task's copy of the same
//! address (or nowhere), A resumes at its handler with a stack that holds no
//! frame, and the handler's `ret` pops garbage: BusyBox `sh` dying with
//! `rip 0x2` between two pipelines when a child's exit posted `SIGCHLD` while
//! the shell sat preempted in user mode.
//!
//! Two forked tasks map one stack page each at the same virtual address (as
//! processes of one shell do) and each has a pending handler signal. Whatever
//! the sweep does with a task, the invariant checked is: a task that was
//! entered into its handler has its frame in *its own* page, and no page
//! holds a frame that is not its owner's.

use super::*;
use crate::task::signal::lf;
use crate::user_ptr;
use x86_64::PhysAddr;

const HANDLER: u64 = 0x0040_1000;
const RESTORER: u64 = 0x0040_2000;
/// The `rip` a victim is "preempted" at; inside no mapping on purpose (the
/// sweep never touches code).
const PREEMPTED_RIP: u64 = 0x0040_0500;
/// One stack page per task, at the same virtual address in both.
const STACK_PAGE: u64 = 0x0070_0000;
const STACK_TOP: u64 = STACK_PAGE + 4096;
/// Bytes below `rsp` the frame builder reserves: the red zone plus the frame.
const FRAME_RESERVE: u64 = 128 + 512;

/// A forked task with its own table, one mapped stack page, a user frame
/// preempted at `rsp`, and `SIGUSR1` pending with a handler installed.
struct Victim {
    slot: usize,
    pml4: PhysAddr,
    /// Physical frame backing [`STACK_PAGE`] in this task's table.
    page: u64,
    rsp: u64,
}

fn victim(rsp: u64) -> Result<Victim, String> {
    // Forked from the kernel task: a fresh, empty user half.
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let pml4 = PhysAddr::new(task::harness::pml4(slot).ok_or("victim has no table")?);
    let pages = process::map_range(pml4, STACK_PAGE, STACK_TOP).map_err(to_string)?;
    let page = pages
        .first()
        .map(|(_, phys)| *phys)
        .ok_or("no stack page")?;
    check!(
        task::harness::set_user_frame(slot, PREEMPTED_RIP, rsp),
        "could not shape task {slot}'s frame"
    );
    signal::set_action(
        slot,
        signal::SIGUSR1,
        Disposition::Handler {
            handler: HANDLER,
            flags: 0,
            restorer: RESTORER,
            mask: 0,
        },
    )
    .map_err(|error| format!("set_action: {error:?}"))?;
    send(slot, signal::SIGUSR1)?;
    Ok(Victim {
        slot,
        pml4,
        page,
        rsp,
    })
}

/// A word of a physical page, read through the physical-memory map so the
/// check is independent of whichever table is active.
fn page_word(page: u64, offset: u64) -> u64 {
    let ptr = mem::phys_to_virt(PhysAddr::new(page + offset)).as_ptr::<u64>();
    // SAFETY: `page` is a frame `map_range` allocated for the test and
    // `offset` stays inside it; the physical map covers every frame.
    unsafe { ptr.read_volatile() }
}

/// How many words of the page equal the restorer pointer: one per frame.
fn frames_in(page: u64) -> usize {
    (0..4096 / 8)
        .filter(|index| page_word(page, index * 8) == RESTORER)
        .count()
}

/// Run one sweep with `active`'s table installed, as a tick that interrupted
/// that task would, with real user-pointer validation on.
fn sweep_with_active(active: &Victim) {
    let kernel = mem::kernel_table();
    let trust = user_ptr::set_trust_kernel_pointers(false);
    mem::switch_to(active.pml4);
    task::harness::run_sweep();
    mem::switch_to(kernel);
    user_ptr::set_trust_kernel_pointers(trust);
}

/// The invariant: entered into the handler means the frame is in the task's
/// own page (restorer, signal number, and the interrupted context all
/// there); not entered means the signal is still pending and the page holds
/// no frame at all.
fn check_frame(v: &Victim, name: &str) -> Result<bool, String> {
    let regs = task::harness::frame_regs(v.slot).ok_or("victim vanished")?;
    let pending = signal::pending(v.slot) & (1 << signal::SIGUSR1) != 0;
    if regs.rip != HANDLER {
        check!(
            regs.rip == PREEMPTED_RIP && regs.rsp == v.rsp && pending,
            "{name}: not entered, yet rip={:#x} rsp={:#x} pending={pending}",
            regs.rip,
            regs.rsp
        );
        check!(
            frames_in(v.page) == 0,
            "{name}: not entered, yet its page holds a frame"
        );
        return Ok(false);
    }
    check!(!pending, "{name}: entered, but SIGUSR1 is still pending");
    let expected =
        signal::harden::handler_frame_below(v.rsp, FRAME_RESERVE).ok_or("frame arithmetic")?;
    check!(
        regs.rsp == expected && regs.rdi == signal::SIGUSR1 as u64,
        "{name}: handler entry rsp={:#x} rdi={:#x}, expected rsp {expected:#x}",
        regs.rsp,
        regs.rdi
    );
    let at = regs.rsp - STACK_PAGE;
    check!(
        page_word(v.page, at) == RESTORER,
        "{name}: entered its handler, but its own stack holds {:#x} where the restorer must be: the frame was written through another address space",
        page_word(v.page, at)
    );
    check!(
        page_word(v.page, at + lf::MCONTEXT + lf::RIP) == PREEMPTED_RIP
            && page_word(v.page, at + lf::MCONTEXT + lf::RSP) == v.rsp,
        "{name}: frame does not hold the interrupted context"
    );
    check!(
        frames_in(v.page) == 1,
        "{name}: its page holds {} frames, expected exactly its own",
        frames_in(v.page)
    );
    Ok(true)
}

/// Finish and reap both victims, freeing their tables and pages.
fn cleanup(victims: &[Victim]) -> Result<(), String> {
    for v in victims {
        task::harness::finish(v.slot, 0);
    }
    for _ in victims {
        task::reap_child().ok_or("victim was not reapable")?;
    }
    Ok(())
}

/// A tick that interrupts B while A also has a handler signal pending: A's
/// frame must be in A's page, never in B's. The sweep can only write through
/// B's table, so it enters B and leaves A pending; A is entered when the
/// scheduler resumes it with its own table installed.
pub fn sweep_frame_in_target_space() -> Result<(), String> {
    fresh()?;
    let a = victim(STACK_TOP - 0x100)?;
    let b = victim(STACK_TOP - 0x500)?;

    sweep_with_active(&b);
    check!(
        !check_frame(&a, "A")?,
        "A was entered by a sweep that could not write through its table"
    );
    check!(
        check_frame(&b, "B")?,
        "B, whose table was active, was not entered"
    );

    // The scheduler switches to A: its table is installed, so A is entered
    // with its frame where it can run, and B's frame stays as it was.
    check!(
        !task::harness::resume_delivery(a.slot),
        "resuming A ended it"
    );
    check!(
        check_frame(&a, "A")?,
        "A was not entered when resumed in its own space"
    );
    check!(check_frame(&b, "B")?, "B lost its frame");

    // A tick that interrupts A itself still delivers to A in place: with the
    // mask restored (as `rt_sigreturn` would), a second signal gets a second
    // frame below the first, in A's own page.
    signal::set_blocked(a.slot, 0);
    send(a.slot, signal::SIGUSR1)?;
    sweep_with_active(&a);
    check!(
        signal::pending(a.slot) & (1 << signal::SIGUSR1) == 0 && frames_in(a.page) == 2,
        "a tick in A's own space did not deliver to A: pending={:#x}, {} frames in its page",
        signal::pending(a.slot),
        frames_in(a.page)
    );

    cleanup(&[a, b])?;
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Soak: many generations of paired victims, alternating which table the
/// tick finds active and resuming the other task afterwards, never put a
/// frame in the wrong page and leak neither frames nor signal registry
/// entries.
pub fn soak_sweep_space_isolation() -> Result<(), String> {
    const ROUNDS: u32 = 200;
    fresh()?;
    let mut baseline = None;
    for round in 0..ROUNDS {
        let a = victim(STACK_TOP - 0x100)?;
        let b = victim(STACK_TOP - 0x500)?;
        let (first, second) = if round % 2 == 0 { (&a, &b) } else { (&b, &a) };
        sweep_with_active(first);
        check_frame(&a, "A").map_err(|e| format!("round {round}, first sweep: {e}"))?;
        check_frame(&b, "B").map_err(|e| format!("round {round}, first sweep: {e}"))?;
        check!(
            !task::harness::resume_delivery(second.slot),
            "round {round}: resuming the second task ended it"
        );
        let entered_a = check_frame(&a, "A").map_err(|e| format!("round {round}: {e}"))?;
        let entered_b = check_frame(&b, "B").map_err(|e| format!("round {round}: {e}"))?;
        check!(
            entered_a && entered_b,
            "round {round}: after a tick in one space and a resume of the other, A entered={entered_a} B entered={entered_b}"
        );
        cleanup(&[a, b]).map_err(|e| format!("round {round}: {e}"))?;

        let free = mem::frame_stats().free;
        let registry = signal::harness::registry_len();
        match baseline {
            None => baseline = Some((free, registry)),
            Some((free0, registry0)) => check!(
                free == free0 && registry == registry0,
                "round {round}: {free} free frames / {registry} registry entries, round 0 left {free0} / {registry0}"
            ),
        }
    }
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}
