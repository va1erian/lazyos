//! The checked-in fuzz seeds (`fuzz/seeds_btfw.py`) run clean, and the two
//! scripted loads really drive a loader to the end.

use crate::step::{Config, Done, Step};

fn seed_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/btfw")
}

fn seed(name: &str) -> std::vec::Vec<u8> {
    std::fs::read(seed_dir().join(name)).unwrap()
}

#[test]
fn every_checked_in_seed_runs() {
    let mut n = 0;
    for entry in std::fs::read_dir(seed_dir()).unwrap() {
        crate::fuzz::run(&std::fs::read(entry.unwrap().path()).unwrap());
        n += 1;
    }
    assert!(n >= 10, "{n} seeds");
}

#[test]
fn the_intel_script_seed_loads_a_controller() {
    assert_eq!(
        crate::fuzz::run_script(&seed("script_intel_load")),
        Some(Step::Done(Done::Operational {
            downloaded: true,
            config: Config::Applied(2)
        }))
    );
}

#[test]
fn the_realtek_script_seed_loads_a_controller() {
    assert_eq!(
        crate::fuzz::run_script(&seed("script_realtek_load")),
        Some(Step::Done(Done::Operational {
            downloaded: true,
            config: Config::NotUsed
        }))
    );
}
