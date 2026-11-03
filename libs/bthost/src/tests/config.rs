//! The boot arguments `init` passes (`trace=1`, `pair=...`).

use super::*;
use crate::config::{parse_addr, MAX_TARGETS};

#[test]
fn a_default_build_has_pairing_off_and_no_report_trace() {
    let config = Config::from_args("");
    assert!(!config.trace_reports);
    assert_eq!(config.pair, PairPolicy::Off);
    // `init` appends the attempt number to every spawn.
    assert_eq!(Config::from_args("attempt=2").pair, PairPolicy::Off);
}

#[test]
fn trace_and_pair_are_read() {
    let config = Config::from_args("trace=1 pair=any attempt=1");
    assert!(config.trace_reports);
    assert_eq!(config.pair, PairPolicy::Any);
    assert_eq!(Config::from_args("pair=none").pair, PairPolicy::Off);
    assert!(!Config::from_args("trace=0").trace_reports);
    let config = Config::from_args("pair=C0:FF:EE:00:00:01,C0:FF:EE:00:00:02");
    assert_eq!(
        config.pair,
        PairPolicy::Targets(vec![addr(KBD), addr(MOUSE)])
    );
}

#[test]
fn a_bad_pair_list_pairs_nobody() {
    for bad in [
        "pair=C0:FF",
        "pair=zz:zz:zz:zz:zz:zz",
        "pair=,,",
        "pair=C0:FF:EE:00:00:01:02",
    ] {
        assert_eq!(Config::from_args(bad).pair, PairPolicy::Off, "{bad}");
    }
    // A good address among bad ones still counts; the list is bounded.
    let many = (0..10)
        .map(|n| format!("C0:FF:EE:00:00:{n:02X}"))
        .collect::<Vec<_>>()
        .join(",");
    match Config::from_args(&format!("pair={many}")).pair {
        PairPolicy::Targets(t) => assert_eq!(t.len(), MAX_TARGETS),
        other => panic!("{other:?}"),
    }
}

#[test]
fn addresses_are_read_in_printed_order() {
    assert_eq!(parse_addr("C0:FF:EE:00:00:01"), Some(addr(KBD)));
    assert_eq!(parse_addr("c0:ff:ee:00:00:01"), Some(addr(KBD)));
    for bad in [
        "",
        "C0:FF:EE:00:00",
        "C0:FF:EE:00:00:0",
        "C0FFEE000001",
        "C0:FF:EE:00:00:01:",
    ] {
        assert_eq!(parse_addr(bad), None, "{bad:?}");
    }
}

#[test]
fn le_targets_are_the_entries_with_the_le_prefix() {
    let config =
        Config::from_args("pair=C0:FF:EE:00:00:01,le:C0:FF:EE:00:01:01,le:C0:FF:EE:00:01:02");
    assert_eq!(config.pair, PairPolicy::Targets(vec![addr(KBD)]));
    assert_eq!(config.le_targets, [addr(LE_KBD), addr(LE_MOUSE)]);
    // Only LE entries: nobody is paged over BR/EDR.
    let config = Config::from_args("pair=le:C0:FF:EE:00:01:01");
    assert_eq!(config.pair, PairPolicy::Off);
    assert_eq!(config.le_targets, [addr(LE_KBD)]);
    // A bad entry pairs nobody; `any` and `none` name no LE target.
    assert!(Config::from_args("pair=le:zz").le_targets.is_empty());
    assert!(Config::from_args("pair=any").le_targets.is_empty());
    assert!(Config::from_args("").le_targets.is_empty());
}
