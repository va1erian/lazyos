//! The `int 0x80` syscall gate: register save stub, dispatch table and the
//! test-harness entry (split out of `process/mod.rs`, issue #194).
//!
//! Each numbered syscall documents its own ABI where its handler lives; this
//! file is only the routing table.

use core::arch::global_asm;
use x86_64::structures::idt::HandlerFunc;

use super::{
    exit, fsops, sys_args, sys_clock, sys_creds, sys_quota, sys_read_char, sys_read_file, sys_sbrk,
    sys_spawn, sys_tasks, sys_wait, sys_write,
};
use crate::task;

/// Saved general-purpose registers, laid out to match the syscall stub's pushes.
#[repr(C)]
struct Regs {
    rax: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
}

// Syscall entry stub: save argument registers, dispatch, restore, iretq. An
// interrupt gate keeps the caller's direction flag, so it is cleared before
// any Rust runs (issue #405; `task::switch` has the full story).
global_asm!(
    r#"
    .global syscall_isr
    syscall_isr:
        push rdi
        push rsi
        push rdx
        push r8
        push r9
        push r10
        push rax
        cld                         /* see task::switch: DF may be set */
        mov rdi, rsp
        call syscall_dispatch
        pop rax
        pop r10
        pop r9
        pop r8
        pop rdx
        pop rsi
        pop rdi
        iretq
    "#
);

extern "C" {
    fn syscall_isr();
}

/// The handler to install at vector `0x80` (DPL 3).
pub fn syscall_gate() -> HandlerFunc {
    // Safety: `syscall_isr` is a naked ISR with a compatible (no ABI) signature.
    unsafe { core::mem::transmute::<*const (), HandlerFunc>(syscall_isr as *const ()) }
}

#[no_mangle]
extern "C" fn syscall_dispatch(regs: *mut Regs) {
    // Safety: the stub passes a valid pointer to saved registers.
    let regs = unsafe { &mut *regs };
    #[cfg(lazyos_tests)]
    task::harness::note_entry_flags();
    // Reclaim slots the scheduler flagged (issue #133): on a syscall entry the
    // current task holds no heap lock, so dropping dead tasks is safe.
    task::reclaim_pending();
    // Post interrupt notifications for claimed device lines and expire ack
    // deadlines (issue #240): the ISR only records the interrupt, so this is
    // the task-context half. Free when nothing is pending.
    crate::dev::intx::service();
    if regs.rax == 0 {
        exit(regs.rdi as u32);
    }
    regs.rax = match regs.rax {
        1 => sys_write(regs.rdi, regs.rsi),
        2 => sys_read_char(),
        3 => sys_read_file(regs.rdi, regs.rsi, regs.rdx),
        4 => sys_sbrk(regs.rdi),
        // 5: the native Messenger surface (issue #69): `rdi` is the op code,
        // `rsi` points at a `MsgArgs` block and `rdx` at a `MsgResult` block.
        5 => crate::ipc::syscalls::dispatch(regs.rdi, regs.rsi, regs.rdx),
        // 6..9: the service supervision surface (issue #93).
        6 => sys_spawn(regs.rdi),
        7 => sys_wait(regs.rdi),
        8 => sys_clock(),
        9 => sys_args(regs.rdi, regs.rsi),
        // 10: the credential gate (issue #101), see the module docs.
        10 => sys_creds(regs.rdi, regs.rsi, regs.rdx),
        // 11: per-uid quota introspection (issue #103), read-only.
        11 => sys_quota(regs.rdi),
        // 12: the display device grant (issue #113), see the module docs.
        12 => crate::display::dispatch(regs.rdi, regs.rsi, regs.rdx),
        // 13: scheduler task-list introspection (MCP debug bridge Phase 2),
        // read-only.
        13 => sys_tasks(regs.rdi),
        // 14: the system-stats snapshot (issue #144), read-only and available
        // to every task; see `crate::sysinfo` and the module docs.
        14 => crate::sysinfo::dispatch(regs.rdi, regs.rsi, regs.rdx),
        // 15..22: the shell's filesystem calls, `power`, and `fsync` (issue
        // #6, #260).
        15..=22 => fsops::dispatch(regs.rax, regs.rdi, regs.rsi, regs.rdx),
        // 23: the device syscall (issue #240): userspace drivers claim a device
        // and reach its BARs, ports, config space and interrupt through a
        // `Device` handle; see `crate::dev::syscall`.
        23 => crate::dev::syscall::dispatch(regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8),
        // 24: the native wall clock (issue #369), see `super::wallsys`.
        24 => super::wallsys::dispatch(regs.rdi, regs.rsi),
        // 25: the raw input event bus (`docs/input-plan.md`), `inputd` only.
        25 => crate::input::rawsys::dispatch(regs.rdi, regs.rsi, regs.rdx),
        // 26: random bytes from the kernel CSPRNG (docs/networking-plan.md N2),
        // open to every task; see `super::randsys`.
        26 => super::randsys::dispatch(regs.rdi, regs.rsi),
        _ => u64::MAX,
    };
}

/// Test-harness entry into the native syscall surface (issue #62 pattern):
/// drive one syscall exactly as the `int 0x80` gate would, without the ring
/// transition. Compiled only for the in-kernel suite.
#[cfg(lazyos_tests)]
pub fn dispatch_for_test(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    match nr {
        1 => sys_write(a1, a2),
        // Only safe to call when the task's stdin is redirected (a terminal
        // stdin would park on the key queue, which the harness never feeds).
        2 => sys_read_char(),
        3 => sys_read_file(a1, a2, a3),
        4 => sys_sbrk(a1),
        5 => crate::ipc::syscalls::dispatch(a1, a2, a3),
        6 => sys_spawn(a1),
        7 => sys_wait(a1),
        8 => sys_clock(),
        9 => sys_args(a1, a2),
        10 => sys_creds(a1, a2, a3),
        11 => sys_quota(a1),
        12 => crate::display::dispatch(a1, a2, a3),
        13 => sys_tasks(a1),
        14 => crate::sysinfo::dispatch(a1, a2, a3),
        15..=22 => fsops::dispatch(nr, a1, a2, a3),
        23 => crate::dev::syscall::dispatch(a1, a2, a3, 0, 0),
        24 => super::wallsys::dispatch(a1, a2),
        25 => crate::input::rawsys::dispatch(a1, a2, a3),
        26 => super::randsys::dispatch(a1, a2),
        _ => u64::MAX,
    }
}
