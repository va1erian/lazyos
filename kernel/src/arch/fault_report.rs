//! The diagnostic a ring-3 fatal fault prints before the process is ended
//! (issue #375).
//!
//! A wild jump (`rip 0x2`, a `ret` into the heap) is only diagnosable from a
//! CI log if the log carries the state around it: the full register set, the
//! task's last syscalls and handler frames ([`crate::task::trace`]), its
//! signal masks and installed handlers, and the top of its user stack (the
//! return addresses a corrupted frame would have popped). Every line starts
//! with `user:` so a log grep finds the whole report next to the existing
//! `user: task N killed by ...` line.

use crate::task::signal::{self, Disposition, UserRegs, NSIG};
use crate::task::trace::{self, Event};

/// Words of the user stack printed from `rsp` upward.
const STACK_WORDS: usize = 16;
/// Instruction bytes printed from `rip` (the longest x86 instruction is 15).
const CODE_BYTES: u64 = 16;

/// Print everything known about the faulting task `slot`. `regs` is the
/// interrupted register set when the exception stub saved one (the naked
/// `#PF`/`#GP`/`#UD`/`#DE` ISRs); the `x86-interrupt` handlers have none.
pub fn print(slot: usize, regs: Option<&UserRegs>) {
    if let Some(regs) = regs {
        print_registers(regs);
        print_code(regs.rip);
        print_stack(regs.rsp);
    }
    print_signals(slot);
    print_history(slot);
}

fn print_registers(regs: &UserRegs) {
    crate::serial_println!(
        "user: regs rip={:#x} rsp={:#x} rflags={:#x} rax={:#x} rbx={:#x} rcx={:#x} rdx={:#x}",
        regs.rip,
        regs.rsp,
        regs.rflags,
        regs.rax,
        regs.rbx,
        regs.rcx,
        regs.rdx
    );
    crate::serial_println!(
        "user: regs rsi={:#x} rdi={:#x} rbp={:#x} r8={:#x} r9={:#x} r10={:#x} r11={:#x}",
        regs.rsi,
        regs.rdi,
        regs.rbp,
        regs.r8,
        regs.r9,
        regs.r10,
        regs.r11
    );
    crate::serial_println!(
        "user: regs r12={:#x} r13={:#x} r14={:#x} r15={:#x}",
        regs.r12,
        regs.r13,
        regs.r14,
        regs.r15
    );
}

/// The bytes at `rip`, the way a debugger's "code:" line shows them: an
/// `#UD` is only diagnosable (a `ud2`, a VEX prefix without AVX state, a
/// garbled JIT page) from the instruction itself. An unreadable byte (the
/// page is gone, or the jump was wild) prints as `??`.
fn print_code(rip: u64) {
    crate::serial_println!("{}", code_line(rip));
}

/// The report line for the instruction bytes at `rip`.
///
/// `rip` is whatever the faulting task jumped to: a kernel address, a
/// non-canonical one, an unmapped one. The read goes through
/// [`user_ptr::try_resident_bytes`], which walks the faulting task's page
/// table and refuses anything at or above the user limit (kernel and
/// non-canonical addresses) and any page that is not present and user
/// readable. It never demand-pages: a fault report must not allocate frames or
/// change the address space of the task it describes. The 16 bytes are read
/// as at most two ranges (the instruction may cross into a page that is not
/// there), so the bytes of the first page still print when the second is gone.
pub fn code_line(rip: u64) -> alloc::string::String {
    use core::fmt::Write;
    let mut bytes = [None; CODE_BYTES as usize];
    let first = (CODE_BYTES.min(0x1000 - (rip & 0xfff))) as usize;
    for (start, len) in [(0, first), (first, CODE_BYTES as usize - first)] {
        if len == 0 {
            continue;
        }
        let range = rip
            .checked_add(start as u64)
            .and_then(|at| crate::user_ptr::try_resident_bytes(at, len).ok());
        if let Some(range) = range {
            for (slot, byte) in bytes[start..start + len].iter_mut().zip(range) {
                *slot = Some(*byte);
            }
        }
    }
    let mut line = alloc::string::String::new();
    let _ = write!(line, "user: code at {rip:#x}:");
    for byte in bytes {
        match byte {
            Some(byte) => {
                let _ = write!(line, " {byte:02x}");
            }
            None => line.push_str(" ??"),
        }
    }
    line
}

/// The words above `rsp`, four per line; an unmapped word prints as `?`. The
/// faulting task's own table is active, so the reads see its memory, and (as
/// for [`code_line`]) a page that is not present is not materialized.
fn print_stack(rsp: u64) {
    for line in 0..STACK_WORDS / 4 {
        let base = rsp.wrapping_add((line * 4 * 8) as u64);
        let word =
            |i: u64| match crate::user_ptr::try_read_resident::<u64>(base.wrapping_add(i * 8)) {
                Ok(value) => alloc::format!("{value:#018x}"),
                Err(_) => alloc::string::String::from("?"),
            };
        crate::serial_println!(
            "user: stack {base:#x}: {} {} {} {}",
            word(0),
            word(1),
            word(2),
            word(3)
        );
    }
}

/// Pending and blocked masks (kernel bit order, `1 << sig`) and every signal
/// whose disposition is not the default.
fn print_signals(slot: usize) {
    crate::serial_println!(
        "user: signals pending={:#x} blocked={:#x}",
        signal::pending(slot),
        signal::blocked(slot)
    );
    for sig in 1..NSIG as u8 {
        match signal::action(slot, sig) {
            Disposition::Default => {}
            Disposition::Ignore => crate::serial_println!("user: sigaction {sig}: ignore"),
            Disposition::Handler {
                handler,
                flags,
                restorer,
                mask,
            } => crate::serial_println!(
                "user: sigaction {sig}: handler={handler:#x} restorer={restorer:#x} flags={flags:#x} mask={mask:#x}"
            ),
        }
    }
}

/// The task's recorded syscalls and handler frames, oldest first.
fn print_history(slot: usize) {
    for event in trace::history(slot) {
        match event {
            Event::None => {}
            Event::Syscall { nr, a1, result } => {
                crate::serial_println!("user: trace syscall {nr} a1={a1:#x} -> {result:#x}")
            }
            Event::Signal {
                sig,
                via: trace::Via::Fault { addr },
                rip,
                rsp,
            } => crate::serial_println!(
                "user: trace signal {sig} via fault si_addr={addr:#x} over rip={rip:#x} rsp={rsp:#x}"
            ),
            Event::Signal { sig, via, rip, rsp } => crate::serial_println!(
                "user: trace signal {sig} via {} over rip={rip:#x} rsp={rsp:#x}",
                via.label()
            ),
        }
    }
}
