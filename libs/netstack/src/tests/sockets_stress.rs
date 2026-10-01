//! The socket layer under sustained load: hundreds of connect/close cycles,
//! thousands of datagrams, and random hostile call sequences between two
//! stacks. What these look for is a leak or a broken bound, not a wrong answer.

use std::vec::Vec;

use crate::stack::{ready, Kind, SockAddr, SockError, MAX_CLOSING, MAX_SOCKETS};
use crate::testpair::*;

use super::sockets::{any, b_addr, drain, CLIENT, PORT, SERVER};

#[test]
fn connect_and_close_many_times_leaks_nothing() {
    let mut p = Pair::new();
    let listener = p.b.socket_open(SERVER, Kind::Stream).unwrap();
    p.b.socket_bind(listener, SERVER, any(PORT)).unwrap();
    p.b.socket_listen(listener, SERVER, 2).unwrap();
    for round in 0..300u32 {
        let c = p.a.socket_open(CLIENT, Kind::Stream).unwrap();
        p.a.socket_connect(c, CLIENT, b_addr(PORT)).unwrap();
        assert!(
            p.run_until(2000, |p| p.a.socket_connect_status(c, CLIENT) == Ok(true)),
            "round {round}"
        );
        assert!(p.run_until(2000, |p| {
            p.b.socket_readiness(listener, SERVER).unwrap() & ready::ACCEPTABLE != 0
        }));
        let (s, _) = p.b.socket_accept(listener, SERVER).unwrap().unwrap();
        p.a.socket_send(c, CLIENT, &round.to_le_bytes()).unwrap();
        assert_eq!(drain(&mut p, false, s, SERVER, 4), round.to_le_bytes());
        p.a.socket_close(c, CLIENT, p.now).unwrap();
        p.b.socket_close(s, SERVER, p.now).unwrap();
    }
    assert!(p.run_until(60_000, |p| p.a.socket_closing() == 0
        && p.b.socket_closing() == 0));
    assert_eq!(p.a.socket_open_count(), 0);
    assert_eq!(p.b.socket_open_count(), 1, "only the listener");
    assert_eq!(p.a.socket_counters().connected, 300);
    assert_eq!(p.b.socket_counters().accepted, 300);
    assert!(p.a.socket_counters().tx_bytes >= 1200);
}

#[test]
fn many_datagrams_in_a_row_all_arrive() {
    let mut p = Pair::new();
    let a = p.a.socket_open(CLIENT, Kind::Datagram).unwrap();
    let b = p.b.socket_open(SERVER, Kind::Datagram).unwrap();
    p.b.socket_bind(b, SERVER, any(7005)).unwrap();
    for i in 0..2000u32 {
        loop {
            match p.a.socket_sendto(a, CLIENT, b_addr(7005), &i.to_le_bytes()) {
                Ok(_) => break,
                Err(SockError::WouldBlock) => p.step(10),
                Err(e) => panic!("{e:?}"),
            }
        }
        p.step(10);
        let (data, _) =
            p.b.socket_recvfrom(b, SERVER, 16)
                .unwrap()
                .expect("delivered");
        assert_eq!(data, i.to_le_bytes());
    }
}

/// Hostile operation sequences: ids that exist and ids that do not, every
/// call in every state, two stacks exchanging whatever they produce. Nothing
/// may panic, and the table's bounds must hold throughout.
#[test]
fn random_socket_calls_never_break_the_bounds() {
    use fuzzkit::{for_seeds, Rng};
    for_seeds("netstack::random_socket_calls", |_, rng| {
        let mut p = Pair::new();
        let mut ids: Vec<u32> = Vec::new();
        let pick = |rng: &mut Rng, ids: &Vec<u32>| -> u32 {
            if ids.is_empty() || rng.one_in(8) {
                rng.next_u32()
            } else {
                ids[rng.below(ids.len() as u64) as usize]
            }
        };
        for _ in 0..rng.range(200, 800) {
            let owner = rng.below(3) + 1;
            let id = pick(rng, &ids);
            let addr = SockAddr {
                addr: match rng.below(4) {
                    0 => B_IP,
                    1 => A_IP,
                    2 => [0; 4],
                    _ => [rng.byte(), rng.byte(), rng.byte(), rng.byte()],
                },
                port: match rng.below(4) {
                    0 => PORT,
                    1 => rng.range(0, 65_535) as u16,
                    _ => 5000 + rng.below(4) as u16,
                },
            };
            let len = rng.range(0, 2000) as usize;
            let data = rng.bytes(len);
            let (read, write) = (rng.one_in(2), rng.one_in(2));
            let backlog = rng.below(12) as u32;
            let max = rng.range(0, 20_000) as usize;
            let kind = if rng.one_in(2) {
                Kind::Stream
            } else {
                Kind::Datagram
            };
            let op = rng.below(14);
            let now = p.now;
            let stack = if rng.one_in(2) { &mut p.a } else { &mut p.b };
            match op {
                0 | 1 => {
                    if let Ok(id) = stack.socket_open(owner, kind) {
                        ids.push(id);
                    }
                }
                2 => drop(stack.socket_bind(id, owner, addr)),
                3 => drop(stack.socket_connect(id, owner, addr)),
                4 => drop(stack.socket_listen(id, owner, backlog)),
                5 => {
                    if let Ok(Some((conn, _))) = stack.socket_accept(id, owner) {
                        ids.push(conn);
                    }
                }
                6 => drop(stack.socket_send(id, owner, &data)),
                7 => drop(stack.socket_recv(id, owner, max)),
                8 => drop(stack.socket_sendto(id, owner, addr, &data)),
                9 => drop(stack.socket_recvfrom(id, owner, max)),
                10 => drop(stack.socket_shutdown(id, owner, read, write)),
                11 => drop(stack.socket_close(id, owner, now)),
                12 => drop(stack.sockets_close_owner(owner, now)),
                _ => {
                    let _ = stack.socket_readiness(id, owner);
                    let _ = stack.socket_local_addr(id, owner);
                    let _ = stack.socket_peer_addr(id, owner);
                }
            }
            p.step(rng.below(30) as i64);
            for stack in [&p.a, &p.b] {
                assert!(stack.socket_open_count() <= MAX_SOCKETS);
                assert!(stack.socket_closing() <= MAX_CLOSING);
                assert!(stack.socket_owners().len() <= MAX_SOCKETS);
            }
        }
    });
}
