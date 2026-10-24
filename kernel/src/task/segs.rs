//! The user's data segment selectors, per task (docs/smp-plan.md S8.3c).
//!
//! A ring-3 task may load DS, ES, FS and GS (`mov gs, ax`), and the kernel
//! never touches them: interrupt gates and `syscall` save and restore only
//! CS, SS and the general registers, and `swapgs` swaps GS's *base*, not its
//! selector. On one CPU the selectors simply stayed in the register file
//! across task switches (so one task saw another's), and nothing noticed;
//! with tasks moving between CPUs a task resumed on another CPU finds that
//! CPU's values instead of its own (`gs_suite`'s hostile program reads its
//! `%gs` selector back after every round). So the selectors are saved when
//! a task leaves a CPU and loaded again when it is installed, only when they
//! differ from what the CPU holds (a load is a few dozen cycles; reading
//! them is cheap).
//!
//! Loading FS or GS in 64-bit mode also loads its base from the descriptor
//! (zero for the user data segment): `install` writes the task's FS base
//! *after* this, and GS is loaded between two `swapgs`, so the kernel's own
//! GS base (the per-CPU area, `arch::percpu`) is in `IA32_KERNEL_GS_BASE`
//! meanwhile and comes back intact. User code cannot set a GS base of its
//! own (`CR4.FSGSBASE` is off, `arch_prctl(SET_GS)` does nothing), so the
//! base the load leaves behind is the one it had.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::MAX_TASKS;

/// Per slot: DS | ES << 16 | FS << 32 | GS << 48, as saved.
static SAVED: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// Per slot: whether [`SAVED`] holds the task's (a new task has none yet
/// and starts with whatever the CPU holds, as before).
static KNOWN: [AtomicBool; MAX_TASKS] = [const { AtomicBool::new(false) }; MAX_TASKS];

/// The four selectors as the CPU holds them now.
fn live() -> u64 {
    let (ds, es, fs, gs): (u16, u16, u16, u16);
    // SAFETY: reading segment registers has no side effects.
    unsafe {
        core::arch::asm!(
            "mov {0:x}, ds",
            "mov {1:x}, es",
            "mov {2:x}, fs",
            "mov {3:x}, gs",
            out(reg) ds,
            out(reg) es,
            out(reg) fs,
            out(reg) gs,
            options(nomem, nostack, preserves_flags)
        );
    }
    u64::from(ds) | u64::from(es) << 16 | u64::from(fs) << 32 | u64::from(gs) << 48
}

/// `slot` leaves this CPU: keep its selectors.
pub(super) fn save(slot: usize) {
    if let (Some(saved), Some(known)) = (SAVED.get(slot), KNOWN.get(slot)) {
        saved.store(live(), Ordering::Relaxed);
        known.store(true, Ordering::Relaxed);
    }
}

/// `slot` is installed on this CPU: load its selectors where they differ.
/// Interrupts off (the scheduler); before the task's FS base is written.
pub(super) fn restore(slot: usize) {
    let (Some(saved), Some(known)) = (SAVED.get(slot), KNOWN.get(slot)) else {
        return;
    };
    if !known.load(Ordering::Relaxed) {
        return;
    }
    let want = saved.load(Ordering::Relaxed);
    let have = live();
    if want == have {
        return;
    }
    let field = |value: u64, at: u32| (value >> at) as u16;
    // SAFETY: each selector was read from this task's own registers when it
    // left a CPU, so it is null or a user data selector of the GDT every
    // CPU shares the layout of (`arch::gdt`): ring 0 may load it, and the
    // load cannot fault. GS goes between two `swapgs` with interrupts off,
    // so the per-CPU base is parked in `IA32_KERNEL_GS_BASE` while the load
    // replaces `IA32_GS_BASE`, and an NMI there finds a user base and swaps
    // for itself (`arch::nmi`'s paranoid entry).
    unsafe {
        if field(want, 0) != field(have, 0) {
            core::arch::asm!("mov ds, {0:x}", in(reg) field(want, 0), options(nostack, preserves_flags));
        }
        if field(want, 16) != field(have, 16) {
            core::arch::asm!("mov es, {0:x}", in(reg) field(want, 16), options(nostack, preserves_flags));
        }
        if field(want, 32) != field(have, 32) {
            core::arch::asm!("mov fs, {0:x}", in(reg) field(want, 32), options(nostack, preserves_flags));
        }
        if field(want, 48) != field(have, 48) {
            core::arch::asm!(
                "swapgs",
                "mov gs, {0:x}",
                "swapgs",
                in(reg) field(want, 48),
                options(nostack, preserves_flags)
            );
        }
    }
}

/// A slot is handed to a new task: it has no selectors of its own yet.
pub(super) fn forget(slot: usize) {
    if let Some(known) = KNOWN.get(slot) {
        known.store(false, Ordering::Relaxed);
    }
}
