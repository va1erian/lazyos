//! The scheduler entry ISRs that drive preemption and blocking.
//!
//! Two gates share one body: [`timer_isr`] (PIT IRQ0, vector 32) and
//! [`yield_isr`] (vector [`YIELD_VECTOR`], raised by `WaitQueue::wait` when a
//! task parks). Both save the general-purpose registers, call
//! [`super::schedule`], then resume the returned stack. `schedule` may switch
//! to a different task/address space, so the `mov rsp, rax` below is what
//! actually performs the switch.
//!
//! The second argument to `schedule` tells a real timer tick (advance the
//! clock, acknowledge the PIC, charge CPU time) apart from a voluntary
//! reschedule, which must do none of those (issue #338): a park is not 10 ms
//! of wall time, and an EOI without a pending IRQ0 is spurious.
//!
//! Each entry pushes `rax` first and only then clobbers `eax` with its flag,
//! so the saved frame layout (r15 .. rax, then the CPU's interrupt frame) is
//! identical for both gates and a task parked by one can be resumed by the
//! other.

use core::arch::global_asm;

/// IDT vector of the voluntary-reschedule gate. Distinct from the PIT (32),
/// the PIC lines (32..48) and the native syscall gate (0x80). Installed with
/// DPL 0, so only kernel code can raise it.
pub const YIELD_VECTOR: u8 = 0x81;

global_asm!(
    r#"
    .global timer_isr
    timer_isr:
        push rax
        mov eax, 1
        jmp 2f

    .global yield_isr
    yield_isr:
        push rax
        xor eax, eax

    2:
        push rbx
        push rcx
        push rdx
        push rsi
        push rdi
        push rbp
        push r8
        push r9
        push r10
        push r11
        push r12
        push r13
        push r14
        push r15

        mov rdi, rsp
        mov esi, eax
        call schedule
        mov rsp, rax

        pop r15
        pop r14
        pop r13
        pop r12
        pop r11
        pop r10
        pop r9
        pop r8
        pop rbp
        pop rdi
        pop rsi
        pop rdx
        pop rcx
        pop rbx
        pop rax
        iretq
    "#
);

extern "C" {
    /// Entry point referenced by the IDT for the PIT (vector 32).
    pub fn timer_isr();
    /// Entry point referenced by the IDT for voluntary reschedules
    /// ([`YIELD_VECTOR`]).
    pub fn yield_isr();
}

/// Enter the scheduler voluntarily: the caller has already marked itself
/// blocked (or wants to give up the CPU) and resumes here when picked again.
///
/// Does not advance the tick counter, acknowledge the PIC, or charge a CPU
/// tick; deadline expiry and task selection run exactly as on a tick.
pub fn yield_now() {
    // SAFETY: `arch::idt::init` installs `yield_isr` at `YIELD_VECTOR`
    // (0x81). The gate saves a full interrupt frame and restores it with
    // `iretq`, so control returns here with every register intact.
    unsafe { x86_64::instructions::interrupts::software_interrupt::<0x81>() };
}
