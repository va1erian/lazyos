//! Native syscall 27, the `AF_INET` pump (docs/networking-plan.md, stage N5).
//!
//! The userspace stack `netd` serves Linux programs' sockets through this one
//! call; the kernel side is [`crate::ipc::inet::pump`]. Registers (the device
//! syscall's layout):
//!
//! ```text
//!   rax = 27  rdi = op  rsi = a1  rdx = a2  r10 = a3        -> value | -errno
//!   ATTACH(0)      -                                   -> 0
//!   NEXT(1)        a1 = 24-byte buffer                 -> 1 (filled) | 0 (none)
//!   READ(2)        a1 = id, a2 = buffer, a3 = length   -> bytes | 0 (empty) | -EPIPE (end)
//!   WRITE(3)       a1 = id, a2 = buffer, a3 = length   -> bytes taken | 0 (full) | -EPIPE (gone)
//!   COMPLETE(4)    a1 = id, a2 = status, a3 = 16-byte address block -> 0
//!   ACCEPTED(5)    a1 = listener id, a2 = address block -> the new id
//!   EOF(6)         a1 = id                             -> 0
//!   ERROR(7)       a1 = id, a2 = errno                 -> 0
//!   CLOSE_ACK(8)   a1 = id                             -> 0
//!   STATS(9)       -                                   -> sockets | queued << 16
//! ```
//!
//! The *address block* is local address (4 octets, port as a little-endian
//! `u16`), then the peer's the same way, then four reserved bytes.
//!
//! **Authority.** Only the stack may drive this: `ATTACH` is refused unless
//! the caller is root or `_netd` (uid 903), and every other op unless the
//! caller is the task that attached. Nothing else can read or answer another
//! task's socket requests.

use crate::ipc::credentials;
use crate::ipc::inet::{self, Addr, Io};
use crate::task;
use crate::user_ptr;

/// `_netd`'s uid (`libs/netpolicy::NETD_UID`).
const NETD_UID: u32 = 903;
const EPERM: i64 = 1;
const EFAULT: i64 = 14;
const EINVAL: i64 = 22;
const EPIPE: i64 = 32;
const ENOSYS: i64 = 38;

/// Most bytes one READ or WRITE looks at (a full datagram message with its
/// header is 1478 bytes; a stream moves at most a ring's worth anyway). Both
/// work on the caller's memory in place, so this only bounds one call.
const MAX_IO: usize = 4 << 20;
/// Bytes of the address block.
const ADDR_BLOCK: usize = 16;

fn fail(errno: i64) -> u64 {
    (errno as u64).wrapping_neg()
}

fn result(outcome: Result<u64, i32>) -> u64 {
    outcome.unwrap_or_else(|errno| fail(i64::from(errno)))
}

fn addr_at(block: &[u8], at: usize) -> Addr {
    Addr {
        ip: [block[at], block[at + 1], block[at + 2], block[at + 3]],
        port: u16::from_le_bytes([block[at + 4], block[at + 5]]),
    }
}

fn read_block(ptr: u64) -> Result<(Addr, Addr), u64> {
    let block = user_ptr::try_bytes(ptr, ADDR_BLOCK).map_err(|_| fail(EFAULT))?;
    Ok((addr_at(block, 0), addr_at(block, 6)))
}

/// Route one syscall-27 call.
pub fn dispatch(op: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let me = task::current();
    if op == 0 {
        let uid = credentials::of(me).uid;
        if uid != 0 && uid != NETD_UID {
            return fail(EPERM);
        }
        inet::attach(me);
        return 0;
    }
    if !inet::is_netd(me) {
        return fail(EPERM);
    }
    let id = a1 as u32;
    match op {
        1 => match inet::next_request() {
            Some(request) => match user_ptr::try_copy_to(a1, &inet::encode(&request)) {
                Ok(()) => 1,
                Err(_) => fail(EFAULT),
            },
            None => 0,
        },
        2 => read(id, a2, a3),
        3 => write(id, a2, a3),
        4 => match read_block(a3) {
            Ok((local, peer)) => result(inet::complete(id, a2 as i32, local, peer).map(|()| 0)),
            Err(e) => e,
        },
        5 => match read_block(a2) {
            Ok((local, peer)) => result(inet::accepted(id, peer, local).map(u64::from)),
            Err(e) => e,
        },
        6 => result(inet::net_eof(id).map(|()| 0)),
        7 => result(inet::net_error(id, a2 as i32).map(|()| 0)),
        8 => result(inet::close_ack(id).map(|()| 0)),
        9 => (inet::live_count() as u64) | (inet::queued_count() as u64) << 16,
        _ => fail(ENOSYS),
    }
}

fn read(id: u32, buf: u64, len: u64) -> u64 {
    let want = (len as usize).min(MAX_IO);
    if want == 0 {
        return fail(EINVAL);
    }
    // Straight into the caller's buffer: `net_read` never blocks, so the
    // borrowed range cannot change under it.
    let Ok(data) = user_ptr::try_bytes_mut(buf, want) else {
        return fail(EFAULT);
    };
    match inet::net_read(id, data) {
        Ok(Io::Data(n)) => n as u64,
        Ok(Io::Empty) => 0,
        Ok(Io::Eof | Io::Gone) => fail(EPIPE),
        Err(e) => fail(i64::from(e)),
    }
}

fn write(id: u32, buf: u64, len: u64) -> u64 {
    let want = (len as usize).min(MAX_IO);
    let Ok(bytes) = user_ptr::try_bytes(buf, want) else {
        return fail(EFAULT);
    };
    match inet::net_write(id, bytes) {
        Ok(Io::Data(n)) => n as u64,
        Ok(Io::Empty) => 0,
        Ok(Io::Eof | Io::Gone) => fail(EPIPE),
        Err(e) => fail(i64::from(e)),
    }
}
