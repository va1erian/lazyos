//! Ping: the echo path, the ping table, timeouts and forged replies.

use crate::config::{Mode, StaticConfig};
use crate::stack::{PingError, PingOutcome, MAX_PINGS};
use crate::testnet::*;

use super::dhcp_lan;

#[test]
fn ping_the_gateway_end_to_end() {
    let mut lan = dhcp_lan();
    let seq = lan.stack.ping(GW_IP, 56, 2000, lan.now).expect("ping");
    let sent_at = lan.now;
    assert!(lan.run_until(2000, |lan| lan.stack.pings_outstanding() == 0));
    let results = lan.stack.take_ping_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].seq, seq);
    let PingOutcome::Reply {
        rtt_ms,
        source,
        bytes,
    } = results[0].outcome
    else {
        panic!("{:?}", results[0])
    };
    assert_eq!((source, bytes), (GW_IP, 56));
    assert!(rtt_ms <= 100 && i64::from(rtt_ms) <= lan.now - sent_at);
    assert_eq!(lan.stack.counters().pings_sent, 1);
    assert_eq!(lan.stack.counters().pings_answered, 1);
    // ARP resolution came first, then the echo request with the right payload.
    assert!(
        lan.gateway.requests_seen.contains(&"arp") && lan.gateway.requests_seen.contains(&"echo")
    );
    let request = lan
        .sent
        .iter()
        .find(|f| f.len() > 34 && f[23] == 1 && f[34] == 8)
        .expect("an echo request");
    assert_eq!(request.len(), 14 + 20 + 8 + 56);
    assert_eq!(&request[30..34], &GW_IP);
}

#[test]
fn many_pings_in_a_row_all_come_back() {
    let mut lan = dhcp_lan();
    for round in 0..50u32 {
        let payload = (round as usize * 29) % 1400;
        lan.stack.ping(GW_IP, payload, 1000, lan.now).expect("ping");
        assert!(
            lan.run_until(1000, |lan| lan.stack.pings_outstanding() == 0),
            "round {round}"
        );
        let results = lan.stack.take_ping_results();
        assert!(
            matches!(results[..], [crate::PingResult { outcome: PingOutcome::Reply { bytes, .. }, .. }] if bytes as usize == payload),
            "round {round}"
        );
    }
    assert_eq!(lan.stack.counters().pings_answered, 50);
}

#[test]
fn the_stack_answers_echo_requests_to_its_own_address() {
    let mut lan = dhcp_lan();
    // smoltcp answers a request only once it knows the sender's MAC (it does not
    // queue a reply behind ARP), so let one exchange teach it the gateway's.
    lan.stack.ping(GW_IP, 8, 500, lan.now).unwrap();
    lan.run_until(500, |lan| lan.stack.pings_outstanding() == 0);
    lan.sent.clear();
    let request = echo_frame(
        STACK_MAC, GW_MAC, GW_IP, LEASE_IP, false, 0x55AA, 9, b"hello",
    );
    assert!(lan.deliver(&request));
    lan.step(20);
    let reply = lan
        .sent
        .iter()
        .find(|f| f.len() > 34 && f[23] == 1 && f[34] == 0)
        .expect("an echo reply");
    assert_eq!(&reply[..6], &GW_MAC);
    assert_eq!(&reply[26..30], &LEASE_IP);
    assert_eq!(&reply[30..34], &GW_IP);
    assert_eq!(
        &reply[38..42],
        &[0x55, 0xAA, 0, 9],
        "ident and sequence echoed"
    );
    assert_eq!(&reply[42..], b"hello");
}

#[test]
fn a_ping_to_nothing_times_out() {
    let mut lan = dhcp_lan();
    lan.gateway.answer_echo = false;
    let seq = lan.stack.ping(GW_IP, 8, 300, lan.now).unwrap();
    assert!(lan.run_until(1000, |lan| lan.stack.pings_outstanding() == 0));
    assert_eq!(
        lan.stack.take_ping_results(),
        std::vec![crate::PingResult {
            seq,
            outcome: PingOutcome::TimedOut
        }]
    );
    assert_eq!(lan.stack.counters().pings_timed_out, 1);
}

#[test]
fn forged_echo_replies_do_not_count() {
    type Bend = fn(&mut Gateway);
    let cases: [(&str, Bend); 3] = [
        ("wrong identifier", |g| g.echo_ident_override = Some(0x1111)),
        ("another source address", |g| {
            g.echo_source_override = Some([10, 0, 2, 99])
        }),
        ("a mangled payload", |g| g.mangle_echo = true),
    ];
    for (name, bend) in cases {
        let mut lan = dhcp_lan();
        bend(&mut lan.gateway);
        lan.stack.ping(GW_IP, 32, 400, lan.now).unwrap();
        assert!(lan.run_until(1000, |lan| lan.stack.pings_outstanding() == 0));
        let results = lan.stack.take_ping_results();
        assert!(
            matches!(
                results[..],
                [crate::PingResult {
                    outcome: PingOutcome::TimedOut,
                    ..
                }]
            ),
            "{name}: {results:?}"
        );
        assert_eq!(lan.stack.counters().pings_answered, 0, "{name}");
    }
}

#[test]
fn an_unsolicited_echo_reply_is_ignored() {
    let mut lan = dhcp_lan();
    let stray = echo_frame(STACK_MAC, GW_MAC, GW_IP, LEASE_IP, true, 7, 7, b"x");
    lan.deliver(&stray);
    lan.step(30);
    assert!(lan.stack.take_ping_results().is_empty());
    assert_eq!(lan.stack.counters().pings_answered, 0);
}

#[test]
fn ping_refuses_bad_arguments() {
    let mut lan = Lan::new(&Mode::Dhcp);
    assert_eq!(
        lan.stack.ping(GW_IP, 8, 100, lan.now),
        Err(PingError::NoAddress),
        "before a lease"
    );
    lan.configure();
    for dst in [
        [0, 0, 0, 0],
        [127, 0, 0, 1],
        [224, 0, 0, 1],
        [255, 255, 255, 255],
    ] {
        assert_eq!(
            lan.stack.ping(dst, 8, 100, lan.now),
            Err(PingError::BadArgument),
            "{dst:?}"
        );
    }
    assert_eq!(
        lan.stack.ping(GW_IP, 1401, 100, lan.now),
        Err(PingError::BadArgument)
    );
    assert_eq!(
        lan.stack.ping(GW_IP, usize::MAX, 100, lan.now),
        Err(PingError::BadArgument)
    );
    assert!(
        lan.stack.ping(GW_IP, 1400, 100, lan.now).is_ok(),
        "the largest payload"
    );
}

#[test]
fn ping_off_link_needs_a_route() {
    let config = StaticConfig {
        addr: [192, 168, 5, 9],
        prefix_len: 24,
        gateway: None,
        dns: None,
    };
    let mut lan = Lan::new(&Mode::Static(config));
    assert_eq!(
        lan.stack.ping([8, 8, 8, 8], 8, 100, lan.now),
        Err(PingError::NoRoute)
    );
    assert!(
        lan.stack.ping([192, 168, 5, 77], 8, 100, lan.now).is_ok(),
        "on-link needs no gateway"
    );
}

#[test]
fn outstanding_pings_are_bounded() {
    let mut lan = dhcp_lan();
    lan.gateway.answer_echo = false;
    for _ in 0..MAX_PINGS {
        lan.stack.ping(GW_IP, 8, 5000, lan.now).unwrap();
    }
    assert_eq!(
        lan.stack.ping(GW_IP, 8, 5000, lan.now),
        Err(PingError::Busy)
    );
    assert_eq!(lan.stack.pings_outstanding(), MAX_PINGS);
    // Cancelling frees a slot; timing out frees them all.
    lan.stack.cancel_ping(0);
    lan.run_until(6000, |lan| lan.stack.pings_outstanding() == 0);
    assert_eq!(lan.stack.pings_outstanding(), 0);
    assert!(lan.stack.ping(GW_IP, 8, 100, lan.now).is_ok());
}

#[test]
fn a_dead_ping_does_not_block_the_ones_behind_it() {
    let mut lan = dhcp_lan();
    // Nobody answers ARP for .77, so its request can never leave the socket's
    // transmit queue; smoltcp keeps it at the head until it can.
    for _ in 0..3 {
        lan.stack.ping([10, 0, 2, 77], 8, 300, lan.now).unwrap();
    }
    lan.run_until(800, |lan| lan.stack.pings_outstanding() == 0);
    assert_eq!(lan.stack.take_ping_results().len(), 3);
    lan.stack.ping(GW_IP, 8, 1500, lan.now).unwrap();
    assert!(lan.run_until(1500, |lan| lan.stack.pings_outstanding() == 0));
    let results = lan.stack.take_ping_results();
    assert!(
        matches!(
            results[..],
            [crate::PingResult {
                outcome: PingOutcome::Reply { .. },
                ..
            }]
        ),
        "{results:?}"
    );
    // Cancelling a ping clears its queued request the same way.
    let doomed = lan.stack.ping([10, 0, 2, 77], 8, 5000, lan.now).unwrap();
    lan.stack.cancel_ping(doomed);
    lan.stack.ping(GW_IP, 8, 1500, lan.now).unwrap();
    assert!(lan.run_until(1500, |lan| lan.stack.pings_outstanding() == 0));
    assert_eq!(lan.stack.counters().pings_answered, 2);
}
