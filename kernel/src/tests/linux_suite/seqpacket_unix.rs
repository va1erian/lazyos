//! `SOCK_SEQPACKET` message boundaries and `AF_UNIX` stream/pathname
//! sockets: shutdown, bind/connect/accept, and a bind/connect soak.

use super::*;

/// `SOCK_SEQPACKET`: one read per message, truncation discards the rest,
/// `-EMSGSIZE` over capacity, and EOF after the peer closes.
pub fn seqpacket_boundaries() -> Result<(), String> {
    fresh()?;
    let (a, b) = socketpair(AF_UNIX | SOCK_SEQPACKET)?;
    check!(write_fd(a, b"hello") == 5, "seqpacket write");
    let mut small = [0u8; 3];
    check!(read_fd(b, &mut small) == 3, "truncated read length");
    check!(&small == b"hel", "truncated read contents");
    // The discarded tail must not leak into the next message.
    check!(write_fd(a, b"xy") == 2, "second seqpacket write");
    let mut big = [0u8; 8];
    check!(read_fd(b, &mut big) == 2, "message after truncation");
    check!(&big[..2] == b"xy", "message boundary lost after truncation");
    // Back-to-back messages stay distinct.
    check!(write_fd(a, b"one") == 3, "write one");
    check!(write_fd(a, b"two") == 3, "write two");
    check!(
        read_fd(b, &mut big) == 3 && &big[..3] == b"one",
        "first message"
    );
    check!(
        read_fd(b, &mut big) == 3 && &big[..3] == b"two",
        "second message"
    );
    // Over-capacity messages are refused whole.
    let huge = vec![0u8; pipe::CAPACITY + 1];
    check!(
        write_fd(a, &huge) == EMSGSIZE,
        "oversized seqpacket write was not EMSGSIZE"
    );
    check!(task::fd_close(a as usize), "close A failed");
    check!(read_fd(b, &mut big) == 0, "seqpacket EOF");
    check!(task::fd_close(b as usize), "close B failed");
    check!(fds_clean(), "seqpacket test left a descriptor");
    check!(pipe::Pipe::live() == 0, "seqpacket test leaked a pipe");
    Ok(())
}

/// Soak: many seqpacket messages of varying length keep their boundaries.
pub fn seqpacket_soak_messages() -> Result<(), String> {
    fresh()?;
    let (a, b) = socketpair(AF_UNIX | SOCK_SEQPACKET)?;
    let mut payload = [0u8; 64];
    let mut out = [0u8; 128];
    for round in 0..20_000u32 {
        let len = (round as usize % 64) + 1;
        for (index, byte) in payload[..len].iter_mut().enumerate() {
            *byte = (round as u8) ^ (index as u8);
        }
        let sent = write_fd(a, &payload[..len]);
        check!(sent == len as u64, "round {round}: write returned {sent}");
        let got = read_fd(b, &mut out);
        check!(got == len as u64, "round {round}: read returned {got}");
        check!(
            out[..len] == payload[..len],
            "round {round}: message contents crossed"
        );
    }
    check!(task::fd_close(a as usize), "close A failed");
    check!(task::fd_close(b as usize), "close B failed");
    check!(fds_clean(), "seqpacket soak leaked a descriptor");
    check!(pipe::Pipe::live() == 0, "seqpacket soak leaked a pipe");
    Ok(())
}

/// Stream `socketpair`: EOF on close and `shutdown(SHUT_WR)` half-close.
pub fn unix_pair_eof_shutdown() -> Result<(), String> {
    fresh()?;
    let (a, b) = socketpair(SOCK_STREAM | SOCK_CLOEXEC)?;
    check!(write_fd(a, b"ping") == 4, "pair write");
    let mut buf = [0u8; 8];
    check!(
        read_fd(b, &mut buf) == 4 && &buf[..4] == b"ping",
        "pair read"
    );
    // Half-close: the peer sees EOF, this end still reads.
    check!(
        process::linux::dispatch_for_test(48, a, 1, 0) == 0,
        "shutdown(SHUT_WR) failed"
    );
    check!(read_fd(b, &mut buf) == 0, "shutdown did not EOF the peer");
    check!(write_fd(b, b"pong") == 4, "peer write after half-close");
    check!(
        read_fd(a, &mut buf) == 4 && &buf[..4] == b"pong",
        "half-closed read"
    );
    check!(
        process::linux::dispatch_for_test(48, a, 2, 0) == 0,
        "shutdown(SHUT_RDWR) failed"
    );
    check!(read_fd(a, &mut buf) == 0, "SHUT_RDWR did not EOF us");

    // A separate pair: closing one end reports EOF to the other.
    let (c, d) = socketpair(SOCK_STREAM)?;
    check!(write_fd(c, b"bye") == 3, "close-EOF write");
    check!(task::fd_close(c as usize), "close C failed");
    check!(read_fd(d, &mut buf) == 3, "close-EOF buffered read");
    check!(read_fd(d, &mut buf) == 0, "close-EOF read");

    for fd in [a, b, d] {
        check!(task::fd_close(fd as usize), "cleanup close failed");
    }
    check!(fds_clean(), "unix pair test left a descriptor");
    check!(pipe::Pipe::live() == 0, "unix pair test leaked a pipe");
    Ok(())
}

/// Pathname `AF_UNIX`: `socket`/`bind`/`listen`/`connect`/`accept4` with
/// data exchange and a clean unregister on close.
pub fn unix_pathname_bind_connect_accept() -> Result<(), String> {
    fresh()?;
    let listener_fd = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
    check!((listener_fd as i64) > 0, "socket returned {listener_fd:#x}");
    let name = b"/tmp/abi-suite.sock";
    let mut sockaddr = [0u8; 110];
    sockaddr[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
    sockaddr[2..2 + name.len()].copy_from_slice(name);
    let addr_ptr = sockaddr.as_ptr() as u64;
    let addr_len = (2 + name.len()) as u64;
    check!(
        process::linux::dispatch_for_test(49, listener_fd, addr_ptr, addr_len) == 0,
        "bind failed"
    );
    check!(
        process::linux::dispatch_for_test(50, listener_fd, 8, 0) == 0,
        "listen failed"
    );
    // Rebinding the same name is refused.
    let other = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
    let duplicate = process::linux::dispatch_for_test(49, other, addr_ptr, addr_len);
    check!(
        duplicate == (-98i64) as u64,
        "duplicate bind returned {duplicate:#x}"
    );
    check!(task::fd_close(other as usize), "close other failed");

    let client_fd = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
    check!(
        (client_fd as i64) > 0,
        "client socket returned {client_fd:#x}"
    );
    check!(
        process::linux::dispatch_for_test(42, client_fd, addr_ptr, addr_len) == 0,
        "connect failed"
    );
    let server_fd = process::linux::dispatch_args_for_test(288, listener_fd, 0, 0, SOCK_CLOEXEC);
    check!((server_fd as i64) > 0, "accept4 returned {server_fd:#x}");
    check!(write_fd(client_fd, b"hello") == 5, "client write");
    let mut buf = [0u8; 8];
    check!(read_fd(server_fd, &mut buf) == 5, "server read");
    check!(&buf[..5] == b"hello", "server data");
    check!(write_fd(server_fd, b"world") == 5, "server write");
    check!(read_fd(client_fd, &mut buf) == 5, "client read");
    check!(&buf[..5] == b"world", "client data");

    for fd in [client_fd, server_fd, listener_fd] {
        check!(task::fd_close(fd as usize), "cleanup close failed");
    }
    check!(fds_clean(), "pathname test left a descriptor");
    check!(unix::bound_count() == 0, "bound name survived its listener");
    check!(pipe::Pipe::live() == 0, "pathname test leaked a pipe");
    Ok(())
}

/// As on Linux, a connection is established at `connect`: the client can
/// write before the server accepts and the data waits in the pair. A
/// listener closed with a connection still pending releases the server
/// side, so the client reads EOF and nothing leaks.
pub fn unix_write_before_accept() -> Result<(), String> {
    fresh()?;
    let name = b"/tmp/abi-early.sock";
    let mut sockaddr = [0u8; 110];
    sockaddr[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
    sockaddr[2..2 + name.len()].copy_from_slice(name);
    let addr_ptr = sockaddr.as_ptr() as u64;
    let addr_len = (2 + name.len()) as u64;
    let listener = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
    check!(
        process::linux::dispatch_for_test(49, listener, addr_ptr, addr_len) == 0,
        "bind failed"
    );
    check!(
        process::linux::dispatch_for_test(50, listener, 4, 0) == 0,
        "listen failed"
    );

    // 1. Write before accept buffers instead of failing with -EPIPE.
    let client = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
    check!(
        process::linux::dispatch_for_test(42, client, addr_ptr, addr_len) == 0,
        "connect failed"
    );
    let early = write_fd(client, b"early");
    check!(early == 5, "write before accept returned {early:#x}");
    let server = process::linux::dispatch_args_for_test(288, listener, 0, 0, 0);
    check!((server as i64) > 0, "accept4 returned {server:#x}");
    let mut buf = [0u8; 8];
    check!(read_fd(server, &mut buf) == 5, "server read");
    check!(&buf[..5] == b"early", "early data lost");

    // 2. A pending connection is released when its listener closes.
    let orphan = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
    check!(
        process::linux::dispatch_for_test(42, orphan, addr_ptr, addr_len) == 0,
        "second connect failed"
    );
    check!(task::fd_close(listener as usize), "close listener failed");
    let eof = read_fd(orphan, &mut buf);
    check!(
        eof == 0,
        "orphaned client read returned {eof:#x}, expected EOF"
    );

    for fd in [client, server, orphan] {
        check!(task::fd_close(fd as usize), "cleanup close failed");
    }
    check!(fds_clean(), "early-write test left a descriptor");
    check!(unix::bound_count() == 0, "bound name survived its listener");
    check!(
        pipe::Pipe::live() == 0,
        "a pending connection leaked a pipe"
    );
    Ok(())
}

/// Soak: repeated bind/connect/accept/exchange/close generations, with no
/// bound-name, descriptor or pipe leak.
pub fn unix_pathname_soak() -> Result<(), String> {
    fresh()?;
    for round in 0..200u64 {
        let name = alloc::format!("/tmp/abi-soak-{round}.sock");
        let name = name.as_bytes();
        let mut sockaddr = [0u8; 110];
        sockaddr[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
        sockaddr[2..2 + name.len()].copy_from_slice(name);
        let addr_ptr = sockaddr.as_ptr() as u64;
        let addr_len = (2 + name.len()) as u64;

        let listener = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
        check!(
            process::linux::dispatch_for_test(49, listener, addr_ptr, addr_len) == 0,
            "round {round}: bind failed"
        );
        check!(
            process::linux::dispatch_for_test(50, listener, 1, 0) == 0,
            "round {round}: listen failed"
        );
        let client = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
        let connected = process::linux::dispatch_for_test(42, client, addr_ptr, addr_len);
        check!(
            connected == 0,
            "round {round}: connect returned {connected:#x}"
        );
        let server = process::linux::dispatch_args_for_test(288, listener, 0, 0, 0);
        check!((server as i64) > 0, "round {round}: accept failed");
        check!(write_fd(client, b"x") == 1, "round {round}: write failed");
        let mut byte = [0u8; 1];
        check!(
            read_fd(server, &mut byte) == 1,
            "round {round}: read failed"
        );
        for fd in [client, server, listener] {
            check!(task::fd_close(fd as usize), "round {round}: close failed");
        }
    }
    check!(fds_clean(), "pathname soak leaked a descriptor");
    check!(
        unix::bound_count() == 0,
        "pathname soak leaked a bound name"
    );
    check!(pipe::Pipe::live() == 0, "pathname soak leaked a pipe");
    Ok(())
}
