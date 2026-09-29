//! Native wall-clock syscall 24 (issue #369): the UTC clock for native
//! services, so `timed` does not need the Linux ABI's `clock_gettime`.
//!
//! | `rdi` op | call | `rsi` | result |
//! |----------|------|-------|--------|
//! | 0 | `get` | - | UTC centiseconds since the Unix epoch |
//! | 1 | `set` | UTC whole seconds | 0, or `-EPERM` / `-EINVAL` |
//!
//! `set` needs `CAP_SYS_TIME`, checked before the argument so an unprivileged
//! caller learns nothing about what would have been valid, and accepts the
//! same range as `clock_settime` (`0..wallclock::MAX_SET_SECS`).

use crate::ipc::credentials::{self, CAP_SYS_TIME};
use crate::{task, wallclock};

/// Native wall-clock ops (syscall 24).
pub mod op {
    /// Read UTC centiseconds since the epoch.
    pub const GET: u64 = 0;
    /// Step the clock to a UTC second count (`CAP_SYS_TIME`).
    pub const SET: u64 = 1;
}

const EPERM: i64 = 1;
const EINVAL: i64 = 22;

fn negative(errno: i64) -> u64 {
    errno.wrapping_neg() as u64
}

/// Route one syscall-24 call.
pub fn dispatch(operation: u64, arg: u64) -> u64 {
    match operation {
        op::GET => {
            let (secs, centis) = wallclock::now();
            (secs as u64) * 100 + u64::from(centis)
        }
        op::SET => set(arg),
        _ => negative(EINVAL),
    }
}

fn set(secs: u64) -> u64 {
    if !credentials::of(task::current()).has_cap(CAP_SYS_TIME) {
        return negative(EPERM);
    }
    if secs >= wallclock::MAX_SET_SECS as u64 {
        return negative(EINVAL);
    }
    wallclock::set(secs as i64, 0);
    0
}
