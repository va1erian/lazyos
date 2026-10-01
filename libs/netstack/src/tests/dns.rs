//! Name lookups: an answer, a name that does not exist, a silent resolver,
//! literal addresses, hostile names and the outstanding-lookup limit.

use crate::stack::{valid_host_name, LookupOutcome, ResolveError, MAX_LOOKUPS};
use crate::testnet::*;

use std::format;

use super::dhcp_lan;

/// A lan whose resolver is the gateway itself, with `example.test` known.
fn lan_with_dns() -> Lan {
    let mut lan = Lan::new(&crate::config::Mode::Dhcp);
    lan.gateway.dns = std::vec![GW_IP];
    lan.gateway.dns_records = std::vec![("example.test", [93, 184, 216, 34])];
    lan.configure();
    lan
}

fn wait_one(lan: &mut Lan, id: u32) -> LookupOutcome {
    assert!(lan.run_until(30_000, |lan| lan.stack.lookups_outstanding() == 0));
    let mut results = lan.stack.take_lookup_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, id);
    results.remove(0).outcome
}

#[test]
fn a_name_resolves_to_its_address() {
    let mut lan = lan_with_dns();
    let id = lan.stack.resolve("example.test", 5000, lan.now).unwrap();
    assert_eq!(
        wait_one(&mut lan, id),
        LookupOutcome::Found(std::vec![[93, 184, 216, 34]])
    );
    assert!(lan.gateway.requests_seen.contains(&"dns"));
    let counters = lan.stack.counters();
    assert_eq!((counters.lookups_sent, counters.lookups_answered), (1, 1));
}

#[test]
fn an_unknown_name_is_reported_as_missing() {
    let mut lan = lan_with_dns();
    let id = lan.stack.resolve("nothing.test", 5000, lan.now).unwrap();
    assert_eq!(wait_one(&mut lan, id), LookupOutcome::NoSuchName);
    assert_eq!(lan.stack.counters().lookups_failed, 1);
}

#[test]
fn a_silent_resolver_ends_at_the_deadline() {
    let mut lan = lan_with_dns();
    lan.gateway.answer_dns = false;
    let started = lan.now;
    let id = lan.stack.resolve("example.test", 700, lan.now).unwrap();
    assert_eq!(wait_one(&mut lan, id), LookupOutcome::TimedOut);
    let waited = lan.now - started;
    assert!((700..=900).contains(&waited), "waited {waited} ms");
    assert_eq!(lan.stack.lookups_outstanding(), 0, "the slot is free again");
    // And a later lookup works once the resolver is back.
    lan.gateway.answer_dns = true;
    let id = lan.stack.resolve("example.test", 5000, lan.now).unwrap();
    assert!(matches!(wait_one(&mut lan, id), LookupOutcome::Found(_)));
}

#[test]
fn a_literal_address_needs_no_query() {
    let mut lan = lan_with_dns();
    let id = lan.stack.resolve("10.1.2.3", 5000, lan.now).unwrap();
    let results = lan.stack.take_lookup_results();
    assert_eq!(results.len(), 1);
    assert_eq!(
        (results[0].id, results[0].outcome.clone()),
        (id, LookupOutcome::Found(std::vec![[10, 1, 2, 3]]))
    );
    assert!(!lan.gateway.requests_seen.contains(&"dns"));
}

#[test]
fn bad_names_are_refused_before_a_query_exists() {
    let mut lan = lan_with_dns();
    let long_label = "a".repeat(64);
    let long_name = std::vec!["abcdefghi"; 30].join(".");
    for name in [
        "",
        ".",
        "a..b",
        "-",
        "a b",
        "a/b",
        "ü.test",
        &long_label,
        &long_name,
        "a\0b",
    ] {
        assert_eq!(
            lan.stack.resolve(name, 1000, lan.now),
            Err(ResolveError::BadName),
            "{name:?}"
        );
    }
    assert!(valid_host_name("a-b.c_d.example.test."));
    assert_eq!(lan.stack.lookups_outstanding(), 0);
    assert!(!lan.gateway.requests_seen.contains(&"dns"));
}

#[test]
fn without_an_address_or_a_resolver_there_is_nothing_to_ask() {
    let mut lan = Lan::new(&crate::config::Mode::Dhcp);
    assert_eq!(
        lan.stack.resolve("example.test", 1000, lan.now),
        Err(ResolveError::NoResolver)
    );
    let mut lan = dhcp_lan();
    lan.stack.renew();
    assert_eq!(
        lan.stack.resolve("example.test", 1000, lan.now),
        Err(ResolveError::NoResolver)
    );
}

#[test]
fn lookups_outstanding_are_bounded() {
    let mut lan = lan_with_dns();
    lan.gateway.answer_dns = false;
    for i in 0..MAX_LOOKUPS {
        lan.stack
            .resolve(&format!("h{i}.test"), 5000, lan.now)
            .unwrap();
    }
    assert_eq!(
        lan.stack.resolve("one-more.test", 5000, lan.now),
        Err(ResolveError::Busy)
    );
    assert!(lan.run_until(10_000, |lan| lan.stack.lookups_outstanding() == 0));
    assert_eq!(lan.stack.take_lookup_results().len(), MAX_LOOKUPS);
    lan.stack.resolve("again.test", 5000, lan.now).unwrap();
}

#[test]
fn many_lookups_in_a_row_all_complete() {
    let mut lan = lan_with_dns();
    for _ in 0..200 {
        let id = lan.stack.resolve("example.test", 5000, lan.now).unwrap();
        assert!(matches!(wait_one(&mut lan, id), LookupOutcome::Found(_)));
    }
    assert_eq!(lan.stack.counters().lookups_answered, 200);
}
