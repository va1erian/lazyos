use super::pair::{inject, sig_pdu, Pair, H};
use crate::manager::{Action, CloseReason, Event};
use crate::pdu::{self, conn_result, Command, Signal, CID_SIGNALLING};
use crate::Error;

#[test]
fn both_hid_channels_open_through_fragmented_acl() {
    let mut p = Pair::new(true);
    let (ctl_a, ctl_b) = p.open(0x11);
    let (int_a, int_b) = p.open(0x13);
    assert_eq!(p.a.l2.open_channels(H), 2);
    assert_eq!(p.b.l2.open_channels(H), 2);
    assert!(p.a.events.contains(&Event::Opened {
        handle: H,
        cid: ctl_a,
        psm: 0x11,
        incoming: false,
        peer_mtu: 672
    }));
    assert!(p.b.events.contains(&Event::Opened {
        handle: H,
        cid: int_b,
        psm: 0x13,
        incoming: true,
        peer_mtu: 672
    }));
    assert_ne!(ctl_a, int_a);
    assert_ne!(ctl_b, int_b);
}

#[test]
fn data_flows_both_ways_and_respects_the_mtu() {
    let mut p = Pair::new(true);
    let (a, b) = p.open(0x13);
    let acts = p.a.l2.send(H, a, &[0xA1, 1, 2, 3]).unwrap();
    p.run(true, acts);
    assert!(p.b.events.contains(&Event::Data {
        handle: H,
        cid: b,
        data: alloc::vec![0xA1, 1, 2, 3]
    }));
    let big = alloc::vec![7u8; 672];
    let acts = p.b.l2.send(H, b, &big).unwrap();
    p.run(false, acts);
    assert!(p.a.events.contains(&Event::Data {
        handle: H,
        cid: a,
        data: big
    }));
    assert_eq!(p.a.l2.send(H, a, &alloc::vec![0; 673]), Err(Error::TooLong));
    // The wire carried the peer's channel id, not ours.
    let sent = p.wire.iter().rev().find(|(from_a, _)| *from_a).unwrap();
    assert_eq!(pdu::parse_basic(&sent.1).unwrap().0, b);
}

#[test]
fn the_peer_mtu_option_is_honoured() {
    let mut p = Pair::new(true);
    let (a, b) = p.open(0x11);
    // Both announced 672, so a 700 byte send is refused on both sides.
    assert_eq!(p.a.l2.channel_info(H, a), Some((0x11, 672)));
    assert_eq!(p.b.l2.send(H, b, &alloc::vec![0; 700]), Err(Error::TooLong));
}

#[test]
fn a_psm_not_served_is_refused() {
    let mut p = Pair::new(true);
    let (_, acts) = p.a.l2.connect(0, H, 0x0F).unwrap();
    p.run(true, acts);
    assert_eq!(
        p.a.events,
        [Event::ConnectFailed {
            handle: H,
            psm: 0x0F,
            result: conn_result::PSM_NOT_SUPPORTED
        }]
    );
    assert_eq!(p.a.l2.open_channels(H), 0);
}

#[test]
fn an_unallowed_psm_is_a_security_block_and_is_reported() {
    let mut p = Pair::new(false);
    let (_, acts) = p.a.l2.connect(0, H, 0x13).unwrap();
    p.run(true, acts);
    assert_eq!(
        p.a.events,
        [Event::ConnectFailed {
            handle: H,
            psm: 0x13,
            result: conn_result::SECURITY_BLOCK
        }]
    );
    assert_eq!(
        p.b.events,
        [Event::Refused {
            handle: H,
            psm: 0x13
        }]
    );
    // Allowing the PSM afterwards (bond confirmed, link encrypted) lets it in.
    p.b.l2.set_allowed(H, &[0x13]);
    p.a.events.clear();
    p.open(0x13);
    assert_eq!(p.b.l2.open_channels(H), 1);
}

#[test]
fn disconnect_from_either_side() {
    let mut p = Pair::new(true);
    let (a, b) = p.open(0x11);
    let acts = p.a.l2.disconnect(0, H, a).unwrap();
    p.run(true, acts);
    assert!(p.a.events.contains(&Event::Closed {
        handle: H,
        cid: a,
        reason: CloseReason::Local
    }));
    assert!(p.b.events.contains(&Event::Closed {
        handle: H,
        cid: b,
        reason: CloseReason::Peer
    }));
    assert_eq!((p.a.l2.open_channels(H), p.b.l2.open_channels(H)), (0, 0));
    assert_eq!(p.a.l2.send(H, a, b"x"), Err(Error::Invalid));
    // And the channel id can be reused.
    let (a2, _) = p.open(0x11);
    assert_eq!(a2, a);
}

#[test]
fn link_down_closes_everything() {
    let mut p = Pair::new(true);
    p.open(0x11);
    p.open(0x13);
    let acts = p.b.l2.link_down(H);
    assert_eq!(acts.len(), 2);
    assert!(acts.iter().all(|a| matches!(
        a,
        Action::Event(Event::Closed {
            reason: CloseReason::LinkDown,
            ..
        })
    )));
    assert!(p.b.l2.link_down(H).is_empty());
}

#[test]
fn unanswered_requests_time_out() {
    let mut p = Pair::new(true);
    let (cid, _lost) = p.a.l2.connect(1_000, H, 0x11).unwrap();
    assert_eq!(p.a.l2.next_deadline(), Some(6_000));
    assert!(p.a.l2.tick(5_999).is_empty());
    let acts = p.a.l2.tick(6_000);
    assert_eq!(
        acts,
        [Action::Event(Event::ConnectFailed {
            handle: H,
            psm: 0x11,
            result: 0xFFFF
        })]
    );
    assert_eq!(p.a.l2.next_deadline(), None);
    // A response arriving now is an unsolicited one.
    let rsp = Signal::ConnectionResponse {
        dcid: 0x50,
        scid: cid,
        result: 0,
        status: 0,
    };
    let acts = inject(&mut p, false, &sig_pdu(1, rsp));
    assert!(acts.is_empty());
    assert_eq!(p.a.l2.violations(H), 1);
}

#[test]
fn a_half_configured_channel_times_out_and_asks_the_peer_to_close() {
    let mut p = Pair::new(true);
    // The peer connects and then goes silent about configuration.
    let req = Signal::ConnectionRequest {
        psm: 0x11,
        scid: 0x70,
    };
    let acts = inject(&mut p, true, &sig_pdu(3, req));
    assert!(matches!(acts[0], Action::Send { .. }));
    assert_eq!(p.b.l2.open_channels(H), 0);
    let acts = p.b.l2.tick(10_000);
    assert!(matches!(
        acts[0],
        Action::Event(Event::Closed {
            reason: CloseReason::Timeout,
            ..
        })
    ));
    assert!(matches!(acts[1], Action::Send { .. }));
}

#[test]
fn soak_many_open_close_cycles_leave_nothing_behind() {
    let mut p = Pair::new(true);
    for i in 0..2_000u32 {
        p.now += 10;
        let (a, _) = p.open(if i % 2 == 0 { 0x11 } else { 0x13 });
        let acts = p.a.l2.send(H, a, &[i as u8; 40]).unwrap();
        p.run(true, acts);
        let acts = p.a.l2.disconnect(p.now, H, a).unwrap();
        p.run(true, acts);
        p.a.events.clear();
        p.b.events.clear();
    }
    assert_eq!((p.a.l2.open_channels(H), p.b.l2.open_channels(H)), (0, 0));
    assert_eq!(
        (p.a.l2.next_deadline(), p.b.l2.next_deadline()),
        (None, None)
    );
    assert_eq!((p.a.l2.violations(H), p.b.l2.violations(H)), (0, 0));
}

#[test]
fn seeded_fuzz_runs() {
    fuzzkit::for_seeds("btl2cap_fuzz_run", |_seed, rng| {
        let n = rng.below(400) as usize;
        let mut data = rng.bytes(n);
        // Shape some inputs like records of well-formed signalling.
        if rng.one_in(2) && data.len() > 8 {
            data[0] &= 3;
            data[1] = rng.below(3) as u8;
            let cmd = Command {
                ident: 1 + rng.below(200) as u8,
                signal: Signal::ConnectionRequest {
                    psm: 0x11,
                    scid: 0x40 + rng.below(8) as u16,
                },
            };
            let pdu = pdu::encode_basic(CID_SIGNALLING, &cmd.encode()).unwrap();
            data[2] = pdu.len() as u8;
            data.splice(3..3, pdu);
        }
        crate::fuzz::run(&data);
    });
}
