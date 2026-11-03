//! Signalling edge cases: refused requests, configuration the peer cannot
//! have, violations, and the LE fixed channels.

use alloc::vec::Vec;
use bthci::Boundary;

use super::pair::{inject, sig_pdu, Pair, H};
use crate::manager::{Action, CloseReason, Event, MAX_VIOLATIONS};
use crate::pdu::{
    self, conf_result, conn_result, Command, ConfOption, Signal, CID_ATT, CID_SIGNALLING, CID_SMP,
};

#[test]
fn bad_connection_requests() {
    let mut p = Pair::new(true);
    let result_of = |acts: Vec<Action>| match &acts[0] {
        Action::Send { pdu: x, .. } => match pdu::parse_commands(pdu::parse_basic(x).unwrap().1)
            .unwrap()[0]
            .signal
            .clone()
        {
            Signal::ConnectionResponse { result, .. } => result,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    let low = inject(
        &mut p,
        true,
        &sig_pdu(
            1,
            Signal::ConnectionRequest {
                psm: 0x11,
                scid: 0x20,
            },
        ),
    );
    assert_eq!(result_of(low), conn_result::INVALID_SOURCE_CID);
    let ok = inject(
        &mut p,
        true,
        &sig_pdu(
            2,
            Signal::ConnectionRequest {
                psm: 0x11,
                scid: 0x70,
            },
        ),
    );
    assert_eq!(result_of(ok), conn_result::SUCCESS);
    let dup = inject(
        &mut p,
        true,
        &sig_pdu(
            3,
            Signal::ConnectionRequest {
                psm: 0x13,
                scid: 0x70,
            },
        ),
    );
    assert_eq!(result_of(dup), conn_result::SOURCE_CID_ALLOCATED);
}

#[test]
fn channels_per_link_are_bounded() {
    let mut p = Pair::new(true);
    for i in 0..8u16 {
        let acts = inject(
            &mut p,
            true,
            &sig_pdu(
                1 + i as u8,
                Signal::ConnectionRequest {
                    psm: 0x11,
                    scid: 0x70 + i,
                },
            ),
        );
        assert!(matches!(acts[0], Action::Send { .. }));
    }
    let acts = inject(
        &mut p,
        true,
        &sig_pdu(
            50,
            Signal::ConnectionRequest {
                psm: 0x11,
                scid: 0x90,
            },
        ),
    );
    let Action::Send { pdu: x, .. } = &acts[0] else {
        panic!()
    };
    let c = &pdu::parse_commands(pdu::parse_basic(x).unwrap().1).unwrap()[0];
    assert!(matches!(
        c.signal,
        Signal::ConnectionResponse {
            result: conn_result::NO_RESOURCES,
            ..
        }
    ));
    // Our own side stops at the same bound.
    for _ in 0..8 {
        p.a.l2.connect(0, H, 0x11).unwrap();
    }
    assert!(p.a.l2.connect(0, H, 0x11).is_err());
}

fn conf_rsp_of(acts: &[Action]) -> (u16, Vec<ConfOption>) {
    for a in acts {
        if let Action::Send { pdu: x, .. } = a {
            if let Signal::ConfigResponse {
                result, options, ..
            } = &pdu::parse_commands(pdu::parse_basic(x).unwrap().1).unwrap()[0].signal
            {
                return (*result, options.clone());
            }
        }
    }
    panic!("no config response in {acts:?}");
}

#[test]
fn configuration_the_peer_asks_for_but_basic_mode_cannot_give() {
    let mut p = Pair::new(true);
    let acts = inject(
        &mut p,
        true,
        &sig_pdu(
            1,
            Signal::ConnectionRequest {
                psm: 0x11,
                scid: 0x70,
            },
        ),
    );
    let Action::Send { pdu: x, .. } = &acts[0] else {
        panic!()
    };
    let Signal::ConnectionResponse { dcid, .. } =
        pdu::parse_commands(pdu::parse_basic(x).unwrap().1).unwrap()[0].signal
    else {
        panic!()
    };
    let conf = |options| Signal::ConfigRequest {
        dcid,
        flags: 0,
        options,
    };

    // Enhanced retransmission: unacceptable, basic mode offered.
    let ertm = ConfOption::Rfc {
        mode: 3,
        rest: [0; 8],
    };
    let (r, o) = conf_rsp_of(&inject(&mut p, true, &sig_pdu(2, conf(alloc::vec![ertm]))));
    assert_eq!(
        (r, o),
        (
            conf_result::UNACCEPTABLE,
            alloc::vec![ConfOption::Rfc {
                mode: 0,
                rest: [0; 8]
            }]
        )
    );
    // An MTU below the minimum.
    let (r, o) = conf_rsp_of(&inject(
        &mut p,
        true,
        &sig_pdu(3, conf(alloc::vec![ConfOption::Mtu(10)])),
    ));
    assert_eq!(
        (r, o),
        (conf_result::UNACCEPTABLE, alloc::vec![ConfOption::Mtu(672)])
    );
    // An unknown option without the hint bit is named back; with it, ignored.
    let odd = ConfOption::Unknown {
        kind: 0x09,
        data: alloc::vec![1],
    };
    let (r, o) = conf_rsp_of(&inject(
        &mut p,
        true,
        &sig_pdu(4, conf(alloc::vec![odd.clone()])),
    ));
    assert_eq!((r, o), (conf_result::UNKNOWN_OPTIONS, alloc::vec![odd]));
    let hint = ConfOption::Unknown {
        kind: 0x89,
        data: alloc::vec![1],
    };
    let (r, _) = conf_rsp_of(&inject(&mut p, true, &sig_pdu(5, conf(alloc::vec![hint]))));
    assert_eq!(r, conf_result::SUCCESS);
    assert_eq!(
        p.b.l2.open_channels(H),
        0,
        "our own request is still unanswered"
    );
}

#[test]
fn a_refused_configuration_ends_the_channel() {
    let mut p = Pair::new(true);
    let (cid_a, acts) = p.a.l2.connect(0, H, 0x11).unwrap();
    // Run only the connection exchange by hand, then refuse the config request.
    p.run(true, acts);
    // Both opened; open a second channel whose config response we corrupt.
    let (cid2, acts) = p.a.l2.connect(0, H, 0x13).unwrap();
    let Action::Send { pdu: first, .. } = &acts[0] else {
        panic!()
    };
    let b_acts = inject(&mut p, true, first);
    let Action::Send { pdu: conn_rsp, .. } = &b_acts[0] else {
        panic!()
    };
    let a_acts = inject(&mut p, false, conn_rsp);
    // a now sent its config request; b would answer it. Make b refuse.
    let Action::Send { pdu: conf_req, .. } = &a_acts[0] else {
        panic!()
    };
    let ident = pdu::parse_commands(pdu::parse_basic(conf_req).unwrap().1).unwrap()[0].ident;
    let refusal = Signal::ConfigResponse {
        scid: cid2,
        flags: 0,
        result: conf_result::UNACCEPTABLE,
        options: Vec::new(),
    };
    let acts = inject(&mut p, false, &sig_pdu(ident, refusal));
    assert!(acts.contains(&Action::Event(Event::Closed {
        handle: H,
        cid: cid2,
        reason: CloseReason::ConfigFailed
    })));
    assert_eq!(
        p.a.l2.open_channels(H),
        1,
        "only the first channel survives"
    );
    assert!(p.a.l2.send(H, cid_a, b"x").is_ok());
}

#[test]
fn a_command_reject_ends_the_request_it_answers() {
    let mut p = Pair::new(true);
    let (cid, _) = p.a.l2.connect(0, H, 0x11).unwrap();
    let rej = Signal::CommandReject {
        reason: pdu::reject::NOT_UNDERSTOOD,
        data: Vec::new(),
    };
    let acts = inject(&mut p, false, &sig_pdu(1, rej));
    assert_eq!(
        acts,
        [Action::Event(Event::ConnectFailed {
            handle: H,
            psm: 0x11,
            result: 0xFFFF
        })]
    );
    assert!(p.a.l2.send(H, cid, b"x").is_err());
}

#[test]
fn echo_and_information_requests_are_answered() {
    let mut p = Pair::new(true);
    let acts = inject(
        &mut p,
        true,
        &sig_pdu(4, Signal::EchoRequest(alloc::vec![1, 2, 3])),
    );
    let Action::Send { pdu: x, .. } = &acts[0] else {
        panic!()
    };
    assert_eq!(
        pdu::parse_commands(pdu::parse_basic(x).unwrap().1).unwrap(),
        [Command {
            ident: 4,
            signal: Signal::EchoResponse(alloc::vec![1, 2, 3])
        }]
    );
    for (t, expect_result) in [(2u16, 0u16), (3, 0), (9, 1)] {
        let acts = inject(
            &mut p,
            true,
            &sig_pdu(5, Signal::InformationRequest { info_type: t }),
        );
        let Action::Send { pdu: x, .. } = &acts[0] else {
            panic!()
        };
        let Signal::InformationResponse {
            info_type, result, ..
        } = pdu::parse_commands(pdu::parse_basic(x).unwrap().1).unwrap()[0]
            .signal
            .clone()
        else {
            panic!()
        };
        assert_eq!((info_type, result), (t, expect_result));
    }
}

#[test]
fn violations_accumulate_into_misbehaved_exactly_once() {
    let mut p = Pair::new(true);
    let mut reports = 0;
    for _ in 0..(MAX_VIOLATIONS as usize * 2) {
        // Data on a channel that does not exist.
        let junk = pdu::encode_basic(0x0099, b"hi").unwrap();
        let acts = inject(&mut p, true, &junk);
        reports += acts
            .iter()
            .filter(|a| matches!(a, Action::Event(Event::Misbehaved { .. })))
            .count();
    }
    assert_eq!(reports, 1);
    assert_eq!(p.b.l2.violations(H), MAX_VIOLATIONS * 2);
}

#[test]
fn data_before_the_channel_is_open_or_over_the_mtu_is_dropped() {
    let mut p = Pair::new(true);
    let acts = inject(
        &mut p,
        true,
        &sig_pdu(
            1,
            Signal::ConnectionRequest {
                psm: 0x13,
                scid: 0x70,
            },
        ),
    );
    let Action::Send { pdu: x, .. } = &acts[0] else {
        panic!()
    };
    let Signal::ConnectionResponse { dcid, .. } =
        pdu::parse_commands(pdu::parse_basic(x).unwrap().1).unwrap()[0].signal
    else {
        panic!()
    };
    let early = pdu::encode_basic(dcid, b"early").unwrap();
    assert!(inject(&mut p, true, &early).is_empty());
    assert_eq!(p.b.l2.violations(H), 1);
    let mut q = Pair::new(true);
    let (_, b) = q.open(0x13);
    let huge = pdu::encode_basic(b, &alloc::vec![0; 700]).unwrap();
    assert!(inject(&mut q, true, &huge).is_empty());
    assert_eq!(q.b.l2.violations(H), 1);
}

#[test]
fn malformed_signalling_gets_a_reject_and_a_violation() {
    let mut p = Pair::new(true);
    let bad = pdu::encode_basic(CID_SIGNALLING, &[0x02, 0x01, 0x09, 0x00]).unwrap();
    let acts = inject(&mut p, true, &bad);
    assert!(matches!(&acts[0], Action::Send { .. }));
    assert_eq!(p.b.l2.violations(H), 1);
    // A reassembly error counts as a violation too.
    assert!(p.b.l2.rx_acl(0, H, Boundary::Continuing, b"zz").is_empty());
    assert_eq!(p.b.l2.violations(H), 2);
}

#[test]
fn unknown_handles_and_duplicate_links() {
    let mut p = Pair::new(true);
    assert!(p
        .b
        .l2
        .rx_acl(0, 0x99, Boundary::FirstFlushable, &[0; 4])
        .is_empty());
    assert!(p.b.l2.link_up(H, crate::LinkKind::BrEdr).is_err());
    assert!(p.b.l2.connect(0, 0x99, 0x11).is_err());
    assert!(!p.b.l2.set_allowed(0x99, &[]));
}

#[test]
fn le_fixed_channels_pass_data_and_signalling_is_answered() {
    let mut p = Pair::le();
    let acts = p.a.l2.send(H, CID_ATT, &[0x02, 0x17, 0x00]).unwrap();
    p.run(true, acts);
    assert!(p.b.events.contains(&Event::Data {
        handle: H,
        cid: CID_ATT,
        data: alloc::vec![0x02, 0x17, 0x00]
    }));
    let acts = p.b.l2.send(H, CID_SMP, &[0x01, 0x03]).unwrap();
    p.run(false, acts);
    assert!(p.a.events.contains(&Event::Data {
        handle: H,
        cid: CID_SMP,
        data: alloc::vec![0x01, 0x03]
    }));
    // Dynamic channels do not exist on LE here.
    assert!(p.a.l2.connect(0, H, 0x11).is_err());
    assert!(p.a.l2.send(H, 0x40, b"x").is_err());
    // A connection parameter update request is answered "rejected".
    let req = Signal::ConnParamUpdateRequest {
        interval_min: 6,
        interval_max: 12,
        latency: 0,
        timeout: 200,
    };
    let bytes = pdu::encode_basic(
        pdu::CID_LE_SIGNALLING,
        &Command {
            ident: 7,
            signal: req,
        }
        .encode(),
    )
    .unwrap();
    let acts = p.b.l2.rx_acl(0, H, Boundary::FirstNonFlushable, &bytes);
    let Action::Send { pdu: x, .. } = &acts[0] else {
        panic!()
    };
    assert_eq!(
        pdu::parse_commands(pdu::parse_basic(x).unwrap().1).unwrap(),
        [Command {
            ident: 7,
            signal: Signal::ConnParamUpdateResponse { result: 1 }
        }]
    );
    // BR/EDR requests on an LE signalling channel are not understood.
    let bad = pdu::encode_basic(
        pdu::CID_LE_SIGNALLING,
        &Command {
            ident: 1,
            signal: Signal::ConnectionRequest {
                psm: 0x11,
                scid: 0x40,
            },
        }
        .encode(),
    )
    .unwrap();
    let acts = p.b.l2.rx_acl(0, H, Boundary::FirstNonFlushable, &bad);
    let Action::Send { pdu: x, .. } = &acts[0] else {
        panic!()
    };
    assert!(matches!(
        pdu::parse_commands(pdu::parse_basic(x).unwrap().1).unwrap()[0].signal,
        Signal::CommandReject { .. }
    ));
    // Channel ids outside the fixed set are violations.
    let stray = pdu::encode_basic(0x0003, b"x").unwrap();
    p.b.l2.rx_acl(0, H, Boundary::FirstNonFlushable, &stray);
    assert_eq!(p.b.l2.violations(H), 1);
}
