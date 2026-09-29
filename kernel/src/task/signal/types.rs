//! Signal vocabulary: dispositions, alt stacks, `siginfo`, saved registers and the per-process table.

use super::*;

/// Whether `code` for `sig` is a fault code whose `siginfo_t` carries
/// `si_addr` (rather than the sender's pid/uid). Positive codes below
/// `SI_KERNEL` are the per-signal fault codes.
pub(super) fn is_fault_info(sig: u8, code: i32) -> bool {
    matches!(sig, SIGSEGV | SIGILL | SIGFPE | SIGBUS) && (1..SI_KERNEL).contains(&code)
}

/// The standard action a signal takes with the default disposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultAction {
    /// End the process (exit status 128 + signo).
    Term,
    /// End the process; the core-dump flavour is informational here.
    Core,
    /// Park the whole process until `SIGCONT`.
    Stop,
    /// Resume a stopped process.
    Cont,
    /// Drop the signal.
    Ignore,
}

/// The kernel-defined disposition of a signal, from Linux's tables.
pub fn default_action(sig: u8) -> DefaultAction {
    match sig {
        SIGCHLD | SIGURG | SIGWINCH => DefaultAction::Ignore,
        SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => DefaultAction::Stop,
        SIGCONT => DefaultAction::Cont,
        SIGQUIT | SIGILL | SIGTRAP | SIGABRT | SIGBUS | SIGFPE | SIGSEGV | SIGXCPU | SIGXFSZ
        | SIGSYS => DefaultAction::Core,
        _ => DefaultAction::Term,
    }
}

/// What a process has installed for a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Kernel default (see [`default_action`]).
    Default,
    /// Explicitly dropped (`SIG_IGN`).
    Ignore,
    /// A user handler: address, `sa_flags`, the restorer (musl's `__restore_rt`),
    /// and the `sa_mask` applied while the handler runs. The mask is stored in
    /// the kernel's internal bit order (`1 << sig`), not Linux's `sigset_t`.
    Handler {
        handler: u64,
        flags: u64,
        restorer: u64,
        mask: u64,
    },
}

/// The alternate signal stack of a process (`sigaltstack`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AltStack {
    pub sp: u64,
    pub size: u64,
    pub enabled: bool,
}

impl AltStack {
    pub const DISABLED: AltStack = AltStack {
        sp: 0,
        size: 0,
        enabled: false,
    };
}

/// `siginfo_t` fields the kernel fills in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SigInfo {
    pub code: i32,
    pub pid: usize,
    pub uid: u32,
    pub addr: u64,
}

impl SigInfo {
    /// A signal sent by a user process (kill/tkill/tgkill).
    pub const fn user(pid: usize, code: i32) -> Self {
        SigInfo {
            code,
            pid,
            uid: 0,
            addr: 0,
        }
    }

    /// A synchronous fault reported by the kernel.
    pub const fn fault(code: i32, addr: u64) -> Self {
        SigInfo {
            code,
            pid: 0,
            uid: 0,
            addr,
        }
    }

    /// A kernel-generated asynchronous event (child exit, terminal INTR).
    pub const fn kernel() -> Self {
        SigInfo {
            code: SI_KERNEL,
            pid: 0,
            uid: 0,
            addr: 0,
        }
    }
}

/// Failures mapped to errno by `process::linux`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalError {
    /// No live process with that pid.
    NoSuchProcess,
    /// The target exists but the sender's credentials do not allow signalling
    /// it (different uid without `CAP_KILL`, or the kernel task).
    NotPermitted,
    /// Bad signal number or bad action.
    Invalid,
}

/// Snapshot of the user registers at a delivery boundary. The field order
/// matches the interrupt frame the kernel stacks, so a frame can be copied in
/// and out without shuffling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserRegs {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
}
