//! Delivering synchronous CPU faults (`#PF`, `#GP`, `#UD`, `#DE`) to a
//! userspace handler through the same frame path as any other signal
//! (issues #7, #246).
//!
//! Only a handler is delivered here. Everything else (default, ignore, a
//! blocked signal, a frame that does not fit) returns false and the caller
//! ends the process, as Linux does for a fault it cannot deliver.

use super::{
    apply_regs_to_frame, arm_handler, bit, current_info, prepare_handler, regs_from_frame,
    with_signals, Disposition, Kind, SigInfo, FPE_INTDIV, ILL_ILLOPC, SA_SIGINFO, SEGV_ACCERR,
    SEGV_MAPERR, SIGFPE, SIGILL, SIGSEGV, SI_KERNEL, TASKS,
};

/// A page fault frame carries the CPU error code before RIP; the exception
/// classes below with an error code (`#GP`) share that layout.
const WITH_ERROR_CODE: usize = super::FAULT_RIP_INDEX;
/// Frame layout of `#DE`/`#UD`: no error code, so RIP follows the registers.
const NO_ERROR_CODE: usize = super::FRAME_RIP_INDEX;

/// The exceptions [`deliver_exception`] can turn into a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exception {
    /// `#DE`: `SIGFPE`, `FPE_INTDIV`.
    DivideError,
    /// `#UD`: `SIGILL`, `ILL_ILLOPC`.
    InvalidOpcode,
    /// `#GP`: `SIGSEGV`, `SI_KERNEL`.
    GeneralProtection,
}

impl Exception {
    /// The signal Linux raises for this exception.
    pub const fn signal(self) -> u8 {
        match self {
            Exception::DivideError => SIGFPE,
            Exception::InvalidOpcode => SIGILL,
            Exception::GeneralProtection => SIGSEGV,
        }
    }

    /// Frame word index of the saved RIP for this exception.
    pub const fn rip_index(self) -> usize {
        match self {
            Exception::GeneralProtection => WITH_ERROR_CODE,
            Exception::DivideError | Exception::InvalidOpcode => NO_ERROR_CODE,
        }
    }

    /// `siginfo_t` for a fault at `rip`. `#GP` has no meaningful address.
    fn info(self, rip: u64) -> SigInfo {
        match self {
            Exception::DivideError => SigInfo::fault(FPE_INTDIV, rip),
            Exception::InvalidOpcode => SigInfo::fault(ILL_ILLOPC, rip),
            Exception::GeneralProtection => SigInfo::fault(SI_KERNEL, 0),
        }
    }
}

/// Deliver `SIGSEGV` for a page fault that COW/demand-zero could not resolve.
/// Returns true when a handler was entered: the caller resumes the faulting
/// task at the handler instead of ending it.
pub fn deliver_fault(frame_rsp: u64, rip_index: usize, fault_addr: u64, error: u64) -> bool {
    let code = if error & 0b10 != 0 {
        SEGV_ACCERR
    } else {
        SEGV_MAPERR
    };
    deliver(
        frame_rsp,
        rip_index,
        SIGSEGV,
        SigInfo::fault(code, fault_addr),
    )
}

/// Deliver the signal for `exception` raised by ring-3 code whose saved frame
/// starts at `frame_rsp`. Returns true when a handler was entered.
pub fn deliver_exception(frame_rsp: u64, exception: Exception) -> bool {
    let rip_index = exception.rip_index();
    // Safety: the caller passes the base of the exception frame it saved.
    let rip = unsafe { regs_from_frame(frame_rsp, rip_index) }.rip;
    deliver(
        frame_rsp,
        rip_index,
        exception.signal(),
        exception.info(rip),
    )
}

fn deliver(frame_rsp: u64, rip_index: usize, sig: u8, info: SigInfo) -> bool {
    let Some((slot, pml4)) = current_info() else {
        return false;
    };
    // A synchronous fault cannot wait for an unblock: the instruction would
    // just fault again. Linux force-kills instead, and so do we.
    let deliverable = with_signals(pml4, |state| {
        matches!(state.actions[sig as usize], Disposition::Handler { .. })
            && state.blocked & bit(sig) == 0
    });
    if !deliverable {
        return false;
    }
    with_signals(pml4, |state| state.infos[sig as usize] = info);
    let Some(armed) = arm_handler(pml4, slot, sig) else {
        return false;
    };
    let native = {
        let tasks = TASKS.lock();
        tasks[slot]
            .as_ref()
            .is_some_and(|task| task.kind == Kind::Native || task.kstack_top == 0)
    };
    // Safety: the caller passes the base of the exception frame it received.
    let mut regs = unsafe { regs_from_frame(frame_rsp, rip_index) };
    // No frame means no handler entry: the caller ends the process.
    let Some(result) = prepare_handler(&regs, sig, &armed, native) else {
        return false;
    };
    regs.rip = result.rip;
    regs.rsp = result.rsp;
    regs.rdi = sig as u64;
    if !native && armed.flags & SA_SIGINFO != 0 {
        regs.rsi = result.info;
        regs.rdx = result.ucontext;
    }
    // Safety: same frame, now rewritten in place.
    unsafe { apply_regs_to_frame(frame_rsp, &regs, rip_index) };
    true
}
