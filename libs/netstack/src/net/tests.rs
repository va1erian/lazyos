//! `Net` over scripted gateways: which interface carries what, how the
//! primary and the resolvers follow the metrics and the links, and how pings
//! and lookups behave when interfaces come and go.

use std::vec::Vec;

use crate::net::IfKind::{Wired, Wireless};
use crate::stack::{LookupOutcome, PingOutcome};
use crate::testmulti::MultiLan;

mod sockets;

fn answered(lan: &mut MultiLan, seq: u16) -> Option<PingOutcome> {
    lan.net
        .take_ping_results()
        .into_iter()
        .find(|r| r.seq == seq)
        .map(|r| r.outcome)
}

fn ping_via(lan: &mut MultiLan, dst: [u8; 4]) -> PingOutcome {
    let seq = lan.net.ping(dst, 8, 2000, lan.now).expect("a route");
    let mut out = None;
    lan.run_until(3000, |lan| {
        out = answered(lan, seq);
        out.is_some()
    });
    out.expect("a result")
}

fn echoes(lan: &MultiLan, n: usize) -> usize {
    lan.seen(n).iter().filter(|r| **r == "echo").count()
}

#[test]
fn every_interface_runs_its_own_dhcp() {
    let mut lan = MultiLan::new(&[Wired, Wired]);
    lan.configure();
    for n in 0..2 {
        let state = lan.net.unit(n).unwrap().stack.state();
        assert_eq!(state.addr, Some([10, 0, 2 + n as u8, 15]));
        assert_eq!(state.gateway, Some([10, 0, 2 + n as u8, 2]));
        assert!(lan.seen(n).contains(&"discover") && lan.seen(n).contains(&"request"));
    }
}

#[test]
fn the_lowest_metric_default_route_is_primary() {
    let mut lan = MultiLan::new(&[Wireless, Wired]);
    lan.configure();
    assert_eq!(lan.net.primary(), Some(1), "a cable beats a radio");
    let mut tied = MultiLan::new(&[Wired, Wired]);
    tied.configure();
    assert_eq!(tied.net.primary(), Some(0), "ties go to the lower slot");
}

#[test]
fn traffic_follows_the_primary_and_on_link_destinations_their_own_interface() {
    let mut lan = MultiLan::new(&[Wired, Wired]);
    lan.configure();
    assert!(matches!(
        ping_via(&mut lan, [8, 8, 8, 8]),
        PingOutcome::Reply { .. }
    ));
    assert_eq!((echoes(&lan, 0), echoes(&lan, 1)), (1, 0));
    // The second network's own gateway is on-link there, whatever the metrics.
    assert!(matches!(
        ping_via(&mut lan, [10, 0, 3, 2]),
        PingOutcome::Reply { .. }
    ));
    assert_eq!((echoes(&lan, 0), echoes(&lan, 1)), (1, 1));
}

#[test]
fn a_dead_link_moves_traffic_and_resolvers_and_up_restarts_dhcp() {
    let mut lan = MultiLan::new(&[Wired, Wired]);
    lan.configure();
    assert_eq!(lan.net.dns_servers(), std::vec![[10, 0, 2, 2]]);
    let generation = lan.net.generation();

    lan.net.set_link(0, false);
    assert_ne!(lan.net.generation(), generation);
    assert_eq!(lan.net.primary(), Some(1));
    assert_eq!(lan.net.dns_servers(), std::vec![[10, 0, 3, 2]]);
    // The lease is kept while the link is down.
    assert_eq!(lan.net.unit(0).unwrap().stack.state().addr, Some([10, 0, 2, 15]));
    assert!(matches!(
        ping_via(&mut lan, [8, 8, 8, 8]),
        PingOutcome::Reply { .. }
    ));
    assert_eq!((echoes(&lan, 0), echoes(&lan, 1)), (0, 1));

    let discovers = |lan: &MultiLan| lan.seen(0).iter().filter(|r| **r == "discover").count();
    assert_eq!(discovers(&lan), 1);
    lan.net.set_link(0, true);
    assert!(lan.run_until(5000, |lan| discovers(lan) == 2
        && lan.net.unit(0).unwrap().stack.state().addr.is_some()));
    assert_eq!(lan.net.primary(), Some(0));
    assert_eq!(lan.net.dns_servers(), std::vec![[10, 0, 2, 2]]);
}

#[test]
fn no_usable_interface_is_unreachable() {
    let mut lan = MultiLan::new(&[Wired]);
    assert_eq!(
        lan.net.ping([8, 8, 8, 8], 8, 1000, lan.now),
        Err(crate::stack::PingError::NoAddress)
    );
    lan.configure();
    lan.net.set_attached(0, false);
    assert_eq!(
        lan.net.ping([8, 8, 8, 8], 8, 1000, lan.now),
        Err(crate::stack::PingError::NoAddress),
        "rings gone: the interface carries nothing"
    );
}

#[test]
fn lookups_ask_the_primarys_resolvers() {
    let mut lan = MultiLan::new(&[Wired, Wired]);
    for gateway in &mut lan.gateways {
        gateway.dns_records = std::vec![("host.test", [192, 0, 2, 7])];
    }
    lan.configure();
    let id = lan.net.resolve("host.test", 2000, lan.now).unwrap();
    let mut got = Vec::new();
    lan.run_until(3000, |lan| {
        got = lan.net.take_lookup_results();
        !got.is_empty()
    });
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, id);
    assert_eq!(got[0].outcome, LookupOutcome::Found(std::vec![[192, 0, 2, 7]]));
    assert!(lan.seen(0).contains(&"dns") && !lan.seen(1).contains(&"dns"));
}

#[test]
fn removing_an_interface_ends_its_probes_and_moves_the_primary() {
    let mut lan = MultiLan::new(&[Wired, Wired]);
    lan.configure();
    lan.gateways[0].answer_echo = false;
    let seq = lan.net.ping([8, 8, 8, 8], 8, 60_000, lan.now).unwrap();
    lan.step(50);
    let name = lan.net.remove_interface(0);
    assert_eq!(name.as_deref(), Some("eth0"));
    let results = lan.net.take_ping_results();
    assert_eq!(results.len(), 1);
    assert_eq!((results[0].seq, results[0].outcome), (seq, PingOutcome::TimedOut));
    assert_eq!(lan.net.primary(), Some(1));
    assert_eq!(lan.net.pings_outstanding(), 0);
    assert!(lan.net.unit(0).is_none());
    // The slot is reused by the next card, under its own name.
    let slot = lan.add(Wired);
    assert_eq!(slot, 0);
}

#[test]
fn a_hot_added_interface_joins_the_routes_and_counters_add_up() {
    let mut lan = MultiLan::new(&[Wired]);
    lan.configure();
    lan.add(Wireless);
    lan.configure();
    assert_eq!(lan.net.routes().len(), 4, "two on-link and two defaults");
    let defaults: Vec<(usize, u32)> = lan
        .net
        .routes()
        .iter()
        .filter(|r| r.prefix_len == 0)
        .map(|r| (r.slot, r.metric))
        .collect();
    assert_eq!(defaults, std::vec![(0, 100), (1, 600)]);
    assert_eq!(lan.net.counters().leases, 2);
    assert!(lan.net.device_stats().tx_frames >= 4);
}

#[test]
fn names_are_unique_and_the_set_is_bounded() {
    let mut lan = MultiLan::new(&[Wired]);
    let stack = crate::stack::Stack::new(
        crate::device::RingDevice::detached(1514),
        [2; 6],
        1,
        0,
        &crate::config::Mode::Dhcp,
    );
    assert_eq!(
        lan.net.add_interface("eth0", Wired, stack, false).err(),
        Some(crate::net::AddError::BadName)
    );
    for _ in 1..crate::net::MAX_INTERFACES {
        lan.add(Wired);
    }
    let stack = crate::stack::Stack::new(
        crate::device::RingDevice::detached(1514),
        [2; 6],
        1,
        0,
        &crate::config::Mode::Dhcp,
    );
    assert_eq!(
        lan.net.add_interface("extra9", Wired, stack, false).err(),
        Some(crate::net::AddError::Full)
    );
}
