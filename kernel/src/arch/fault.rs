//! Containing CPU exceptions raised by ring-3 code (issue #7).
//!
//! A user program that dereferences a bad pointer, executes a privileged
//! instruction, divides by zero or hits any other exception must cost the
//! machine only that program. The exception handlers in `arch::idt` call
//! [`contain`] with the interrupted code segment: a ring-0 fault is a kernel
//! bug and still halts with a diagnostic, but a ring-3 fault ends the faulting
//! process (every thread sharing its address space, like a fatal signal) with
//! the conventional `128 + signal` status. The parent sees `SIGCHLD` and can
//! `wait` for the status, so the shell or supervisor carries on.

use crate::task::{current, pml4_of, signal, KERNEL_TASK};

/// The exception classes a user program can raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    DivideError,
    InvalidOpcode,
    DeviceNotAvailable,
    /// #GP: privileged instruction, bad segment, non-canonical address.
    GeneralProtection,
    StackSegment,
    SegmentNotPresent,
    InvalidTss,
    /// x87 or SIMD floating point exception.
    FloatingPoint,
    AlignmentCheck,
    /// #PF that COW, demand paging and a `SIGSEGV` handler did not resolve.
    BadAccess,
}

impl Fault {
    /// The signal Linux raises for this exception, which fixes the exit
    /// status (`128 + signal`) the parent observes.
    pub const fn signal(self) -> u8 {
        match self {
            Fault::DivideError | Fault::FloatingPoint => signal::SIGFPE,
            Fault::InvalidOpcode | Fault::DeviceNotAvailable => signal::SIGILL,
            Fault::AlignmentCheck => signal::SIGBUS,
            Fault::GeneralProtection
            | Fault::StackSegment
            | Fault::SegmentNotPresent
            | Fault::InvalidTss
            | Fault::BadAccess => signal::SIGSEGV,
        }
    }

    /// Short name for the serial diagnostic.
    pub const fn name(self) -> &'static str {
        match self {
            Fault::DivideError => "divide error",
            Fault::InvalidOpcode => "invalid opcode",
            Fault::DeviceNotAvailable => "device not available",
            Fault::GeneralProtection => "general protection fault",
            Fault::StackSegment => "stack segment fault",
            Fault::SegmentNotPresent => "segment not present",
            Fault::InvalidTss => "invalid TSS",
            Fault::FloatingPoint => "floating point exception",
            Fault::AlignmentCheck => "alignment check",
            Fault::BadAccess => "page fault",
        }
    }
}

/// Whether a code-segment selector saved by an exception belongs to ring 3.
/// The RPL is the CPL at the time of the fault, so this is the CPU's own
/// statement of who faulted, not something the program can spoof.
pub const fn from_user(code_segment: u64) -> bool {
    code_segment & 3 == 3
}

/// The exit status recorded for a process killed by `fault`.
pub const fn exit_status(fault: Fault) -> u64 {
    128 + fault.signal() as u64
}

/// End the process that owns the current task, as a fatal `fault`. Returns
/// the number of tasks terminated (0 for the kernel task, which is never
/// killed this way).
pub fn kill_current(fault: Fault) -> usize {
    let slot = current();
    if slot == KERNEL_TASK {
        return 0;
    }
    let Some(pml4) = pml4_of(slot) else {
        return 0;
    };
    signal::terminate_process(pml4, exit_status(fault))
}

/// Handle an exception with saved code segment `code_segment`. Returns only
/// when the fault came from ring 0 (the caller then halts as before). A ring-3
/// fault never returns: the process is terminated and this task waits for the
/// scheduler to switch away, exactly as the `exit` syscall does.
pub fn contain(code_segment: u64, fault: Fault, detail: core::fmt::Arguments) {
    if !from_user(code_segment) {
        return;
    }
    crate::serial_println!(
        "user: task {} killed by {} ({}), status {}",
        current(),
        fault.name(),
        detail,
        exit_status(fault)
    );
    kill_current(fault);
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}
