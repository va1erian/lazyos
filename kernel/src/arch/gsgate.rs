//! Kernel entry gates that keep `%gs` trustworthy (the `swapgs` discipline,
//! `docs/architecture/smp-audit.md`, "Boot, CPU and entry").
//!
//! **The invariant.** In ring 0, `IA32_GS_BASE` is this CPU's per-CPU area
//! (`arch::percpu`); in ring 3, `IA32_KERNEL_GS_BASE` holds it and
//! `IA32_GS_BASE` is whatever ring 3 loaded. Ring 3 can rewrite its own GS
//! base at will — `mov gs, ax` with any DPL-3 selector loads that
//! descriptor's base (0) — and nothing a user does reaches
//! `IA32_KERNEL_GS_BASE` (`swapgs` and `wrmsr` are privileged, and
//! `CR4.FSGSBASE` stays off). So every entry from ring 3 executes exactly one
//! `swapgs` before its first `gs:` access, and every exit to ring 3 exactly
//! one before its `iretq`/`sysretq`. On a CPU the two alternate however
//! tasks switch in between, because the CPU itself alternates user and
//! kernel; the user GS value a task finds on return may be another task's
//! (always 0: every user-loadable descriptor has base 0), which is harmless
//! because user GS is unsupported (`ARCH_SET_GS` is a stub).
//!
//! **Which test decides the swap.** A maskable interrupt or an `int` gate
//! can only arrive with `IF` set, and every swap window (a stub between its
//! entry and its `swapgs`, or between its exit `swapgs` and `iretq`) runs
//! with `IF` clear, so the interrupted `CS` is exact: [`irq_gate!`] tests it
//! on entry and the *resumed* frame's `CS` on exit (a handler may switch
//! tasks; the frame it resumes is the one that decides). An exception or an
//! NMI can land inside a window — a faulting `iretq`/`sysretq`, an NMI
//! anywhere — where `CS` is the kernel's but `GS` still the user's, so
//! [`paranoid_gate!`] reads `IA32_GS_BASE` itself: a kernel-half address
//! (sign bit set) is the kernel's, anything else is a user's and is swapped.
//! It swaps back on exit iff it swapped on entry (these handlers never
//! switch tasks on a path that returns). The naked `#DE`/`#UD`/`#GP`/`#PF`
//! stubs, whose dispatch may resume another task, take the same entry but
//! swap back by the resumed frame's `CS`; the one case where those rules
//! disagree — a fault inside a window — halts (`gs_entry_paranoid!`).
//! Every exit `cli`s before its `swapgs`, so the windows really are IF-clear,
//! and `arch::cpu::init` keeps `CR4.FSGSBASE` off on every CPU. The NMI runs
//! on its own IST stack: it can land where `rsp` is still the user's.
//!
//! The targets are `extern "C"` functions taking the hardware frame
//! (`&InterruptStackFrameValue`, plus the error code where the CPU pushes
//! one): the compiler's `x86-interrupt` ABI ends in its own `iretq`, which
//! leaves no place for the exit `swapgs`.

/// Push the registers a C call may clobber, plus `rbx` (the paranoid gate's
/// swapped flag) and `rbp` (the pre-alignment `rsp`): 11 qwords, 88 bytes.
macro_rules! gate_save {
    () => {
        "push rax\n push rcx\n push rdx\n push rsi\n push rdi\n push r8\n push r9\n \
         push r10\n push r11\n push rbx\n push rbp\n"
    };
}

/// Undo [`gate_save!`].
macro_rules! gate_restore {
    () => {
        "pop rbp\n pop rbx\n pop r11\n pop r10\n pop r9\n pop r8\n pop rdi\n pop rsi\n \
         pop rdx\n pop rcx\n pop rax\n"
    };
}

/// Call the target with `rsp` 16-aligned (the CPU aligns only before its
/// own pushes) and `cld` (ring 3 may run with `DF` set, issue #405).
macro_rules! gate_call {
    () => {
        "cld\n mov rbp, rsp\n and rsp, -16\n call {target}\n mov rsp, rbp\n"
    };
}

/// A gate for a maskable interrupt (or a software `int`): swap on a ring-3
/// `CS`, entering and leaving. `$stub` is the IDT address; `$target` an
/// `extern "C" fn(&InterruptStackFrameValue)`.
macro_rules! irq_gate {
    ($stub:ident => $target:path) => {
        core::arch::global_asm!(
            concat!(
                ".global ", stringify!($stub), "\n",
                stringify!($stub), ":\n",
                "test byte ptr [rsp + 8], 3\n jz 1f\n swapgs\n1:\n",
                $crate::arch::gsgate::gate_save!(),
                "lea rdi, [rsp + 88]\n",
                $crate::arch::gsgate::gate_call!(),
                $crate::arch::gsgate::gate_restore!(),
                "cli\n test byte ptr [rsp + 8], 3\n jz 2f\n swapgs\n2:\n iretq\n"
            ),
            target = sym $target,
        );
        extern "C" {
            pub(crate) fn $stub();
        }
    };
}

/// A gate for an exception or NMI: swap iff `IA32_GS_BASE` is not a
/// kernel-half address, swap back iff it swapped (its targets resume the
/// frame they entered on, or never return). `$target` takes the frame, and
/// the error code when `err` is given.
macro_rules! paranoid_gate {
    ($stub:ident => $target:path) => {
        $crate::arch::gsgate::paranoid_gate!(@emit $stub, $target, "lea rdi, [rsp + 88]\n", "");
    };
    ($stub:ident => $target:path, err) => {
        $crate::arch::gsgate::paranoid_gate!(
            @emit $stub,
            $target,
            "lea rdi, [rsp + 96]\n mov rsi, [rsp + 88]\n",
            "add rsp, 8\n"
        );
    };
    (@emit $stub:ident, $target:path, $args:literal, $drop:literal) => {
        core::arch::global_asm!(
            concat!(
                ".global ", stringify!($stub), "\n",
                stringify!($stub), ":\n",
                $crate::arch::gsgate::gate_save!(),
                "xor ebx, ebx\n mov ecx, 0xC0000101\n rdmsr\n test edx, edx\n js 1f\n",
                "swapgs\n mov ebx, 1\n1:\n",
                $args,
                $crate::arch::gsgate::gate_call!(),
                "test ebx, ebx\n jz 2f\n swapgs\n2:\n",
                $crate::arch::gsgate::gate_restore!(),
                $drop,
                "iretq\n"
            ),
            target = sym $target,
        );
        extern "C" {
            pub(crate) fn $stub();
        }
    };
}

// The asm the naked stubs elsewhere (`task::switch`, `process::gate`,
// `arch::idt`, `arch::linux`) splice in, so every gate follows one rule.

// Local labels: these snippets own `7:`, `8:` and `9:`; a stub that splices
// them must not jump across a splice to its own label of the same number.

/// [`irq_gate!`]'s exit test for a naked stub, right before its `iretq`
/// (`rsp` at the resumed frame's RIP). The `cli` makes "IF is clear in every
/// swap window" hold by construction rather than by every syscall body
/// ending with interrupts off (`iretq` restores `IF` from the frame).
#[macro_export]
macro_rules! gs_exit_swap_on_user_cs {
    () => {
        "cli\n test byte ptr [rsp + 8], 3\n jz 8f\n swapgs\n8:\n"
    };
}

/// [`paranoid_gate!`]'s entry test for a naked exception stub, after it saved
/// `rax`, `rcx` and `rdx` (it clobbers them). `$cs` is the operand naming the
/// interrupted `CS` at that point. These stubs swap back by the *resumed*
/// frame's `CS` (a dispatch may resume another task), which disagrees with
/// the entry in exactly one case: a fault raised inside a swap window, with
/// a kernel `CS` and a user GS (a faulting exit `iretq`/`sysretq`).
/// Resuming such a frame would leave ring 3 on the kernel's GS base, so the
/// stub halts in [`gs_window_fault`] instead.
#[macro_export]
macro_rules! gs_entry_paranoid {
    ($cs:literal) => {
        concat!(
            "mov ecx, 0xC0000101\n rdmsr\n test edx, edx\n js 9f\n swapgs\n",
            "test byte ptr ",
            $cs,
            ", 3\n jnz 9f\n and rsp, -16\n call gs_window_fault\n9:\n"
        )
    };
}

/// [`irq_gate!`]'s entry test for a naked `int`/interrupt stub, before any
/// push (`[rsp + 8]` is the interrupted `CS`: no error code).
#[macro_export]
macro_rules! gs_entry_swap_on_user_cs {
    () => {
        "test byte ptr [rsp + 8], 3\n jz 7f\n swapgs\n7:\n"
    };
}

pub(crate) use {gate_call, gate_restore, gate_save, irq_gate, paranoid_gate};

/// A fault inside a `swapgs` window (`gs_entry_paranoid!`): the kernel's own
/// exit `iretq`/`sysretq` faulted. Unreachable while every return frame is
/// kernel-built and hardened (`task::signal::harden`); never resumed.
#[no_mangle]
extern "C" fn gs_window_fault() -> ! {
    use core::fmt::Write;
    let _ = writeln!(
        super::raw_serial::RawSerial,
        "EXCEPTION: fault inside a swapgs window (a kernel exit faulted)"
    );
    crate::halt();
}

/// Set up this CPU's two GS bases for the invariant: `IA32_GS_BASE` = its
/// area (ring 0 runs on it), `IA32_KERNEL_GS_BASE` = 0 (the user GS base the
/// first exit to ring 3 swaps in). The BSP's boot and every AP's bring-up.
pub fn install(area: u64) {
    const IA32_GS_BASE: u32 = 0xC000_0101;
    const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;
    super::msr::write(IA32_GS_BASE, area);
    super::msr::write(IA32_KERNEL_GS_BASE, 0);
}
