//! `netctl socksoak=<n>`: `n` rounds of opening, using and closing sockets
//! against the host's echo servers (the gateway, ports 47771 and 47772), then a
//! check that nothing is left behind.
//!
//! (A connection to a closed port is refused or, on hosts whose user-mode
//! networking stays silent, times out; it must never succeed.)
//!
//! A leak of anything per round shows up in the socket counters (`open` back
//! to its starting value, `opened - closed` consistent, every connection and
//! datagram accounted for) and in the fabric snapshot's per-task usage of
//! `netd`, which must be exactly what it was before the first round. The
//! traffic is real, so the capture shows every handshake and every FIN.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::netsock::{wire, Addr, Client};
use user::messenger::netstd::{TcpListener, TcpStream, UdpSocket};
use user::messenger::{fabric_stats, TaskUsage};
use user::sysinfo;

use super::common::{fail, nap};

const GATEWAY: [u8; 4] = [10, 0, 2, 2];
/// The host's TCP echo server (`tools/net/run.py`).
pub(super) const ECHO_TCP: u16 = 47_771;
/// The host's UDP echo server.
pub(super) const ECHO_UDP: u16 = 47_772;
/// A port nothing on the host listens on.
const CLOSED_PORT: u16 = 47_999;

/// What `netd` holds right now.
fn netd_holdings() -> Result<Option<TaskUsage>, String> {
    let fabric = fabric_stats().map_err(fail("fabric stats"))?;
    let snapshot =
        sysinfo::snapshot().map_err(|code| format!("system snapshot: errno {}", -code))?;
    for task in snapshot.live_tasks() {
        let name = task.name().to_ascii_lowercase();
        if name == "netd" || name.starts_with("netd.") {
            return Ok(fabric.tasks.get(task.pid as usize).copied());
        }
    }
    Ok(None)
}

/// One round: a stream echo, a datagram echo, a listener opened and dropped,
/// and every eighth round a refused connection. Returns the bytes echoed.
fn round(client: &Rc<Client>, round: u32) -> Result<u64, String> {
    let stream =
        TcpStream::connect(client, Addr::new(GATEWAY, ECHO_TCP), 5000).map_err(fail("connect"))?;
    let payload: Vec<u8> = (0..(round * 53) % 900 + 1)
        .map(|i| (i ^ round) as u8)
        .collect();
    stream.write_all(&payload, 3000).map_err(fail("send"))?;
    let mut echoed = Vec::new();
    while echoed.len() < payload.len() {
        let chunk = stream
            .read(payload.len() - echoed.len(), 3000)
            .map_err(fail("recv"))?;
        if chunk.is_empty() {
            return Err(String::from("the echo ended early"));
        }
        echoed.extend_from_slice(&chunk);
    }
    if echoed != payload {
        return Err(String::from("the echo differs from what was sent"));
    }
    drop(stream);

    let udp = UdpSocket::bind(client, 0).map_err(fail("bind"))?;
    let body = [round as u8; 64];
    udp.send_to(&body, Addr::new(GATEWAY, ECHO_UDP))
        .map_err(fail("sendto"))?;
    let (reply, _) = udp.recv_from(2048, 3000).map_err(fail("recvfrom"))?;
    if reply != body {
        return Err(String::from("the datagram echo differs"));
    }
    drop(udp);

    drop(TcpListener::bind(client, 0, 4).map_err(fail("listen"))?);
    if round.is_multiple_of(8) {
        let s = client.open(wire::SOCK_KIND_STREAM).map_err(fail("open"))?;
        let attempt = client.connect_to(s, Addr::new(GATEWAY, CLOSED_PORT), 2500);
        client.close(s).map_err(fail("close"))?;
        if attempt.is_ok() {
            return Err(String::from("a connection to a closed port succeeded"));
        }
    }
    Ok(payload.len() as u64)
}

/// Run `iterations` rounds; returns how many completed.
pub(super) fn run(client: &Rc<Client>, iterations: u32) -> Result<u32, String> {
    let before = netd_holdings()?;
    let stats_before = client.stats().map_err(fail("socket stats"))?;
    let mut bytes = 0u64;
    for n in 1..=iterations {
        bytes += round(client, n).map_err(|e| format!("round {n}: {e}"))?;
    }
    // Let the closing handshakes finish.
    for _ in 0..60 {
        nap();
    }
    let stats_after = client.stats().map_err(fail("socket stats"))?;
    if stats_after.open != stats_before.open {
        return Err(format!(
            "{} sockets open after the soak, {} before",
            stats_after.open, stats_before.open
        ));
    }
    let opened = stats_after.opened - stats_before.opened;
    let closed = stats_after.closed - stats_before.closed;
    if opened != closed {
        return Err(format!("{opened} sockets opened but {closed} closed"));
    }
    let connected = stats_after.connected - stats_before.connected;
    if connected != u64::from(iterations) {
        return Err(format!("{connected} connections for {iterations} rounds"));
    }
    if stats_after.tx_bytes - stats_before.tx_bytes < bytes
        || stats_after.rx_bytes - stats_before.rx_bytes < bytes
    {
        return Err(String::from("the byte counters are below what was echoed"));
    }
    if stats_after.not_owner != stats_before.not_owner {
        return Err(String::from(
            "a call was refused for ownership during the soak",
        ));
    }
    let after = netd_holdings()?;
    if let (Some(was), Some(now)) = (before, after) {
        for (what, a, b) in [
            ("handles", was.handles, now.handles),
            ("shared buffers", was.buffers, now.buffers),
            ("buffer bytes", was.buffer_bytes, now.buffer_bytes),
        ] {
            if a != b {
                return Err(format!(
                    "netd: {what} went from {a} to {b} over {iterations} rounds"
                ));
            }
        }
    }
    Ok(iterations)
}
