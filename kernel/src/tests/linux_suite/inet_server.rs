//! `AF_INET` servers and datagrams through the Linux syscalls: listen and
//! accept (blocking, non-blocking, the bounded queue) and UDP messages, with
//! the fake `netd` of `inet_core.rs` on the other side.

use super::inet_core::*;
use super::*;
use crate::ipc::inet::{self, Addr};

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
