//! `AF_INET` core (docs/networking-plan.md N5): the shared helpers, a fake
//! `netd`, and the tests of the socket object and its pump with no syscalls in
//! the way. The Linux-call tests are in `inet_calls.rs`, the soaks in
//! `inet_soak.rs`.

use super::*;
use crate::ipc::inet::{self, errno as ie, Addr, Io, Kind, Op, State};
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

// ---- the socket object and its pump -------------------------------------------

/// Bind, connect and listen are requests `netd` must answer; the answers
/// change the socket's state, addresses and data path.
pub fn inet_requests_and_states() -> Result<(), String> {
    inet_fresh()?;
    *inet::RESPONDER.lock() = None;
    let sock = inet::create(Kind::Stream).ok_or("create")?;
    check!(sock.state() == State::Fresh, "a new socket is fresh");
    // bind: queued, answered with the port the stack chose
    let ticket = sock
        .begin_bind(Addr {
            ip: [0; 4],
            port: 0,
        })
        .map_err(|e| format!("bind {e}"))?;
    check!(inet::queued_count() == 1, "the bind was queued");
    let request = inet::next_request().ok_or("no request")?;
    check!(
        request.id == sock.id() && request.kind == Kind::Stream,
        "request identity"
    );
    check!(
        matches!(request.op, Op::Bind(a) if a == Addr { ip: [0; 4], port: 0 }),
        "the bind request carries the address"
    );
    check!(
        sock.begin_bind(Addr::ANY).err() == Some(ie::EALREADY),
        "a second bind is refused while one is pending"
    );
    let local = Addr {
        ip: [10, 0, 2, 15],
        port: 49200,
    };
    inet::complete(request.id, 0, local, Addr::ANY).map_err(|e| format!("complete {e}"))?;
    sock.finish(ticket).map_err(|e| format!("finish {e}"))?;
    check!(
        sock.state() == State::Bound && sock.local() == local,
        "bound state and address"
    );
    check!(
        sock.pair().is_none(),
        "a stream has no data path before it connects"
    );
    // a failed answer is reported to the waiter and leaves the socket as it was
    let ticket = sock.begin_listen(0).map_err(|e| format!("listen {e}"))?;
    let request = inet::next_request().ok_or("no listen request")?;
    check!(
        matches!(request.op, Op::Listen(1)),
        "a backlog of 0 is raised to 1"
    );
    inet::complete(request.id, ie::EADDRINUSE, Addr::ANY, Addr::ANY).map_err(|e| format!("{e}"))?;
    check!(
        sock.finish(ticket) == Err(ie::EADDRINUSE),
        "the refusal reaches the caller"
    );
    check!(
        sock.state() == State::Bound,
        "a refused listen leaves the state"
    );
    // connect: the data path appears with the answer
    let ticket = sock
        .begin_connect(Addr {
            ip: [10, 0, 2, 2],
            port: 7,
        })
        .map_err(|e| format!("connect {e}"))?
        .ok_or("a stream connect needs netd")?;
    check!(sock.state() == State::Connecting, "connecting");
    check!(
        sock.begin_connect(Addr {
            ip: [10, 0, 2, 2],
            port: 7
        })
        .err()
            == Some(ie::EALREADY),
        "EALREADY"
    );
    let request = inet::next_request().ok_or("no connect request")?;
    inet::complete(
        request.id,
        0,
        local,
        Addr {
            ip: [10, 0, 2, 2],
            port: 7,
        },
    )
    .map_err(|e| format!("{e}"))?;
    sock.finish(ticket).map_err(|e| format!("finish {e}"))?;
    check!(
        sock.state() == State::Connected && sock.pair().is_some(),
        "connected with a data path"
    );
    check!(
        sock.peer()
            == Some(Addr {
                ip: [10, 0, 2, 2],
                port: 7
            }),
        "peer address"
    );
    check!(
        sock.begin_connect(Addr {
            ip: [1, 1, 1, 1],
            port: 1
        })
        .err()
            == Some(ie::EISCONN),
        "EISCONN"
    );
    // closing queues the close; the slot lives until it is acknowledged
    let id = sock.id();
    drop(sock);
    check!(
        inet::live_count() == 1,
        "the slot waits for the acknowledgement"
    );
    let request = inet::next_request().ok_or("no close request")?;
    check!(
        request.id == id && request.op == Op::Close,
        "a close was queued"
    );
    check!(
        inet::close_ack(id).is_ok() && inet::close_ack(id) == Err(ie::EBADF),
        "ack once"
    );
    inet_done("inet_requests_and_states")
}

/// Bytes cross the pump both ways; end of stream and errors reach the
/// application; a datagram socket keeps message boundaries.
pub fn inet_pump_moves_bytes() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    let id = id_of(fd);
    check!(write_fd(fd, b"ping") == 4, "write");
    check!(
        net_take(id, 64) == b"ping",
        "netd reads what the application wrote"
    );
    check!(
        matches!(inet::net_read(id, &mut [0u8; 8]), Ok(Io::Empty)),
        "nothing more yet"
    );
    check!(
        matches!(inet::net_write(id, b"pong!"), Ok(Io::Data(5))),
        "netd writes"
    );
    let mut buf = [0u8; 16];
    check!(
        read_fd(fd, &mut buf) == 5 && &buf[..5] == b"pong!",
        "the application reads it"
    );
    // the application half-closes: netd sees the end of its stream, then can still send
    check!(sys6(48, [fd, 1, 0, 0, 0, 0]) == 0, "shutdown(WR)");
    check!(
        matches!(inet::net_read(id, &mut [0u8; 8]), Ok(Io::Eof)),
        "netd sees the end"
    );
    check!(
        matches!(inet::net_write(id, b"tail"), Ok(Io::Data(4))),
        "netd still sends"
    );
    check!(read_fd(fd, &mut buf) == 4, "the application still reads");
    // the network side ends: the application reads end of stream
    inet::net_eof(id).map_err(|e| format!("eof {e}"))?;
    check!(read_fd(fd, &mut buf) == 0, "end of stream");
    check!(close(fd) == 0, "close");
    check!(
        matches!(inet::net_write(id, b"x"), Ok(Io::Gone)),
        "netd learns the application is gone"
    );
    inet_done("inet_pump_moves_bytes")
}

/// A reset is an end of stream plus `SO_ERROR`, readable once.
pub fn inet_net_error_reaches_the_application() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    inet::net_error(id_of(fd), 104).map_err(|e| format!("{e}"))?;
    let events = poll_one(fd, POLLIN);
    check!(
        events & POLLERR != 0 && events & POLLHUP != 0,
        "poll reports the error: {events:#x}"
    );
    check!(so_error(fd) == 104, "SO_ERROR is ECONNRESET");
    check!(so_error(fd) == 0, "and reading it clears it");
    let mut buf = [0u8; 4];
    check!(read_fd(fd, &mut buf) == 0, "the stream is at its end");
    check!(close(fd) == 0, "close");
    inet_done("inet_net_error")
}

/// The wire form of a request is exact (`netd` parses these bytes).
pub fn inet_request_wire_form() -> Result<(), String> {
    use crate::ipc::inet::Request;
    let bind = inet::encode(&Request {
        id: 0x0102_0305,
        kind: Kind::Dgram,
        op: Op::Bind(Addr {
            ip: [1, 2, 3, 4],
            port: 0x1F90,
        }),
    });
    check!(bind[0..4] == [1, 0, 0, 0], "bind code");
    check!(bind[4..8] == [5, 3, 2, 1], "id is little endian");
    check!(
        bind[8..12] == [1, 2, 3, 4] && bind[12..14] == [0x90, 0x1F],
        "address"
    );
    check!(bind[14..16] == [1, 0], "kind");
    check!(bind[16..24] == [0; 8], "no aux, reserved zero");
    let listen = inet::encode(&Request {
        id: 1,
        kind: Kind::Stream,
        op: Op::Listen(7),
    });
    check!(
        listen[0..4] == [3, 0, 0, 0] && listen[16..20] == [7, 0, 0, 0],
        "listen backlog in aux"
    );
    let connect = inet::encode(&Request {
        id: 1,
        kind: Kind::Stream,
        op: Op::Connect(Addr {
            ip: [9, 9, 9, 9],
            port: 80,
        }),
    });
    check!(
        connect[0..4] == [2, 0, 0, 0] && connect[8..12] == [9; 4],
        "connect"
    );
    let close = inet::encode(&Request {
        id: 1,
        kind: Kind::Stream,
        op: Op::Close,
    });
    check!(
        close[0..4] == [4, 0, 0, 0] && close[8..16] == [0; 8],
        "close"
    );
    Ok(())
}

/// Only the attached `netd` may drive the pump, and only root or `_netd` may
/// attach; a restarted `netd` finds the old sockets gone.
pub fn inet_pump_authority() -> Result<(), String> {
    use crate::ipc::credentials::{self, Cred};
    inet_fresh()?;
    let me = task::current();
    let saved = credentials::of(me);
    let pump = process::inetsys::dispatch;
    // an ordinary user may not attach, nor use any op
    credentials::set(me, Cred::new(1000, 1000, 0, 0, 1));
    check!(pump(0, 0, 0, 0) == neg(1), "uid 1000 attached");
    inet::attach(me + 1);
    check!(pump(1, 0, 0, 0) == neg(1), "a non-netd used NEXT");
    check!(pump(9, 0, 0, 0) == neg(1), "a non-netd used STATS");
    // _netd (uid 903) may attach, and then drive
    credentials::set(me, Cred::new(903, 903, 0, 0, 1));
    check!(pump(0, 0, 0, 0) == 0, "_netd could not attach");
    check!(pump(9, 0, 0, 0) == 0, "STATS on an empty table");
    credentials::set(me, saved);
    check!(pump(99, 0, 0, 0) == neg(38), "an unknown op is ENOSYS");
    // attaching again forgets every socket
    let sock = inet::create(Kind::Stream).ok_or("create")?;
    let _ = sock.begin_bind(Addr::ANY);
    check!(pump(0, 0, 0, 0) == 0, "re-attach");
    check!(
        inet::live_count() == 0 && inet::queued_count() == 0,
        "re-attach clears the table"
    );
    drop(sock);
    check!(
        inet::queued_count() == 0,
        "a forgotten socket queues nothing when dropped"
    );
    inet_done("inet_pump_authority")
}

/// Stale and invalid ids, calls out of order, and hostile sizes are refused.
pub fn inet_pump_hostile_input() -> Result<(), String> {
    inet_fresh()?;
    let pump = process::inetsys::dispatch;
    let junk = [0u8; 32];
    for id in [0u64, 1, 0x100, 0xDEAD_BEEF, u32::MAX as u64] {
        for op in [2u64, 3, 6, 7, 8] {
            check!(
                pump(op, id, junk.as_ptr() as u64, 8) == neg(9),
                "op {op} on id {id:#x}"
            );
        }
        check!(
            pump(4, id, 0, junk.as_ptr() as u64) == neg(9),
            "complete on {id:#x}"
        );
        check!(
            pump(5, id, junk.as_ptr() as u64, 0) == neg(9),
            "accepted on {id:#x}"
        );
    }
    let sock = inet::create(Kind::Stream).ok_or("create")?;
    let id = u64::from(sock.id());
    check!(
        pump(4, id, 0, junk.as_ptr() as u64) == neg(22),
        "complete with nothing pending"
    );
    check!(
        pump(5, id, junk.as_ptr() as u64, 0) == neg(22),
        "accepted on a socket that is not listening"
    );
    check!(
        pump(2, id, junk.as_ptr() as u64, 8) == neg(9),
        "no data path yet"
    );
    drop(sock);
    inet_done("inet_pump_hostile_input")
}

/// Pointers that are not the caller's are refused with `EFAULT` by every call
/// that takes one, with the socket left as it was.
pub fn inet_bad_pointers() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    let udp = socket(SOCK_DGRAM);
    let id = id_of(fd);
    let pump = process::inetsys::dispatch;
    // a request must be waiting for NEXT to have a buffer to write
    let pending = inet::create(Kind::Stream).ok_or("create")?;
    let _ = pending.begin_bind(Addr::ANY);
    let strict = crate::user_ptr::set_trust_kernel_pointers(false);
    let results = [
        ("connect", sys6(42, [fd, 0xdead_0000, 16, 0, 0, 0])),
        ("bind", sys6(49, [fd, 0xdead_0000, 16, 0, 0, 0])),
        (
            "sendto addr",
            sys6(44, [udp, 0xdead_0000, 4, 0, 0xdead_0000, 16]),
        ),
        ("setsockopt", sys6(54, [fd, 1, 2, 0xdead_0000, 4, 0])),
        (
            "getsockopt",
            sys6(55, [fd, 1, 4, 0xdead_0000, 0xdead_0008, 0]),
        ),
        ("NEXT", pump(1, 0xdead_0000, 0, 0)),
        ("COMPLETE", pump(4, u64::from(id), 0, 0xdead_0000)),
        ("ACCEPTED", pump(5, u64::from(id), 0xdead_0000, 0)),
    ];
    crate::user_ptr::set_trust_kernel_pointers(strict);
    for (what, code) in results {
        check!(code == neg(14), "{what} with a bad pointer -> {code:#x}");
    }
    check!(
        inet::queued_count() == 0,
        "the request NEXT popped was the only one"
    );
    drop(pending);
    check!(close(fd) == 0 && close(udp) == 0, "close");
    inet_done("inet_bad_pointers")
}
