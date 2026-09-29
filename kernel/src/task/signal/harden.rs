//! Validation of the user-controlled values signal delivery and `rt_sigreturn`
//! feed to the return-to-user path (issues #221 and #223).
//!
//! Two things reach `sysretq`/`iretq` from user memory: the addresses of a
//! handler frame (from `rsp`, a `sigaltstack`, a `sigaction` handler) and the
//! registers a `rt_sigreturn` frame restores. Unchecked, a small `rsp` or a
//! huge alternate stack overflows the frame arithmetic (a dev-build panic), and
//! a forged frame can make `sysretq` fault in ring 0 on an attacker-chosen
//! stack (non-canonical `rip`, the SYSRET privilege-escalation pattern) or
//! load IOPL/`IF` straight from user memory. Everything here fails closed: the
//! caller force-terminates the task with `SIGSEGV`, as Linux does when it
//! cannot build or restore a frame.

use super::{
    current, halt_forever, lf, linux_sigset_to_kernel, read_u64, slot_info, terminate_process,
    UserRegs, LINUX_FRAME_SIZE, SIGSEGV,
};

/// First address above user space: the canonical lower half ends here.
pub const USER_MAX: u64 = 0x0000_8000_0000_0000;

/// Page 0 is never user memory, so a frame cannot legitimately start below it.
const MIN_FRAME_ADDR: u64 = 0x1000;

const RFLAGS_IF: u64 = 1 << 9;
/// Bit 1 of RFLAGS always reads as one.
const RFLAGS_FIXED: u64 = 1 << 1;
/// The flags user code may change (Linux's `FIX_EFLAGS`): CF PF AF ZF SF TF DF
/// OF AC RF. IOPL, IF, VM, VIF/VIP, ID and NT are never taken from user memory.
const USER_FLAGS_MASK: u64 = 0x0001 // CF
    | 0x0004 // PF
    | 0x0010 // AF
    | 0x0040 // ZF
    | 0x0080 // SF
    | 0x0100 // TF
    | 0x0400 // DF
    | 0x0800 // OF
    | 0x1_0000 // RF
    | 0x4_0000; // AC

/// RFLAGS as `sysretq` may safely load them: only user-modifiable bits from
/// `flags`, interrupts always enabled (so a task can never run with `IF` clear
/// and starve the single CPU), IOPL zero.
pub const fn sanitize_rflags(flags: u64) -> u64 {
    (flags & USER_FLAGS_MASK) | RFLAGS_IF | RFLAGS_FIXED
}

/// Whether `addr` lies in user space (and therefore is canonical).
pub const fn is_user_addr(addr: u64) -> bool {
    addr < USER_MAX
}

/// The 16-byte-aligned base of a frame of `reserve` bytes (frame plus red zone)
/// laid out below `stack_top`, or `None` when the stack is not in user space or
/// too small: the arithmetic must not underflow on a tiny `rsp` (#223).
pub fn frame_below(stack_top: u64, reserve: u64) -> Option<u64> {
    if stack_top > USER_MAX {
        return None;
    }
    let base = stack_top.checked_sub(reserve)? & !0xF;
    (base >= MIN_FRAME_ADDR).then_some(base)
}

/// The top of an enabled alternate stack, or `None` if `sp + size` wraps or
/// leaves user space.
pub fn altstack_top(sp: u64, size: u64) -> Option<u64> {
    sp.checked_add(size).filter(|top| *top <= USER_MAX)
}

/// Make `regs` safe to return to user mode: sanitise the flags and require the
/// resume address and stack pointer to be user addresses. `false` means the
/// context must not be resumed.
pub fn sanitize_regs(regs: &mut UserRegs) -> bool {
    regs.rflags = sanitize_rflags(regs.rflags);
    is_user_addr(regs.rip) && is_user_addr(regs.rsp)
}

/// Read the interrupted context out of the `rt_sigframe` a `rt_sigreturn` was
/// entered with (`user_rsp` sits just past `pretcode`). `None` when the frame
/// address itself is not a user range; the values are *not* yet trusted, see
/// [`sanitize_regs`].
pub fn parse_frame(user_rsp: u64) -> Option<(UserRegs, u64)> {
    let frame = user_rsp.checked_sub(8)?;
    if frame.checked_add(LINUX_FRAME_SIZE)? > USER_MAX {
        return None;
    }
    let mc = frame + lf::MCONTEXT;
    let regs = UserRegs {
        r8: read_u64(mc + lf::R8),
        r9: read_u64(mc + lf::R9),
        r10: read_u64(mc + lf::R10),
        r11: read_u64(mc + lf::R11),
        r12: read_u64(mc + lf::R12),
        r13: read_u64(mc + lf::R13),
        r14: read_u64(mc + lf::R14),
        r15: read_u64(mc + lf::R15),
        rdi: read_u64(mc + lf::RDI),
        rsi: read_u64(mc + lf::RSI),
        rbp: read_u64(mc + lf::RBP),
        rbx: read_u64(mc + lf::RBX),
        rdx: read_u64(mc + lf::RDX),
        rax: read_u64(mc + lf::RAX),
        rcx: read_u64(mc + lf::RCX),
        rsp: read_u64(mc + lf::RSP),
        rip: read_u64(mc + lf::RIP),
        rflags: read_u64(mc + lf::EFLAGS),
    };
    Some((
        regs,
        linux_sigset_to_kernel(read_u64(frame + lf::UC_SIGMASK)),
    ))
}

/// The registers and mask `rt_sigreturn` may resume with, or `None` for a
/// forged or unreadable frame.
pub fn restore_frame(user_rsp: u64) -> Option<(UserRegs, u64)> {
    let (mut regs, mask) = parse_frame(user_rsp)?;
    sanitize_regs(&mut regs).then_some((regs, mask))
}

/// Terminate the current process as if it took an unhandled `SIGSEGV` and never
/// return: used when a frame cannot be built or restored.
pub fn die_with_segv() -> ! {
    if let Some((pml4, _)) = slot_info(current()) {
        terminate_process(pml4, 128 + SIGSEGV as u64);
    }
    halt_forever()
}
