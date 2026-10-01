//! The `AF_INET` pump (syscall 27, docs/networking-plan.md N5): how `netd`
//! serves the sockets of Linux programs. See `kernel/src/process/inetsys.rs`
//! for the contract; only the attached `netd` (or root) may call it.

use core::arch::asm;

/// `inet(op, a1, a2, a3)`.
pub const SYS_INET: u64 = 27;

const ATTACH: u64 = 0;
const NEXT: u64 = 1;
const READ: u64 = 2;
const WRITE: u64 = 3;
const COMPLETE: u64 = 4;
const ACCEPTED: u64 = 5;
const EOF: u64 = 6;
const ERROR: u64 = 7;
const CLOSE_ACK: u64 = 8;
const STATS: u64 = 9;

/// Bytes of a request in the buffer [`inet_next`] fills.
pub const INET_REQUEST_BYTES: usize = 24;
/// Bytes of the address block [`inet_complete`] and [`inet_accepted`] take:
/// local address and port, peer address and port (each 4 octets plus a
/// little-endian `u16`), four reserved bytes.
pub const INET_ADDR_BLOCK: usize = 16;

/// What a byte-moving call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InetIo {
    /// This many bytes moved (never zero).
    Data(usize),
    /// Nothing to read, or no room to write, right now.
    Empty,
    /// Read: the application will send no more. Write: it is gone.
    End,
}

fn call(op: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let result: u64;
    // Safety: `int 0x80` with syscall 27; the kernel validates every pointer
    // against this task's address space before touching it.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_INET,
            in("rdi") op,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            lateout("rax") result,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    result as i64
}

fn unit(code: i64) -> Result<(), i64> {
    if code < 0 {
        Err(code)
    } else {
        Ok(())
    }
}

fn io(code: i64) -> Result<InetIo, i64> {
    match code {
        0 => Ok(InetIo::Empty),
        -32 => Ok(InetIo::End),
        n if n > 0 => Ok(InetIo::Data(n as usize)),
        e => Err(e),
    }
}

/// Announce this task as the pump's server (root or `_netd` only). A new
/// attach discards every socket the previous server had.
pub fn inet_attach() -> Result<(), i64> {
    unit(call(ATTACH, 0, 0, 0))
}

/// The next request the kernel has for the stack, if any.
pub fn inet_next(out: &mut [u8; INET_REQUEST_BYTES]) -> Result<bool, i64> {
    match call(NEXT, out.as_mut_ptr() as u64, 0, 0) {
        1 => Ok(true),
        0 => Ok(false),
        e => Err(e),
    }
}

/// Take bytes the application wrote on socket `id`.
pub fn inet_read(id: u32, buf: &mut [u8]) -> Result<InetIo, i64> {
    io(call(
        READ,
        u64::from(id),
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ))
}

/// Give the application bytes from the network.
pub fn inet_write(id: u32, data: &[u8]) -> Result<InetIo, i64> {
    io(call(
        WRITE,
        u64::from(id),
        data.as_ptr() as u64,
        data.len() as u64,
    ))
}

/// Answer the request on `id`: `status` 0 or an errno, with the addresses.
pub fn inet_complete(id: u32, status: i32, block: &[u8; INET_ADDR_BLOCK]) -> Result<(), i64> {
    unit(call(
        COMPLETE,
        u64::from(id),
        status as u64,
        block.as_ptr() as u64,
    ))
}

/// Hand in a connection accepted for listener `listener`; the new socket's id.
pub fn inet_accepted(listener: u32, block: &[u8; INET_ADDR_BLOCK]) -> Result<u32, i64> {
    let code = call(ACCEPTED, u64::from(listener), block.as_ptr() as u64, 0);
    if code < 0 {
        Err(code)
    } else {
        Ok(code as u32)
    }
}

/// The network finished sending on `id`.
pub fn inet_eof(id: u32) -> Result<(), i64> {
    unit(call(EOF, u64::from(id), 0, 0))
}

/// The connection on `id` failed with `errno`.
pub fn inet_error(id: u32, errno: i32) -> Result<(), i64> {
    unit(call(ERROR, u64::from(id), errno as u64, 0))
}

/// The stack released its socket for `id`.
pub fn inet_close_ack(id: u32) -> Result<(), i64> {
    unit(call(CLOSE_ACK, u64::from(id), 0, 0))
}

/// `(sockets in the kernel table, requests waiting)`.
pub fn inet_stats() -> (u32, u32) {
    let v = call(STATS, 0, 0, 0).max(0) as u64;
    ((v & 0xFFFF) as u32, (v >> 16) as u32)
}
