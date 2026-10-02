//! Rotation arithmetic and the budget, on the ledger alone.

use logstore::rotate::{file_name, parse_any, parse_name, Action, Footprint, Ledger};
use logstore::{BUDGET, FILE_CAP};

fn rotate(source: &str) -> Action {
    Action::Rotate(source.into())
}

#[test]
fn file_names_round_trip() {
    assert_eq!(file_name("confd", 0), "confd.log");
    assert_eq!(file_name("confd", 1), "confd.log.1");
    assert_eq!(file_name("confd", 2), "confd.log.2");
    for generation in 0..3 {
        assert_eq!(
            parse_name(&file_name("confd", generation)),
            Some(("confd", generation))
        );
    }
    for foreign in [
        "confd.log.3",
        "confd.txt",
        "Confd.log",
        ".log",
        "a b.log",
        "x.log.1.2",
    ] {
        assert_eq!(parse_name(foreign), None, "{foreign}");
    }
    // `pkg.log` is a journal name, but pkgd's.
    assert_eq!(parse_any("pkg.log"), Some(("pkg", 0)));
    assert_eq!(parse_name("pkg.log"), None);
}

#[test]
fn foreign_files_are_not_counted() {
    let mut ledger = Ledger::new();
    ledger.insert("pkg.log", 5 * 1024 * 1024);
    ledger.insert("notes.txt", 100);
    ledger.insert("system.log", 10);
    ledger.insert("system.log.2", 20);
    assert_eq!(ledger.total(), 30);
    assert_eq!(ledger.footprint("system"), Footprint([10, 0, 20]));
    // Re-inserting a name replaces its size.
    ledger.insert("system.log", 15);
    assert_eq!(ledger.total(), 35);
}

#[test]
fn an_append_past_the_cap_rotates_first() {
    let mut ledger = Ledger::new();
    ledger.insert("a.log", FILE_CAP - 100);
    assert!(ledger.plan("a", 100).is_empty());
    assert_eq!(ledger.plan("a", 101), vec![rotate("a")]);
    ledger.insert("a.log.1", 7);
    ledger.insert("a.log.2", 9);
    let plan = ledger.plan("a", 101);
    ledger.apply(&plan[0]);
    assert_eq!(ledger.footprint("a"), Footprint([0, FILE_CAP - 100, 7]));
    assert_eq!(ledger.total(), FILE_CAP - 100 + 7);
    // An empty live file is never rotated.
    assert!(Ledger::new().plan("b", 200).is_empty());
}

#[test]
fn the_budget_sheds_the_largest_source_oldest_first() {
    let mut ledger = Ledger::new();
    // 31 sources at 256 KiB plus a big one with all three generations.
    for index in 0..29 {
        ledger.insert(&format!("s{index:02}.log"), FILE_CAP);
    }
    ledger.insert("big.log", FILE_CAP / 2);
    ledger.insert("big.log.1", FILE_CAP);
    ledger.insert("big.log.2", FILE_CAP);
    let room = BUDGET - ledger.total();
    assert!(ledger.plan("new", room).is_empty());
    let plan = ledger.plan("new", room + 1);
    assert_eq!(plan, vec![Action::Remove("big".into(), 2)]);
    for action in &plan {
        ledger.apply(action);
    }
    assert!(ledger.total() + room < BUDGET);
    // `big` is still the largest: next it gives up `.1`.
    let plan = ledger.plan("new", BUDGET - ledger.total() + 1);
    assert_eq!(plan, vec![Action::Remove("big".into(), 1)]);
}

#[test]
fn a_source_with_only_a_live_file_is_rotated_then_dropped() {
    let mut ledger = Ledger::new();
    ledger.insert("only.log", BUDGET);
    let plan = ledger.plan("only", 10);
    assert_eq!(plan, vec![rotate("only"), Action::Remove("only".into(), 1)]);
    for action in &plan {
        ledger.apply(action);
    }
    assert_eq!(ledger.total(), 0);
}

#[test]
fn ties_shed_in_name_order() {
    let mut ledger = Ledger::new();
    for name in ["b", "a", "c"] {
        ledger.insert(&format!("{name}.log.1"), BUDGET / 3);
    }
    let plan = ledger.plan("z", BUDGET - ledger.total() + 1);
    assert_eq!(plan, vec![Action::Remove("a".into(), 1)]);
}

/// Seeded arithmetic soak: random appends across many sources, carrying out
/// every plan; the invariants hold after every step.
#[test]
fn seeded_plans_keep_every_invariant() {
    let mut seed = 0x5eed_1234_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut ledger = Ledger::new();
    for _ in 0..200_000 {
        let source = format!("s{}", next() % 40);
        let bytes = 40 + next() % 2000;
        for action in ledger.plan(&source, bytes) {
            ledger.apply(&action);
        }
        ledger.grow(&source, bytes);
        assert!(
            ledger.total() <= BUDGET,
            "budget exceeded: {}",
            ledger.total()
        );
        let sum: u64 = ledger.sources().map(|(_, sizes)| sizes.total()).sum();
        assert_eq!(sum, ledger.total());
        assert!(ledger.footprint(&source).live() <= FILE_CAP);
    }
}
