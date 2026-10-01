//! The socket layer over two stacks wired back to back: TCP open, transfer and
//! close, refusal, ownership, quotas, UDP, reclaim, and a connect/close soak.

use std::vec::Vec;

use crate::stack::{ready, Kind, SockAddr, SockError, MAX_PER_OWNER, MAX_SOCKETS};
use crate::testpair::*;

pub(super) const SERVER: u64 = 7;
pub(super) const CLIENT: u64 = 9;
pub(super) const PORT: u16 = 5000;

pub(super) fn b_addr(port: u16) -> SockAddr {
    SockAddr { addr: B_IP, port }
}

pub(super) fn any(port: u16) -> SockAddr {
    SockAddr { addr: [0; 4], port }
}

/// A listener on B and a connected client on A: `(listener, client, accepted)`.
pub(super) fn connected(p: &mut Pair) -> (u32, u32, u32) {
    let listener = p.b.socket_open(SERVER, Kind::Stream).unwrap();
    p.b.socket_bind(listener, SERVER, any(PORT)).unwrap();
    p.b.socket_listen(listener, SERVER, 4).unwrap();
    let client = p.a.socket_open(CLIENT, Kind::Stream).unwrap();
    p.a.socket_connect(client, CLIENT, b_addr(PORT)).unwrap();
    assert!(
        p.run_until(2000, |p| p.a.socket_connect_status(client, CLIENT)
            == Ok(true))
    );
    assert!(p.run_until(2000, |p| {
        p.b.socket_readiness(listener, SERVER).unwrap() & ready::ACCEPTABLE != 0
    }));
    let (accepted, peer) =
        p.b.socket_accept(listener, SERVER)
            .unwrap()
            .expect("a connection");
    assert_eq!(peer.addr, A_IP);
    (listener, client, accepted)
}

/// Read until the stream ends or `want` bytes arrived.
pub(super) fn drain(p: &mut Pair, side_a: bool, id: u32, owner: u64, want: usize) -> Vec<u8> {
    let mut out = Vec::new();
    p.run_until(20_000, |p| {
        let stack = if side_a { &mut p.a } else { &mut p.b };
        while let Ok(Some(chunk)) = stack.socket_recv(id, owner, 4096) {
            if chunk.is_empty() {
                return true;
            }
            out.extend_from_slice(&chunk);
        }
        out.len() >= want
    });
    out
}

#[test]
fn a_stream_round_trips_both_ways() {
    let mut p = Pair::new();
    let (_l, client, accepted) = connected(&mut p);
    assert_eq!(
        p.a.socket_send(client, CLIENT, b"hello server").unwrap(),
        12
    );
    assert_eq!(drain(&mut p, false, accepted, SERVER, 12), b"hello server");
    p.b.socket_send(accepted, SERVER, b"hello client").unwrap();
    assert_eq!(drain(&mut p, true, client, CLIENT, 12), b"hello client");
    let local = p.a.socket_local_addr(client, CLIENT).unwrap();
    assert_eq!(local.addr, A_IP);
    assert!(local.port >= crate::stack::EPHEMERAL_FIRST);
    assert_eq!(p.a.socket_peer_addr(client, CLIENT).unwrap(), b_addr(PORT));
}

#[test]
fn more_than_one_buffer_arrives_in_order() {
    let mut p = Pair::new();
    let (_l, client, accepted) = connected(&mut p);
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let mut sent = 0;
    let mut got = Vec::new();
    while got.len() < data.len() {
        if sent < data.len() {
            let end = (sent + 4096).min(data.len());
            match p.a.socket_send(client, CLIENT, &data[sent..end]) {
                Ok(n) => sent += n,
                Err(SockError::WouldBlock) => {}
                Err(e) => panic!("{e:?}"),
            }
        }
        p.step(10);
        while let Ok(Some(chunk)) = p.b.socket_recv(accepted, SERVER, 16 * 1024) {
            got.extend_from_slice(&chunk);
        }
        assert!(
            p.now < 120_000,
            "stalled at {} of {}",
            got.len(),
            data.len()
        );
    }
    assert_eq!(got, data);
}

#[test]
fn a_full_send_buffer_would_block_and_recovers() {
    let mut p = Pair::new();
    let (_l, client, accepted) = connected(&mut p);
    let chunk = [7u8; 4096];
    let mut queued = 0;
    loop {
        match p.a.socket_send(client, CLIENT, &chunk) {
            Ok(n) => queued += n,
            Err(SockError::WouldBlock) => break,
            Err(e) => panic!("{e:?}"),
        }
        assert!(queued <= 64 * 1024, "the buffer is bounded");
    }
    assert!(queued >= 16 * 1024 - 4096);
    assert_eq!(drain(&mut p, false, accepted, SERVER, queued).len(), queued);
    assert!(
        p.a.socket_send(client, CLIENT, &chunk).is_ok(),
        "room again"
    );
}

#[test]
fn close_is_seen_as_an_empty_read_after_the_data() {
    let mut p = Pair::new();
    let (_l, client, accepted) = connected(&mut p);
    p.a.socket_send(client, CLIENT, b"bye").unwrap();
    p.a.socket_close(client, CLIENT, p.now).unwrap();
    let got = drain(&mut p, false, accepted, SERVER, usize::MAX);
    assert_eq!(got, b"bye");
    assert_eq!(p.b.socket_recv(accepted, SERVER, 10), Ok(Some(Vec::new())));
    // The closed socket's id is gone for good.
    assert_eq!(
        p.a.socket_send(client, CLIENT, b"x"),
        Err(SockError::BadSocket)
    );
    p.b.socket_close(accepted, SERVER, p.now).unwrap();
    assert!(p.run_until(5000, |p| p.a.socket_closing() == 0
        && p.b.socket_closing() == 0));
}

#[test]
fn shutdown_write_ends_the_peers_stream_but_not_ours() {
    let mut p = Pair::new();
    let (_l, client, accepted) = connected(&mut p);
    p.a.socket_shutdown(client, CLIENT, false, true).unwrap();
    assert_eq!(p.a.socket_send(client, CLIENT, b"x"), Err(SockError::Pipe));
    assert_eq!(drain(&mut p, false, accepted, SERVER, usize::MAX), b"");
    p.b.socket_send(accepted, SERVER, b"still here").unwrap();
    assert_eq!(drain(&mut p, true, client, CLIENT, 10), b"still here");
}

#[test]
fn a_connection_to_a_closed_port_is_refused() {
    let mut p = Pair::new();
    let client = p.a.socket_open(CLIENT, Kind::Stream).unwrap();
    p.a.socket_connect(client, CLIENT, b_addr(PORT)).unwrap();
    assert!(p.run_until(2000, |p| p.a.socket_connect_status(client, CLIENT).is_err()));
    assert_eq!(
        p.a.socket_connect_status(client, CLIENT),
        Err(SockError::Refused)
    );
    assert_eq!(p.a.socket_counters().refused, 1);
    let bits = p.a.socket_readiness(client, CLIENT).unwrap();
    assert!(bits & ready::ERROR != 0);
}

#[test]
fn only_the_owner_may_use_a_socket() {
    let mut p = Pair::new();
    let (listener, client, _a) = connected(&mut p);
    assert_eq!(
        p.a.socket_send(client, CLIENT + 1, b"x"),
        Err(SockError::NotOwner)
    );
    assert_eq!(
        p.a.socket_close(client, CLIENT + 1, 0),
        Err(SockError::NotOwner)
    );
    assert_eq!(
        p.b.socket_accept(listener, CLIENT),
        Err(SockError::NotOwner)
    );
    assert_eq!(
        p.a.socket_recv(0xDEAD_BEEF, CLIENT, 10),
        Err(SockError::BadSocket)
    );
}

#[test]
fn bad_arguments_are_refused_before_anything_happens() {
    let mut p = Pair::new();
    let s = p.a.socket_open(CLIENT, Kind::Stream).unwrap();
    let to = |addr, port| SockAddr { addr, port };
    assert_eq!(
        p.a.socket_bind(s, CLIENT, any(80)),
        Err(SockError::Privileged)
    );
    assert_eq!(
        p.a.socket_bind(s, CLIENT, to(B_IP, 6000)),
        Err(SockError::BadAddress)
    );
    assert_eq!(
        p.a.socket_connect(s, CLIENT, b_addr(0)),
        Err(SockError::BadAddress)
    );
    assert_eq!(
        p.a.socket_connect(s, CLIENT, to([255; 4], 9)),
        Err(SockError::BadAddress)
    );
    assert_eq!(
        p.a.socket_connect(s, CLIENT, to([127, 0, 0, 1], 9)),
        Err(SockError::BadAddress)
    );
    assert_eq!(
        p.a.socket_connect(s, CLIENT, to([192, 168, 1, 1], 9)),
        Err(SockError::Unreachable),
        "no gateway"
    );
    assert_eq!(p.a.socket_recv(s, CLIENT, 10), Err(SockError::NotConnected));
    assert_eq!(
        p.a.socket_send(s, CLIENT, b"x"),
        Err(SockError::NotConnected)
    );
    assert_eq!(p.a.socket_recv(s, CLIENT, 0), Err(SockError::BadAddress));
    assert_eq!(p.a.socket_send(s, CLIENT, &[]), Err(SockError::BadAddress));
    assert_eq!(p.a.socket_listen(s, CLIENT, 0), Err(SockError::BadAddress));
    assert_eq!(p.a.socket_listen(s, CLIENT, 99), Err(SockError::BadAddress));
    assert_eq!(p.a.socket_accept(s, CLIENT), Err(SockError::InvalidState));
    p.a.socket_listen(s, CLIENT, 1).unwrap();
    assert_eq!(p.a.socket_recv(s, CLIENT, 10), Err(SockError::InvalidState));
    assert_eq!(
        p.a.socket_listen(s, CLIENT, 1),
        Err(SockError::InvalidState)
    );
}

#[test]
fn a_port_in_use_cannot_be_bound_twice() {
    let mut p = Pair::new();
    let a = p.a.socket_open(CLIENT, Kind::Stream).unwrap();
    let b = p.a.socket_open(CLIENT, Kind::Stream).unwrap();
    p.a.socket_bind(a, CLIENT, any(6000)).unwrap();
    p.a.socket_listen(a, CLIENT, 1).unwrap();
    assert_eq!(
        p.a.socket_bind(b, CLIENT, any(6000)),
        Err(SockError::AddrInUse)
    );
    // A datagram socket has its own port space.
    let u = p.a.socket_open(CLIENT, Kind::Datagram).unwrap();
    p.a.socket_bind(u, CLIENT, any(6000)).unwrap();
}

#[test]
fn quotas_hold_per_owner_and_in_total() {
    let mut p = Pair::new();
    for _ in 0..MAX_PER_OWNER {
        p.a.socket_open(1, Kind::Datagram).unwrap();
    }
    assert_eq!(
        p.a.socket_open(1, Kind::Datagram),
        Err(SockError::TooManyForOwner)
    );
    for owner in 2..=(MAX_SOCKETS / MAX_PER_OWNER) as u64 {
        for _ in 0..MAX_PER_OWNER {
            p.a.socket_open(owner, Kind::Stream).unwrap();
        }
    }
    assert_eq!(p.a.socket_open(100, Kind::Stream), Err(SockError::TooMany));
    assert_eq!(p.a.sockets_close_owner(1, p.now), MAX_PER_OWNER);
    p.a.socket_open(100, Kind::Stream).unwrap();
}

#[test]
fn ids_are_not_reused_for_a_new_socket() {
    let mut p = Pair::new();
    let first = p.a.socket_open(1, Kind::Datagram).unwrap();
    p.a.socket_close(first, 1, p.now).unwrap();
    let second = p.a.socket_open(1, Kind::Datagram).unwrap();
    assert_ne!(first, second);
    assert_eq!(p.a.socket_close(first, 1, p.now), Err(SockError::BadSocket));
}

#[test]
fn reclaiming_an_owner_ends_its_connections() {
    let mut p = Pair::new();
    let (_l, client, accepted) = connected(&mut p);
    p.a.socket_send(client, CLIENT, b"x").unwrap();
    p.step(50);
    assert_eq!(p.a.sockets_close_owner(CLIENT, p.now), 1);
    assert_eq!(p.a.socket_counters().reclaimed, 1);
    // The far end reads the byte, then learns the stream ended.
    assert_eq!(drain(&mut p, false, accepted, SERVER, usize::MAX), b"x");
}

#[test]
fn a_listener_serves_several_connections() {
    let mut p = Pair::new();
    let listener = p.b.socket_open(SERVER, Kind::Stream).unwrap();
    p.b.socket_bind(listener, SERVER, any(PORT)).unwrap();
    p.b.socket_listen(listener, SERVER, 4).unwrap();
    let mut clients = Vec::new();
    for owner in 20..23u64 {
        let c = p.a.socket_open(owner, Kind::Stream).unwrap();
        p.a.socket_connect(c, owner, b_addr(PORT)).unwrap();
        clients.push((owner, c));
    }
    assert!(p.run_until(3000, |p| {
        clients
            .iter()
            .all(|(o, c)| p.a.socket_connect_status(*c, *o) == Ok(true))
    }));
    let mut ports = Vec::new();
    for _ in 0..3 {
        let (id, peer) =
            p.b.socket_accept(listener, SERVER)
                .unwrap()
                .expect("queued");
        ports.push(peer.port);
        p.b.socket_close(id, SERVER, p.now).unwrap();
    }
    ports.sort();
    ports.dedup();
    assert_eq!(ports.len(), 3, "three different clients");
    assert_eq!(p.b.socket_accept(listener, SERVER), Ok(None));
}

#[test]
fn datagrams_go_both_ways_with_the_sender_named() {
    let mut p = Pair::new();
    let a = p.a.socket_open(CLIENT, Kind::Datagram).unwrap();
    let b = p.b.socket_open(SERVER, Kind::Datagram).unwrap();
    p.b.socket_bind(b, SERVER, any(7000)).unwrap();
    assert_eq!(
        p.a.socket_sendto(a, CLIENT, b_addr(7000), b"ping").unwrap(),
        4
    );
    assert!(p.run_until(1000, |p| {
        p.b.socket_readiness(b, SERVER).unwrap() & ready::READABLE != 0
    }));
    let (data, from) = p.b.socket_recvfrom(b, SERVER, 100).unwrap().unwrap();
    assert_eq!(data, b"ping");
    assert_eq!(from.addr, A_IP);
    p.b.socket_sendto(b, SERVER, from, b"pong").unwrap();
    assert!(p.run_until(1000, |p| {
        p.a.socket_readiness(a, CLIENT).unwrap() & ready::READABLE != 0
    }));
    let (data, from) = p.a.socket_recvfrom(a, CLIENT, 100).unwrap().unwrap();
    assert_eq!((data.as_slice(), from), (&b"pong"[..], b_addr(7000)));
    assert_eq!(p.a.socket_recvfrom(a, CLIENT, 100), Ok(None));
}

#[test]
fn a_long_datagram_is_cut_to_max_and_a_huge_one_is_refused() {
    let mut p = Pair::new();
    let a = p.a.socket_open(CLIENT, Kind::Datagram).unwrap();
    let b = p.b.socket_open(SERVER, Kind::Datagram).unwrap();
    p.b.socket_bind(b, SERVER, any(7001)).unwrap();
    p.a.socket_sendto(a, CLIENT, b_addr(7001), &[1u8; 300])
        .unwrap();
    p.step(50);
    let (data, _) = p.b.socket_recvfrom(b, SERVER, 100).unwrap().unwrap();
    assert_eq!(data.len(), 100);
    let too_big = [0u8; crate::stack::UDP_PAYLOAD + 1];
    assert_eq!(
        p.a.socket_sendto(a, CLIENT, b_addr(7001), &too_big),
        Err(SockError::MessageSize)
    );
    assert_eq!(
        p.a.socket_sendto(a, CLIENT, b_addr(0), b"x"),
        Err(SockError::BadAddress)
    );
    assert_eq!(
        p.b.socket_recvfrom(b, SERVER, 0),
        Err(SockError::BadAddress)
    );
}

#[test]
fn a_connected_datagram_socket_only_hears_its_peer() {
    let mut p = Pair::new();
    let a = p.a.socket_open(CLIENT, Kind::Datagram).unwrap();
    let b = p.b.socket_open(SERVER, Kind::Datagram).unwrap();
    let other = p.b.socket_open(SERVER, Kind::Datagram).unwrap();
    p.a.socket_bind(a, CLIENT, any(7002)).unwrap();
    p.a.socket_connect(a, CLIENT, b_addr(7003)).unwrap();
    p.b.socket_bind(b, SERVER, any(7003)).unwrap();
    p.b.socket_bind(other, SERVER, any(7004)).unwrap();
    let to_a = SockAddr {
        addr: A_IP,
        port: 7002,
    };
    p.b.socket_sendto(other, SERVER, to_a, b"stranger").unwrap();
    p.b.socket_sendto(b, SERVER, to_a, b"peer").unwrap();
    p.step(100);
    let (data, from) = p.a.socket_recvfrom(a, CLIENT, 100).unwrap().unwrap();
    assert_eq!((data.as_slice(), from.port), (&b"peer"[..], 7003));
    assert_eq!(p.a.socket_recvfrom(a, CLIENT, 100), Ok(None));
    assert_eq!(p.a.socket_send(a, CLIENT, b"hi").unwrap(), 2);
    assert_eq!(p.a.socket_peer_addr(a, CLIENT).unwrap(), b_addr(7003));
}
