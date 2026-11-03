//! The bond store through the file layer: load before the controller is up,
//! atomic save, the damaged file kept aside, a crash between write and rename.

use std::collections::BTreeMap;

use btbond::BondStore;
use btsim::{PeerId, SimController};

use super::*;
use crate::store::{load, save, Fs, Loaded, Paths, ENOENT};

const PATHS: Paths = Paths {
    dirs: &["/conf/svc", "/conf/svc/btd"],
    bonds: "/conf/svc/btd/bonds",
    new: "/conf/svc/btd/bonds.new",
    bad: "/conf/svc/btd/bonds.bad",
};

/// A memory file system that records modes and can crash on a rename.
#[derive(Default)]
struct MemFs {
    files: BTreeMap<String, Vec<u8>>,
    modes: BTreeMap<String, u16>,
    dirs: Vec<String>,
    crash_before_rename: bool,
    synced: Vec<String>,
}

impl Fs for MemFs {
    fn read(&mut self, path: &str) -> Result<Vec<u8>, i64> {
        self.files.get(path).cloned().ok_or(ENOENT)
    }
    fn make_dir(&mut self, dir: &str) -> Result<(), i64> {
        self.dirs.push(dir.to_string());
        Ok(())
    }
    fn write_secret(&mut self, path: &str, bytes: &[u8]) -> Result<(), i64> {
        self.files.insert(path.to_string(), bytes.to_vec());
        self.modes.insert(path.to_string(), 0o600);
        Ok(())
    }
    fn sync(&mut self, path: &str) -> Result<(), i64> {
        self.synced.push(path.to_string());
        Ok(())
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<(), i64> {
        if self.crash_before_rename && from == PATHS.new {
            return Err(5);
        }
        let bytes = self.files.remove(from).ok_or(ENOENT)?;
        self.files.insert(to.to_string(), bytes);
        if let Some(mode) = self.modes.remove(from) {
            self.modes.insert(to.to_string(), mode);
        }
        Ok(())
    }
    fn remove(&mut self, path: &str) -> Result<(), i64> {
        self.files.remove(path).map(|_| ()).ok_or(ENOENT)
    }
}

/// A rig that has paired the keyboard and the mouse: its bond file bytes and
/// the simulator (which holds the peers' side of the keys).
fn paired() -> (Vec<u8>, SimController, PeerId) {
    let (mut sim, kbd, mouse) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    sim.br_mut(mouse).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD, MOUSE]));
    rig.run_for(8_000);
    assert!(rig.host.is_typing(addr(KBD)) && rig.host.is_typing(addr(MOUSE)));
    (rig.saved.clone().expect("saved"), rig.sim, kbd)
}

fn paired_bytes() -> Vec<u8> {
    paired().0
}

#[test]
fn a_missing_file_is_an_empty_store() {
    let mut fs = MemFs::default();
    let (store, how) = load(&mut fs, &PATHS);
    assert!(store.is_empty());
    assert_eq!(how, Loaded::Missing);
}

#[test]
fn save_then_load_round_trips_and_the_file_is_private() {
    let bytes = paired_bytes();
    let mut fs = MemFs::default();
    save(&mut fs, &PATHS, &bytes).unwrap();
    assert_eq!(fs.dirs, PATHS.dirs, "the directories are made first");
    assert_eq!(fs.modes[PATHS.bonds], 0o600);
    assert!(
        !fs.files.contains_key(PATHS.new),
        "the temporary file is gone"
    );
    assert_eq!(
        fs.synced,
        [PATHS.new, PATHS.bonds],
        "flushed before and after"
    );
    let (store, how) = load(&mut fs, &PATHS);
    assert_eq!(how, Loaded::Bonds(2));
    assert_eq!(store, BondStore::decode(&bytes).unwrap());
}

#[test]
fn a_crash_before_the_rename_leaves_the_old_file_whole() {
    let bytes = paired_bytes();
    let mut fs = MemFs::default();
    save(&mut fs, &PATHS, &bytes).unwrap();
    let old = fs.files[PATHS.bonds].clone();
    fs.crash_before_rename = true;
    assert!(save(&mut fs, &PATHS, &BondStore::new().encode().unwrap()).is_err());
    assert_eq!(fs.files[PATHS.bonds], old, "the bonds survive");
    assert!(!fs.files.contains_key(PATHS.new), "and no debris");
    let (store, how) = load(&mut fs, &PATHS);
    assert_eq!(how, Loaded::Bonds(2));
    assert_eq!(store.len(), 2);
}

#[test]
fn a_damaged_file_is_kept_aside_and_never_overwritten() {
    let mut bytes = paired_bytes();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    let mut fs = MemFs::default();
    fs.files.insert(PATHS.bonds.to_string(), bytes.clone());
    let (store, how) = load(&mut fs, &PATHS);
    assert!(store.is_empty());
    assert_eq!(how, Loaded::Damaged { moved: true });
    assert_eq!(fs.files[PATHS.bad], bytes);
    assert!(!fs.files.contains_key(PATHS.bonds));
    // The next save writes a fresh file and leaves the evidence.
    save(&mut fs, &PATHS, &BondStore::new().encode().unwrap()).unwrap();
    assert_eq!(fs.files[PATHS.bad], bytes);
}

#[test]
fn truncated_and_garbage_files_never_panic() {
    let bytes = paired_bytes();
    for cut in [0, 1, 4, 10, bytes.len() / 2, bytes.len() - 1] {
        let mut fs = MemFs::default();
        fs.files
            .insert(PATHS.bonds.to_string(), bytes[..cut].to_vec());
        let (store, how) = load(&mut fs, &PATHS);
        assert!(store.is_empty());
        assert_eq!(how, Loaded::Damaged { moved: true }, "cut at {cut}");
    }
}

#[test]
fn a_boot_from_the_loaded_file_types_from_the_first_moment() {
    let (bytes, mut sim, kbd) = paired();
    let mut fs = MemFs::default();
    save(&mut fs, &PATHS, &bytes).unwrap();
    // The host reboots: the file layer loads the bonds before the controller
    // is brought up, and the keyboard is already being pressed.
    let (store, how) = load(&mut fs, &PATHS);
    assert_eq!(how, Loaded::Bonds(2));
    sim.power_cycle();
    sim.press_key(kbd, 30_000, KEY_A);
    let mut rig = Rig::boot(sim, store, Config::default(), 30_000);
    rig.run_for(3_000);
    rig.assert_clean();
    assert_eq!(rig.typed(), [4]);
    assert!(rig.host.is_typing(addr(KBD)));
}
