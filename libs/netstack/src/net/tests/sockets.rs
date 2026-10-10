//! Sockets through `Net` over real peers: the interface a connection takes,
//! listeners and datagram sockets that serve every interface, interfaces that
//! appear and vanish under open sockets, and the accounting.

use std::vec::Vec;

use crate::net::IfKind::{Wired, Wireless};
use crate::stack::{Kind, SockAddr, SockError, Stack};
use crate::testmulti::MultiPair;

const ME: u64 = 1;
const PEER: u64 = 2;

fn at(addr: [u8; 4], port: u16) -> SockAddr {
    SockAddr { addr, port }
}

fn any(port: u16) -> SockAddr {
    at([0; 4], port)
}

/// A peer-side listener on `port`.
fn peer_listener(peer: &mut Stack, port: u16) -> u32 {
    let id = peer.socket_open(PEER, Kind::Stream).unwrap();
    peer.socket_bind(id, PEER, any(port)).unwrap();
    peer.socket_listen(id, PEER, 2).unwrap();
    id
}

/// A peer-side connection to the `Net` at `to`.
fn peer_connect(peer: &mut Stack, to: SockAddr) -> u32 {
    let id = peer.socket_open(PEER, Kind::Stream).unwrap();
    peer.socket_connect(id, PEER, to).unwrap();
    id
}

#[test]
fn a_connection_takes_the_interface_that_routes_its_peer() {
    let mut pair = MultiPair::new(&[Wired, Wired]);
    for n in 0..2 {
        let listener = peer_listener(&mut pair.peers[n], 8080);
        let id = pair.net.socket_open(ME, Kind::Stream).unwrap();
        pair.net
            .socket_connect(id, ME, at(MultiPair::peer_ip(n), 8080))
            .unwrap();
        assert!(pair.run_until(3000, |p| p.net.socket_connect_status(id, ME) == Ok(true)));
        assert_eq!(
            pair.net.socket_local_addr(id, ME).unwrap().addr,
            MultiPair::net_ip(n),
            "left by interface {n}"
        );
        assert_eq!(pair.net.socket_peer_addr(id, ME).unwrap().addr, MultiPair::peer_ip(n));
        assert!(pair.peers[n].socket_accept(listener, PEER).unwrap().is_some());
    }
    assert_eq!(pair.net.socket_counters().connected, 2);
}

#[test]
fn a_listener_serves_every_interface_and_a_late_one() {
    let mut pair = MultiPair::new(&[Wired]);
    let listener = pair.net.socket_open(ME, Kind::Stream).unwrap();
    pair.net.socket_bind(listener, ME, any(5000)).unwrap();
    pair.net.socket_listen(listener, ME, 4).unwrap();
    // A second card appears after the listener exists.
    pair.add(Wireless);
    let mut seen = Vec::new();
    for n in 0..2 {
        peer_connect(&mut pair.peers[n], at(MultiPair::net_ip(n), 5000));
    }
    assert!(pair.run_until(3000, |p| {
        while let Some((_, from)) = p.net.socket_accept(listener, ME).unwrap() {
            seen.push(from.addr);
        }
        seen.len() == 2
    }));
    seen.sort();
    assert_eq!(seen, [MultiPair::peer_ip(0), MultiPair::peer_ip(1)]);
    assert_eq!(pair.net.socket_counters().accepted, 2);
    // The listener is one socket for the quota and the counters.
    assert_eq!(pair.net.socket_counters().opened, 3);
}

#[test]
fn a_listener_bound_to_an_address_serves_that_interface_only() {
    let mut pair = MultiPair::new(&[Wired, Wired]);
    let listener = pair.net.socket_open(ME, Kind::Stream).unwrap();
    pair.net
        .socket_bind(listener, ME, at(MultiPair::net_ip(1), 5001))
        .unwrap();
    pair.net.socket_listen(listener, ME, 2).unwrap();
    let refused = peer_connect(&mut pair.peers[0], at(MultiPair::net_ip(0), 5001));
    let ok = peer_connect(&mut pair.peers[1], at(MultiPair::net_ip(1), 5001));
    let mut accepted = false;
    pair.run_until(3000, |p| {
        accepted |= p.net.socket_accept(listener, ME).unwrap().is_some();
        accepted && p.peers[0].socket_connect_status(refused, PEER) == Err(SockError::Refused)
    });
    assert!(accepted);
    assert_eq!(pair.peers[1].socket_connect_status(ok, PEER), Ok(true));
    assert_eq!(
        pair.net.socket_bind(listener, ME, any(5002)),
        Err(SockError::InvalidState)
    );
}

#[test]
fn a_datagram_socket_receives_on_all_and_sends_by_route() {
    let mut pair = MultiPair::new(&[Wired, Wired]);
    let id = pair.net.socket_open(ME, Kind::Datagram).unwrap();
    pair.net.socket_bind(id, ME, any(6000)).unwrap();
    let mut peer_ids = Vec::new();
    for n in 0..2 {
        let p = pair.peers[n].socket_open(PEER, Kind::Datagram).unwrap();
        pair.peers[n].socket_bind(p, PEER, any(7000)).unwrap();
        pair.peers[n]
            .socket_sendto(p, PEER, at(MultiPair::net_ip(n), 6000), &[n as u8; 4])
            .unwrap();
        peer_ids.push(p);
    }
    let mut got = Vec::new();
    assert!(pair.run_until(3000, |p| {
        while let Some((data, from)) = p.net.socket_recvfrom(id, ME, 100).unwrap() {
            got.push((from.addr, data));
        }
        got.len() == 2
    }));
    got.sort();
    assert_eq!(got[0], (MultiPair::peer_ip(0), std::vec![0; 4]));
    assert_eq!(got[1], (MultiPair::peer_ip(1), std::vec![1; 4]));
    // Each reply leaves by the interface that routes its destination.
    for n in 0..2 {
        pair.net
            .socket_sendto(id, ME, at(MultiPair::peer_ip(n), 7000), b"back")
            .unwrap();
    }
    for n in 0..2 {
        let mut reply = None;
        assert!(pair.run_until(3000, |p| {
            reply = p.peers[n].socket_recvfrom(peer_ids[n], PEER, 100).unwrap();
            reply.is_some()
        }));
        let (data, from) = reply.unwrap();
        assert_eq!(data, b"back");
        assert_eq!(from.addr, MultiPair::net_ip(n), "source address of interface {n}");
    }
}

#[test]
fn removing_an_interface_resets_its_connections_and_keeps_wildcard_sockets() {
    let mut pair = MultiPair::new(&[Wired, Wired]);
    let listener = pair.net.socket_open(ME, Kind::Stream).unwrap();
    pair.net.socket_bind(listener, ME, any(5000)).unwrap();
    pair.net.socket_listen(listener, ME, 2).unwrap();
    peer_listener(&mut pair.peers[0], 8080);
    let conn = pair.net.socket_open(ME, Kind::Stream).unwrap();
    pair.net
        .socket_connect(conn, ME, at(MultiPair::peer_ip(0), 8080))
        .unwrap();
    assert!(pair.run_until(3000, |p| p.net.socket_connect_status(conn, ME) == Ok(true)));

    pair.net.remove_interface(0);
    assert_eq!(pair.net.socket_send(conn, ME, b"x"), Err(SockError::Reset));
    assert_eq!(pair.net.socket_recv(conn, ME, 10), Err(SockError::Reset));
    // The listener still serves the interface that is left.
    peer_connect(&mut pair.peers[1], at(MultiPair::net_ip(1), 5000));
    let mut accepted = false;
    assert!(pair.run_until(3000, |p| {
        accepted |= p.net.socket_accept(listener, ME).unwrap().is_some();
        accepted
    }));
    pair.net.socket_close(conn, ME, pair.now).unwrap();
    assert_eq!(pair.net.socket_close(conn, ME, pair.now), Err(SockError::BadSocket));
}

#[test]
fn quotas_owners_and_ids_are_the_nets() {
    let mut pair = MultiPair::new(&[Wired, Wired]);
    let mut ids = Vec::new();
    for _ in 0..crate::stack::MAX_PER_OWNER {
        ids.push(pair.net.socket_open(ME, Kind::Datagram).unwrap());
    }
    assert_eq!(
        pair.net.socket_open(ME, Kind::Stream),
        Err(SockError::TooManyForOwner)
    );
    // Every one of them can spread over both interfaces at once.
    for (i, id) in ids.iter().enumerate() {
        pair.net.socket_bind(*id, ME, any(6100 + i as u16)).unwrap();
    }
    assert_eq!(
        pair.net.socket_bind(ids[0], PEER, any(6200)),
        Err(SockError::NotOwner)
    );
    assert_eq!(
        pair.net.socket_bind(ids[1], ME, any(6300)),
        Err(SockError::InvalidState),
        "a bound socket cannot bind again"
    );
    let fresh = pair.net.socket_open(PEER, Kind::Datagram).unwrap();
    assert_eq!(
        pair.net.socket_bind(fresh, PEER, any(6100)),
        Err(SockError::AddrInUse)
    );
    assert_eq!(
        pair.net.socket_bind(fresh, PEER, any(80)),
        Err(SockError::Privileged)
    );
    assert_eq!(
        pair.net.socket_bind(fresh, PEER, at([10, 9, 9, 9], 6300)),
        Err(SockError::BadAddress),
        "not an address of ours"
    );
    assert_eq!(pair.net.socket_owners(), std::vec![ME, PEER]);
    assert_eq!(pair.net.sockets_close_owner(ME, pair.now), 8);
    assert_eq!(pair.net.socket_open_count(), 1);
    assert_eq!(pair.net.socket_counters().reclaimed, 8);
    let stale = ids[0];
    assert_eq!(
        pair.net.socket_close(stale, ME, pair.now),
        Err(SockError::BadSocket)
    );
}

#[test]
fn destinations_nothing_routes_are_refused() {
    let mut pair = MultiPair::new(&[Wired]);
    pair.net.set_link(0, false);
    let id = pair.net.socket_open(ME, Kind::Stream).unwrap();
    assert_eq!(
        pair.net.socket_connect(id, ME, at([10, 0, 0, 2], 8080)),
        Err(SockError::Unreachable)
    );
    assert_eq!(
        pair.net.socket_connect(id, ME, at([127, 0, 0, 1], 8080)),
        Err(SockError::BadAddress)
    );
    assert_eq!(pair.net.socket_recv(id, ME, 10), Err(SockError::NotConnected));
    let d = pair.net.socket_open(ME, Kind::Datagram).unwrap();
    assert_eq!(
        pair.net.socket_sendto(d, ME, at([10, 0, 0, 2], 9), b"x"),
        Err(SockError::Unreachable)
    );
}
