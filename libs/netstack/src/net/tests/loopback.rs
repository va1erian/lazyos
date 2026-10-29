//! Loopback through `Net` (issue #1019): connections and datagrams to
//! 127.0.0.1 and to a card's own address stay on `lo`, behave as real TCP and
//! UDP (bytes, close, an immediate refusal), keep bound addresses apart, and
//! leave nothing behind after many rounds.

use std::vec::Vec;

use crate::net::IfKind::Wired;
use crate::stack::{Kind, SockAddr, SockError, LOOPBACK_ADDR};
use crate::testmulti::MultiPair;

const SERVER: u64 = 1;
const CLIENT: u64 = 2;

fn at(addr: [u8; 4], port: u16) -> SockAddr {
    SockAddr { addr, port }
}

/// A listener of `SERVER` bound to `local`.
fn listener(pair: &mut MultiPair, local: SockAddr) -> u32 {
    let id = pair.net.socket_open(SERVER, Kind::Stream).unwrap();
    pair.net.socket_bind(id, SERVER, local).unwrap();
    pair.net.socket_listen(id, SERVER, 4).unwrap();
    id
}

/// A connection of `CLIENT` to `to` and how it ended: `Ok(true)` connected.
fn connect(pair: &mut MultiPair, to: SockAddr) -> (u32, Result<bool, SockError>) {
    let id = pair.net.socket_open(CLIENT, Kind::Stream).unwrap();
    pair.net.socket_connect(id, CLIENT, to).unwrap();
    let mut status = Ok(false);
    pair.run_until(200, |p| {
        status = p.net.socket_connect_status(id, CLIENT);
        status != Ok(false)
    });
    (id, status)
}

/// Accept one connection on `listener`, waiting up to 200 ms.
fn accept(pair: &mut MultiPair, listener: u32) -> (u32, SockAddr) {
    let mut got = None;
    pair.run_until(200, |p| {
        got = p.net.socket_accept(listener, SERVER).unwrap();
        got.is_some()
    });
    got.expect("a connection to accept")
}

/// Move `data` from `from` (of `tx`) to `to` (of `rx`) and return what arrived.
fn transfer(
    pair: &mut MultiPair,
    (from, tx): (u32, u64),
    (to, rx): (u32, u64),
    data: &[u8],
) -> Vec<u8> {
    let mut sent = 0;
    let mut got = Vec::new();
    pair.run_until(5000, |p| {
        while sent < data.len() {
            let end = data.len().min(sent + 16 * 1024);
            match p.net.socket_send(from, tx, &data[sent..end]) {
                Ok(n) => sent += n,
                Err(SockError::WouldBlock) => break,
                Err(error) => panic!("send: {error:?}"),
            }
        }
        while let Some(chunk) = p.net.socket_recv(to, rx, 16 * 1024).unwrap() {
            if chunk.is_empty() {
                break;
            }
            got.extend_from_slice(&chunk);
        }
        got.len() == data.len()
    });
    got
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed)
        .collect()
}

#[test]
fn a_stream_to_127_0_0_1_carries_bytes_both_ways_and_closes() {
    let mut pair = MultiPair::new(&[Wired]);
    let wire_before = pair.peers[0].device_stats().rx_frames;
    let server = listener(&mut pair, at(LOOPBACK_ADDR, 5000));
    assert_eq!(
        pair.net.socket_local_addr(server, SERVER),
        Ok(at(LOOPBACK_ADDR, 5000))
    );
    let (client, status) = connect(&mut pair, at(LOOPBACK_ADDR, 5000));
    assert_eq!(status, Ok(true));
    let (conn, from) = accept(&mut pair, server);
    let client_addr = pair.net.socket_local_addr(client, CLIENT).unwrap();
    assert_eq!(client_addr.addr, LOOPBACK_ADDR);
    assert_eq!(from, client_addr, "the server sees the client's address");
    assert_eq!(
        pair.net.socket_peer_addr(client, CLIENT),
        Ok(at(LOOPBACK_ADDR, 5000))
    );
    assert_eq!(
        pair.net.socket_local_addr(conn, SERVER),
        Ok(at(LOOPBACK_ADDR, 5000))
    );

    let up = pattern(300_000, 7);
    assert_eq!(
        transfer(&mut pair, (client, CLIENT), (conn, SERVER), &up),
        up
    );
    let down = pattern(200_000, 9);
    assert_eq!(
        transfer(&mut pair, (conn, SERVER), (client, CLIENT), &down),
        down
    );

    // The client closes: the server reads the end of the stream.
    pair.net.socket_close(client, CLIENT, pair.now).unwrap();
    assert!(pair.run_until(1000, |p| {
        p.net.socket_recv(conn, SERVER, 1024) == Ok(Some(Vec::new()))
    }));
    pair.net.socket_close(conn, SERVER, pair.now).unwrap();
    pair.net.socket_close(server, SERVER, pair.now).unwrap();
    assert_eq!(
        pair.peers[0].device_stats().rx_frames,
        wire_before,
        "nothing reached the card's wire"
    );
}

#[test]
fn the_cards_own_address_reaches_a_wildcard_listener_on_lo() {
    let mut pair = MultiPair::new(&[Wired]);
    let me = MultiPair::net_ip(0);
    let wire_before = pair.peers[0].device_stats().rx_frames;
    let server = listener(&mut pair, at([0; 4], 6000));
    let (client, status) = connect(&mut pair, at(me, 6000));
    assert_eq!(status, Ok(true));
    let (conn, from) = accept(&mut pair, server);
    assert_eq!(
        from.addr, me,
        "the connection comes from the card's address"
    );
    assert_eq!(pair.net.socket_local_addr(client, CLIENT).unwrap().addr, me);
    assert_eq!(pair.net.socket_peer_addr(client, CLIENT), Ok(at(me, 6000)));
    let data = pattern(50_000, 3);
    assert_eq!(
        transfer(&mut pair, (client, CLIENT), (conn, SERVER), &data),
        data
    );
    // 127.0.0.1 reaches the same wildcard listener.
    let (_, status) = connect(&mut pair, at(LOOPBACK_ADDR, 6000));
    assert_eq!(status, Ok(true));
    assert_eq!(pair.peers[0].device_stats().rx_frames, wire_before);
}

#[test]
fn a_connect_to_a_local_port_with_no_listener_is_refused_at_once() {
    let mut pair = MultiPair::new(&[Wired]);
    for to in [LOOPBACK_ADDR, MultiPair::net_ip(0)] {
        let start = pair.now;
        let (_, status) = connect(&mut pair, at(to, 7000));
        assert_eq!(status, Err(SockError::Refused), "to {to:?}");
        assert!(pair.now - start <= 50, "refused in {} ms", pair.now - start);
    }
    assert_eq!(pair.net.socket_counters().refused, 2);
}

#[test]
fn loopback_works_with_no_card_at_all() {
    let mut pair = MultiPair::new(&[]);
    let server = listener(&mut pair, at([0; 4], 5100));
    let (client, status) = connect(&mut pair, at(LOOPBACK_ADDR, 5100));
    assert_eq!(status, Ok(true));
    let (conn, _) = accept(&mut pair, server);
    let data = pattern(10_000, 1);
    assert_eq!(
        transfer(&mut pair, (client, CLIENT), (conn, SERVER), &data),
        data
    );
    // Other destinations stay unreachable.
    let other = pair.net.socket_open(CLIENT, Kind::Stream).unwrap();
    assert_eq!(
        pair.net
            .socket_connect(other, CLIENT, at([10, 9, 9, 9], 80)),
        Err(SockError::Unreachable)
    );
}

#[test]
fn bound_addresses_are_kept_apart_on_lo() {
    let mut pair = MultiPair::new(&[Wired]);
    let me = MultiPair::net_ip(0);
    listener(&mut pair, at(LOOPBACK_ADDR, 8000));
    let (_, status) = connect(&mut pair, at(me, 8000));
    assert_eq!(status, Err(SockError::Refused), "127.0.0.1-only listener");
    listener(&mut pair, at(me, 8001));
    let (_, status) = connect(&mut pair, at(LOOPBACK_ADDR, 8001));
    assert_eq!(
        status,
        Err(SockError::Refused),
        "card-address-only listener"
    );
    let (_, status) = connect(&mut pair, at(me, 8001));
    assert_eq!(status, Ok(true));
    // A socket bound to 127.0.0.1 reports it before it is placed, and the
    // rest of 127/8 is not this machine.
    let id = pair.net.socket_open(CLIENT, Kind::Stream).unwrap();
    pair.net
        .socket_bind(id, CLIENT, at(LOOPBACK_ADDR, 8002))
        .unwrap();
    assert_eq!(
        pair.net.socket_local_addr(id, CLIENT),
        Ok(at(LOOPBACK_ADDR, 8002))
    );
    let other = pair.net.socket_open(CLIENT, Kind::Stream).unwrap();
    assert_eq!(
        pair.net
            .socket_bind(other, CLIENT, at([127, 0, 0, 2], 8003)),
        Err(SockError::BadAddress)
    );
    // A socket bound to 127.0.0.1 cannot reach the network.
    assert_eq!(
        pair.net
            .socket_connect(id, CLIENT, at(MultiPair::peer_ip(0), 80)),
        Err(SockError::Unreachable)
    );
}

#[test]
fn datagrams_to_127_0_0_1_and_the_cards_address_reach_a_bound_socket() {
    let mut pair = MultiPair::new(&[Wired]);
    let me = MultiPair::net_ip(0);
    let wire_before = pair.peers[0].device_stats().rx_frames;
    let server = pair.net.socket_open(SERVER, Kind::Datagram).unwrap();
    pair.net
        .socket_bind(server, SERVER, at([0; 4], 9000))
        .unwrap();
    let only_lo = pair.net.socket_open(SERVER, Kind::Datagram).unwrap();
    pair.net
        .socket_bind(only_lo, SERVER, at(LOOPBACK_ADDR, 9001))
        .unwrap();
    let client = pair.net.socket_open(CLIENT, Kind::Datagram).unwrap();
    for (n, to) in [
        at(LOOPBACK_ADDR, 9000),
        at(me, 9000),
        at(LOOPBACK_ADDR, 9001),
    ]
    .into_iter()
    .enumerate()
    {
        let payload = pattern(100 + n, n as u8);
        pair.net
            .socket_sendto(client, CLIENT, to, &payload)
            .unwrap();
        let target = if to.port == 9001 { only_lo } else { server };
        let mut got = None;
        pair.run_until(200, |p| {
            got = p.net.socket_recvfrom(target, SERVER, 2048).unwrap();
            got.is_some()
        });
        let (data, from) = got.expect("the datagram");
        assert_eq!(data, payload);
        assert_eq!(from.addr, to.addr, "sent from the address it was sent to");
        // The answer goes back to where it came from.
        pair.net
            .socket_sendto(target, SERVER, from, b"ack")
            .unwrap();
        let mut back = None;
        pair.run_until(200, |p| {
            back = p.net.socket_recvfrom(client, CLIENT, 64).unwrap();
            back.is_some()
        });
        assert_eq!(back.expect("the answer").0, b"ack");
    }
    // A datagram to the card's address does not reach the 127.0.0.1 socket.
    pair.net
        .socket_sendto(client, CLIENT, at(me, 9001), b"x")
        .unwrap();
    pair.step(50);
    assert_eq!(pair.net.socket_recvfrom(only_lo, SERVER, 64), Ok(None));
    assert_eq!(pair.peers[0].device_stats().rx_frames, wire_before);
}

#[test]
fn a_card_that_leaves_takes_what_lo_held_for_its_address() {
    let mut pair = MultiPair::new(&[Wired]);
    let me = MultiPair::net_ip(0);
    let open_on_lo = pair.net.loopback().socket_open_count();
    // Listeners get their `lo` replica only when a local connection comes.
    let bound = listener(&mut pair, at(me, 8100));
    let any = listener(&mut pair, at([0; 4], 8101));
    assert_eq!(pair.net.loopback().socket_open_count(), open_on_lo);
    let (client, status) = connect(&mut pair, at(me, 8101));
    assert_eq!(status, Ok(true));
    let (conn, _) = accept(&mut pair, any);
    let (_, status) = connect(&mut pair, at(me, 8100));
    assert_eq!(status, Ok(true));
    let slot = pair.net.units().next().unwrap().0;
    pair.net.remove_interface(slot);
    // The connection over the card's address is reset at both ends, the
    // listener bound to it is gone; the wildcard listener stays.
    assert_eq!(
        pair.net.socket_recv(client, CLIENT, 16),
        Err(SockError::Reset)
    );
    assert_eq!(
        pair.net.socket_recv(conn, SERVER, 16),
        Err(SockError::Reset)
    );
    assert_eq!(
        pair.net.socket_accept(bound, SERVER),
        Err(SockError::InvalidState)
    );
    for (id, owner) in [(client, CLIENT), (conn, SERVER), (bound, SERVER)] {
        pair.net.socket_close(id, owner, pair.now).unwrap();
    }
    let (_, status) = connect(&mut pair, at(LOOPBACK_ADDR, 8101));
    assert_eq!(status, Ok(true));
}

#[test]
fn a_connected_datagram_socket_moves_between_lo_and_a_card() {
    let mut pair = MultiPair::new(&[Wired]);
    let server = pair.net.socket_open(SERVER, Kind::Datagram).unwrap();
    pair.net
        .socket_bind(server, SERVER, at(LOOPBACK_ADDR, 9100))
        .unwrap();
    let client = pair.net.socket_open(CLIENT, Kind::Datagram).unwrap();
    let remote = at(MultiPair::peer_ip(0), 53);
    for peer in [
        at(LOOPBACK_ADDR, 9100),
        remote,
        at(LOOPBACK_ADDR, 9100),
        remote,
    ] {
        pair.net.socket_connect(client, CLIENT, peer).unwrap();
        assert_eq!(pair.net.socket_peer_addr(client, CLIENT), Ok(peer));
    }
    pair.net
        .socket_connect(client, CLIENT, at(LOOPBACK_ADDR, 9100))
        .unwrap();
    pair.net.socket_send(client, CLIENT, b"here").unwrap();
    let mut got = None;
    pair.run_until(200, |p| {
        got = p.net.socket_recvfrom(server, SERVER, 64).unwrap();
        got.is_some()
    });
    assert_eq!(got.expect("the datagram").0, b"here");
}

#[test]
fn a_socket_on_any_address_does_not_report_127_0_0_1() {
    let mut pair = MultiPair::new(&[]);
    let udp = pair.net.socket_open(SERVER, Kind::Datagram).unwrap();
    pair.net.socket_bind(udp, SERVER, at([0; 4], 9200)).unwrap();
    assert_eq!(
        pair.net.socket_local_addr(udp, SERVER),
        Ok(at([0; 4], 9200))
    );
    let tcp = listener(&mut pair, at([0; 4], 9201));
    // Its `lo` replica, made by this connection, does not change that.
    assert_eq!(connect(&mut pair, at(LOOPBACK_ADDR, 9201)).1, Ok(true));
    assert_eq!(
        pair.net.socket_local_addr(tcp, SERVER),
        Ok(at([0; 4], 9201))
    );
}

#[test]
fn eight_cards_still_bind_and_the_first_seven_are_local() {
    let mut pair = MultiPair::new(&[Wired; 8]);
    for n in 0..8 {
        let id = listener(&mut pair, at(MultiPair::net_ip(n), 8300 + n as u16));
        assert!(pair.net.socket_local_addr(id, SERVER).is_ok(), "card {n}");
    }
    for n in 0..7 {
        let (_, status) = connect(&mut pair, at(MultiPair::net_ip(n), 8300 + n as u16));
        assert_eq!(status, Ok(true), "card {n}");
    }
}

#[test]
fn stress_many_loopback_connections_leave_nothing_behind() {
    let mut pair = MultiPair::new(&[Wired]);
    let server = listener(&mut pair, at([0; 4], 5200));
    let to = [at(LOOPBACK_ADDR, 5200), at(MultiPair::net_ip(0), 5200)];
    for round in 0..300 {
        let (client, status) = connect(&mut pair, to[round % 2]);
        assert_eq!(status, Ok(true), "round {round}");
        let (conn, _) = accept(&mut pair, server);
        let data = pattern(1 + round * 37 % 5000, round as u8);
        assert_eq!(
            transfer(&mut pair, (client, CLIENT), (conn, SERVER), &data),
            data
        );
        pair.net.socket_close(client, CLIENT, pair.now).unwrap();
        pair.net.socket_close(conn, SERVER, pair.now).unwrap();
        if round % 50 == 0 {
            let (refused, status) = connect(&mut pair, at(LOOPBACK_ADDR, 5201));
            assert_eq!(status, Err(SockError::Refused));
            pair.net.socket_close(refused, CLIENT, pair.now).unwrap();
        }
    }
    pair.net.socket_close(server, SERVER, pair.now).unwrap();
    assert!(pair.run_until(40_000, |p| p.net.socket_closing() == 0));
    assert_eq!(pair.net.socket_open_count(), 0);
    assert_eq!(pair.net.loopback().socket_open_count(), 0);
    assert_eq!(pair.net.loopback().looped_frames(), 0);
    let counters = pair.net.socket_counters();
    assert_eq!(counters.opened, counters.closed);
}
