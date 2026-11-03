//! The checked-in fuzz seeds (`fuzz/seeds_bluetooth.py`) run clean, and the
//! init script seed really drives the init machine to the end.

use crate::init::Phase;

fn seed_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/bthci")
}

#[test]
fn every_checked_in_seed_runs() {
    let mut n = 0;
    for entry in std::fs::read_dir(seed_dir()).unwrap() {
        crate::fuzz::run(&std::fs::read(entry.unwrap().path()).unwrap());
        n += 1;
    }
    assert!(n > 20, "{n} seeds");
}

#[test]
fn the_init_script_seed_completes_the_init_sequence() {
    let seed = std::fs::read(seed_dir().join("script_init")).unwrap();
    assert_eq!(crate::fuzz::run_script(&seed), Phase::Complete);
}
