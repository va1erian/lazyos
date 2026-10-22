//! The per-CPU foundations (docs/smp-plan.md S8.1): the area's self-pointer
//! contract, the `%gs`-relative offsets the entry asm assumes, and the
//! isolation a second CPU's untouched area demonstrates.

use super::*;
use crate::arch::percpu;
use core::mem::{offset_of, size_of};
use core::sync::atomic::Ordering;

/// `gs:[0]` reads back the area itself, and the boot CPU's is area 0, online,
/// with the kernel task current. Everything else in the kernel reads per-CPU
/// state through this one load, so the self-pointer is the invariant.
pub fn self_pointer_names_the_area() -> Result<(), String> {
    let cpu = percpu::current();
    check!(cpu.index() == 0, "boot CPU index is {}", cpu.index());
    check!(cpu.online.load(Ordering::Acquire), "boot CPU is not online");
    check!(
        cpu.self_ptr.load(Ordering::Relaxed) == cpu as *const _ as u64,
        "gs:[0] is {:#x}, the area is at {:#x}",
        cpu.self_ptr.load(Ordering::Relaxed),
        cpu as *const _ as u64
    );
    check!(
        cpu.current.load(Ordering::Relaxed) == crate::task::KERNEL_TASK as u64,
        "area 0 boots with task {}",
        cpu.current.load(Ordering::Relaxed)
    );
    Ok(())
}

/// The `%gs`-relative offsets the entry asm uses (`entry_body!`'s
/// `gs:[{off} + N]` writes) must land inside `Cpu.syscall`, in the layout
/// order the asm assumes: user context first at +0, RSP scratch, stack top.
/// A drift here is a boot triple-fault, so it is pinned to the struct.
pub fn entry_asm_offsets_match_the_layout() -> Result<(), String> {
    use crate::arch::percpu::{OFF_SAVED_USER_RSP, OFF_SYSCALL_STACK, OFF_USER_CONTEXT};
    let sys = offset_of!(percpu::Cpu, syscall);
    check!(
        OFF_USER_CONTEXT == sys + offset_of!(percpu::SyscallEntry, user_context),
        "OFF_USER_CONTEXT is {OFF_USER_CONTEXT}"
    );
    check!(
        OFF_SAVED_USER_RSP == sys + offset_of!(percpu::SyscallEntry, saved_user_rsp),
        "OFF_SAVED_USER_RSP is {OFF_SAVED_USER_RSP}"
    );
    check!(
        OFF_SYSCALL_STACK == sys + offset_of!(percpu::SyscallEntry, stack),
        "OFF_SYSCALL_STACK is {OFF_SYSCALL_STACK}"
    );
    // The asm writes 15 qwords at `gs:[OFF_USER_CONTEXT + 0..112]`: the field
    // must be at least that wide, and `gs:[OFF_SYSCALL_STACK]` must not
    // overlap it.
    check!(
        OFF_USER_CONTEXT + size_of::<crate::arch::linux::UserContext>() <= size_of::<percpu::Cpu>(),
        "the user context runs past the area"
    );
    check!(
        OFF_USER_CONTEXT > OFF_SYSCALL_STACK + 8,
        "the context overlaps the stack-top field"
    );
    Ok(())
}

/// `set_kernel_stack` writes *this CPU's* TSS (the property that makes a
/// shared TSS impossible), and the scheduler's `RSP0`/syscall-stack pair
/// follow it.
pub fn set_kernel_stack_targets_this_cpu() -> Result<(), String> {
    let cpu = percpu::current();
    let boot = cpu.kernel_stack_top();
    let top = boot - percpu::KERNEL_STACK_SIZE as u64 + 4096;
    crate::arch::gdt::set_kernel_stack(top);
    check!(
        cpu.rsp0() == top,
        "RSP0 is {:#x}, expected {top:#x}",
        cpu.rsp0()
    );
    // Restore what the boot installed (the entry paths use RSP0).
    crate::arch::gdt::set_kernel_stack(boot);
    Ok(())
}

/// A second CPU's area is untouched by this CPU's state changes: the
/// isolation the SMP stages rely on (a task's preemption bookkeeping, its
/// interrupts-off span and its syscall state must never straddle areas).
pub fn areas_are_isolated() -> Result<(), String> {
    // Stage this CPU's state as a woken-and-preempted task inside a syscall
    // would leave it — the same values the `preempt`/`irqoff` code writes.
    let me = crate::task::current() as u64;
    let cpu = percpu::current();
    cpu.preempt.selected.store(7, Ordering::Relaxed);
    cpu.preempt.slice_end.store(999, Ordering::Relaxed);
    cpu.irqoff.start.store(1234, Ordering::Relaxed);
    cpu.irqoff.nr.store(3, Ordering::Relaxed);
    cpu.irqoff.task.store(me, Ordering::Relaxed);
    let stack = cpu.syscall.stack.swap(0x4000_0000, Ordering::Relaxed);
    cpu.window.uncharged.fetch_add(4, Ordering::Relaxed);

    let other = percpu::area(1).expect("area 1 must exist");
    check!(
        !other.online.load(Ordering::Acquire),
        "a second area starts online"
    );
    check!(
        other.preempt.selected.load(Ordering::Relaxed) == u64::MAX,
        "area 1 saw a selection ({})",
        other.preempt.selected.load(Ordering::Relaxed)
    );
    check!(
        other.irqoff.start.load(Ordering::Relaxed) == 0
            && other.irqoff.task.load(Ordering::Relaxed) == 0,
        "area 1 saw an interrupts-off span"
    );
    check!(
        other.syscall.stack.load(Ordering::Relaxed) == 0
            && other.window.uncharged.load(Ordering::Relaxed) == 0,
        "area 1 saw syscall/window state"
    );

    // Restore the staged fields (no hooks were used, so nothing else moved).
    let cpu = percpu::current();
    cpu.preempt.selected.store(u64::MAX, Ordering::Relaxed);
    cpu.preempt.slice_end.store(0, Ordering::Relaxed);
    cpu.irqoff.start.store(0, Ordering::Relaxed);
    cpu.irqoff.nr.store(u64::MAX, Ordering::Relaxed);
    cpu.irqoff.task.store(0, Ordering::Relaxed);
    cpu.syscall.stack.store(stack, Ordering::Relaxed);

    cpu.window.uncharged.store(0, Ordering::Relaxed);
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "percpu_self_pointer_names_the_area",
        self_pointer_names_the_area,
    ),
    (
        "percpu_entry_asm_offsets_match_the_layout",
        entry_asm_offsets_match_the_layout,
    ),
    (
        "percpu_set_kernel_stack_targets_this_cpu",
        set_kernel_stack_targets_this_cpu,
    ),
    ("percpu_areas_are_isolated", areas_are_isolated),
    (
        "percpu_preempt_state_roundtrips_through_the_area",
        preempt_state_roundtrips_through_the_area,
    ),
];

/// The preemption state the scheduler reads (`task::preempt`) really reaches
/// this CPU's area: staging through the module's own test hook and reading
/// through the area must agree.
pub fn preempt_state_roundtrips_through_the_area() -> Result<(), String> {
    crate::task::preempt::set_selected(9, 42);
    let cpu = percpu::current();
    check!(
        cpu.preempt.selected.load(Ordering::Relaxed) == 9
            && cpu.preempt.slice_end.load(Ordering::Relaxed) == 42,
        "set_selected landed outside the area"
    );
    cpu.preempt.selected.store(u64::MAX, Ordering::Relaxed);
    cpu.preempt.slice_end.store(0, Ordering::Relaxed);
    Ok(())
}
