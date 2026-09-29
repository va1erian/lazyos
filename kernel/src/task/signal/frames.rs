//! Linux/native signal frame layout, building and parsing, and interrupt-frame register helpers.

use super::*;

// ---------------------------------------------------------------------------
// Linux signal frames (`struct rt_sigframe`)
// ---------------------------------------------------------------------------

/// Frame word indices for the timer/syscall interrupt frame, after the 15
/// general registers: RIP, CS, RFLAGS, RSP, SS.
pub const FRAME_RIP_INDEX: usize = 15;
/// A page fault frame carries the CPU error code before RIP.
pub const FAULT_RIP_INDEX: usize = 16;

/// Size of the frame we build on the user stack: `pretcode` (8) + `ucontext_t`
/// (304) + `siginfo_t` (128), rounded up with slack.
pub(super) const LINUX_FRAME_SIZE: u64 = 512;
/// x86_64 System V red zone, which the interrupted code may be using below RSP.
pub(super) const RED_ZONE: u64 = 128;

/// Offsets inside the Linux frame. `mcontext` is the kernel `struct sigcontext`
/// musl also uses (`mcontext_t`).
pub(crate) mod lf {
    // ucontext_t starts after `pretcode`.
    pub const UC_FLAGS: u64 = 8;
    pub const UC_LINK: u64 = 16;
    pub const UC_STACK: u64 = 24;
    pub const MCONTEXT: u64 = 48;
    pub const UC_SIGMASK: u64 = 304;
    pub const SIGINFO: u64 = 312;
    // sigcontext fields, relative to MCONTEXT.
    pub const R8: u64 = 0;
    pub const R9: u64 = 8;
    pub const R10: u64 = 16;
    pub const R11: u64 = 24;
    pub const R12: u64 = 32;
    pub const R13: u64 = 40;
    pub const R14: u64 = 48;
    pub const R15: u64 = 56;
    pub const RDI: u64 = 64;
    pub const RSI: u64 = 72;
    pub const RBP: u64 = 80;
    pub const RBX: u64 = 88;
    pub const RDX: u64 = 96;
    pub const RAX: u64 = 104;
    pub const RCX: u64 = 112;
    pub const RSP: u64 = 120;
    pub const RIP: u64 = 128;
    pub const EFLAGS: u64 = 136;
    pub const CS: u64 = 144;
    pub const GS: u64 = 146;
    pub const FS: u64 = 148;
    pub const SS: u64 = 150;
    pub const ERR: u64 = 152;
    pub const TRAPNO: u64 = 160;
    pub const OLDMASK: u64 = 168;
    pub const CR2: u64 = 176;
    pub const FPSTATE: u64 = 184;
}

/// Result of laying a handler frame on the user stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameResult {
    /// Entry point the task resumes at.
    pub rip: u64,
    /// Stack pointer the handler runs with (points at `pretcode`).
    pub rsp: u64,
    /// Address of the `siginfo_t` passed to an `SA_SIGINFO` handler.
    pub info: u64,
    /// Address of the `ucontext_t` passed to an `SA_SIGINFO` handler.
    pub ucontext: u64,
}

pub(super) fn write_u64(addr: u64, value: u64) {
    // Safety: the caller works within a mapped user stack.
    unsafe { user_ptr::write::<u64>(addr, value) };
}

pub(super) fn write_u32(addr: u64, value: u32) {
    // Safety: the caller works within a mapped user stack.
    unsafe { user_ptr::write::<u32>(addr, value) };
}

pub(super) fn write_i32(addr: u64, value: i32) {
    // Safety: the caller works within a mapped user stack.
    unsafe { user_ptr::write::<i32>(addr, value) };
}

pub(super) fn write_u16(addr: u64, value: u16) {
    // Safety: the caller works within a mapped user stack.
    unsafe { user_ptr::write::<u16>(addr, value) };
}

pub(super) fn read_u64(addr: u64) -> u64 {
    // Safety: the caller works within a mapped user stack.
    unsafe { user_ptr::read::<u64>(addr) }
}

/// Line up the frame below `stack_top`, leaving the red zone free. `None` when
/// the stack cannot hold it (tiny `rsp`, or not a user address).
pub(super) fn frame_base(stack_top: u64) -> Option<u64> {
    harden::frame_below(stack_top, RED_ZONE + LINUX_FRAME_SIZE)
}

/// Build the Linux `rt_sigframe` at the top of `stack_top`. Writes the
/// restorer pointer, `ucontext_t` (with the interrupted registers and the
/// pre-handler mask) and `siginfo_t`, and returns the handler's entry context.
/// The action's `sa_mask` composition is done by the caller before it calls
/// this: `saved_mask` is what `rt_sigreturn` will restore. `mask` and
/// `saved_mask` are in kernel bit order; both are translated to Linux
/// `sigset_t` bit order as they are written into the frame. `None` when the
/// frame does not fit below `stack_top`.
#[allow(clippy::too_many_arguments)] // each field is independently meaningful ABI-frame state
pub fn build_linux_frame(
    stack_top: u64,
    regs: &UserRegs,
    sig: u8,
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
    saved_mask: u64,
    info: &SigInfo,
) -> Option<FrameResult> {
    let frame = frame_base(stack_top)?;
    write_u64(frame, restorer);
    // ucontext_t.
    write_u64(frame + lf::UC_FLAGS, 0);
    write_u64(frame + lf::UC_LINK, 0);
    // uc_stack: report the (empty by default) alternate stack slot musl reads.
    write_u64(frame + lf::UC_STACK, 0);
    write_u32(frame + lf::UC_STACK + 8, 0);
    write_u64(frame + lf::UC_STACK + 16, 0);
    let mc = frame + lf::MCONTEXT;
    write_u64(mc + lf::R8, regs.r8);
    write_u64(mc + lf::R9, regs.r9);
    write_u64(mc + lf::R10, regs.r10);
    write_u64(mc + lf::R11, regs.r11);
    write_u64(mc + lf::R12, regs.r12);
    write_u64(mc + lf::R13, regs.r13);
    write_u64(mc + lf::R14, regs.r14);
    write_u64(mc + lf::R15, regs.r15);
    write_u64(mc + lf::RDI, regs.rdi);
    write_u64(mc + lf::RSI, regs.rsi);
    write_u64(mc + lf::RBP, regs.rbp);
    write_u64(mc + lf::RBX, regs.rbx);
    write_u64(mc + lf::RDX, regs.rdx);
    write_u64(mc + lf::RAX, regs.rax);
    write_u64(mc + lf::RCX, regs.rcx);
    write_u64(mc + lf::RSP, regs.rsp);
    write_u64(mc + lf::RIP, regs.rip);
    write_u64(mc + lf::EFLAGS, regs.rflags);
    let selectors = crate::arch::gdt::selectors();
    write_u16(mc + lf::CS, selectors.user_code);
    write_u16(mc + lf::GS, 0);
    write_u16(mc + lf::FS, 0);
    write_u16(mc + lf::SS, selectors.user_data);
    write_u64(mc + lf::ERR, 0);
    write_u64(mc + lf::TRAPNO, 0);
    write_u64(mc + lf::OLDMASK, kernel_to_linux_sigset(mask));
    write_u64(mc + lf::CR2, 0);
    write_u64(mc + lf::FPSTATE, 0);
    write_u64(frame + lf::UC_SIGMASK, kernel_to_linux_sigset(saved_mask));
    // siginfo_t.
    let si = frame + lf::SIGINFO;
    write_i32(si, sig as i32);
    write_i32(si + 4, 0);
    write_i32(si + 8, info.code);
    write_u32(si + 12, 0);
    if is_fault_info(sig, info.code) {
        write_u64(si + 16, info.addr);
    } else {
        write_u32(si + 16, info.pid as u32);
        write_u32(si + 20, info.uid);
    }

    // SIG_DFL/SIG_IGN cannot be reached here (the shim replaces them with a
    // Default/Ignore disposition), so `handler` is a real user address.
    let _ = (flags, SIG_DFL, SIG_IGN);
    Some(FrameResult {
        rip: handler,
        rsp: frame,
        info: si,
        ucontext: frame + lf::UC_FLAGS,
    })
}

/// Word layout of a native signal frame: return context first, then the
/// interrupted registers, so a future native `sigreturn` can pop them.
pub(super) const NATIVE_FRAME_WORDS: u64 = 20;
pub(super) const NATIVE_FRAME_SIZE: u64 = NATIVE_FRAME_WORDS * 8;

/// Minimal native frame: `[old_rip, old_rsp, old_rflags, sig, GP regs...]`.
/// The handler starts with RSP pointing at `old_rip`, so a bare `ret` returns
/// to the interrupted instruction (register state is not restored; native
/// programs have no restorer yet).
pub fn build_native_frame(stack_top: u64, regs: &UserRegs, sig: u8) -> Option<FrameResult> {
    let frame = harden::frame_below(stack_top, RED_ZONE + NATIVE_FRAME_SIZE)?;
    write_u64(frame, regs.rip);
    write_u64(frame + 8, regs.rsp);
    write_u64(frame + 16, regs.rflags);
    write_u64(frame + 24, sig as u64);
    let gp = [
        regs.r15, regs.r14, regs.r13, regs.r12, regs.r11, regs.r10, regs.r9, regs.r8, regs.rbp,
        regs.rdi, regs.rsi, regs.rdx, regs.rcx, regs.rbx, regs.rax,
    ];
    for (i, value) in gp.iter().enumerate() {
        write_u64(frame + 32 + i as u64 * 8, *value);
    }
    Some(FrameResult {
        rip: 0, // filled by the caller with the handler address
        rsp: frame,
        info: 0,
        ucontext: 0,
    })
}

/// Parse a native frame back, as a native sigreturn would. Used by the
/// in-kernel round-trip test until a native `sigreturn` syscall exists.
#[allow(dead_code)]
pub fn parse_native_frame(frame: u64) -> (UserRegs, u8) {
    let mut regs = UserRegs {
        rip: read_u64(frame),
        rsp: read_u64(frame + 8),
        rflags: read_u64(frame + 16),
        ..UserRegs::default()
    };
    let gp = [
        &mut regs.r15,
        &mut regs.r14,
        &mut regs.r13,
        &mut regs.r12,
        &mut regs.r11,
        &mut regs.r10,
        &mut regs.r9,
        &mut regs.r8,
        &mut regs.rbp,
        &mut regs.rdi,
        &mut regs.rsi,
        &mut regs.rdx,
        &mut regs.rcx,
        &mut regs.rbx,
        &mut regs.rax,
    ];
    for (i, slot) in gp.into_iter().enumerate() {
        *slot = read_u64(frame + 32 + i as u64 * 8);
    }
    (regs, read_u64(frame + 24) as u8)
}

// ---------------------------------------------------------------------------
// Saved user frame access (timer and page-fault layouts)
// ---------------------------------------------------------------------------

pub(super) fn frame_word(rsp: u64, index: usize) -> u64 {
    // Safety: `rsp` points at an interrupt frame the kernel saved.
    unsafe { crate::task::sys::frame_word(rsp, index) }
}

pub(super) fn put_frame_word(rsp: u64, index: usize, value: u64) {
    // Safety: `rsp` points at an interrupt frame the kernel saved.
    unsafe { crate::task::sys::put_frame_word(rsp, index, value) };
}

/// Read a saved interrupt frame into a register context. `rip_index` is 15 for
/// timer/syscall frames and 16 for a page fault (whose error code sits first).
pub(crate) unsafe fn regs_from_frame(rsp: u64, rip_index: usize) -> UserRegs {
    UserRegs {
        r15: frame_word(rsp, 0),
        r14: frame_word(rsp, 1),
        r13: frame_word(rsp, 2),
        r12: frame_word(rsp, 3),
        r11: frame_word(rsp, 4),
        r10: frame_word(rsp, 5),
        r9: frame_word(rsp, 6),
        r8: frame_word(rsp, 7),
        rbp: frame_word(rsp, 8),
        rdi: frame_word(rsp, 9),
        rsi: frame_word(rsp, 10),
        rdx: frame_word(rsp, 11),
        rcx: frame_word(rsp, 12),
        rbx: frame_word(rsp, 13),
        rax: frame_word(rsp, 14),
        rip: frame_word(rsp, rip_index),
        rsp: frame_word(rsp, rip_index + 3),
        rflags: frame_word(rsp, rip_index + 2),
    }
}

/// Rewrite a saved interrupt frame in place; the inverse of [`regs_from_frame`].
pub(crate) unsafe fn apply_regs_to_frame(rsp: u64, regs: &UserRegs, rip_index: usize) {
    let gp = [
        regs.r15, regs.r14, regs.r13, regs.r12, regs.r11, regs.r10, regs.r9, regs.r8, regs.rbp,
        regs.rdi, regs.rsi, regs.rdx, regs.rcx, regs.rbx, regs.rax,
    ];
    for (i, value) in gp.iter().enumerate() {
        put_frame_word(rsp, i, *value);
    }
    put_frame_word(rsp, rip_index, regs.rip);
    put_frame_word(rsp, rip_index + 2, regs.rflags);
    put_frame_word(rsp, rip_index + 3, regs.rsp);
}

/// Whether a saved frame's `CS` says ring 3.
pub(super) fn frame_is_user(rsp: u64, rip_index: usize) -> bool {
    frame_word(rsp, rip_index + 1) & 3 == 3
}
