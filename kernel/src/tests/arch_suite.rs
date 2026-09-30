//! Architecture-level invariants that only bite on real hardware: the
//! `sysretq` return path's selector arithmetic, and locks an IRQ handler also
//! takes.

use super::*;

/// `sysretq` must land in ring 3 with exactly the GDT's user selectors, RPL 3
/// included. AMD loads SS = STAR[63:48] + 8 without forcing RPL 3, so a base
/// of bare 0x10 left Linux-ABI tasks running with SS = 0x18 and the next
/// `iretq` back to them faulted with #GP(0x18) (only under KVM on AMD; Intel
/// and TCG force RPL 3 and hid it).
pub fn sysret_selectors_rpl3() -> Result<(), String> {
    use crate::arch::{gdt, linux, msr};
    let star = msr::read(msr::IA32_STAR);
    let sysret_base = (star >> 48) as u16;
    let syscall_base = ((star >> 32) & 0xffff) as u16;
    check!(
        sysret_base == linux::STAR_SYSRET_BASE,
        "STAR[63:48] is {sysret_base:#x}, expected {:#x}",
        linux::STAR_SYSRET_BASE
    );
    check!(
        syscall_base == linux::STAR_SYSCALL_BASE,
        "STAR[47:32] is {syscall_base:#x}, expected {:#x}",
        linux::STAR_SYSCALL_BASE
    );
    let selectors = gdt::selectors();
    let sysret_ss = sysret_base.wrapping_add(8);
    let sysret_cs = sysret_base.wrapping_add(16);
    check!(
        sysret_ss == selectors.user_data,
        "sysret SS would be {sysret_ss:#x} (as AMD loads it), GDT user data is {:#x}",
        selectors.user_data
    );
    check!(
        sysret_cs == selectors.user_code,
        "sysret CS would be {sysret_cs:#x}, GDT user code is {:#x}",
        selectors.user_code
    );
    check!(
        sysret_ss & 3 == 3 && sysret_cs & 3 == 3,
        "sysret selectors CS={sysret_cs:#x} SS={sysret_ss:#x} are not RPL 3"
    );
    Ok(())
}

/// The real syscall entry body (run through `arch::linux::probe_entry`, which
/// shares its instructions with `linux_syscall_entry`) must reach
/// `call linux_dispatch` with `rsp` 16-aligned and with Linux `r9` in the
/// seventh SysV argument slot (`a6`, `preadv2`'s flags).
///
/// Optimised (release) builds use aligned SSE stores on their frames, so an
/// 8-byte skew corrupted the first Linux-ABI syscall's formatting and froze
/// the release desktop while debug builds hid it; and the padding slot that
/// fixed it is also `a6`'s slot, which must be written, not left stale.
pub fn syscall_entry_call_alignment() -> Result<(), String> {
    use crate::arch::linux::{probe_entry, ENTRY_CALL_PAD, ENTRY_PUSHED_QWORDS};
    const GETPID: u64 = 39;
    const R9_MARK: u64 = 0x5eed_c0de_1234_5678;
    for slot in [0, 1, crate::task::MAX_TASKS - 1] {
        let top = crate::task::kstack_top(slot);
        check!(
            top % 16 == 0,
            "kernel stack top of slot {slot} ({top:#x}) is not 16-aligned"
        );
        let (rsp, a6) = probe_entry(GETPID, R9_MARK, top);
        check!(
            rsp % 16 == 0,
            "slot {slot}: rsp at `call linux_dispatch` is {rsp:#x} (misaligned)"
        );
        check!(
            rsp == top - ENTRY_PUSHED_QWORDS * 8 - ENTRY_CALL_PAD,
            "slot {slot}: rsp at the call is {rsp:#x}, unexpected frame layout"
        );
        check!(
            a6 == R9_MARK,
            "slot {slot}: seventh argument slot holds {a6:#x}, not Linux r9"
        );
    }
    Ok(())
}

/// Run `f` with interrupts enabled but every PIC line masked, so the
/// IRQ-shared helpers see `IF=1` (as the kernel mux does) without any
/// interrupt actually firing into the scheduler-less harness.
fn with_irqs_on_masked<R>(f: impl FnOnce() -> R) -> R {
    use crate::arch::io::{inb, outb};
    // Safety: reading/writing the 8259 IMRs only changes which IRQ lines
    // are masked; the harness runs with interrupts off, and the previous
    // masks are restored below before interrupts are disabled again.
    let (m1, m2) = unsafe { (inb(0x21), inb(0xA1)) };
    // Safety: see above; masking every line keeps `sti` inert.
    unsafe {
        outb(0x21, 0xFF);
        outb(0xA1, 0xFF);
    }
    x86_64::instructions::interrupts::enable();
    let result = f();
    x86_64::instructions::interrupts::disable();
    // Safety: restores the masks read above.
    unsafe {
        outb(0x21, m1);
        outb(0xA1, m2);
    }
    result
}

/// Locks that an IRQ handler also takes (`TASKS` via `task::live`, the mouse
/// `STATE`) must be held with interrupts off even when the caller runs with
/// them on: the kernel mux calls both every frame with `IF=1`, and a
/// timer/mouse IRQ inside the critical section deadlocked the xui sysmon boot
/// under KVM. The caller's `IF` must also be restored.
pub fn irq_shared_locks_disable_interrupts() -> Result<(), String> {
    use crate::task::harness;
    let _ = harness::take_critical_stats();
    let restored = with_irqs_on_masked(|| {
        let _ = crate::task::live(0);
        let after_live = x86_64::instructions::interrupts::are_enabled();
        let _ = crate::input::mouse::take_moved();
        let after_mouse = x86_64::instructions::interrupts::are_enabled();
        after_live && after_mouse
    });
    let (checks, irqs_on) = harness::take_critical_stats();
    check!(checks >= 2, "only {checks} critical sections were observed");
    check!(
        irqs_on == 0,
        "{irqs_on} of {checks} IRQ-shared critical sections ran with interrupts on"
    );
    check!(restored, "the caller's interrupt flag was not restored");
    // With interrupts already off the helpers must leave them off.
    let _ = crate::task::live(0);
    check!(
        !x86_64::instructions::interrupts::are_enabled(),
        "task::live enabled interrupts for an IF=0 caller"
    );
    let _ = harness::take_critical_stats();
    Ok(())
}

/// Soak: many mux-style polls with `IF=1`, every one of which must take its
/// lock with interrupts off and hand `IF=1` back.
pub fn soak_irq_shared_locks() -> Result<(), String> {
    use crate::task::harness;
    const ROUNDS: u64 = 200_000;
    let _ = harness::take_critical_stats();
    let lost_if = with_irqs_on_masked(|| {
        let mut lost = 0u64;
        for round in 0..ROUNDS {
            let _ = crate::task::live((round as usize) % crate::task::MAX_TASKS);
            let _ = crate::input::mouse::take_moved();
            if !x86_64::instructions::interrupts::are_enabled() {
                lost += 1;
            }
        }
        lost
    });
    let (checks, irqs_on) = harness::take_critical_stats();
    check!(
        checks == ROUNDS * 2,
        "{checks} critical sections observed, expected {}",
        ROUNDS * 2
    );
    check!(
        irqs_on == 0,
        "{irqs_on} critical sections ran with interrupts on"
    );
    check!(
        lost_if == 0,
        "{lost_if} rounds returned with interrupts off"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "arch_syscall_entry_call_alignment",
        syscall_entry_call_alignment,
    ),
    ("arch_sysret_selectors_rpl3", sysret_selectors_rpl3),
    (
        "arch_irq_shared_locks_disable_interrupts",
        irq_shared_locks_disable_interrupts,
    ),
    ("arch_soak_irq_shared_locks", soak_irq_shared_locks),
];
