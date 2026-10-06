//! `AF_INET` core (docs/networking-plan.md N5): the shared helpers and a fake
//! `netd`. The tests of the socket object and its pump are in `inet_pump.rs`,
//! the Linux-call tests in `inet_calls.rs` and `inet_server.rs`, the soaks in
//! `inet_soak.rs`.

use super::*;
use crate::ipc::inet::{self, Addr, Io, Op};
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

pub(super) const AF_INET: u64 = 2;
pub(super) const SOCK_DGRAM: u64 = 2;
pub(super) const SOCK_NONBLOCK: u64 = 0o4000;
pub(super) const POLLIN: u16 = 1;
pub(super) const POLLOUT: u16 = 4;
pub(super) const POLLERR: u16 = 8;
pub(super) const POLLHUP: u16 = 0x10;

/// What the fake `netd` does with a `Connect`.
static REFUSE: AtomicBool = AtomicBool::new(false);
static NEXT_PORT: AtomicU16 = AtomicU16::new(49152);

pub(super) fn neg(errno: i64) -> u64 {
    (-errno) as u64
}

/// A `struct sockaddr_in`.
pub(super) fn sockaddr(ip: [u8; 4], port: u16) -> [u8; 16] {
    let mut raw = [0u8; 16];
    raw[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    raw[2..4].copy_from_slice(&port.to_be_bytes());
    raw[4..8].copy_from_slice(&ip);
    raw
}

pub(super) fn addr_of(raw: &[u8; 16]) -> ([u8; 4], u16) {
    (
        [raw[4], raw[5], raw[6], raw[7]],
        u16::from_be_bytes([raw[2], raw[3]]),
    )
}

pub(super) fn sys6(nr: u64, a: [u64; 6]) -> u64 {
    process::linux::dispatch_args6_for_test(nr, a)
}

pub(super) fn socket(kind: u64) -> u64 {
    sys6(41, [AF_INET, kind, 0, 0, 0, 0])
}

pub(super) fn connect(fd: u64, ip: [u8; 4], port: u16) -> u64 {
    let raw = sockaddr(ip, port);
    sys6(42, [fd, raw.as_ptr() as u64, 16, 0, 0, 0])
}

pub(super) fn bind(fd: u64, ip: [u8; 4], port: u16) -> u64 {
    let raw = sockaddr(ip, port);
    sys6(49, [fd, raw.as_ptr() as u64, 16, 0, 0, 0])
}

pub(super) fn listen(fd: u64) -> u64 {
    sys6(50, [fd, 4, 0, 0, 0, 0])
}

pub(super) fn close(fd: u64) -> u64 {
    sys6(3, [fd, 0, 0, 0, 0, 0])
}

pub(super) fn sendto(fd: u64, data: &[u8], to: Option<([u8; 4], u16)>) -> u64 {
    let raw = to.map(|(ip, port)| sockaddr(ip, port));
    let (addr, len) = raw.as_ref().map_or((0, 0), |r| (r.as_ptr() as u64, 16));
    sys6(
        44,
        [fd, data.as_ptr() as u64, data.len() as u64, 0, addr, len],
    )
}

/// `recvfrom` with a source address buffer; the sender is returned too.
pub(super) fn recvfrom(fd: u64, buf: &mut [u8]) -> (u64, ([u8; 4], u16)) {
    let mut raw = [0u8; 16];
    let mut len = 16u32;
    let n = sys6(
        45,
        [
            fd,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
            0,
            raw.as_mut_ptr() as u64,
            &mut len as *mut u32 as u64,
        ],
    );
    (n, addr_of(&raw))
}

/// `poll` one descriptor for `events`, no waiting.
pub(super) fn poll_one(fd: u64, events: u16) -> u16 {
    let mut entry = [0u8; 8];
    entry[0..4].copy_from_slice(&(fd as i32).to_le_bytes());
    entry[4..6].copy_from_slice(&events.to_le_bytes());
    let n = sys6(7, [entry.as_mut_ptr() as u64, 1, 0, 0, 0, 0]);
    if n == 1 {
        u16::from_le_bytes([entry[6], entry[7]])
    } else {
        0
    }
}

pub(super) fn so_error(fd: u64) -> i32 {
    let mut value = 0i32;
    let mut len = 4u32;
    let r = sys6(
        55,
        [
            fd,
            1,
            4,
            &mut value as *mut i32 as u64,
            &mut len as *mut u32 as u64,
            0,
        ],
    );
    assert_eq!(r, 0, "getsockopt(SO_ERROR)");
    value
}

/// The fake `netd`: answer every queued request the way a healthy stack would
/// (or refuse connections while [`REFUSE`] is set).
pub(super) fn fake_netd() {
    while let Some(request) = inet::next_request() {
        match request.op {
            Op::Bind(addr) => {
                let port = if addr.port == 0 {
                    NEXT_PORT.fetch_add(1, Ordering::AcqRel)
                } else {
                    addr.port
                };
                let local = Addr {
                    ip: [10, 0, 2, 15],
                    port,
                };
                let _ = inet::complete(request.id, 0, local, Addr::ANY);
            }
            Op::Connect(peer) => {
                let status = if REFUSE.load(Ordering::Acquire) {
                    111
                } else {
                    0
                };
                let local = Addr {
                    ip: [10, 0, 2, 15],
                    port: NEXT_PORT.fetch_add(1, Ordering::AcqRel),
                };
                let _ = inet::complete(request.id, status, local, peer);
            }
            Op::Listen(_) => {
                let _ = inet::complete(request.id, 0, Addr::ANY, Addr::ANY);
            }
            Op::Close => {
                let _ = inet::close_ack(request.id);
            }
        }
    }
}

/// Start a test: a clean ABI surface, an empty socket table, the fake `netd`
/// answering and registered as the pump's owner.
pub(super) fn inet_fresh() -> Result<(), String> {
    fresh()?;
    inet::reset();
    REFUSE.store(false, Ordering::Release);
    *inet::RESPONDER.lock() = Some(fake_netd);
    inet::attach(task::current());
    Ok(())
}

/// End a test: no responder, every socket closed and acknowledged, no rings left.
pub(super) fn inet_done(name: &str) -> Result<(), String> {
    *inet::RESPONDER.lock() = None;
    fake_netd();
    check!(fds_clean(), "{name} left a descriptor");
    check!(
        inet::live_count() == 0,
        "{name} left {} sockets",
        inet::live_count()
    );
    check!(inet::queued_count() == 0, "{name} left requests queued");
    check!(pipe::Pipe::live_small() == 0, "{name} leaked a small ring");
    Ok(())
}

pub(super) fn set_refuse(on: bool) {
    REFUSE.store(on, Ordering::Release);
}

/// Read what the application wrote, as `netd` would.
pub(super) fn net_take(id: u32, max: usize) -> Vec<u8> {
    let mut buf = vec![0u8; max];
    match inet::net_read(id, &mut buf) {
        Ok(Io::Data(n)) => buf[..n].to_vec(),
        _ => Vec::new(),
    }
}

/// The id of the socket behind `fd` (through its table slot).
pub(super) fn id_of(fd: u64) -> u32 {
    match task::fd_clone(fd as usize).as_ref() {
        Some(task::Fd::Inet { sock }) => sock.id(),
        _ => 0,
    }
}
