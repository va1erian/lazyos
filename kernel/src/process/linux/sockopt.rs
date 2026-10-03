//! `setsockopt` and `getsockopt` on an `AF_INET` socket.
//!
//! `SO_RCVTIMEO` and `SO_SNDTIMEO` take a `struct timeval` and work as on
//! Linux (docs/tls-plan.md §5.4): the value is checked (`tv_usec` outside
//! `0..1_000_000` is `EDOM`, a short `optlen` `EINVAL`), `{0, 0}` means no
//! timeout, a negative `tv_sec` gives up at once, and the stored value is
//! rounded up to whole scheduler ticks, which `getsockopt` reports back. The
//! other options programs set as a matter of course are accepted and have no
//! effect (buffer sizes, keepalive, address reuse, `TCP_NODELAY`); their value
//! pointer is still validated.

use crate::ipc::inet::{Dir, Kind, State};
use crate::user_ptr;

use super::errno::{err, EFAULT, EINVAL, ENOPROTOOPT};
use super::flags::SOCK_STREAM;
use super::inet::inet_of;

const SOL_SOCKET: u64 = 1;
const SO_TYPE: u64 = 3;
const SO_ERROR: u64 = 4;
const SO_SNDBUF: u64 = 7;
const SO_RCVBUF: u64 = 8;
const SO_RCVTIMEO: u64 = 20;
const SO_SNDTIMEO: u64 = 21;
const SO_ACCEPTCONN: u64 = 30;
/// The 64-bit-`time_t` spellings; on x86_64 the layout is the same.
const SO_RCVTIMEO_NEW: u64 = 66;
const SO_SNDTIMEO_NEW: u64 = 67;
const SOCK_DGRAM: i32 = 2;
/// `EDOM`: Linux's answer to a `tv_usec` out of range.
const EDOM: u64 = 33;

/// `struct timeval` on x86_64: two `i64`s.
const TIMEVAL: usize = 16;
/// Scheduler ticks per second (the PIT runs at 100 Hz).
const HZ: i64 = 100;
const USEC_PER_TICK: i64 = 1_000_000 / HZ;
/// The longest timeout Linux stores; anything at or past it means none.
const MAX_SECS: i64 = i64::MAX / HZ - 1;

/// The timeout an option name stands for, if it is one.
fn timeout_dir(level: u64, name: u64) -> Option<Dir> {
    match (level, name) {
        (SOL_SOCKET, SO_RCVTIMEO | SO_RCVTIMEO_NEW) => Some(Dir::Recv),
        (SOL_SOCKET, SO_SNDTIMEO | SO_SNDTIMEO_NEW) => Some(Dir::Send),
        _ => None,
    }
}

/// A `timeval` as stored ticks (Linux's `sock_set_timeout`): `Ok(None)` is
/// no timeout, `Ok(Some(0))` gives up at once.
pub(super) fn timeval_to_ticks(sec: i64, usec: i64) -> Result<Option<u64>, u64> {
    if !(0..1_000_000).contains(&usec) {
        return Err(EDOM);
    }
    if sec < 0 {
        return Ok(Some(0));
    }
    if sec == 0 && usec == 0 || sec >= MAX_SECS {
        return Ok(None);
    }
    // In range: `sec * HZ` is below `i64::MAX - HZ`, so neither step overflows.
    let ticks = sec * HZ + (usec + USEC_PER_TICK - 1) / USEC_PER_TICK;
    Ok(Some(ticks as u64))
}

/// Stored ticks back as a `timeval` (Linux's `sock_get_timeout`): no timeout
/// reads as `{0, 0}`.
pub(super) fn ticks_to_timeval(ticks: Option<u64>) -> (i64, i64) {
    match ticks {
        None => (0, 0),
        Some(ticks) => {
            let ticks = ticks.min(i64::MAX as u64) as i64;
            (ticks / HZ, (ticks % HZ) * USEC_PER_TICK)
        }
    }
}

/// `setsockopt(fd, level, name, value, len)`.
pub(super) fn sys_setsockopt(fd: u64, level: u64, name: u64, value: u64, len: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    // `optlen` is a C `int`.
    let len = len as u32 as i32;
    if len < 0 {
        return err(EINVAL);
    }
    let len = len as usize;
    if let Some(dir) = timeout_dir(level, name) {
        if len < TIMEVAL {
            return err(EINVAL);
        }
        let Ok(raw) = user_ptr::try_bytes(value, TIMEVAL) else {
            return err(EFAULT);
        };
        let word = |at: usize| i64::from_le_bytes(raw[at..at + 8].try_into().unwrap_or([0; 8]));
        return match timeval_to_ticks(word(0), word(8)) {
            Ok(ticks) => {
                sock.set_timeout(dir, ticks);
                0
            }
            Err(e) => err(e),
        };
    }
    if len > 0 && user_ptr::try_bytes(value, len.min(256)).is_err() {
        return err(EFAULT);
    }
    0
}

/// `getsockopt(fd, level, name, value, lenptr)`: the answer is cut to the
/// caller's length, which is set to what was written, as on Linux.
pub(super) fn sys_getsockopt(fd: u64, level: u64, name: u64, value: u64, lenptr: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    if level != SOL_SOCKET {
        return err(ENOPROTOOPT);
    }
    let mut answer = [0u8; TIMEVAL];
    let size = if let Some(dir) = timeout_dir(level, name) {
        let (sec, usec) = ticks_to_timeval(sock.timeout(dir));
        answer[..8].copy_from_slice(&sec.to_le_bytes());
        answer[8..].copy_from_slice(&usec.to_le_bytes());
        TIMEVAL
    } else {
        let int: i32 = match name {
            SO_ERROR => sock.take_error(),
            SO_TYPE => match sock.kind() {
                Kind::Stream => SOCK_STREAM as i32,
                Kind::Dgram => SOCK_DGRAM,
            },
            SO_RCVBUF | SO_SNDBUF => crate::ipc::pipe::SMALL_CAPACITY as i32,
            SO_ACCEPTCONN => i32::from(sock.state() == State::Listening),
            _ => 0,
        };
        answer[..4].copy_from_slice(&int.to_le_bytes());
        4
    };
    let Ok(room) = user_ptr::try_read::<i32>(lenptr) else {
        return err(EFAULT);
    };
    if room < 0 {
        return err(EINVAL);
    }
    let n = (room as usize).min(size);
    if user_ptr::try_copy_to(value, &answer[..n]).is_err()
        || user_ptr::try_write::<u32>(lenptr, n as u32).is_err()
    {
        return err(EFAULT);
    }
    0
}
