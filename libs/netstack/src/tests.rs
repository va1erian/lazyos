//! Unit tests: DHCP, ARP, the echo path, lease validation, forged echo replies,
//! bounded work and a poisoned ring. The randomized frame fuzzing is in
//! `fuzz.rs`; the ping tests are in `tests/ping.rs`.

use std::vec::Vec;

use crate::config::{Mode, StaticConfig};
use crate::stack::{DhcpState, Source};
use crate::testnet::*;

mod dns;
mod ping;
mod sockets;
mod sockets_stress;

pub(super) fn dhcp_lan() -> Lan {
    let mut lan = Lan::new(&Mode::Dhcp);
    lan.configure();
    lan
}

#[test]
fn dhcp_completes_and_configures_the_interface() {
    let lan = dhcp_lan();
    let state = lan.stack.state();
    assert_eq!(state.addr, Some(LEASE_IP));
    assert_eq!(state.prefix_len, 24);
    assert_eq!(state.gateway, Some(GW_IP));
    assert_eq!(state.dns, std::vec![[10, 0, 2, 3]]);
    assert_eq!(state.source, Source::Dhcp);
    assert_eq!(state.dhcp, DhcpState::Bound);
    assert!(
        state
            .lease_ends_ms
            .is_some_and(|end| end > lan.now + 80_000_000),
        "a one-day lease"
    );
    assert_eq!(
        lan.gateway.requests_seen,
        ["discover", "request"],
        "a complete exchange and nothing else"
    );
    assert_eq!(lan.stack.counters().leases, 1);
    assert!(lan.stack.epoch() >= 1);
}

#[test]
fn the_dhcp_frames_are_well_formed_broadcasts() {
    let lan = dhcp_lan();
    let discover = &lan.sent[0];
    assert_eq!(&discover[..6], &[0xFF; 6], "broadcast destination");
    assert_eq!(&discover[6..12], &STACK_MAC);
    assert_eq!(&discover[12..14], &[0x08, 0x00]);
    assert_eq!(discover[23], 17, "UDP");
    assert_eq!(&discover[34..38], &[0, 68, 0, 67], "ports 68 -> 67");
    assert!(lan.sent.iter().all(|f| f.len() >= 14 && f.len() <= 1514));
}

#[test]
fn without_a_server_the_client_keeps_asking() {
    let mut lan = Lan::new(&Mode::Dhcp);
    lan.gateway.answer_dhcp = false;
    assert!(!lan.run_until(60_000, |lan| lan.stack.state().addr.is_some()));
    let discovers = lan
        .gateway
        .requests_seen
        .iter()
        .filter(|r| **r == "discover")
        .count();
    assert!(
        (3..=40).contains(&discovers),
        "{discovers} discovers in a minute: retried, but with backoff"
    );
    assert_eq!(lan.stack.state().dhcp, DhcpState::Discovering);
    assert_eq!(lan.stack.state().addr, None);
}

#[test]
fn renew_drops_the_lease_and_gets_a_new_one() {
    let mut lan = dhcp_lan();
    let epoch = lan.stack.epoch();
    lan.stack.renew();
    assert_eq!(lan.stack.state().addr, None);
    assert_eq!(lan.stack.state().dhcp, DhcpState::Discovering);
    assert_eq!(lan.stack.counters().lease_losses, 1);
    lan.configure();
    assert_eq!(lan.stack.state().addr, Some(LEASE_IP));
    assert_eq!(lan.stack.counters().leases, 2);
    assert!(lan.stack.epoch() > epoch);
}

#[test]
fn hostile_leases_are_not_applied() {
    let cases: [(&str, [u8; 4], [u8; 4]); 6] = [
        ("unspecified", [0, 0, 0, 0], [255, 255, 255, 0]),
        ("loopback", [127, 0, 0, 1], [255, 0, 0, 0]),
        ("multicast", [224, 0, 0, 5], [255, 255, 255, 0]),
        ("broadcast mask /0", [10, 0, 2, 15], [0, 0, 0, 0]),
        ("point-to-point /31", [10, 0, 2, 15], [255, 255, 255, 254]),
        ("host /32", [10, 0, 2, 15], [255, 255, 255, 255]),
    ];
    for (name, ip, mask) in cases {
        let mut lan = Lan::new(&Mode::Dhcp);
        lan.gateway.offer_ip = ip;
        lan.gateway.mask = mask;
        assert!(
            !lan.run_until(3000, |lan| lan.stack.state().addr.is_some()),
            "{name}: a bad lease was applied"
        );
        assert_eq!(lan.stack.state().addr, None, "{name}");
    }
}

#[test]
fn a_renewal_the_stack_rejects_drops_the_old_address() {
    let mut lan = dhcp_lan();
    lan.gateway.lease_secs = 20;
    // Start over with a short lease so the renewal falls inside the test.
    lan.stack.renew();
    assert!(lan.run_until(3000, |lan| lan.stack.state().addr.is_some()));
    let losses = lan.stack.counters().lease_losses;
    // The server now "renews" us onto an address we refuse.
    lan.gateway.offer_ip = [127, 0, 0, 1];
    assert!(
        lan.run_until(30_000, |lan| lan.stack.state().addr.is_none()),
        "the old address outlived a renewal the server changed"
    );
    assert!(lan.stack.counters().lease_losses > losses);
    assert_eq!(lan.stack.state().gateway, None);
    assert_eq!(lan.stack.state().dhcp, DhcpState::Discovering);
    // And it keeps looking: a good server gets us configured again.
    lan.gateway.offer_ip = LEASE_IP;
    assert!(
        lan.run_until(30_000, |lan| lan.stack.state().addr.is_some()),
        "the stack did not go back to discovering"
    );
}

#[test]
fn a_bad_router_or_resolver_is_dropped_but_the_lease_stands() {
    let mut lan = Lan::new(&Mode::Dhcp);
    lan.gateway.router = Some([10, 0, 2, 15]); // our own address
    lan.gateway.dns = std::vec![[0, 0, 0, 0], [224, 0, 0, 1], [10, 0, 2, 3]];
    lan.configure();
    let state = lan.stack.state();
    assert_eq!(state.addr, Some(LEASE_IP));
    assert_eq!(
        state.gateway, None,
        "a router equal to our own address is refused"
    );
    assert!(
        state.dns.iter().all(|d| d[0] == 10),
        "only usable resolvers"
    );
    assert!(state.dns.len() <= 3);
}

#[test]
fn a_static_setup_needs_no_dhcp() {
    let config = StaticConfig {
        addr: [192, 168, 5, 9],
        prefix_len: 24,
        gateway: Some([192, 168, 5, 1]),
        dns: Some([192, 168, 5, 1]),
    };
    let mut lan = Lan::new(&Mode::Static(config));
    let state = lan.stack.state();
    assert_eq!(
        (state.addr, state.prefix_len, state.gateway),
        (Some([192, 168, 5, 9]), 24, Some([192, 168, 5, 1]))
    );
    assert_eq!((state.source, state.dhcp), (Source::Static, DhcpState::Off));
    lan.step(5000);
    assert!(
        lan.gateway.requests_seen.iter().all(|r| *r != "discover"),
        "no DHCP on a static interface"
    );
    lan.stack.renew();
    assert_eq!(
        lan.stack.state().addr,
        Some([192, 168, 5, 9]),
        "renew is a no-op when static"
    );
}

#[test]
fn work_per_poll_is_bounded_under_a_flood() {
    let mut lan = dhcp_lan();
    // Fill the receive ring with echo requests faster than one poll drains.
    let request = echo_frame(STACK_MAC, GW_MAC, GW_IP, LEASE_IP, false, 1, 1, &[0u8; 100]);
    let mut queued = 0;
    while lan.deliver(&request) {
        queued += 1;
    }
    assert_eq!(queued, SLOTS as usize);
    let before = lan.stack.device_stats().rx_frames;
    lan.stack.poll(lan.now);
    let taken = lan.stack.device_stats().rx_frames - before;
    assert!(taken >= 1 && taken <= u64::from(crate::stack::INGRESS_BUDGET));
    lan.step(50);
    assert!(
        lan.stack.device_stats().rx_frames - before >= queued as u64,
        "everything is processed eventually"
    );
}

#[test]
fn a_full_transmit_ring_loses_frames_not_the_stack() {
    let mut lan = dhcp_lan();
    // Nobody drains the stack's transmit ring: flood it with requests.
    let request = echo_frame(STACK_MAC, GW_MAC, GW_IP, LEASE_IP, false, 1, 1, &[0u8; 20]);
    for _ in 0..200 {
        lan.deliver(&request);
        lan.stack.poll(lan.now);
    }
    assert!(lan.stack.device_stats().tx_frames <= u64::from(SLOTS) + lan.sent.len() as u64 + 8);
    assert!(!lan.stack.device().is_poisoned());
    // Draining restores service.
    lan.exchange();
    lan.step(50);
    assert!(lan.stack.ping(GW_IP, 8, 500, lan.now).is_ok());
}

#[test]
fn a_corrupt_receive_ring_stops_the_device_without_a_panic() {
    let mut lan = dhcp_lan();
    // The peer claims far more frames than the ring holds.
    lan.mem.bytes()[framering::off::HEAD..framering::off::HEAD + 4]
        .copy_from_slice(&0x7FFF_0000u32.to_le_bytes());
    lan.stack.poll(lan.now);
    assert!(lan.stack.device().is_poisoned());
    let frames = lan.stack.device_stats().rx_frames;
    lan.step(100);
    assert_eq!(
        lan.stack.device_stats().rx_frames,
        frames,
        "a poisoned ring delivers nothing"
    );
    lan.mem.assert_guards();
}

#[test]
fn oversized_ring_slots_are_skipped_and_counted() {
    let mut lan = dhcp_lan();
    let ok = echo_frame(STACK_MAC, GW_MAC, GW_IP, LEASE_IP, false, 2, 2, &[1; 10]);
    lan.deliver(&ok);
    lan.deliver(&ok);
    // Scribble 60000 into the first unread slot's length.
    let consumed = lan.stack.device_stats().rx_frames as usize;
    let slot = framering::HEADER_BYTES + (consumed % SLOTS as usize) * framering::SLOT_BYTES;
    lan.mem.bytes()[slot..slot + 2].copy_from_slice(&60_000u16.to_le_bytes());
    lan.step(30);
    assert_eq!(lan.stack.device_stats().rx_bad_length, 1);
    assert!(
        lan.stack.device_stats().rx_frames as usize > consumed,
        "the next frame still arrives"
    );
}

#[test]
fn everything_the_stack_sends_is_a_legal_frame() {
    let mut lan = dhcp_lan();
    for n in [0usize, 1, 500, 1400] {
        lan.stack.ping(GW_IP, n, 500, lan.now).unwrap();
        lan.run_until(500, |lan| lan.stack.pings_outstanding() == 0);
    }
    let sizes: Vec<usize> = lan.sent.iter().map(|f| f.len()).collect();
    assert!(sizes.iter().all(|n| (14..=1514).contains(n)), "{sizes:?}");
    assert!(
        sizes.contains(&(14 + 20 + 8 + 1400)),
        "the biggest echo fits exactly"
    );
}

#[test]
fn losing_and_regaining_the_nic_keeps_the_lease() {
    let mut lan = dhcp_lan();
    let epoch = lan.stack.epoch();
    lan.stack.device_mut().detach();
    assert!(!lan.stack.device().is_attached());
    // A detached device moves nothing, and nothing breaks.
    let frames = lan.stack.device_stats().tx_frames;
    lan.stack.ping(GW_IP, 8, 300, lan.now).unwrap();
    lan.step(400);
    assert_eq!(
        lan.stack.device_stats().tx_frames,
        frames,
        "nothing was sent with no NIC"
    );
    assert_eq!(lan.stack.state().addr, Some(LEASE_IP), "the lease survives");
    assert_eq!(lan.stack.counters().pings_timed_out, 1);
    // The driver is back: new rings, same stack, service resumes.
    lan.rewire();
    lan.stack.ping(GW_IP, 8, 1000, lan.now).unwrap();
    assert!(lan.run_until(1000, |lan| lan.stack.pings_outstanding() == 0));
    assert_eq!(lan.stack.counters().pings_answered, 1);
    assert_eq!(lan.stack.epoch(), epoch, "the address did not change");
}

#[test]
fn a_poisoned_device_recovers_through_detach_and_attach() {
    let mut lan = dhcp_lan();
    lan.mem.bytes()[framering::off::HEAD..framering::off::HEAD + 4]
        .copy_from_slice(&0x7FFF_0000u32.to_le_bytes());
    lan.stack.poll(lan.now);
    assert!(lan.stack.device().is_poisoned());
    lan.stack.device_mut().detach();
    lan.rewire();
    assert!(!lan.stack.device().is_poisoned());
    lan.stack.ping(GW_IP, 8, 1000, lan.now).unwrap();
    assert!(lan.run_until(1000, |lan| lan.stack.pings_outstanding() == 0));
    assert_eq!(lan.stack.counters().pings_answered, 1);
}
