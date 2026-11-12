//! `netctl probe=1`: malformed and out-of-contract requests to `netd`, and a
//! check that it stays up, answers honestly and keeps its counters straight.
//!
//! Every check states what the stack must answer; a wrong answer (or a stack
//! that stops answering) fails the probe with the check's name. The pings that
//! are *allowed* go to the gateway or to an on-link address nobody answers, so
//! the packet capture shows what actually left the machine.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use libmessenger::client::CallError;
use libmessenger::{flags, Header, Parcel, VERSION};
use user::messenger::errno;
use user::messenger::netstack::{self, parcel, wire};
use user::sys;

use super::common::{
    answered_with, connect, endpoint_of, fail, failed, is_errno, local, nap, send_parcel,
    wait_for_address,
};

const GATEWAY: [u8; 4] = [10, 0, 2, 2];
/// An on-link address nobody answers: ARP for it gets no reply.
const NOBODY: [u8; 4] = [10, 0, 2, 77];
/// `ENETUNREACH`, `EAGAIN` as positive numbers.
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
        result: Result<T, CallError>,
        code: i64,
    ) -> Result<(), String> {
        self.expect(&format!("{name} -> errno {code}"), is_errno(&result, code))
    }

    fn rejected<T>(&mut self, name: &str, result: Result<T, CallError>) -> Result<(), String> {
        self.expect(&format!("{name} is refused"), failed(&result))
    }
}

/// The body of a `Ping` for `dst`, whatever its length: the hostile cases
/// need addresses the typed `ping` never takes.
fn ping_body(dst: &[u8], payload_len: u32, timeout_ms: u32) -> Result<Vec<u8>, String> {
    wire::encode_ping_args(&wire::PingArgs {
        dst: dst.to_vec(),
        payload_len,
        timeout_ms,
    })
    .map_err(|error| format!("encoding a ping: {error:?}"))
}

/// A `Ping` request whose caller allows nesting, so several can be in flight
/// on the one channel.
fn ping_parcel(dst: &[u8], payload_len: u32, timeout_ms: u32) -> Result<Parcel, String> {
    let body = ping_body(dst, payload_len, timeout_ms)?;
    let mut request = parcel(wire::METHOD_PING, body);
    request.header.flags = flags::ALLOW_NESTED;
    Ok(request)
}

/// Run the probe; returns how many checks passed.
pub(super) fn run() -> Result<u32, String> {
    let client = connect()?;
    let conn = client.connection();
    let mut checks = Checks { done: 0 };
    wait_for_address(&client)?;
    let before = client.stats().map_err(fail("stats"))?;

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
    checks.refused(
        "a call on a foreign interface id",
        send_parcel(conn, &foreign, None),
        errno::EINVAL,
    )?;
    checks.refused(
        "an unknown method",
        send_parcel(conn, &parcel(0xDEAD_BEEF, Vec::new()), None),
        errno::EINVAL,
    )?;
    checks.rejected(
        "a Ping with an empty body",
        send_parcel(conn, &parcel(wire::METHOD_PING, Vec::new()), None),
    )?;
    checks.rejected(
        "a Ping with a garbage body",
        send_parcel(conn, &parcel(wire::METHOD_PING, vec![0xFF; 9]), None),
    )?;

    // --- Bad arguments: each refused before anything is sent. ----------------
    for dst in [
        &[][..],
        &[10][..],
        &[10, 0, 2][..],
        &[10, 0, 2, 2, 1][..],
        &[0u8; 16][..],
        &[1u8; 200][..],
    ] {
        let body = ping_body(dst, 8, 1000)?;
        checks.refused(
            &format!("a Ping to a {}-byte address", dst.len()),
            send_parcel(conn, &parcel(wire::METHOD_PING, body), None),
            errno::EINVAL,
        )?;
    }
    for payload in [1401u32, 65_536, u32::MAX] {
        let body = ping_body(&GATEWAY, payload, 1000)?;
        checks.refused(
            &format!("a Ping with {payload} payload bytes"),
            send_parcel(conn, &parcel(wire::METHOD_PING, body), None),
            errno::EINVAL,
        )?;
    }
    for timeout in [0u32, 1, 9, 60_001, u32::MAX] {
        let body = ping_body(&GATEWAY, 8, timeout)?;
        checks.refused(
            &format!("a Ping with a {timeout} ms timeout"),
            send_parcel(conn, &parcel(wire::METHOD_PING, body), None),
            errno::EINVAL,
        )?;
    }
    for dst in [
        [0u8, 0, 0, 0],
        [127, 0, 0, 1],
        [224, 0, 0, 1],
        [255, 255, 255, 255],
        [240, 1, 2, 3],
    ] {
        let body = ping_body(&dst, 8, 1000)?;
        checks.refused(
            &format!("a Ping to {dst:?}"),
            send_parcel(conn, &parcel(wire::METHOD_PING, body), None),
            errno::EINVAL,
        )?;
    }
    // A one-way message cannot be answered: it is ignored, and nothing breaks.
    let mut oneway = ping_parcel(&GATEWAY, 8, 1000)?;
    oneway.header.flags = flags::ONE_WAY;
    endpoint_of(conn)
        .map_err(fail("resolving the stack"))?
        .send(&oneway)
        .map_err(local("sending a one-way ping"))?;
    client
        .interfaces()
        .map_err(fail("interfaces after a one-way ping"))?;
    checks.expect("a one-way Ping is ignored", true)?;

    // --- Reads are honest. ---------------------------------------------------
    let interfaces = client.interfaces().map_err(fail("interfaces"))?;
    checks.expect(
        "eth0 first, every interface with a six-byte MAC and a metric",
        interfaces.first().is_some_and(|i| i.name == "eth0")
            && interfaces.iter().all(|i| i.mac.len() == 6 && i.metric > 0),
    )?;
    let addresses = client.addresses().map_err(fail("addresses"))?;
    checks.expect(
        "an IPv4 address with a /1../30 prefix on every interface that has one",
        !addresses.is_empty()
            && addresses
                .iter()
                .all(|a| a.addr.len() == 4 && (1..=30).contains(&a.prefix_len)),
    )?;
    let routes = client.routes().map_err(fail("routes"))?;
    checks.expect(
        "an on-link route and a default route via the gateway",
        routes
            .iter()
            .any(|r| r.prefix_len == 0 && r.gateway == GATEWAY)
            && routes
                .iter()
                .any(|r| r.prefix_len == addresses[0].prefix_len),
    )?;

    // --- Parked pings: a cap per caller, honest timeouts. -------------------
    let endpoint = endpoint_of(conn).map_err(fail("resolving the stack"))?;
    let mut parked = Vec::new();
    for _ in 0..4 {
        let deadline = sys::clock() + 600;
        parked.push(
            endpoint
                .begin_call(&ping_parcel(&NOBODY, 8, 1500)?, Some(deadline))
                .map_err(local("starting a parked ping"))?,
        );
    }
    let fifth = send_parcel(
        conn,
        &ping_parcel(&NOBODY, 8, 1500)?,
        Some(sys::clock() + 200),
    );
    checks.refused("a fifth parked ping from one caller", fifth, EAGAIN)?;
    for (i, txn) in parked.into_iter().enumerate() {
        let reply = endpoint.await_reply(txn);
        let timed_out = answered_with(reply, errno::ETIMEDOUT);
        checks.expect(
            &format!("parked ping {i} ends with ETIMEDOUT, not silence"),
            timed_out,
        )?;
    }

    // A caller that gives up: the stack must survive answering nobody.
    let gave_up = send_parcel(conn, &ping_parcel(&NOBODY, 8, 500)?, Some(sys::clock() + 3));
    checks.refused(
        "a caller whose own deadline passes first",
        gave_up,
        errno::ETIMEDOUT,
    )?;
    for _ in 0..80 {
        nap();
    }
    let after = client
        .stats()
        .map_err(fail("stats after the abandoned ping"))?;
    checks.expect(
        "the stack counted five timed-out pings",
        after.pings_timed_out == before.pings_timed_out + 5,
    )?;
    checks.expect("and sent five", after.pings_sent == before.pings_sent + 5)?;

    // --- A real ping still works, and a renewal and a reattach are survived. -
    let echo = netstack::ping(&client, GATEWAY, 56, 2000).map_err(fail("a ping to the gateway"))?;
    checks.expect(
        "the gateway answers with the 56 bytes sent",
        echo.bytes == 56 && echo.source == GATEWAY,
    )?;
    let leases = client.stats().map_err(fail("stats"))?.leases;
    client.renew().map_err(fail("renew"))?;
    wait_for_address(&client)?;
    let renewed = client.stats().map_err(fail("stats"))?;
    checks.expect(
        "a renewal obtains a fresh lease",
        renewed.leases == leases + 1 && renewed.lease_losses > before.lease_losses,
    )?;
    client.reattach().map_err(fail("reattach"))?;
    let deadline = sys::clock() + 800;
    while !client
        .interfaces()
        .map_err(fail("interfaces"))?
        .first()
        .is_some_and(|i| i.link)
        && sys::clock() < deadline
    {
        nap();
    }
    let rebuilt = client.stats().map_err(fail("stats"))?;
    checks.expect(
        "the NIC attachment was rebuilt once",
        rebuilt.nic_resets == before.nic_resets + 1,
    )?;
    let echo =
        netstack::ping(&client, GATEWAY, 8, 3000).map_err(fail("a ping after reattaching"))?;
    checks.expect("the stack pings again after reattaching", echo.bytes == 8)?;
    checks.expect(
        "no frame was dropped for length or ring trouble",
        rebuilt.rx_bad_length == 0 && rebuilt.tx_dropped == before.tx_dropped,
    )?;
    Ok(checks.done)
}
