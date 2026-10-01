//! Native `kill` syscall 29: a supervisor stops one task it started.
//!
//! `rdi` is the target's task slot (what `spawn` returned), `rsi` the signal:
//! `0` (an existence and permission probe), `SIGTERM` or `SIGKILL`. It is the
//! single-target half of Linux `kill(2)` and nothing more: group and broadcast
//! forms (`pid <= 0`) are refused with `-EINVAL`, so a native service cannot
//! fan a signal out, and every other signal number is `-EINVAL` as well (native
//! programs install no handlers, so the two terminating signals and the probe are
//! all there is to ask for).
//!
//! The permission rule is the one every sender goes through
//! (`task::signal::send`): the target must share the caller's uid or the caller
//! must hold `CAP_KILL`. `init` is root, which is how it stops an installed app
//! running as the session's user; an ordinary task cannot signal a task of
//! another uid. Errors are `-errno`: `-ESRCH` (no such task), `-EPERM`.
//! `kill(2)` itself is also reachable by Linux-ABI programs; this call exists
//! because a native supervisor has no other way to end a child it must replace.

use crate::task::signal::{self, SigInfo, SignalError};

const EPERM: i64 = 1;
const ESRCH: i64 = 3;
const EINVAL: i64 = 22;

fn failed(errno: i64) -> u64 {
    errno.wrapping_neg() as u64
}

/// Whether `sig` is one this call accepts.
fn allowed_signal(sig: u64) -> bool {
    sig == 0 || sig == u64::from(signal::SIGTERM) || sig == u64::from(signal::SIGKILL)
}

/// Route one syscall-29 call.
pub fn dispatch(pid: u64, sig: u64) -> u64 {
    // `pid` is a slot: a value that is zero, or does not fit a positive `i64`,
    // would be a group or broadcast form in `signal::kill`.
    let Ok(target) = i64::try_from(pid) else {
        return failed(EINVAL);
    };
    if target <= 0 || !allowed_signal(sig) {
        return failed(EINVAL);
    }
    let me = crate::task::current();
    let info = SigInfo::user(me, signal::SI_USER);
    match signal::kill(me, target, sig as u8, info) {
        Ok(()) => 0,
        Err(SignalError::NoSuchProcess) => failed(ESRCH),
        Err(SignalError::NotPermitted) => failed(EPERM),
        Err(SignalError::Invalid) => failed(EINVAL),
    }
}
