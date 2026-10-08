//! The native `kill` syscall (29): a supervisor ending one task it started.
//! See `kernel/src/process/killsys.rs` for the rules.

use crate::nr;

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
    // SAFETY: syscall 29 takes no pointer.
    let code = unsafe { crate::raw::syscall2(nr::KILL, slot, sig) };
    match code {
        0 => Ok(()),
        error => Err(error),
    }
}
