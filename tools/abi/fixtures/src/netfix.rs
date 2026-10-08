//! `netfix` — `std::net` over the Linux ABI's `AF_INET` shim (stage N5).
//!
//! Unlike the other fixtures this one needs a network: it talks to the echo
//! servers `tools/net/run.py` runs on the host (reached as the gateway,
//! 10.0.2.2) and to the harness itself through a port forward, so it runs under
//! `netd demo=1` (`linux:/system/bin/netfix`), never as `/system/bin/abi-init`.
//!
//! Checks (each prints `NETFIX:<name>:PASS` or `NETFIX:<name>:FAIL:<why>`):
//! `tcp` (200 000 bytes out and back through a duplicated socket and a
//! half-close), `connect_timeout` (a non-blocking connect completed through
//! `poll`), `refused` (a connect that must fail), `addrs` (local and peer
//! addresses), `udp` (datagrams, a connected socket, the size limit),
//! `listen` (accept a connection the harness opens and echo it), and the
//! stage T1 name checks of [`netfix_names`] (`resolv_conf`, `hosts`, `trust`,
//! `dns`): musl's own `getaddrinfo` over `/etc/resolv.conf` and `/etc/hosts`,
//! and [`netfix_msgpoll`] (`msgpoll`, issue #667): a `TcpStream` and a
//! Messenger endpoint in one `epoll_wait`.
//! The final line is `ABI:netfix:PASS`/`FAIL` as for every fixture.

mod common;
mod netfix_msgpoll;
mod netfix_names;

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

const GATEWAY: [u8; 4] = [10, 0, 2, 2];
const ECHO_TCP: u16 = 47771;
const ECHO_UDP: u16 = 47772;
const CLOSED: u16 = 47999;
const LISTEN: u16 = 47774;

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from((GATEWAY, port))
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed ^ ((i >> 8) as u8))
        .collect()
}

fn check(name: &str, outcome: Result<String, String>, failures: &mut Vec<String>) {
    match outcome {
        Ok(detail) => println!("NETFIX:{name}:PASS {detail}"),
        Err(why) => {
            println!("NETFIX:{name}:FAIL:{why}");
            failures.push(format!("{name}: {why}"));
        }
    }
}

fn tcp() -> Result<String, String> {
    let data = pattern(200_000, 7);
    let mut stream = TcpStream::connect(addr(ECHO_TCP)).map_err(|e| format!("connect: {e}"))?;
    // A duplicate shares the connection (threads would too, but the shim gives a
    // thread its own descriptor table); the half-close goes through it.
    let closer = stream.try_clone().map_err(|e| format!("try_clone: {e}"))?;
    stream.set_nonblocking(true).map_err(|e| format!("set_nonblocking: {e}"))?;
    let (mut sent, mut back) = (0usize, Vec::new());
    let mut buf = vec![0u8; 16384];
    let mut half_closed = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let mut progressed = false;
        if sent < data.len() {
            match stream.write(&data[sent..(sent + 8192).min(data.len())]) {
                Ok(n) => {
                    sent += n;
                    progressed = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(format!("write: {e}")),
            }
        } else if !half_closed {
            closer.shutdown(Shutdown::Write).map_err(|e| format!("shutdown: {e}"))?;
            half_closed = true;
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                back.extend_from_slice(&buf[..n]);
                progressed = true;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("read: {e}")),
        }
        if Instant::now() > deadline {
            return Err(format!("stalled at {sent} sent, {} back", back.len()));
        }
        if !progressed {
            thread::sleep(Duration::from_millis(2));
        }
    }
    if back != data {
        let at = back.iter().zip(&data).position(|(a, b)| a != b).unwrap_or(back.len().min(data.len()));
        return Err(format!("echo differs: {} bytes back, first difference at {at}", back.len()));
    }
    Ok(format!("bytes={}", back.len()))
}

fn connect_timeout() -> Result<String, String> {
    let mut stream = TcpStream::connect_timeout(&addr(ECHO_TCP), Duration::from_secs(5))
        .map_err(|e| format!("connect_timeout: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| format!("{e}"))?;
    stream.write_all(b"ping").map_err(|e| format!("write: {e}"))?;
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).map_err(|e| format!("read: {e}"))?;
    if &buf != b"ping" {
        return Err(format!("echoed {buf:?}"));
    }
    stream.set_nodelay(true).map_err(|e| format!("set_nodelay: {e}"))?;
    Ok(String::from("non-blocking connect completed"))
}

fn refused() -> Result<String, String> {
    let started = Instant::now();
    match TcpStream::connect_timeout(&addr(CLOSED), Duration::from_secs(3)) {
        Ok(stream) => Err(format!(
            "a connection to a closed port succeeded after {} ms (peer {:?}, error {:?})",
            started.elapsed().as_millis(),
            stream.peer_addr(),
            stream.take_error()
        )),
        // The host's user networking resets on some hosts and stays silent on others.
        Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::TimedOut) => {
            Ok(format!("{:?} after {} ms", e.kind(), started.elapsed().as_millis()))
        }
        Err(e) => Err(format!("unexpected error {e}")),
    }
}

fn addrs() -> Result<String, String> {
    let stream = TcpStream::connect(addr(ECHO_TCP)).map_err(|e| format!("connect: {e}"))?;
    let peer = stream.peer_addr().map_err(|e| format!("peer_addr: {e}"))?;
    let local = stream.local_addr().map_err(|e| format!("local_addr: {e}"))?;
    if peer != addr(ECHO_TCP) {
        return Err(format!("peer {peer}"));
    }
    if local.port() < 49152 || local.ip().is_unspecified() {
        return Err(format!("local {local}"));
    }
    Ok(format!("{local} -> {peer}"))
}

fn udp() -> Result<String, String> {
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("bind: {e}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(5))).map_err(|e| format!("{e}"))?;
    let port = socket.local_addr().map_err(|e| format!("local_addr: {e}"))?.port();
    if port < 49152 {
        return Err(format!("ephemeral port {port}"));
    }
    let mut buf = [0u8; 2048];
    for round in 0..20u8 {
        let message = pattern(1 + usize::from(round) * 70, round);
        socket.send_to(&message, addr(ECHO_UDP)).map_err(|e| format!("send_to: {e}"))?;
        let (n, from) = socket.recv_from(&mut buf).map_err(|e| format!("recv_from: {e}"))?;
        if from != addr(ECHO_UDP) || buf[..n] != message[..] {
            return Err(format!("round {round}: {n} bytes from {from}"));
        }
    }
    socket.connect(addr(ECHO_UDP)).map_err(|e| format!("connect: {e}"))?;
    socket.send(b"connected").map_err(|e| format!("send: {e}"))?;
    let n = socket.recv(&mut buf).map_err(|e| format!("recv: {e}"))?;
    if &buf[..n] != b"connected" {
        return Err(format!("connected echo {:?}", &buf[..n]));
    }
    let full = vec![1u8; 1472];
    socket.send(&full).map_err(|e| format!("a 1472-byte datagram: {e}"))?;
    let n = socket.recv(&mut buf).map_err(|e| format!("recv of the full datagram: {e}"))?;
    if n != 1472 || buf[..n] != full[..] {
        return Err(format!("the full datagram came back as {n} bytes"));
    }
    if socket.send(&vec![1u8; 1473]).is_ok() {
        return Err(String::from("a 1473-byte datagram was accepted"));
    }
    Ok(format!("local port {port}"))
}

fn listen() -> Result<String, String> {
    let listener = TcpListener::bind(("0.0.0.0", LISTEN)).map_err(|e| format!("bind: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| format!("set_nonblocking: {e}"))?;
    println!("NETFIX:LISTENING port={LISTEN}");
    let deadline = Instant::now() + Duration::from_secs(90);
    let (mut conn, peer) = loop {
        match listener.accept() {
            Ok(pair) => break pair,
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("accept: {e}")),
        }
    };
    conn.set_nonblocking(false).map_err(|e| format!("{e}"))?;
    let mut total = 0usize;
    let mut buf = vec![0u8; 8192];
    loop {
        let n = conn.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        conn.write_all(&buf[..n]).map_err(|e| format!("echo: {e}"))?;
        total += n;
    }
    Ok(format!("{total} bytes echoed for {peer}"))
}

fn main() {
    let mut failures = Vec::new();
    check("tcp", tcp(), &mut failures);
    check("connect_timeout", connect_timeout(), &mut failures);
    check("refused", refused(), &mut failures);
    check("addrs", addrs(), &mut failures);
    check("udp", udp(), &mut failures);
    check("resolv_conf", netfix_names::resolv_conf(), &mut failures);
    check("hosts", netfix_names::hosts(), &mut failures);
    check("trust", netfix_names::trust(), &mut failures);
    match netfix_names::dns() {
        Ok(Some(detail)) => check("dns", Ok(detail), &mut failures),
        Ok(None) => {} // `NETFIX:dns:OFFLINE` already printed
        Err(why) => check("dns", Err(why), &mut failures),
    }
    check("msgpoll", netfix_msgpoll::msgpoll(addr(ECHO_TCP)), &mut failures);
    check("listen", listen(), &mut failures);
    common::report("netfix", failures.is_empty(), &failures.join("; "));
}
