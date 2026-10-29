//! Loopback as interfaces and addresses change under it (issue #1019): a
//! card that leaves or loses its lease takes what `lo` held for its address
//! with it, a connected datagram socket moves between `lo` and a card, a
//! listener placed on `lo` late still gets its port, and eight cards fit.

use crate::net::IfKind::Wired;
use crate::stack::{Kind, SockError, LOOPBACK_ADDR};
use crate::testmulti::{MultiLan, MultiPair};

use super::loopback::{accept, at, connect, listener, CLIENT, SERVER};

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
fn a_lease_that_ends_resets_the_loopback_connections_that_used_it() {
    let mut lan = MultiLan::new(&[Wired]);
    lan.configure();
    let me = lan
        .net
        .units()
        .next()
        .unwrap()
        .1
        .stack
        .state()
        .addr
        .unwrap();
    let server = lan.net.socket_open(SERVER, Kind::Stream).unwrap();
    lan.net
        .socket_bind(server, SERVER, at([0; 4], 8400))
        .unwrap();
    lan.net.socket_listen(server, SERVER, 2).unwrap();
    let client = lan.net.socket_open(CLIENT, Kind::Stream).unwrap();
    lan.net
        .socket_connect(client, CLIENT, at(me, 8400))
        .unwrap();
    assert!(
        lan.run_until(200, |l| l.net.socket_connect_status(client, CLIENT)
            == Ok(true))
    );
    let mut conn = None;
    assert!(lan.run_until(200, |l| {
        conn = l.net.socket_accept(server, SERVER).unwrap();
        conn.is_some()
    }));
    let (conn, _) = conn.unwrap();
    // The lease goes (a renewal drops it at once): both ends are reset at
    // the next poll, not left to time out.
    lan.net.renew();
    lan.step(10);
    assert_eq!(
        lan.net.socket_recv(client, CLIENT, 16),
        Err(SockError::Reset)
    );
    assert_eq!(lan.net.socket_recv(conn, SERVER, 16), Err(SockError::Reset));
    // 127.0.0.1 still reaches the listener.
    let again = lan.net.socket_open(CLIENT, Kind::Stream).unwrap();
    lan.net
        .socket_connect(again, CLIENT, at(LOOPBACK_ADDR, 8400))
        .unwrap();
    assert!(
        lan.run_until(200, |l| l.net.socket_connect_status(again, CLIENT)
            == Ok(true))
    );
}

#[test]
fn a_listener_placed_on_lo_late_still_gets_its_port() {
    let mut pair = MultiPair::new(&[Wired]);
    // Many unbound loopback connections take ports first; none may take the
    // port of a listener that has no `lo` replica yet.
    let server = listener(&mut pair, at([0; 4], 0));
    let port = pair.net.socket_local_addr(server, SERVER).unwrap().port;
    let target = listener(&mut pair, at([0; 4], 8500));
    let mut held = std::vec::Vec::new();
    for _ in 0..6 {
        let (id, status) = connect(&mut pair, at(LOOPBACK_ADDR, 8500));
        assert_eq!(status, Ok(true));
        let local = pair.net.socket_local_addr(id, CLIENT).unwrap();
        assert_ne!(local.port, port, "a client took the listener's port");
        held.push(id);
        accept(&mut pair, target);
    }
    let (_, status) = connect(&mut pair, at(LOOPBACK_ADDR, port));
    assert_eq!(status, Ok(true));
}

#[test]
fn a_datagram_socket_connected_to_the_cards_address_reports_it() {
    let mut pair = MultiPair::new(&[Wired]);
    let me = MultiPair::net_ip(0);
    let udp = pair.net.socket_open(CLIENT, Kind::Datagram).unwrap();
    pair.net.socket_connect(udp, CLIENT, at(me, 9300)).unwrap();
    assert_eq!(pair.net.socket_local_addr(udp, CLIENT).unwrap().addr, me);
    pair.net
        .socket_connect(udp, CLIENT, at(LOOPBACK_ADDR, 9300))
        .unwrap();
    assert_eq!(
        pair.net.socket_local_addr(udp, CLIENT).unwrap().addr,
        LOOPBACK_ADDR
    );
}
