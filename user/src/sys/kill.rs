//! The native `kill` syscall (29): a supervisor ending one task it started.
//! See `kernel/src/process/killsys.rs` for the rules.

use core::arch::asm;

/// `kill(slot, sig)` — end one task.
pub const SYS_KILL: u64 = 29;

/// Signal `0`: check that the task exists and may be signalled.
pub const SIG_PROBE: u64 = 0;
/// `SIGTERM`.
pub const SIG_TERM: u64 = 15;
/// `SIGKILL`: cannot be caught or ignored.
pub const SIG_KILL: u64 = 9;

/// Send `sig` (0, [`SIG_TERM`] or [`SIG_KILL`]) to the task in `slot`, the pid
/// `spawn` returned. The caller must share the target's uid or hold `CAP_KILL`.
/// The error is the negative errno (`-ESRCH`, `-EPERM`, `-EINVAL`).
pub fn kill(slot: u64, sig: u64) -> Result<(), i64> {
    let code: u64;
    // SAFETY: `int 0x80` with syscall 29; no pointers cross the gate and the
    // kernel validates the slot and the signal.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_KILL,
            in("rdi") slot,
            in("rsi") sig,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    match code as i64 {
        0 => Ok(()),
        error => Err(error),
    }
}
