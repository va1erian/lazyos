//! `netfix`'s `loopback` check (issue #1019): a Linux program that listens and
//! connects to itself. A stream to 127.0.0.1 and one to the machine's own
//! address reach listeners on this machine, echo bytes both ways through
//! non-blocking sockets, and close in order; a connect to a local port nobody
//! listens on is refused at once (`connect_timeout`: a non-blocking connect,
//! `poll` and `SO_ERROR`); a read timeout (`SO_RCVTIMEO`) expires; a
//! datagram to 127.0.0.1 reaches a socket bound there. None of it may reach
//! the wire: `tools/net/run.py` checks the capture holds no such frame.

use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

const LO: Ipv4Addr = Ipv4Addr::LOCALHOST;

fn err(what: &str) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// The address the machine's card holds: a connected UDP socket learns it
/// without sending anything.
fn own_address(gateway: SocketAddr) -> Result<IpAddr, String> {
    let probe = UdpSocket::bind("0.0.0.0:0").map_err(err("bind"))?;
    probe.connect(gateway).map_err(err("connect"))?;
    Ok(probe.local_addr().map_err(err("local_addr"))?.ip())
}

/// Accept one connection on a non-blocking listener within five seconds.
fn accept(listener: &TcpListener) -> Result<(TcpStream, SocketAddr), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok(pair) => return Ok(pair),
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(format!("accept: {e}")),
        }
    }
}

/// Send `data` from `client` to `server`, which echoes it; the client
/// half-closes, the server sees the end and closes, the client sees the end.
/// Both ends are non-blocking and driven from this one thread.
fn echo(mut client: TcpStream, mut server: TcpStream, data: &[u8]) -> Result<(), String> {
    client.set_nonblocking(true).map_err(err("nonblocking"))?;
    server.set_nonblocking(true).map_err(err("nonblocking"))?;
    let (mut sent, mut back, mut held) = (0, Vec::new(), Vec::new());
    let (mut half_closed, mut server_done, mut server_closed) = (false, false, false);
    let mut client_done = false;
    let mut buf = vec![0u8; 16384];
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client_done {
        if Instant::now() > deadline {
            return Err(format!("stalled at {sent} sent, {} back", back.len()));
        }
        if sent < data.len() {
            match client.write(&data[sent..(sent + 8192).min(data.len())]) {
                Ok(n) => sent += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(format!("client write: {e}")),
            }
        } else if !half_closed {
            client.shutdown(Shutdown::Write).map_err(err("shutdown"))?;
            half_closed = true;
        }
        if !server_done {
            match server.read(&mut buf) {
                Ok(0) => {
                    server_done = true;
                }
                Ok(n) => held.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(format!("server read: {e}")),
            }
        }
        if !held.is_empty() {
            match server.write(&held) {
                Ok(n) => {
                    held.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(format!("server write: {e}")),
            }
        }
        if server_done && held.is_empty() && !server_closed {
            server
                .shutdown(Shutdown::Write)
                .map_err(err("server shutdown"))?;
            server_closed = true;
        }
        match client.read(&mut buf) {
            Ok(0) => client_done = true,
            Ok(n) => back.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(1)),
            Err(e) => return Err(format!("client read: {e}")),
        }
    }
    if back != data {
        return Err(format!(
            "{} of {} bytes came back intact",
            back.len(),
            data.len()
        ));
    }
    Ok(())
}

/// Connect to `to` (reached by `listener`), check the four addresses, and
/// echo `data`.
fn stream(listener: &TcpListener, to: SocketAddr, data: &[u8]) -> Result<(), String> {
    let client = TcpStream::connect(to).map_err(err("connect"))?;
    let (server, from) = accept(listener)?;
    let local = client.local_addr().map_err(err("local_addr"))?;
    if local.ip() != to.ip() || from != local {
        return Err(format!(
            "client {local}, server saw {from}, connected to {to}"
        ));
    }
    let peer = client.peer_addr().map_err(err("peer_addr"))?;
    let served = server.local_addr().map_err(err("server local_addr"))?;
    if peer != to || served != to {
        return Err(format!("peer {peer}, server's own {served}, wanted {to}"));
    }
    echo(client, server, data)
}

/// A connect to a port nobody listens on fails with `ECONNREFUSED` well
/// before any timeout.
fn refused(port: u16) -> Result<u128, String> {
    let started = Instant::now();
    let to = SocketAddr::from((LO, port));
    match TcpStream::connect_timeout(&to, Duration::from_secs(5)) {
        Ok(_) => Err(format!("a connection to {to} with no listener succeeded")),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
            let ms = started.elapsed().as_millis();
            if ms > 1000 {
                return Err(format!("refused only after {ms} ms"));
            }
            Ok(ms)
        }
        Err(e) => Err(format!("connect to {to}: {e}")),
    }
}

/// `SO_RCVTIMEO` on a loopback stream nobody writes to.
fn read_timeout(listener: &TcpListener, to: SocketAddr) -> Result<(), String> {
    let mut client = TcpStream::connect(to).map_err(err("connect"))?;
    let _server = accept(listener)?;
    client
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(err("timeout"))?;
    let started = Instant::now();
    match client.read(&mut [0u8; 8]) {
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            let ms = started.elapsed().as_millis();
            if (150..5000).contains(&ms) {
                Ok(())
            } else {
                Err(format!("the 200 ms read timeout took {ms} ms"))
            }
        }
        other => Err(format!("a read with nothing to read gave {other:?}")),
    }
}

/// A datagram to 127.0.0.1 and its answer.
fn datagram() -> Result<(), String> {
    let server = UdpSocket::bind((LO, 0)).map_err(err("udp bind"))?;
    let client = UdpSocket::bind("0.0.0.0:0").map_err(err("udp bind"))?;
    for socket in [&server, &client] {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(err("timeout"))?;
    }
    let to = server.local_addr().map_err(err("local_addr"))?;
    if to.ip() != IpAddr::V4(LO) {
        return Err(format!("a socket bound to 127.0.0.1 reports {to}"));
    }
    client.send_to(b"loop", to).map_err(err("send_to"))?;
    let mut buf = [0u8; 64];
    let (n, from) = server.recv_from(&mut buf).map_err(err("recv_from"))?;
    if &buf[..n] != b"loop" || from.ip() != IpAddr::V4(LO) {
        return Err(format!("{n} bytes from {from}"));
    }
    server.send_to(b"back", from).map_err(err("answer"))?;
    let (n, _) = client.recv_from(&mut buf).map_err(err("recv the answer"))?;
    if &buf[..n] != b"back" {
        return Err(format!("the answer was {:?}", &buf[..n]));
    }
    Ok(())
}

pub fn loopback(gateway: SocketAddr, data: &[u8]) -> Result<String, String> {
    let own = own_address(gateway)?;
    // A listener on 127.0.0.1 and one on every address.
    let lo_only = TcpListener::bind((LO, 0)).map_err(err("bind 127.0.0.1"))?;
    let lo_at = lo_only.local_addr().map_err(err("local_addr"))?;
    if lo_at.ip() != IpAddr::V4(LO) {
        return Err(format!("a listener bound to 127.0.0.1 reports {lo_at}"));
    }
    let any = TcpListener::bind("0.0.0.0:0").map_err(err("bind any"))?;
    let any_port = any.local_addr().map_err(err("local_addr"))?.port();
    for listener in [&lo_only, &any] {
        listener.set_nonblocking(true).map_err(err("nonblocking"))?;
    }
    stream(&lo_only, lo_at, data).map_err(|e| format!("127.0.0.1: {e}"))?;
    stream(&any, SocketAddr::new(own, any_port), data).map_err(|e| format!("{own}: {e}"))?;
    stream(&any, SocketAddr::from((LO, any_port)), b"x")
        .map_err(|e| format!("127.0.0.1 to any: {e}"))?;
    // A port that was just closed has nobody behind it.
    let closed = {
        let gone = TcpListener::bind((LO, 0)).map_err(err("bind"))?;
        gone.local_addr().map_err(err("local_addr"))?.port()
    };
    let refused_ms = refused(closed)?;
    read_timeout(&lo_only, lo_at)?;
    datagram()?;
    Ok(format!(
        "echoed {} bytes via 127.0.0.1 and {own}, refused in {refused_ms} ms",
        data.len()
    ))
}
