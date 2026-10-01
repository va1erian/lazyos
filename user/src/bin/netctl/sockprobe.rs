//! `netctl sockprobe=1`: malformed and out-of-contract requests to the socket
//! interface, quotas, parked-call limits, ownership and closing under a
//! waiting caller.
//!
//! Every check states what `netd` must answer; a wrong answer (or a `netd`
//! that stops answering) fails the probe with the check's name. The one check
//! that touches the wire is a connection to a port nobody listens on, which
//! the gateway refuses.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use libmessenger::{flags, Header, Parcel, VERSION};
use user::messenger::netsock::{errno as se, parcel, wire, Addr, Client, MAX_CHUNK};
use user::messenger::{errno, Error as MsgError};
use user::sys;

use super::common::{fail, failed, is_errno, nap};

const GATEWAY: [u8; 4] = [10, 0, 2, 2];
/// A port nothing on the host listens on.
const CLOSED_PORT: u16 = 47_999;
const EAGAIN: i64 = errno::EAGAIN;

struct Checks {
    done: u32,
}

impl Checks {
    fn expect(&mut self, name: &str, ok: bool) -> Result<(), String> {
        if ok {
            self.done += 1;
            Ok(())
        } else {
            Err(format!("check failed: {name}"))
        }
    }

    fn refused<T>(
        &mut self,
        name: &str,
        result: Result<T, MsgError>,
        code: i64,
    ) -> Result<(), String> {
        let got = match &result {
            Ok(_) => String::from("succeeded"),
            Err(MsgError::Errno(c)) => format!("errno {}", -c),
            Err(other) => String::from(other.message()),
        };
        self.expect(
            &format!("{name} -> errno {code} (got: {got})"),
            is_errno(&result, code),
        )
    }

    fn rejected<T>(&mut self, name: &str, result: Result<T, MsgError>) -> Result<(), String> {
        self.expect(&format!("{name} is refused"), failed(&result))
    }
}

/// A request the caller allows to nest, so several can wait on one channel.
fn nested(method: u32, body: Vec<u8>) -> Parcel {
    let mut request = parcel(method, body);
    request.header.flags = flags::ALLOW_NESTED;
    request
}

/// A request that failed to encode (a bug here, not in `netd`).
fn encoding(error: libmessenger::Error) -> String {
    format!("encoding a request: {error:?}")
}

fn any(port: u16) -> Addr {
    Addr::new([0; 4], port)
}

/// Run the probe; returns how many checks passed. `connect` is the already
/// resolved socket client.
pub(super) fn run(client: &Client) -> Result<u32, String> {
    let mut c = Checks { done: 0 };
    let before = client.stats().map_err(fail("socket stats"))?;

    // --- Out-of-contract calls. ---------------------------------------------
    let foreign = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: 0x1234_5678_9ABC_DEF0,
            method: wire::METHOD_STATS,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        ..Parcel::default()
    };
    c.refused(
        "a foreign interface id",
        client.call_parcel(foreign, None),
        errno::EINVAL,
    )?;
    c.refused(
        "an unknown method",
        client.raw(0xDEAD_BEEF, Vec::new()),
        errno::EINVAL,
    )?;
    for (name, method) in [
        ("Open", wire::METHOD_OPEN),
        ("Bind", wire::METHOD_BIND),
        ("Connect", wire::METHOD_CONNECT),
        ("Send", wire::METHOD_SEND),
        ("Recv", wire::METHOD_RECV),
        ("SendTo", wire::METHOD_SENDTO),
    ] {
        c.rejected(
            &format!("{name} with a garbage body"),
            client.raw(method, vec![0xFF; 9]),
        )?;
        // An empty `Open` is a valid stream `Open` (every field defaults), so it
        // is the one empty body that must not be refused.
        if method != wire::METHOD_OPEN {
            c.rejected(
                &format!("{name} with an empty body"),
                client.raw(method, Vec::new()),
            )?;
        }
    }
    c.refused("Open of kind 2", client.open(2), errno::EINVAL)?;
    c.refused(
        "Open of kind u32::MAX",
        client.open(u32::MAX),
        errno::EINVAL,
    )?;

    // --- Ids that name nothing. ----------------------------------------------
    for id in [0u32, 1, 0xFF, 0x0100_0000, 0xDEAD_BEEF, u32::MAX] {
        c.refused(&format!("Close of {id:#x}"), client.close(id), se::EBADF)?;
        c.refused(
            &format!("Send on {id:#x}"),
            client.send(id, b"x", 100),
            se::EBADF,
        )?;
        c.refused(
            &format!("Recv on {id:#x}"),
            client.recv(id, 10, 100),
            se::EBADF,
        )?;
        c.refused(
            &format!("Poll on {id:#x}"),
            client.poll(id, 1, 100),
            se::EBADF,
        )?;
        c.refused(
            &format!("LocalAddr of {id:#x}"),
            client.local_addr(id),
            se::EBADF,
        )?;
    }

    // --- Arguments, each refused before anything happens. ---------------------
    let tcp = client.open(wire::SOCK_KIND_STREAM).map_err(fail("open"))?;
    let udp = client
        .open(wire::SOCK_KIND_DATAGRAM)
        .map_err(fail("open"))?;
    c.refused("Bind to port 80", client.bind(tcp, any(80)), errno::EACCES)?;
    c.refused(
        "Bind to port 1023",
        client.bind(tcp, any(1023)),
        errno::EACCES,
    )?;
    c.refused(
        "Bind to another host's address",
        client.bind(tcp, Addr::new([8, 8, 8, 8], 6000)),
        errno::EINVAL,
    )?;
    let short = wire::encode_bind_args(&wire::BindArgs {
        sock: tcp,
        addr: wire::SockAddr {
            addr: vec![1, 2, 3],
            port: 5000,
        },
    })
    .map_err(encoding)?;
    c.refused(
        "Bind with a three-byte address",
        client.raw(wire::METHOD_BIND, short),
        errno::EINVAL,
    )?;
    let wide = wire::encode_bind_args(&wire::BindArgs {
        sock: tcp,
        addr: wire::SockAddr {
            addr: vec![0; 4],
            port: 70_000,
        },
    })
    .map_err(encoding)?;
    c.refused(
        "Bind to port 70000",
        client.raw(wire::METHOD_BIND, wide),
        errno::EINVAL,
    )?;
    for to in [
        Addr::new(GATEWAY, 0),
        Addr::new([0, 0, 0, 0], 80),
        Addr::new([127, 0, 0, 1], 80),
        Addr::new([224, 0, 0, 1], 80),
        Addr::new([255, 255, 255, 255], 80),
    ] {
        c.refused(
            &format!("Connect to {to:?}"),
            client.connect_to(tcp, to, 100),
            errno::EINVAL,
        )?;
    }
    for timeout in [1u32, 9, 60_001, u32::MAX] {
        let body = wire::encode_connect_args(&wire::ConnectArgs {
            sock: tcp,
            addr: wire::SockAddr {
                addr: GATEWAY.to_vec(),
                port: 80,
            },
            timeout_ms: timeout,
        })
        .map_err(encoding)?;
        c.refused(
            &format!("Connect with a {timeout} ms timeout"),
            client.raw(wire::METHOD_CONNECT, body),
            errno::EINVAL,
        )?;
    }
    c.refused("Recv of 0 bytes", client.recv(tcp, 0, 100), errno::EINVAL)?;
    for (what, max) in [("0", 0u32), ("16385", 16_385), ("u32::MAX", u32::MAX)] {
        let body = wire::encode_recv_args(&wire::RecvArgs {
            sock: tcp,
            max,
            timeout_ms: 100,
        })
        .map_err(encoding)?;
        c.rejected(
            &format!("Recv of {what} bytes"),
            client.raw(wire::METHOD_RECV, body),
        )?;
    }
    for len in [0usize, MAX_CHUNK + 1] {
        let body = wire::encode_send_args(&wire::SendArgs {
            sock: tcp,
            data: vec![7; len],
            timeout_ms: 100,
        })
        .map_err(encoding)?;
        c.refused(
            &format!("Send of {len} bytes"),
            client.raw(wire::METHOD_SEND, body),
            errno::EINVAL,
        )?;
    }
    // Far over the limit the request does not even fit `netd`'s buffer: it is
    // dropped unanswered and the caller's own deadline ends the wait. The
    // service must carry on (the next check proves it).
    let huge = wire::encode_send_args(&wire::SendArgs {
        sock: tcp,
        data: vec![7; 3 * MAX_CHUNK],
        timeout_ms: 100,
    })
    .map_err(encoding)?;
    let dropped = client.call_parcel(nested(wire::METHOD_SEND, huge), Some(sys::clock() + 30));
    c.refused(
        "a Send of 48 KiB, which fits no buffer",
        dropped,
        errno::ETIMEDOUT,
    )?;
    c.refused(
        "Send on an unconnected stream",
        client.send(tcp, b"x", 100),
        se::ENOTCONN,
    )?;
    c.refused(
        "Recv on an unconnected stream",
        client.recv(tcp, 10, 100),
        se::ENOTCONN,
    )?;
    c.refused(
        "Listen with a backlog of 0",
        client.listen(tcp, 0),
        errno::EINVAL,
    )?;
    c.refused(
        "Listen with a backlog of 9",
        client.listen(tcp, 9),
        errno::EINVAL,
    )?;
    for interest in [0u32, 0x20, u32::MAX] {
        c.refused(
            &format!("Poll with interest {interest:#x}"),
            client.poll(tcp, interest, 100),
            errno::EINVAL,
        )?;
    }
    c.refused(
        "Shutdown with how = 7",
        client.shutdown(tcp, 7),
        errno::EINVAL,
    )?;
    c.refused(
        "Shutdown of an unconnected stream",
        client.shutdown(tcp, wire::SHUTDOWN_BOTH),
        se::ENOTCONN,
    )?;
    c.refused(
        "SendTo a datagram over 1472 bytes",
        client.send_to(udp, Addr::new(GATEWAY, 9), &vec![0; 1473]),
        se::EMSGSIZE,
    )?;
    c.refused(
        "SendTo port 0",
        client.send_to(udp, Addr::new(GATEWAY, 0), b"x"),
        errno::EINVAL,
    )?;
    c.refused(
        "RecvFrom on a stream",
        client.recv_from(tcp, 10, 100),
        errno::EINVAL,
    )?;
    c.refused(
        "Accept on a stream that does not listen",
        client.accept(tcp, 100),
        errno::EINVAL,
    )?;
    client
        .bind(tcp, any(0))
        .map_err(fail("bind to an ephemeral port"))?;
    let local = client.local_addr(tcp).map_err(fail("local address"))?;
    c.expect("an ephemeral port is above 49151", local.port >= 49_152)?;
    c.refused("Bind twice", client.bind(tcp, any(0)), errno::EINVAL)?;
    let other = client.open(wire::SOCK_KIND_STREAM).map_err(fail("open"))?;
    c.refused(
        "Bind to a port in use",
        client.bind(other, any(local.port)),
        se::EADDRINUSE,
    )?;
    client.close(other).map_err(fail("close"))?;
    client.close(tcp).map_err(fail("close"))?;
    client.close(udp).map_err(fail("close"))?;
    c.refused("Close twice", client.close(tcp), se::EBADF)?;

    // --- Ownership: a second task may not touch this task's socket. ----------
    let others_refused = super::sockowner::second_task_is_refused(client)?;
    c.expect(
        "another task is refused every call on this task's socket",
        others_refused,
    )?;

    // --- The per-owner socket quota. ------------------------------------------
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(
            client
                .open(wire::SOCK_KIND_DATAGRAM)
                .map_err(fail("open within the quota"))?,
        );
    }
    c.refused(
        "a ninth socket",
        client.open(wire::SOCK_KIND_DATAGRAM),
        se::EMFILE,
    )?;
    for id in held {
        client.close(id).map_err(fail("close"))?;
    }
    let again = client
        .open(wire::SOCK_KIND_DATAGRAM)
        .map_err(fail("open after closing"))?;
    client.close(again).map_err(fail("close"))?;

    // --- Parked calls: a cap per caller, honest timeouts, and Close under a waiter.
    let waiter = client
        .open(wire::SOCK_KIND_DATAGRAM)
        .map_err(fail("open"))?;
    client.bind(waiter, any(0)).map_err(fail("bind"))?;
    let endpoint = client.endpoint();
    let recv_body = |ms| {
        wire::encode_recv_from_args(&wire::RecvFromArgs {
            sock: waiter,
            max: 100,
            timeout_ms: ms,
        })
    };
    let mut txns = Vec::new();
    for _ in 0..4 {
        let request = nested(wire::METHOD_RECVFROM, recv_body(1500).map_err(encoding)?);
        txns.push(
            endpoint
                .begin_call(&request, Some(sys::clock() + 600))
                .map_err(fail("starting a parked RecvFrom"))?,
        );
    }
    let fifth = client.call_parcel(
        nested(wire::METHOD_RECVFROM, recv_body(1500).map_err(encoding)?),
        Some(sys::clock() + 100),
    );
    c.refused("a fifth parked call from one caller", fifth, EAGAIN)?;
    for (i, txn) in txns.into_iter().enumerate() {
        let reply = endpoint.await_reply(txn);
        let timed_out = matches!(&reply, Ok(p) if user::messenger::services::error_field(p).ok().flatten() == Some(errno::ETIMEDOUT));
        c.expect(
            &format!("parked call {i} ends with ETIMEDOUT, not silence"),
            timed_out,
        )?;
    }
    let request = nested(wire::METHOD_RECVFROM, recv_body(30_000).map_err(encoding)?);
    let txn = endpoint
        .begin_call(&request, Some(sys::clock() + 3000))
        .map_err(fail("starting a parked call to close under"))?;
    for _ in 0..5 {
        nap();
    }
    let close = wire::encode_close_args(&wire::CloseArgs { sock: waiter }).map_err(encoding)?;
    client
        .call_parcel(nested(wire::METHOD_CLOSE, close), Some(sys::clock() + 300))
        .map_err(fail("close under a waiter"))?;
    let reply = endpoint.await_reply(txn);
    let ebadf = matches!(&reply, Ok(p) if user::messenger::services::error_field(p).ok().flatten() == Some(se::EBADF));
    c.expect("closing a socket answers its waiter with EBADF", ebadf)?;
    // A caller whose own deadline passes first: `netd` must survive answering nobody.
    let lone = client
        .open(wire::SOCK_KIND_DATAGRAM)
        .map_err(fail("open"))?;
    client.bind(lone, any(0)).map_err(fail("bind"))?;
    let body = wire::encode_recv_from_args(&wire::RecvFromArgs {
        sock: lone,
        max: 10,
        timeout_ms: 500,
    })
    .map_err(encoding)?;
    let gave_up = client.call_parcel(nested(wire::METHOD_RECVFROM, body), Some(sys::clock() + 3));
    c.refused(
        "a caller whose own deadline passes first",
        gave_up,
        errno::ETIMEDOUT,
    )?;
    for _ in 0..80 {
        nap();
    }
    client.close(lone).map_err(fail("close"))?;

    // --- Real traffic: a refused connection, then everything is tidy. ----------
    // QEMU's user networking answers a connection to a closed host port with a
    // reset on some hosts and with silence on others (Windows): either way the
    // connection must fail, and the stack must not claim it was refused when
    // nothing said so.
    let refused = client.open(wire::SOCK_KIND_STREAM).map_err(fail("open"))?;
    let attempt = client.connect_to(refused, Addr::new(GATEWAY, CLOSED_PORT), 2500);
    c.expect(
        "a connection to a closed port on the gateway fails with ECONNREFUSED or ETIMEDOUT",
        is_errno(&attempt, se::ECONNREFUSED) || is_errno(&attempt, errno::ETIMEDOUT),
    )?;
    let was_refused = is_errno(&attempt, se::ECONNREFUSED);
    client.close(refused).map_err(fail("close"))?;
    for _ in 0..30 {
        nap();
    }
    let after = client.stats().map_err(fail("socket stats"))?;
    c.expect("no socket is left open", after.open == before.open)?;
    c.expect(
        "every socket opened was closed",
        after.opened - before.opened == after.closed - before.closed,
    )?;
    c.expect(
        "a refusal is counted exactly when the peer sent one",
        after.refused == before.refused + u64::from(was_refused),
    )?;
    c.expect(
        "calls were parked and four of them timed out",
        after.park_timeouts >= before.park_timeouts + 4,
    )?;
    Ok(c.done)
}
