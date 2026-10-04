//! `socket_send_with` and `socket_recv_with` (P4.2): bytes are queued and
//! dequeued exactly as the closures say, partial takes leave the rest in
//! order, the end of the stream and the owner checks behave as for
//! `socket_send` and `socket_recv`, and a bulk transfer through them alone is
//! byte-exact.

use std::vec::Vec;

use crate::stack::{Kind, Received, SockError};
use crate::testpair::*;

use super::sockets::{connected, CLIENT, SERVER};

fn byte(at: usize) -> u8 {
    (at as u8).wrapping_mul(13) ^ (at >> 8) as u8
}

#[test]
fn send_with_queues_exactly_what_the_closure_wrote() {
    let mut p = Pair::new();
    let (_, c, s) = connected(&mut p);
    let n = p.a.socket_send_with(c, CLIENT, 5, |buf| {
        assert_eq!(buf.len(), 5, "max bounds the offer");
        buf[..3].copy_from_slice(b"abc");
        3
    });
    assert_eq!(n, Ok(3));
    // A closure that claims more than it was offered is clamped.
    assert_eq!(
        p.a.socket_send_with(c, CLIENT, 2, |buf| {
            buf.copy_from_slice(b"de");
            99
        }),
        Ok(2)
    );
    assert_eq!(p.a.socket_send_with(c, CLIENT, 64, |_| 0), Ok(0));
    let mut got = Vec::new();
    assert!(p.run_until(2000, |p| {
        while let Ok(Received::Data(k)) = p.b.socket_recv_with(s, SERVER, 64, |data| {
            got.extend_from_slice(data);
            data.len()
        }) {
            if k == 0 {
                break;
            }
        }
        got.len() >= 5
    }));
    assert_eq!(got, b"abcde");
}

#[test]
fn recv_with_leaves_what_the_closure_did_not_take() {
    let mut p = Pair::new();
    let (_, c, s) = connected(&mut p);
    p.a.socket_send(c, CLIENT, b"0123456789").unwrap();
    assert!(
        p.run_until(2000, |p| p.b.socket_recv_with(s, SERVER, 64, |_| 0)
            != Ok(Received::Empty))
    );
    assert_eq!(
        p.b.socket_recv_with(s, SERVER, 64, |_| 0),
        Ok(Received::Data(0))
    );
    assert_eq!(
        p.b.socket_recv_with(s, SERVER, 4, |data| {
            assert_eq!(data, b"0123");
            2
        }),
        Ok(Received::Data(2))
    );
    assert_eq!(
        p.b.socket_recv(s, SERVER, 64),
        Ok(Some(b"23456789".to_vec()))
    );
    assert_eq!(
        p.b.socket_recv_with(s, SERVER, 64, |d| d.len()),
        Ok(Received::Empty)
    );
    p.a.socket_shutdown(c, CLIENT, false, true).unwrap();
    assert!(
        p.run_until(2000, |p| p.b.socket_recv_with(s, SERVER, 64, |d| d.len())
            == Ok(Received::End))
    );
}

#[test]
fn the_owner_and_kind_checks_hold() {
    let mut p = Pair::new();
    let (listener, c, _) = connected(&mut p);
    assert_eq!(
        p.a.socket_send_with(c, SERVER, 8, |_| 0),
        Err(SockError::NotOwner)
    );
    assert_eq!(
        p.a.socket_recv_with(c, SERVER, 8, |_| 0),
        Err(SockError::NotOwner)
    );
    assert_eq!(
        p.b.socket_send_with(listener, SERVER, 8, |_| 0),
        Err(SockError::InvalidState)
    );
    let udp = p.a.socket_open(CLIENT, Kind::Datagram).unwrap();
    assert_eq!(
        p.a.socket_recv_with(udp, CLIENT, 8, |_| 0),
        Err(SockError::InvalidState)
    );
    let fresh = p.a.socket_open(CLIENT, Kind::Stream).unwrap();
    assert_eq!(
        p.a.socket_send_with(fresh, CLIENT, 8, |_| 0),
        Err(SockError::NotConnected)
    );
    p.a.socket_shutdown(c, CLIENT, false, true).unwrap();
    assert_eq!(
        p.a.socket_send_with(c, CLIENT, 8, |_| 0),
        Err(SockError::Pipe)
    );
}

/// 4 MiB each way through the in-place calls only, with uneven offers and
/// partial takes: byte-exact and in order.
#[test]
fn bulk_through_the_in_place_calls_is_byte_exact() {
    const TOTAL: usize = 4 << 20;
    let mut p = Pair::new();
    let (_, c, s) = connected(&mut p);
    let (mut sent, mut got, mut round) = (0usize, 0usize, 0usize);
    let done = p.run_until(600_000, |p| {
        round += 1;
        loop {
            let want = (TOTAL - sent).min(1000 + round % 7000);
            let n = p.a.socket_send_with(c, CLIENT, want, |buf| {
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = byte(sent + i);
                }
                buf.len()
            });
            match n {
                Ok(0) => break,
                Ok(n) => sent += n,
                Err(e) => panic!("send: {e:?}"),
            }
            if sent == TOTAL {
                break;
            }
        }
        loop {
            let r = p.b.socket_recv_with(s, SERVER, 1 + round % 5000, |data| {
                // Take a little less than offered now and then.
                let take = if round % 3 == 0 {
                    data.len() / 2
                } else {
                    data.len()
                };
                for (i, &b) in data[..take].iter().enumerate() {
                    assert_eq!(b, byte(got + i), "byte {}", got + i);
                }
                got += take;
                take
            });
            match r {
                Ok(Received::Data(0)) | Ok(Received::Empty) => break,
                Ok(Received::Data(_)) => {}
                other => panic!("recv: {other:?}"),
            }
        }
        got == TOTAL
    });
    assert!(done, "sent {sent}, got {got}");
}
