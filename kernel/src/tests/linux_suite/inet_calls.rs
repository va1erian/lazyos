//! `AF_INET` through the Linux syscalls: creation, the argument checks, a TCP
//! connection both ways, a non-blocking connect, listen and accept, datagrams,
//! `poll`/`epoll`, socket options, `dup` and `close`, with the fake `netd` of
//! `inet_core.rs` on the other side.

use super::inet_core::*;
use super::*;
use crate::ipc::inet::{self, Addr, Kind};

/// `socket` accepts TCP and UDP, refuses what it cannot serve, and hands out
/// descriptors that close cleanly.
pub fn inet_socket_creation() -> Result<(), String> {
    inet_fresh()?;
    let tcp = socket(1);
    let udp = socket(SOCK_DGRAM);
    check!(
        tcp < 16 && udp < 16 && tcp != udp,
        "descriptors: {tcp:#x} {udp:#x}"
    );
    check!(
        task::fd_kind(tcp as usize) == task::FdKind::Inet,
        "an Inet descriptor"
    );
    check!(
        close(sys6(41, [AF_INET, 1, 6, 0, 0, 0])) == 0,
        "explicit TCP protocol"
    );
    check!(
        close(sys6(41, [AF_INET, 2, 17, 0, 0, 0])) == 0,
        "explicit UDP protocol"
    );
    check!(
        sys6(41, [AF_INET, 1, 17, 0, 0, 0]) == neg(93),
        "TCP with the UDP protocol"
    );
    check!(
        sys6(41, [AF_INET, 2, 6, 0, 0, 0]) == neg(93),
        "UDP with the TCP protocol"
    );
    check!(
        sys6(41, [AF_INET, 3, 0, 0, 0, 0]) == neg(22),
        "SOCK_RAW is not offered"
    );
    check!(
        sys6(41, [AF_INET, 5, 0, 0, 0, 0]) == neg(22),
        "neither is SEQPACKET"
    );
    check!(
        sys6(41, [10, 1, 0, 0, 0, 0]) == neg(97),
        "AF_INET6 is still EAFNOSUPPORT"
    );
    let nb = sys6(41, [AF_INET, 1 | SOCK_NONBLOCK, 0, 0, 0, 0]);
    check!(
        task::fd_status(nb as usize).unwrap_or(0) & task::O_NONBLOCK != 0,
        "SOCK_NONBLOCK"
    );
    check!(
        close(tcp) == 0 && close(udp) == 0 && close(nb) == 0,
        "close"
    );
    check!(close(tcp) == neg(9), "a second close is EBADF");
    inet_done("inet_socket_creation")
}

/// Hostile arguments are refused before anything reaches `netd`.
pub fn inet_argument_checks() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    let good = sockaddr([10, 0, 2, 2], 80);
    let ptr = good.as_ptr() as u64;
    check!(
        sys6(42, [fd, ptr, 15, 0, 0, 0]) == neg(22),
        "a short sockaddr"
    );
    check!(
        sys6(42, [fd, ptr, 0, 0, 0, 0]) == neg(22),
        "a zero-length sockaddr"
    );
    let mut wrong_family = good;
    wrong_family[0] = 1; // AF_UNIX
    check!(
        sys6(42, [fd, wrong_family.as_ptr() as u64, 16, 0, 0, 0]) == neg(97),
        "wrong family"
    );
    check!(connect(fd, [10, 0, 2, 2], 0) == neg(22), "port 0");
    check!(
        inet::queued_count() == 0,
        "nothing was queued for any of that"
    );
    check!(
        sys6(48, [fd, 1, 0, 0, 0, 0]) == neg(107),
        "shutdown of an unconnected socket"
    );
    check!(
        sys6(52, [fd, 0, 0, 0, 0, 0]) == neg(107),
        "getpeername of an unconnected socket"
    );
    let mut buf = [0u8; 8];
    check!(
        read_fd(fd, &mut buf) == neg(107),
        "read of an unconnected stream"
    );
    check!(
        write_fd(fd, b"x") == neg(107),
        "write of an unconnected stream"
    );
    check!(
        sendto(fd, b"x", None) == neg(107),
        "send on an unconnected stream"
    );
    check!(
        sys6(43, [fd, 0, 0, 0, 0, 0]) == neg(22),
        "accept on a socket that does not listen"
    );
    // a connection works, and then connecting again is EISCONN
    check!(connect(fd, [10, 0, 2, 2], 80) == 0, "connect");
    check!(connect(fd, [10, 0, 2, 2], 80) == neg(106), "second connect");
    check!(bind(fd, [0; 4], 5000) == neg(22), "bind after connect");
    check!(listen(fd) == neg(22), "listen after connect");
    // wrong kinds of descriptor
    check!(
        connect(99, [1, 1, 1, 1], 1) == neg(9),
        "connect on a closed slot"
    );
    check!(
        sys6(54, [99, 1, 2, 0, 0, 0]) == neg(9),
        "setsockopt on a closed slot"
    );
    let (a, _b) = socketpair(AF_UNIX | SOCK_STREAM)?;
    check!(
        sys6(54, [a, 1, 2, 0, 0, 0]) == neg(38),
        "setsockopt on a unix socket stays ENOSYS"
    );
    check!(close(fd) == 0 && close(a) == 0 && close(_b) == 0, "close");
    // datagram specifics
    let udp = socket(SOCK_DGRAM);
    check!(listen(udp) == neg(95), "listen on a datagram socket");
    check!(
        sendto(udp, &[0u8; 1473], Some(([10, 0, 2, 2], 9))) == neg(90),
        "over-long datagram"
    );
    check!(
        sendto(udp, b"x", Some(([10, 0, 2, 2], 0))) == neg(22),
        "datagram to port 0"
    );
    check!(
        sendto(udp, b"x", None) == neg(89),
        "datagram with no destination"
    );
    check!(close(udp) == 0, "close udp");
    inet_done("inet_argument_checks")
}

/// A TCP connection through every call a client makes, and a refused one.
pub fn inet_tcp_client() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    let mut name = [0u8; 16];
    let mut len = 16u32;
    check!(
        sys6(
            51,
            [
                fd,
                name.as_mut_ptr() as u64,
                &mut len as *mut u32 as u64,
                0,
                0,
                0
            ]
        ) == 0,
        "getsockname"
    );
    let (ip, port) = addr_of(&name);
    check!(
        ip == [10, 0, 2, 15] && port >= 49152 && len == 16,
        "local address {ip:?}:{port}"
    );
    check!(
        sys6(
            52,
            [
                fd,
                name.as_mut_ptr() as u64,
                &mut len as *mut u32 as u64,
                0,
                0,
                0
            ]
        ) == 0,
        "getpeername"
    );
    check!(addr_of(&name) == ([10, 0, 2, 2], 7), "peer address");
    check!(
        poll_one(fd, POLLOUT) & POLLOUT != 0,
        "writable once connected"
    );
    check!(poll_one(fd, POLLIN) & POLLIN == 0, "not readable yet");
    check!(sendto(fd, b"GET /", None) == 5, "send");
    check!(net_take(id_of(fd), 64) == b"GET /", "netd saw the bytes");
    inet::net_write(id_of(fd), b"200 OK").map_err(|e| format!("{e}"))?;
    check!(poll_one(fd, POLLIN) & POLLIN != 0, "readable after data");
    let mut buf = [0u8; 16];
    let (n, from) = recvfrom(fd, &mut buf);
    check!(
        n == 6 && &buf[..6] == b"200 OK" && from == ([10, 0, 2, 2], 7),
        "recv with the peer's address"
    );
    check!(close(fd) == 0, "close");
    // refused
    set_refuse(true);
    let fd = socket(1);
    check!(
        connect(fd, [10, 0, 2, 2], 7) == neg(111),
        "a refused connection"
    );
    check!(
        sys6(52, [fd, 0, 0, 0, 0, 0]) == neg(107),
        "still unconnected"
    );
    set_refuse(false);
    check!(
        connect(fd, [10, 0, 2, 2], 7) == 0,
        "and the socket can try again"
    );
    check!(close(fd) == 0, "close");
    inet_done("inet_tcp_client")
}

/// A non-blocking connect returns `EINPROGRESS`, completes in the background
/// and reports through `poll` and `SO_ERROR`.
pub fn inet_nonblocking_connect() -> Result<(), String> {
    inet_fresh()?;
    *inet::RESPONDER.lock() = None; // nothing answers until the test says so
    let fd = sys6(41, [AF_INET, 1 | SOCK_NONBLOCK, 0, 0, 0, 0]);
    check!(connect(fd, [10, 0, 2, 2], 7) == neg(115), "EINPROGRESS");
    check!(
        connect(fd, [10, 0, 2, 2], 7) == neg(114),
        "EALREADY while in progress"
    );
    check!(poll_one(fd, POLLOUT | POLLIN) == 0, "nothing to report yet");
    fake_netd();
    check!(
        poll_one(fd, POLLOUT) & POLLOUT != 0,
        "writable when connected"
    );
    check!(so_error(fd) == 0, "no error");
    check!(sendto(fd, b"hi", None) == 2, "usable as a connected socket");
    check!(close(fd) == 0, "close");
    // the failing case
    set_refuse(true);
    let fd = sys6(41, [AF_INET, 1 | SOCK_NONBLOCK, 0, 0, 0, 0]);
    check!(
        connect(fd, [10, 0, 2, 2], 7) == neg(115),
        "EINPROGRESS again"
    );
    fake_netd();
    let events = poll_one(fd, POLLOUT);
    check!(
        events & POLLERR != 0 && events & POLLOUT != 0,
        "error and writable: {events:#x}"
    );
    check!(so_error(fd) == 111, "SO_ERROR is ECONNREFUSED");
    check!(so_error(fd) == 0, "and is cleared by reading it");
    check!(close(fd) == 0, "close");
    inet_done("inet_nonblocking_connect")
}

/// A server: bind, listen, and connections `netd` accepts become descriptors.
pub fn inet_listen_accept() -> Result<(), String> {
    inet_fresh()?;
    let srv = socket(1);
    check!(bind(srv, [0; 4], 8080) == 0, "bind");
    let mut name = [0u8; 16];
    let mut len = 16u32;
    check!(
        sys6(
            51,
            [
                srv,
                name.as_mut_ptr() as u64,
                &mut len as *mut u32 as u64,
                0,
                0,
                0
            ]
        ) == 0,
        "getsockname"
    );
    check!(addr_of(&name).1 == 8080, "bound port");
    check!(listen(srv) == 0, "listen");
    inet_nb_accept(srv)?;
    inet_done("inet_listen_accept")
}

fn inet_nb_accept(srv: u64) -> Result<(), String> {
    check!(task::fd_set_status(srv as usize, true), "O_NONBLOCK");
    check!(
        sys6(43, [srv, 0, 0, 0, 0, 0]) == neg(11),
        "accept with nothing pending is EAGAIN"
    );
    check!(poll_one(srv, POLLIN) == 0, "listener not readable");
    let peer = Addr {
        ip: [10, 0, 2, 2],
        port: 54321,
    };
    let local = Addr {
        ip: [10, 0, 2, 15],
        port: 8080,
    };
    let id = inet::accepted(id_of(srv), peer, local).map_err(|e| format!("accepted {e}"))?;
    check!(
        id != 0 && poll_one(srv, POLLIN) & POLLIN != 0,
        "listener readable"
    );
    let mut raw = [0u8; 16];
    let mut len = 16u32;
    let conn = sys6(
        43,
        [
            srv,
            raw.as_mut_ptr() as u64,
            &mut len as *mut u32 as u64,
            0,
            0,
            0,
        ],
    );
    check!(conn < 16, "accept returned {conn:#x}");
    check!(
        addr_of(&raw) == ([10, 0, 2, 2], 54321),
        "peer address from accept"
    );
    check!(poll_one(srv, POLLIN) == 0, "queue drained");
    check!(id_of(conn) == id, "the descriptor is the socket netd named");
    inet::net_write(id, b"hello server").map_err(|e| format!("{e}"))?;
    let mut buf = [0u8; 32];
    check!(
        read_fd(conn, &mut buf) == 12,
        "read from the accepted socket"
    );
    check!(
        write_fd(conn, b"reply") == 5 && net_take(id, 16) == b"reply",
        "write to it"
    );
    // accept4 flags
    inet::accepted(id_of(srv), peer, local).map_err(|e| format!("{e}"))?;
    let second = sys6(288, [srv, 0, 0, 0o4000 | 0o2000000, 0, 0]);
    check!(second < 16, "accept4");
    check!(
        task::fd_status(second as usize).unwrap_or(0) & task::O_NONBLOCK != 0,
        "accept4 SOCK_NONBLOCK"
    );
    check!(task::fd_cloexec(second as usize), "accept4 SOCK_CLOEXEC");
    check!(
        close(second) == 0 && close(conn) == 0 && close(srv) == 0,
        "close"
    );
    Ok(())
}

/// The accept queue is bounded; the overflow is refused to `netd`.
pub fn inet_accept_queue_is_bounded() -> Result<(), String> {
    inet_fresh()?;
    let srv = socket(1);
    check!(bind(srv, [0; 4], 9) == 0 && listen(srv) == 0, "listen");
    let peer = Addr {
        ip: [10, 0, 2, 2],
        port: 1,
    };
    for n in 0..inet::ACCEPT_QUEUE {
        check!(
            inet::accepted(id_of(srv), peer, Addr::ANY).is_ok(),
            "connection {n} fits"
        );
    }
    check!(
        inet::accepted(id_of(srv), peer, Addr::ANY) == Err(inet::errno::ENOBUFS),
        "one more is refused"
    );
    // closing the listener drops what was queued, each with its own close
    check!(close(srv) == 0, "close the listener");
    fake_netd();
    inet_done("inet_accept_queue")
}

/// Datagrams carry the peer's address both ways.
pub fn inet_udp_messages() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(SOCK_DGRAM);
    let dest = ([10, 0, 2, 3], 53);
    check!(
        sendto(fd, b"query", Some(dest)) == 5,
        "sendto binds implicitly and sends"
    );
    let id = id_of(fd);
    let frame = net_take(id, 2048);
    check!(
        frame.len() == 6 + 5 && frame[..4] == [10, 0, 2, 3] && frame[4..6] == [0, 53],
        "header: {frame:?}"
    );
    check!(&frame[6..] == b"query", "payload");
    // a reply from the server, and one from a stranger
    let mut reply = vec![10, 0, 2, 3, 0, 53];
    reply.extend_from_slice(b"answer");
    check!(
        matches!(inet::net_write(id, &reply), Ok(inet::Io::Data(12))),
        "netd delivers"
    );
    let mut buf = [0u8; 64];
    let (n, from) = recvfrom(fd, &mut buf);
    check!(
        n == 6 && &buf[..6] == b"answer" && from == dest,
        "recvfrom: {n:#x} {from:?}"
    );
    // message boundaries and truncation
    for text in [&b"one"[..], b"twotwo", b"3"] {
        let mut m = vec![1, 2, 3, 4, 0, 9];
        m.extend_from_slice(text);
        inet::net_write(id, &m).map_err(|e| format!("{e}"))?;
    }
    let mut small = [0u8; 2];
    let (n, _) = recvfrom(fd, &mut small);
    check!(
        n == 2 && &small == b"on",
        "a short buffer truncates one message"
    );
    let (n, from) = recvfrom(fd, &mut buf);
    check!(
        n == 6 && &buf[..6] == b"twotwo" && from == ([1, 2, 3, 4], 9),
        "the next message is whole"
    );
    check!(
        read_fd(fd, &mut buf) == 1 && buf[0] == b'3',
        "read() takes a message too"
    );
    // connect fixes the destination for write()
    check!(
        connect(fd, [10, 0, 2, 3], 5353) == 0,
        "connect on UDP is local"
    );
    check!(write_fd(fd, b"hi") == 2, "write to the default peer");
    let frame = net_take(id, 64);
    check!(
        frame[..6] == [10, 0, 2, 3, 0x14, 0xE9] && &frame[6..] == b"hi",
        "default peer in the header"
    );
    check!(
        sys6(52, [fd, 0, 0, 0, 0, 0]) == 0,
        "getpeername after connect"
    );
    check!(close(fd) == 0, "close");
    inet_done("inet_udp_messages")
}

/// `poll` and `epoll` see an inet socket's readiness, including edges.
pub fn inet_poll_and_epoll() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    let ep = sys6(291, [0, 0, 0, 0, 0, 0]); // epoll_create1
    check!(ep < 16, "epoll_create1");
    let add = epoll_event(EPOLLIN | EPOLLET, 77);
    check!(
        epoll_ctl(ep, EPOLL_CTL_ADD, fd, &add) == 0,
        "add the socket"
    );
    let mut out = [0u8; 12 * 4];
    check!(epoll_wait0(ep, &mut out) == 0, "not ready");
    inet::net_write(id_of(fd), b"abc").map_err(|e| format!("{e}"))?;
    check!(epoll_wait0(ep, &mut out) == 1, "ready after data");
    check!(unpack_event(&out).1 == 77, "the registered data");
    check!(
        epoll_wait0(ep, &mut out) == 0,
        "edge-triggered: reported once"
    );
    inet::net_write(id_of(fd), b"def").map_err(|e| format!("{e}"))?;
    check!(epoll_wait0(ep, &mut out) == 1, "a new edge after more data");
    let mut buf = [0u8; 16];
    check!(read_fd(fd, &mut buf) == 6, "read both");
    inet::net_eof(id_of(fd)).map_err(|e| format!("{e}"))?;
    check!(
        poll_one(fd, POLLIN) & (POLLIN | POLLHUP) != 0,
        "hangup is reported"
    );
    check!(close(ep) == 0 && close(fd) == 0, "close");
    inet_done("inet_poll_and_epoll")
}

/// Options programs set as a matter of course are accepted; the ones that
/// report something do.
pub fn inet_socket_options() -> Result<(), String> {
    inet_fresh()?;
    let tcp = socket(1);
    let udp = socket(SOCK_DGRAM);
    let one = 1i32;
    for (level, name) in [
        (1u64, 2u64),
        (1, 9),
        (1, 6),
        (1, 7),
        (1, 8),
        (6, 1),
        (1, 20),
        (1, 21),
    ] {
        check!(
            sys6(54, [tcp, level, name, &one as *const i32 as u64, 4, 0]) == 0,
            "setsockopt({level}, {name})"
        );
    }
    let get = |fd: u64, level: u64, name: u64| -> (u64, i32) {
        let mut value = 0x7777i32;
        let mut len = 4u32;
        let r = sys6(
            55,
            [
                fd,
                level,
                name,
                &mut value as *mut i32 as u64,
                &mut len as *mut u32 as u64,
                0,
            ],
        );
        (r, value)
    };
    check!(get(tcp, 1, 3) == (0, 1), "SO_TYPE of a stream");
    check!(get(udp, 1, 3) == (0, 2), "SO_TYPE of a datagram socket");
    check!(get(tcp, 1, 4) == (0, 0), "SO_ERROR");
    check!(get(tcp, 1, 30) == (0, 0), "SO_ACCEPTCONN before listen");
    check!(
        get(tcp, 6, 1).0 == neg(92),
        "an option of another level is ENOPROTOOPT"
    );
    check!(close(tcp) == 0 && close(udp) == 0, "close");
    inet_done("inet_socket_options")
}

/// `dup` shares a socket; the close reaches `netd` only with the last
/// descriptor, and a duplicate keeps the connection usable.
pub fn inet_dup_keeps_the_socket() -> Result<(), String> {
    inet_fresh()?;
    let fd = socket(1);
    check!(connect(fd, [10, 0, 2, 2], 7) == 0, "connect");
    let copy = sys6(32, [fd, 0, 0, 0, 0, 0]);
    check!(copy < 16 && copy != fd, "dup");
    check!(close(fd) == 0, "close the original");
    check!(
        inet::queued_count() == 0,
        "no close request while a duplicate lives"
    );
    check!(
        write_fd(copy, b"still here") == 10,
        "the duplicate still writes"
    );
    check!(
        net_take(id_of(copy), 32) == b"still here",
        "and netd sees it"
    );
    check!(close(copy) == 0, "close the duplicate");
    check!(inet::queued_count() == 1, "now the close is queued");
    inet_done("inet_dup")
}

/// Sockets are bounded: 64 at once, and every slot comes back.
pub fn inet_socket_table_is_bounded() -> Result<(), String> {
    inet_fresh()?;
    let mut held = Vec::new();
    for n in 0..inet::MAX_SOCKETS {
        // Only 13 descriptors fit in the table, so hold the sockets directly.
        let sock = inet::create(Kind::Stream).ok_or_else(|| format!("socket {n} refused"))?;
        held.push(sock);
    }
    check!(
        inet::create(Kind::Stream).is_none(),
        "the 65th socket is refused"
    );
    check!(socket(1) == neg(23), "socket() reports ENFILE");
    held.truncate(10);
    check!(
        inet::create(Kind::Stream).is_none(),
        "dropped sockets still hold their slot until acknowledged"
    );
    fake_netd();
    check!(
        inet::create(Kind::Stream).is_some(),
        "an acknowledged slot is free again"
    );
    drop(held);
    inet_done("inet_socket_table_is_bounded")
}
