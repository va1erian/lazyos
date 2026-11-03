//! The checked-in fuzz seeds (`fuzz/seeds_bluetooth.py`) run clean and reach
//! the paths they were written for.

use alloc::vec::Vec;

use crate::manager::Event;

fn seed_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/btl2cap")
}

fn seed(name: &str) -> Vec<u8> {
    std::fs::read(seed_dir().join(name)).unwrap()
}

#[test]
fn every_checked_in_seed_runs() {
    let mut n = 0;
    for entry in std::fs::read_dir(seed_dir()).unwrap() {
        crate::fuzz::run(&std::fs::read(entry.unwrap().path()).unwrap());
        n += 1;
    }
    assert!(n >= 8, "{n} seeds");
}

#[test]
fn the_open_close_seed_opens_and_closes_a_channel() {
    let events = crate::fuzz::script(&seed("script_br_open_close"));
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Opened {
                psm: 0x11,
                incoming: true,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(events.iter().any(|e| matches!(e, Event::Closed { .. })));
}

#[test]
fn the_other_seeds_reach_their_paths() {
    let le = crate::fuzz::script(&seed("script_le_fixed_channels"));
    assert!(le.iter().any(|e| matches!(e, Event::Data { cid: 4, .. })));
    assert!(le.iter().any(|e| matches!(e, Event::Data { cid: 6, .. })));
    let refused = crate::fuzz::script(&seed("script_br_refused"));
    assert!(refused
        .iter()
        .any(|e| matches!(e, Event::Refused { psm: 0x11, .. })));
    let split = crate::fuzz::script(&seed("script_br_fragmented"));
    assert!(!split.iter().any(|e| matches!(e, Event::Misbehaved { .. })));
    let oversize = crate::fuzz::script(&seed("script_br_oversize"));
    assert!(oversize.is_empty());
}
