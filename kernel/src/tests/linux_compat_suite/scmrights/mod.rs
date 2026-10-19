//! `SCM_RIGHTS`: descriptors passed over `AF_UNIX` stream and seqpacket
//! sockets with `sendmsg`/`recvmsg`. Each case runs two processes (a fork
//! with a table of its own) so the receiver's descriptors really are
//! installed in another table.

use super::*;
use crate::ipc::pipe;
use crate::quota::{self, Resource};

mod blocked;
mod containers;
mod delivery;
mod queue;

pub use blocked::*;
pub use containers::*;
pub use delivery::*;
pub use queue::*;

const SENDMSG: u64 = 46;
const RECVMSG: u64 = 47;
const MSG_PEEK: u64 = 0x2;
const MSG_CMSG_CLOEXEC: u64 = 0x4000_0000;
const MSG_CTRUNC: i32 = 0x8;
const MSG_TRUNC: i32 = 0x20;
const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;
const SCM_CREDENTIALS: i32 = 2;
const ENOTSOCK: u64 = neg(88);
const ETOOMANYREFS: u64 = neg(109);
const F_SETFL: u64 = 4;
const O_NONBLOCK: u64 = 0o4000;

/// A control buffer holding one message of `level`/`kind` whose payload is
/// `data`, padded to `CMSG_SPACE`.
fn control_message(level: i32, kind: i32, data: &[u8]) -> Vec<u8> {
    let len = 16 + data.len();
    let mut out = Vec::new();
    out.extend_from_slice(&(len as u64).to_le_bytes());
    out.extend_from_slice(&level.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(data);
    out.resize((len + 7) & !7, 0);
    out
}

/// A `SCM_RIGHTS` control buffer for `fds`.
fn rights_control(fds: &[i32]) -> Vec<u8> {
    let data: Vec<u8> = fds.iter().flat_map(|fd| fd.to_le_bytes()).collect();
    control_message(SOL_SOCKET, SCM_RIGHTS, &data)
}

/// `sendmsg(sock, data, control)`.
fn sendmsg_control(sock: u64, data: &[u8], control: &[u8], flags: u64) -> u64 {
    let iov = [data.as_ptr() as u64, data.len() as u64];
    let (ptr, len) = if control.is_empty() {
        (0, 0)
    } else {
        (control.as_ptr() as u64, control.len() as u64)
    };
    let msg = [0u64, 0, iov.as_ptr() as u64, 1, ptr, len, 0];
    sys(SENDMSG, &[sock, msg.as_ptr() as u64, flags])
}

/// `sendmsg` of `data` carrying `fds` (none: no control buffer at all).
fn send_fds(sock: u64, data: &[u8], fds: &[u64]) -> u64 {
    let fds: Vec<i32> = fds.iter().map(|fd| *fd as i32).collect();
    let control = if fds.is_empty() {
        Vec::new()
    } else {
        rights_control(&fds)
    };
    sendmsg_control(sock, data, &control, 0)
}

/// What one `recvmsg` returned.
struct Got {
    n: u64,
    fds: Vec<u64>,
    flags: i32,
}

/// `recvmsg` into `buf` with room for `room` descriptors (`None`: no control
/// buffer at all), decoding the `SCM_RIGHTS` message it wrote.
fn recv_fds(sock: u64, buf: &mut [u8], room: Option<usize>, flags: u64) -> Got {
    // CMSG_LEN, not CMSG_SPACE: the padding would hold one more descriptor.
    let control_len = room.map_or(0, |room| 16 + room * 4);
    let mut control = alloc::vec![0xAAu8; control_len];
    let iov = [buf.as_mut_ptr() as u64, buf.len() as u64];
    let control_ptr = if control_len == 0 {
        0
    } else {
        control.as_mut_ptr() as u64
    };
    let mut msg = [
        0u64,
        0,
        iov.as_ptr() as u64,
        1,
        control_ptr,
        control_len as u64,
        0xffff_ffff,
    ];
    let n = sys(RECVMSG, &[sock, msg.as_mut_ptr() as u64, flags]);
    let mut fds = Vec::new();
    let used = msg[5] as usize;
    if (n as i64) >= 0 && used >= 16 {
        let cmsg_len = u64::from_le_bytes(control[..8].try_into().unwrap()) as usize;
        let level = i32::from_le_bytes(control[8..12].try_into().unwrap());
        let kind = i32::from_le_bytes(control[12..16].try_into().unwrap());
        if level == SOL_SOCKET && kind == SCM_RIGHTS {
            for chunk in control[16..cmsg_len].chunks(4) {
                fds.push(i32::from_le_bytes(chunk.try_into().unwrap()) as u64);
            }
        }
    }
    Got {
        n,
        fds,
        flags: msg[6] as u32 as i32,
    }
}

/// A seqpacket pair.
fn seqpair() -> Result<(u64, u64), String> {
    let mut sv = [0i32; 2];
    check!(
        sys(53, &[1, 5, 0, sv.as_mut_ptr() as u64]) == 0,
        "seqpacket pair"
    );
    Ok((sv[0] as u64, sv[1] as u64))
}

/// A pipe whose read end never blocks, so a leaked write end shows up as
/// `EAGAIN` instead of a hung test.
fn pipe_nonblocking() -> Result<(u64, u64), String> {
    let (r, w) = pipe()?;
    check!(
        sys(72, &[r, F_SETFL, O_NONBLOCK]) == 0,
        "O_NONBLOCK on the read end"
    );
    Ok((r, w))
}

/// Read end `r` is at end-of-file: every write end is closed.
fn at_eof(r: u64) -> bool {
    let mut byte = [0u8; 1];
    sys(0, &[r, byte.as_mut_ptr() as u64, 1]) == 0
}

/// A leader process and a forked child, both holding a stream pair `(a, b)`,
/// a seqpacket pair `(sa, sb)` and a pipe `(r, w)`; the current process is
/// the leader.
struct Pair {
    leader: usize,
    child: usize,
    a: u64,
    b: u64,
    sa: u64,
    sb: u64,
    r: u64,
    w: u64,
}

fn processes() -> Result<Pair, String> {
    fresh()?;
    let leader = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
    task::harness::switch_current(leader);
    let (a, b) = socketpair()?;
    let (sa, sb) = seqpair()?;
    let (r, w) = pipe_nonblocking()?;
    let child = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
    Ok(Pair {
        leader,
        child,
        a,
        b,
        sa,
        sb,
        r,
        w,
    })
}

fn close(fd: u64) {
    sys(3, &[fd]);
}

fn open_count(slot: usize) -> usize {
    task::harness::fd_open_count(slot)
}

/// Nothing is left in flight and every pipe is gone.
fn settled(pipes_before: usize) -> Result<(), String> {
    check!(
        pipe::in_flight() == 0,
        "{} descriptors still in flight",
        pipe::in_flight()
    );
    check!(
        pipe::Pipe::live() <= pipes_before,
        "pipes leaked: {} -> {}",
        pipes_before,
        pipe::Pipe::live()
    );
    Ok(())
}
