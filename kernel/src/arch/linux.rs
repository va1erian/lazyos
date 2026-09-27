//! Linux `syscall`/`sysret` entry.
//!
//! `syscall` does not switch stacks, so the entry stub uses `swapgs` to reach a
//! per-CPU struct holding the current task's kernel stack, saves the user
//! general registers, calls [`crate::process::linux_dispatch`], then restores
//! and returns with `sysretq`.

use crate::arch::msr;
use core::arch::global_asm;
use core::ptr::addr_of_mut;

/// Per-CPU data reachable through `%gs` right after the entry `swapgs`.
#[repr(C)]
struct PerCpu {
    /// Top of the current task's kernel stack (at offset 0 — `gs:[0]`).
    kstack: u64,
}

static mut PERCPU: PerCpu = PerCpu { kstack: 0 };

/// Transient scratch for the user RSP during entry (single CPU; only touched
/// with interrupts disabled, before dispatch can be preempted).
#[no_mangle]
pub static mut SAVED_USER_RSP: u64 = 0;

global_asm!(
    r#"
    .global linux_syscall_entry
    linux_syscall_entry:
        swapgs
        mov [rip + SAVED_USER_RSP], rsp
        mov rsp, gs:[0]                 /* current task kernel stack */
        push qword ptr [rip + SAVED_USER_RSP]
        push r11                        /* user RFLAGS */
        push rcx                        /* user RIP */
        push rdi
        push rsi
        push rdx
        push r8
        push r9
        push r10
        /* shuffle to SysV: linux_dispatch(nr, a1..a6) */
        mov r9, r8
        mov r8, r10
        mov rcx, rdx
        mov rdx, rsi
        mov rsi, rdi
        mov rdi, rax
        call linux_dispatch
        pop r10
        pop r9
        pop r8
        pop rdx
        pop rsi
        pop rdi
        pop rcx
        pop r11
        pop rsp                         /* rsp = user RSP (rax holds result) */
        swapgs
        sysretq
    "#
);

extern "C" {
    fn linux_syscall_entry();
}

/// Program the MSRs and the per-CPU struct for Linux syscalls.
pub fn init() {
    // STAR: SYSCALL CS=0x08/SS=0x10, SYSRET CS=0x20/SS=0x18 (see the GDT order).
    let star = (0x10u64 << 48) | (0x08u64 << 32);
    msr::write(msr::IA32_STAR, star);
    msr::write(msr::IA32_LSTAR, linux_syscall_entry as *const () as u64);
    // Clear IF/TF/DF on entry.
    msr::write(msr::IA32_FMASK, 0x700);
    // Enable SCE in EFER.
    let efer = msr::read(msr::IA32_EFER);
    msr::write(msr::IA32_EFER, efer | 1);

    // `swapgs` loads MSR_KERNEL_GS_BASE on entry; point it at our per-CPU data.
    let percpu = addr_of_mut!(PERCPU) as u64;
    msr::write(msr::IA32_KERNEL_GS_BASE, percpu);
    msr::write(msr::IA32_GS_BASE, 0);
}

/// Update the kernel stack the next Linux syscall will switch to.
pub fn set_kernel_stack(top: u64) {
    // Safety: single CPU; called from the scheduler with the task held.
    unsafe { *addr_of_mut!((*addr_of_mut!(PERCPU)).kstack) = top };
}
