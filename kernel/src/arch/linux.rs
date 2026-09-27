//! Linux `syscall`/`sysret` entry.
//!
//! `syscall` does not switch stacks, so the entry stub switches to the current
//! task's kernel stack (a global, since the kernel never uses `%gs`), saves the
//! user general registers, calls [`crate::process::linux_dispatch`], then
//! restores and returns with `sysretq`.
//!
//! We deliberately avoid `swapgs`: tasks can block *inside* a syscall (futex)
//! and be context-switched, which would desynchronise the per-task GS state.

use crate::arch::msr;
use core::arch::global_asm;
use core::ptr::addr_of_mut;

/// Kernel stack top the next Linux syscall switches to (single CPU). The
/// scheduler updates this alongside the TSS `RSP0` when it switches tasks.
#[no_mangle]
pub static mut KERNEL_STACK: u64 = 0;

/// Transient scratch for the user RSP during entry (single CPU; only touched
/// with interrupts disabled, before dispatch can be preempted).
#[no_mangle]
pub static mut SAVED_USER_RSP: u64 = 0;

/// User register state captured at `syscall` entry. Linux preserves these across
/// the syscall, so `clone` can build the child thread's first frame from them.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UserContext {
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
}

/// Single-CPU snapshot of the registers at the last `syscall` entry.
#[no_mangle]
pub static mut USER_CONTEXT: UserContext = UserContext {
    rip: 0,
    rflags: 0,
    rsp: 0,
    rbx: 0,
    rbp: 0,
    r12: 0,
    r13: 0,
    r14: 0,
    r15: 0,
    rdi: 0,
    rsi: 0,
    rdx: 0,
    r8: 0,
    r9: 0,
    r10: 0,
};

/// Snapshot of the registers at the current task's last `syscall` entry.
pub fn user_context() -> UserContext {
    // Safety: single CPU; read while handling the syscall that captured it.
    unsafe { core::ptr::read(addr_of_mut!(USER_CONTEXT)) }
}

global_asm!(
    r#"
    .global linux_syscall_entry
    linux_syscall_entry:
        /* Save the user context (Linux preserves it across `syscall`). */
        mov [rip + USER_CONTEXT + 0], rcx
        mov [rip + USER_CONTEXT + 8], r11
        mov [rip + USER_CONTEXT + 16], rsp
        mov [rip + USER_CONTEXT + 24], rbx
        mov [rip + USER_CONTEXT + 32], rbp
        mov [rip + USER_CONTEXT + 40], r12
        mov [rip + USER_CONTEXT + 48], r13
        mov [rip + USER_CONTEXT + 56], r14
        mov [rip + USER_CONTEXT + 64], r15
        mov [rip + USER_CONTEXT + 72], rdi
        mov [rip + USER_CONTEXT + 80], rsi
        mov [rip + USER_CONTEXT + 88], rdx
        mov [rip + USER_CONTEXT + 96], r8
        mov [rip + USER_CONTEXT + 104], r9
        mov [rip + USER_CONTEXT + 112], r10

        mov [rip + SAVED_USER_RSP], rsp
        mov rsp, [rip + KERNEL_STACK]    /* current task kernel stack */
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
        sysretq
    "#
);

extern "C" {
    fn linux_syscall_entry();
}

/// Program the MSRs for Linux syscalls.
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
}

/// Update the kernel stack the next Linux syscall will switch to.
pub fn set_kernel_stack(top: u64) {
    // Safety: single CPU; called from the scheduler with the task held.
    unsafe { *addr_of_mut!(KERNEL_STACK) = top };
}
