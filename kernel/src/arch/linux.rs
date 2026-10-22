//! Linux `syscall`/`sysret` entry.
//!
//! `syscall` does not switch stacks, so the entry stub `swapgs`es to this
//! CPU's kernel GS base (`arch::gsgate`: GS state is the CPU's, never the
//! task's, since tasks block inside syscalls), switches to the CPU's syscall
//! stack (a per-CPU area field, `arch::percpu`), saves the user general
//! registers, calls [`crate::process::linux_dispatch`], then restores,
//! swaps back and returns with `sysretq`.

use super::msr;
use super::percpu;
use core::arch::global_asm;
use core::sync::atomic::Ordering;

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

impl UserContext {
    /// The area's entry-time value (`linux_syscall_entry` overwrites it on
    /// the first `syscall`).
    pub const ZEROED: UserContext = UserContext {
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
}

/// Snapshot of the registers at the current CPU's last `syscall` entry.
pub fn user_context() -> UserContext {
    percpu::user_context()
}

/// Replace the captured context (after `rt_sigreturn` restores a frame, the
/// next signal delivery must build on the restored registers, not the
/// `rt_sigreturn` entry state).
pub fn set_user_context(context: UserContext) {
    percpu::set_user_context(context);
}

/// The user RSP `linux_syscall_entry` saved for the in-progress syscall. Read
/// from the task's own return stack, so a task that blocked inside the syscall
/// cannot observe another task's snapshot.
pub fn saved_user_rsp() -> u64 {
    // Safety: we are inside the syscall, on this CPU's syscall stack (read
    // through `%gs`; only this CPU touches its area).
    unsafe { *((syscall_stack() as *const u64).offset(-1)) }
}

/// Saved user registers `linux_syscall_entry` will reload before `sysretq`,
/// by slot. The order mirrors the pushes: r15, r14, r13, r12, rbp, rbx, rdi,
/// rsi, rdx, r8, r9, r10. `rt_sigreturn` writes every slot.
pub fn set_saved_register(slot: usize, value: u64) {
    debug_assert!(slot < 12);
    // Safety: the pushes sit at fixed offsets below the kernel stack top,
    // and we are inside the syscall on this task's stack.
    unsafe {
        let top = syscall_stack() as *mut u64;
        *top.offset(-4 - slot as isize) = value;
    }
}

/// Qwords `linux_syscall_entry` pushes onto the kernel stack before the call:
/// user RSP, RFLAGS, RIP, 12 registers (see [`set_saved_register`]), then the
/// full-return flag and the user RCX and R11 it carries ([`set_full_return`]).
pub const ENTRY_PUSHED_QWORDS: u64 = 18;
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
        /* Save the user context (Linux preserves it across `syscall`) into
           this CPU's per-CPU area (`arch::percpu`, addressed through `%gs`:
           the real entry swapped it in first; the test probe is entered
           from ring 0, where it already is). */
        mov gs:[{off_uc} + 0], rcx
        mov gs:[{off_uc} + 8], r11
        mov gs:[{off_uc} + 16], rsp
        mov gs:[{off_uc} + 24], rbx
        mov gs:[{off_uc} + 32], rbp
        mov gs:[{off_uc} + 40], r12
        mov gs:[{off_uc} + 48], r13
        mov gs:[{off_uc} + 56], r14
        mov gs:[{off_uc} + 64], r15
        mov gs:[{off_uc} + 72], rdi
        mov gs:[{off_uc} + 80], rsi
        mov gs:[{off_uc} + 88], rdx
        mov gs:[{off_uc} + 96], r8
        mov gs:[{off_uc} + 104], r9
        mov gs:[{off_uc} + 112], r10

        mov gs:[{off_rsp}], rsp
        mov rsp, gs:[{off_stack}]      /* this CPU's syscall stack */
        push qword ptr gs:[{off_rsp}]
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
        /* Full-return slots (`set_full_return`): flag, user rcx, user r11.
           `syscall` and `sysretq` make rcx and r11 hold the resume RIP and
           RFLAGS, so a context whose rcx/r11 are anything else (an
           `rt_sigreturn` into code interrupted by a signal) must leave
           through `iretq`. Zero means the ordinary `sysretq` exit. */
        push 0
        push 0
        push 0
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
        /* `syscall` only comes from ring 3: take this CPU's kernel GS base
           before the first `gs:` access (`arch::gsgate`). IF is clear
           (FMASK), so nothing can run between here and the swap. */
        swapgs
"#,
        entry_body!(""),
        r#"
        /* Run a task the call woke if it outranks this one (P1.1). The
           result rides on the stack; 15 pushed qwords plus this one leave
           rsp 16-aligned for the call, and every other register is
           reloaded from the stack below. */
        push rax
        sub rsp, 8                      /* 18 pushes + rax + 8: 16-aligned */
        call linux_syscall_return
        add rsp, 8
        pop rax
        cmp qword ptr [rsp + 16], 0
        jne 2f
        add rsp, 24                     /* drop the full-return slots */
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
        cli                             /* no IRQ in the swap window (r11 has IF) */
        swapgs                          /* back to the user's GS base */
        sysretq
    2:
        /* Full return: every register, including rcx and r11, comes from the
           frame, so leave through `iretq`. rcx and r11 are free until the
           end (everything else is reloaded from the stack, rax is the
           result). */
        mov r11, [rsp]
        mov rcx, [rsp + 8]
        add rsp, 24
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
        /* rsp -> [user RIP, user RFLAGS, user RSP]: build the iret frame
           (RIP, CS, RFLAGS, RSP, SS from low to high) below them. */
        push {user_ss}
        push qword ptr [rsp + 24]
        push qword ptr [rsp + 24]
        push {user_cs}
        push qword ptr [rsp + 32]
        cli                             /* no IRQ in the swap window */
        swapgs                          /* back to the user's GS base */
        iretq
    "#
    ),
    pad = const ENTRY_CALL_PAD,
    user_cs = const STAR_SYSRET_BASE as u64 + 16,
    user_ss = const STAR_SYSRET_BASE as u64 + 8,
    off_uc = const percpu::OFF_USER_CONTEXT,
    off_rsp = const percpu::OFF_SAVED_USER_RSP,
    off_stack = const percpu::OFF_SYSCALL_STACK,
);

// Test-only twin of `linux_syscall_entry`: the same body (via `entry_body!`),
// but entered with a plain `call` from ring 0 and left with `ret`, recording
// `rsp` and the seventh-argument slot at the `call linux_dispatch`. Takes
// `(nr, r9)`; every other syscall register is zero.
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
        mov rsp, gs:[{off_rsp}]
        ret
    "#
    ),
    pad = const ENTRY_CALL_PAD,
    off_uc = const percpu::OFF_USER_CONTEXT,
    off_rsp = const percpu::OFF_SAVED_USER_RSP,
    off_stack = const percpu::OFF_SYSCALL_STACK,
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
/// `(rsp at the call, a6 slot at the call)`. Saves and restores the per-CPU
/// state the stub overwrites. Interrupts must be off (the harness's normal
/// state).
#[cfg(lazyos_tests)]
pub fn probe_entry(nr: u64, r9: u64, stack_top: u64) -> (u64, u64) {
    let context = user_context();
    let syscall = &percpu::current().syscall;
    let stack = syscall.stack.load(Ordering::Relaxed);
    let user_rsp = syscall.saved_user_rsp.load(Ordering::Relaxed);
    syscall.stack.store(stack_top, Ordering::Relaxed);
    // SAFETY: the probe is the real entry body; the state it touches is
    // restored below, and interrupts are off (no per-CPU state can change).
    unsafe {
        lazyos_entry_probe(nr, r9);
    }
    let syscall = &percpu::current().syscall;
    syscall.stack.store(stack, Ordering::Relaxed);
    syscall.saved_user_rsp.store(user_rsp, Ordering::Relaxed);
    set_user_context(context);
    // SAFETY: statics the probe's asm wrote, single-CPU, interrupts off.
    unsafe { (PROBE_RSP, PROBE_A6) }
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

/// Program the MSRs for Linux syscalls. `%gs` is already the CPU's area
/// (`percpu::init` from `gdt::init`), so the stub's `gs:`-relative loads find
/// it.
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

/// Update the kernel stack the next Linux syscall on this CPU switches to.
pub fn set_kernel_stack(top: u64) {
    percpu::current()
        .syscall
        .stack
        .store(top, Ordering::Relaxed);
}

/// The kernel stack top this CPU's Linux `syscall` entry switched to.
fn syscall_stack() -> u64 {
    percpu::current().syscall.stack.load(Ordering::Relaxed)
}

/// Make the in-progress syscall return through `iretq` with `rcx` and `r11`
/// set to the given values, instead of `sysretq` (which loads them with the
/// resume RIP and RFLAGS). `rt_sigreturn` needs this: the interrupted code's
/// `rcx` and `r11` are live registers like any other. The entry stub clears
/// the flag on every syscall.
pub fn set_full_return(rcx: u64, r11: u64) {
    // Safety: the three slots sit at fixed offsets below the saved registers
    // (see `ENTRY_PUSHED_QWORDS`), on this task's stack.
    unsafe {
        let top = syscall_stack() as *mut u64;
        *top.offset(-4 - 12) = 1;
        *top.offset(-4 - 13) = rcx;
        *top.offset(-4 - 14) = r11;
    }
}

/// Rewrite the return context of the in-progress syscall. `execve` uses this to
/// make `sysretq` resume at a fresh program's entry point instead of returning
/// to the caller: the entry stub pushed `[user_rsp, rflags, rip]` just below the
/// current kernel-stack top.
pub fn set_user_return(rip: u64, rsp: u64, rflags: u64) {
    // Safety: we are inside the syscall, on this task's stack.
    unsafe {
        let top = syscall_stack() as *mut u64;
        *top.offset(-3) = rip;
        *top.offset(-2) = rflags;
        *top.offset(-1) = rsp;
    }
    let mut context = percpu::user_context();
    context.rip = rip;
    context.rsp = rsp;
    context.rflags = rflags;
    percpu::set_user_context(context);
}
