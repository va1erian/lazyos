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

/// Replace the captured context (after `rt_sigreturn` restores a frame, the
/// next signal delivery must build on the restored registers, not the
/// `rt_sigreturn` entry state).
pub fn set_user_context(context: UserContext) {
    // Safety: single CPU; called inside the syscall gate.
    unsafe { *addr_of_mut!(USER_CONTEXT) = context };
}

/// The user RSP `linux_syscall_entry` saved for the in-progress syscall. Read
/// from the task's own return stack, so a task that blocked inside the syscall
/// cannot observe another task's snapshot.
pub fn saved_user_rsp() -> u64 {
    // Safety: single CPU; we are inside the syscall on this task's stack.
    unsafe { *((KERNEL_STACK as *const u64).offset(-1)) }
}

/// Saved user registers `linux_syscall_entry` will reload before `sysretq`,
/// by slot. The order mirrors the pushes: r15, r14, r13, r12, rbp, rbx, rdi,
/// rsi, rdx, r8, r9, r10. `rt_sigreturn` writes every slot.
pub fn set_saved_register(slot: usize, value: u64) {
    debug_assert!(slot < 12);
    // Safety: single CPU; the pushes sit at fixed offsets below the kernel
    // stack top, and we are inside the syscall on this task's stack.
    unsafe {
        let top = KERNEL_STACK as *mut u64;
        *top.offset(-4 - slot as isize) = value;
    }
}

/// Qwords `linux_syscall_entry` pushes onto the kernel stack before the call:
/// user RSP, RFLAGS, RIP, then 12 registers (see [`set_saved_register`]).
pub const ENTRY_PUSHED_QWORDS: u64 = 15;
/// Padding the stub inserts so `rsp` is 16-aligned at `call linux_dispatch`
/// (the SysV ABI; optimised code uses aligned SSE stores on its frame). The
/// kernel stack tops are 16-aligned, so only the push count matters.
pub const ENTRY_CALL_PAD: u64 = (16 - (ENTRY_PUSHED_QWORDS * 8) % 16) % 16;

/// The entry stub from the register save through the `call`, shared with the
/// test-only `lazyos_entry_probe` so the test exercises the real instructions.
/// `$hook` is asm inserted just before the call (empty in the real stub).
macro_rules! entry_body {
    ($hook:literal) => {
        concat!(
            r#"
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
        /* The callee-saved user registers are pushed too: `rt_sigreturn` must
           be able to reload every register from the signal frame, and this is
           the only place the return path reads them from. */
        push r15
        push r14
        push r13
        push r12
        push rbp
        push rbx
        push rdi
        push rsi
        push rdx
        push r8
        push r9
        push r10
        /* linux_dispatch(nr, a1..a6) takes a6 - Linux r9 - as its seventh
           argument, on the stack at [rsp] at the call. The padding slot is
           exactly that slot, so it must be written, not left stale. */
        sub rsp, {pad}                  /* 16-align rsp for the call */
        mov [rsp], r9
        /* shuffle to SysV */
        mov r9, r8
        mov r8, r10
        mov rcx, rdx
        mov rdx, rsi
        mov rsi, rdi
        mov rdi, rax
        "#,
            $hook,
            r#"
        call linux_dispatch
        add rsp, {pad}
"#
        )
    };
}

// Frame layout, `cld` and switch-order contract: docs/architecture/tasks.md,
// "Entry stub contract".
global_asm!(
    concat!(
        r#"
    .global linux_syscall_entry
    linux_syscall_entry:
"#,
        entry_body!(""),
        r#"
        /* Run a task the call woke if it outranks this one (P1.1). The
           result rides on the stack; 15 pushed qwords plus this one leave
           rsp 16-aligned for the call, and every other register is
           reloaded from the stack below. */
        push rax
        call linux_syscall_return
        pop rax
        pop r10
        pop r9
        pop r8
        pop rdx
        pop rsi
        pop rdi
        pop rbx
        pop rbp
        pop r12
        pop r13
        pop r14
        pop r15
        pop rcx
        pop r11
        pop rsp                         /* rsp = user RSP (rax holds result) */
        sysretq
    "#
    ),
    pad = const ENTRY_CALL_PAD,
);

/// Test-only twin of `linux_syscall_entry`: the same body (via `entry_body!`),
/// but entered with a plain `call` from ring 0 and left with `ret`, recording
/// `rsp` and the seventh-argument slot at the `call linux_dispatch`. Takes
/// `(nr, r9)`; every other syscall register is zero.
#[cfg(lazyos_tests)]
global_asm!(
    concat!(
        r#"
    .global lazyos_entry_probe
    lazyos_entry_probe:
        mov rax, rdi
        mov r9, rsi
        xor edi, edi
        xor esi, esi
        xor edx, edx
        xor r8d, r8d
        xor r10d, r10d
        xor ecx, ecx
        mov r11d, 2
"#,
        entry_body!(
            "mov [rip + PROBE_RSP], rsp
        mov rax, [rsp]
        mov [rip + PROBE_A6], rax"
        ),
        r#"
        mov rsp, [rip + SAVED_USER_RSP]
        ret
    "#
    ),
    pad = const ENTRY_CALL_PAD,
);

/// `rsp` / seventh-argument slot the probe saw at `call linux_dispatch`.
#[cfg(lazyos_tests)]
#[no_mangle]
pub static mut PROBE_RSP: u64 = 0;
#[cfg(lazyos_tests)]
#[no_mangle]
pub static mut PROBE_A6: u64 = 0;

#[cfg(lazyos_tests)]
extern "C" {
    fn lazyos_entry_probe(nr: u64, r9: u64) -> u64;
}

/// Run syscall `nr` through the real entry body on `stack_top`, returning
/// `(rsp at the call, a6 slot at the call)`. Saves and restores the globals
/// the stub overwrites. Interrupts must be off (the harness's normal state).
#[cfg(lazyos_tests)]
pub fn probe_entry(nr: u64, r9: u64, stack_top: u64) -> (u64, u64) {
    let context = user_context();
    // Safety: single CPU, interrupts off; the globals are restored below.
    unsafe {
        let stack = KERNEL_STACK;
        let user_rsp = SAVED_USER_RSP;
        KERNEL_STACK = stack_top;
        lazyos_entry_probe(nr, r9);
        KERNEL_STACK = stack;
        SAVED_USER_RSP = user_rsp;
        set_user_context(context);
        (PROBE_RSP, PROBE_A6)
    }
}

extern "C" {
    fn linux_syscall_entry();
}

/// `IA32_STAR[63:48]`: the base `sysretq` derives the user selectors from
/// (CS = base + 16, SS = base + 8). It carries RPL 3 because AMD CPUs load
/// SS from it verbatim: with a bare `0x10`, `sysretq` on AMD leaves ring 3
/// running with SS = 0x18 (RPL 0), and the next `iretq` back to that task
/// (an `int 0x80` or a timer preemption) takes #GP(0x18). Intel and QEMU's
/// TCG force RPL 3, which is why this only showed up under KVM on AMD hosts.
/// Linux does the same (`__USER32_CS` has RPL 3).
pub const STAR_SYSRET_BASE: u16 = 0x10 | 3;
/// `IA32_STAR[47:32]`: `syscall` loads CS = base, SS = base + 8.
pub const STAR_SYSCALL_BASE: u16 = 0x08;

/// Program the MSRs for Linux syscalls.
pub fn init() {
    // STAR: SYSCALL CS=0x08/SS=0x10, SYSRET CS=0x23/SS=0x1b (see the GDT order).
    let star = ((STAR_SYSRET_BASE as u64) << 48) | ((STAR_SYSCALL_BASE as u64) << 32);
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

/// Rewrite the return context of the in-progress syscall. `execve` uses this to
/// make `sysretq` resume at a fresh program's entry point instead of returning
/// to the caller: the entry stub pushed `[user_rsp, rflags, rip]` just below the
/// current kernel-stack top.
pub fn set_user_return(rip: u64, rsp: u64, rflags: u64) {
    // Safety: single CPU; we are inside the syscall on this task's stack.
    unsafe {
        let top = KERNEL_STACK as *mut u64;
        *top.offset(-3) = rip;
        *top.offset(-2) = rflags;
        *top.offset(-1) = rsp;
        let context = addr_of_mut!(USER_CONTEXT);
        (*context).rip = rip;
        (*context).rsp = rsp;
        (*context).rflags = rflags;
    }
}
