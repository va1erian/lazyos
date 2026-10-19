//! `SCM_RIGHTS` of descriptors that hold sockets (audit H7): an epoll
//! instance watches the descriptors it was given, a listener keeps the server
//! half of every connection nobody accepted. Sent through one of those
//! sockets, either closes a cycle (socket pair -> carried descriptor -> the
//! same pair) that nothing reads or purges, so each round leaked two of the
//! 64 pipes for the rest of the boot.

use super::*;

const SYS_SOCKET: u64 = 41;
const SYS_CONNECT: u64 = 42;
const SYS_BIND: u64 = 49;
const SYS_LISTEN: u64 = 50;
const SYS_EPOLL_CTL: u64 = 233;
const SYS_EPOLL_CREATE1: u64 = 291;
const AF_UNIX: u64 = 1;
const SOCK_STREAM: u64 = 1;
const EPOLL_CTL_ADD: u64 = 1;
const EPOLLIN: u32 = 1;

fn epoll_create() -> Result<u64, String> {
    let ep = sys(SYS_EPOLL_CREATE1, &[0]);
    check!((ep as i64) >= 0, "epoll_create1 returned {ep:#x}");
    Ok(ep)
}

/// `epoll_ctl(ep, ADD, fd)` for input readiness.
fn epoll_watch(ep: u64, fd: u64) -> u64 {
    let mut event = [0u8; 12];
    event[..4].copy_from_slice(&EPOLLIN.to_le_bytes());
    sys(
        SYS_EPOLL_CTL,
        &[ep, EPOLL_CTL_ADD, fd, event.as_ptr() as u64],
    )
}

/// An abstract `sockaddr_un` named after `id`.
fn abstract_name(id: u32) -> ([u8; 110], u64) {
    let name = alloc::format!("\0scm-containers-{id}");
    let mut sockaddr = [0u8; 110];
    sockaddr[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
    sockaddr[2..2 + name.len()].copy_from_slice(name.as_bytes());
    (sockaddr, (2 + name.len()) as u64)
}

/// A listening socket and one client connected to it (the connection sits
/// unaccepted in the listener). Returns `(listener, client)`.
fn listener_with_connection(id: u32) -> Result<(u64, u64), String> {
    let (sockaddr, len) = abstract_name(id);
    let addr = sockaddr.as_ptr() as u64;
    let listener = sys(SYS_SOCKET, &[AF_UNIX, SOCK_STREAM, 0]);
    check!((listener as i64) >= 0, "listener socket {listener:#x}");
    check!(sys(SYS_BIND, &[listener, addr, len]) == 0, "bind");
    check!(sys(SYS_LISTEN, &[listener, 4]) == 0, "listen");
    let client = sys(SYS_SOCKET, &[AF_UNIX, SOCK_STREAM, 0]);
    check!((client as i64) >= 0, "client socket {client:#x}");
    check!(sys(SYS_CONNECT, &[client, addr, len]) == 0, "connect");
    Ok((listener, client))
}

/// An epoll instance never travels, least of all through a pair it watches:
/// the pair would carry the epoll that holds the pair.
pub fn scmrights_epoll_never_travels() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    let ep = epoll_create()?;
    check!(epoll_watch(ep, p.a) == 0, "epoll_ctl add");
    check!(
        send_fds(p.b, b"e", &[ep]) == ETOOMANYREFS,
        "an epoll watching a pair through that pair"
    );
    let idle = epoll_create()?;
    check!(
        send_fds(p.sa, b"e", &[idle]) == ETOOMANYREFS,
        "an idle epoll through another pair"
    );
    check!(
        pipe::in_flight() == 0,
        "a refusal left {} descriptors in flight",
        pipe::in_flight()
    );
    for fd in [ep, idle, p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.child);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    task::harness::reset();
    settled(pipes_before)
}

/// 150 rounds of the epoll cycle (the 64 pipes ran out after 32): every
/// `socketpair` still succeeds and nothing is left.
pub fn scmrights_epoll_cycle_soak() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    for round in 0..150 {
        let (x, y) = socketpair().map_err(|e| format!("round {round}: {e}"))?;
        let ep = epoll_create()?;
        check!(epoll_watch(ep, x) == 0, "round {round}: epoll_ctl");
        check!(
            send_fds(y, b"e", &[ep]) == ETOOMANYREFS,
            "round {round}: the cycle was accepted"
        );
        for fd in [x, y, ep] {
            close(fd);
        }
    }
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.child);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    task::harness::reset();
    settled(pipes_before)
}

/// A listener with a connection pending cannot be sent through that
/// connection (it holds the connection's other half), nor through a pair that
/// connection carries; it travels through anything else, and a listener
/// nobody connected to is free to go.
pub fn scmrights_listener_pending_connection() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    let (listener, client) = listener_with_connection(0)?;
    check!(
        send_fds(client, b"l", &[listener]) == ETOOMANYREFS,
        "a listener through its own pending connection"
    );
    check!(
        pipe::in_flight() == 0,
        "a refusal left {} descriptors in flight",
        pipe::in_flight()
    );
    check!(
        send_fds(p.a, b"l", &[listener]) == 1,
        "a listener through an unrelated pair"
    );
    let mut buf = [0u8; 4];
    let got = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        got.n == 1 && got.fds.len() == 1,
        "receive the listener: n={:#x} fds={}",
        got.n,
        got.fds.len()
    );
    close(got.fds[0]);
    // A pending connection that carries descriptors does not pin the
    // listener: it travels unless what the connection carries leads back.
    check!(
        send_fds(client, b"c", &[p.sa]) == 1,
        "the pending connection carries an idle pair"
    );
    check!(
        send_fds(p.a, b"l", &[listener]) == 1,
        "a listener holding a carrying connection through an unrelated pair"
    );
    let got = recv_fds(p.b, &mut buf, Some(4), 0);
    check!(
        got.n == 1 && got.fds.len() == 1,
        "receive the listener again: n={:#x}",
        got.n
    );
    close(got.fds[0]);
    check!(
        send_fds(p.sa, b"l", &[listener]) == ETOOMANYREFS,
        "a listener through the pair its pending connection carries"
    );
    for fd in [listener, client, p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.child);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    task::harness::reset();
    settled(pipes_before)
}

/// 150 rounds of the listener cycle: nothing is left, and a new listener and
/// connection can still be made every time.
pub fn scmrights_listener_cycle_soak() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    for round in 0..150u32 {
        let (listener, client) =
            listener_with_connection(round + 1).map_err(|e| format!("round {round}: {e}"))?;
        check!(
            send_fds(client, b"l", &[listener]) == ETOOMANYREFS,
            "round {round}: the cycle was accepted"
        );
        for fd in [listener, client] {
            close(fd);
        }
    }
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.child);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    task::harness::reset();
    settled(pipes_before)
}

/// A listener sent through a pair `D` (it sits in `D`'s queue), then a new
/// connection to it: the connection is pending in the listener, so sending
/// `D`'s end through that connection would close the loop `D` -> listener ->
/// connection -> `D`. Refused whichever end of `D` is sent, and the first
/// connection made before the listener travelled behaves the same. 100 rounds,
/// nothing left behind.
pub fn scmrights_listener_sent_then_connected() -> Result<(), String> {
    let pipes_before = pipe::Pipe::live();
    let p = processes()?;
    for round in 0..100u32 {
        let id = 1000 + round;
        let (sockaddr, len) = abstract_name(id);
        let addr = sockaddr.as_ptr() as u64;
        let listener = sys(SYS_SOCKET, &[AF_UNIX, SOCK_STREAM, 0]);
        check!(
            (listener as i64) >= 0,
            "round {round}: listener {listener:#x}"
        );
        check!(
            sys(SYS_BIND, &[listener, addr, len]) == 0,
            "round {round}: bind"
        );
        check!(
            sys(SYS_LISTEN, &[listener, 4]) == 0,
            "round {round}: listen"
        );
        // The listener is in flight inside D, nobody connected yet: allowed.
        check!(
            send_fds(p.a, b"l", &[listener]) == 1,
            "round {round}: a listener with nothing pending through a pair"
        );
        let client = sys(SYS_SOCKET, &[AF_UNIX, SOCK_STREAM, 0]);
        check!((client as i64) >= 0, "round {round}: client {client:#x}");
        check!(
            sys(SYS_CONNECT, &[client, addr, len]) == 0,
            "round {round}: connect"
        );
        for (name, end) in [("a", p.a), ("b", p.b)] {
            check!(
                send_fds(client, b"d", &[end]) == ETOOMANYREFS,
                "round {round}: end {name} of the pair holding the listener through the new connection"
            );
        }
        check!(
            pipe::in_flight() == 1,
            "round {round}: {} descriptors in flight, want the listener",
            pipe::in_flight()
        );
        // Drain D so the next round starts clean.
        let mut buf = [0u8; 4];
        let got = recv_fds(p.b, &mut buf, Some(4), 0);
        check!(
            got.n == 1 && got.fds.len() == 1,
            "round {round}: receive the listener: n={:#x}",
            got.n
        );
        for fd in [got.fds[0], listener, client] {
            close(fd);
        }
    }
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.child);
    for fd in [p.a, p.b, p.sa, p.sb, p.r, p.w] {
        close(fd);
    }
    task::harness::switch_current(p.leader);
    task::harness::reset();
    settled(pipes_before)
}
