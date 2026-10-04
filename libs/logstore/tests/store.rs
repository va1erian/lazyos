//! The store over an in-memory directory.

use std::collections::BTreeMap;

use logstore::rotate::file_name;
use logstore::store::{JournalFs, Store, TailError, FLUSH_RECORDS, FLUSH_TICKS, MAX_SOURCES};
use logstore::{verify, BUDGET, FILE_CAP};

/// A directory in a map, with an optional byte quota that makes appends fail
/// like a full disk.
#[derive(Default)]
struct MemFs {
    files: BTreeMap<String, Vec<u8>>,
    quota: Option<usize>,
    syncs: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fail {
    NoSpace,
    Missing,
}

impl MemFs {
    fn used(&self) -> usize {
        self.files.values().map(Vec::len).sum()
    }

    fn text(&self, name: &str) -> String {
        String::from_utf8(self.files.get(name).cloned().unwrap_or_default()).unwrap()
    }
}

impl JournalFs for &mut MemFs {
    type Error = Fail;

    fn list(&mut self) -> Result<Vec<(String, u64)>, Fail> {
        Ok(self
            .files
            .iter()
            .map(|(name, data)| (name.clone(), data.len() as u64))
            .collect())
    }

    fn append(&mut self, name: &str, data: &[u8]) -> Result<(), Fail> {
        if self
            .quota
            .is_some_and(|quota| self.used() + data.len() > quota)
        {
            return Err(Fail::NoSpace);
        }
        self.files
            .entry(name.into())
            .or_default()
            .extend_from_slice(data);
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Fail> {
        assert!(!self.files.contains_key(to), "rename over {to}");
        let data = self.files.remove(from).ok_or(Fail::Missing)?;
        self.files.insert(to.into(), data);
        Ok(())
    }

    fn remove(&mut self, name: &str) -> Result<(), Fail> {
        self.files.remove(name);
        Ok(())
    }

    fn sync(&mut self) -> Result<(), Fail> {
        self.syncs += 1;
        Ok(())
    }

    fn read(&mut self, name: &str, out: &mut Vec<u8>) -> Result<bool, Fail> {
        out.clear();
        match self.files.get(name) {
            Some(data) => {
                out.extend_from_slice(data);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

#[test]
fn appends_are_buffered_then_flushed() {
    let mut fs = MemFs::default();
    let mut store = Store::open(&mut fs, 0xb007, 0).unwrap();
    for seq in 1..FLUSH_RECORDS {
        store
            .append(seq, 1, "system/health/confd", "status=ok")
            .unwrap();
    }
    assert_eq!(store.persisted(), 0);
    // A service that parks between events wakes exactly when `tick` flushes.
    assert_eq!(store.flush_due(), Some(FLUSH_TICKS));
    store.tick(FLUSH_TICKS - 1).unwrap();
    assert_eq!(store.persisted(), 0);
    store.tick(FLUSH_TICKS).unwrap();
    assert_eq!(store.persisted(), FLUSH_RECORDS - 1);
    assert_eq!(store.flush_due(), None, "nothing buffered, nothing due");
    for seq in 0..FLUSH_RECORDS {
        store
            .append(100 + seq, 200, "system/health/confd", "status=ok")
            .unwrap();
    }
    assert_eq!(store.persisted(), 2 * FLUSH_RECORDS - 1);
    assert_eq!(store.pending(), 0);
    store.sync(300).unwrap();
    drop(store);
    assert_eq!(fs.syncs, 1);
    let text = fs.text("confd.log");
    assert!(text.starts_with("0\t1\tboot\tid=000000000000b007\t"));
    assert_eq!(verify(&text), Ok(2 * FLUSH_RECORDS - 1));
}

#[test]
fn each_boot_adds_a_boot_line_and_its_own_chain() {
    let mut fs = MemFs::default();
    for boot in 1..=3u64 {
        let mut store = Store::open(&mut fs, boot, 0).unwrap();
        store
            .append(1, 5, "system/events/security/denial", "denies=1")
            .unwrap();
        store.append(2, 6, "weird", "x").unwrap();
        store.sync(7).unwrap();
    }
    let kernel = fs.text("kernel.log");
    assert_eq!(kernel.matches("\tboot\t").count(), 3);
    assert_eq!(verify(&kernel), Ok(3));
    assert_eq!(verify(&fs.text("system.log")), Ok(3));
}

#[test]
fn rotation_keeps_three_files_and_continues_the_chain() {
    let mut fs = MemFs::default();
    let mut store = Store::open(&mut fs, 9, 0).unwrap();
    let detail = "d".repeat(1000);
    for seq in 1..=2000 {
        store
            .append(seq, seq, "system/events/svc/x", &detail)
            .unwrap();
    }
    store.sync(0).unwrap();
    drop(store);
    let names: Vec<&String> = fs.files.keys().collect();
    assert_eq!(names, ["svc.log", "svc.log.1", "svc.log.2"]);
    for generation in 0..3 {
        let text = fs.text(&file_name("svc", generation));
        assert!(text.len() as u64 <= FILE_CAP);
        assert!(verify(&text).unwrap() > 0, "generation {generation}");
    }
    // The live file's boot line continues `.1`'s last hash.
    let previous = fs.text("svc.log.1");
    let last = previous.trim_end().rsplit('\n').next().unwrap();
    let hash = last.rsplit('\t').next().unwrap();
    assert!(fs
        .text("svc.log")
        .lines()
        .next()
        .unwrap()
        .contains(&format!("cont={hash}")));
}

#[test]
fn the_budget_holds_and_pkg_log_is_left_alone() {
    let mut fs = MemFs::default();
    fs.files
        .insert("pkg.log".into(), vec![b'p'; 3 * 1024 * 1024]);
    fs.files.insert("notes.txt".into(), b"keep".to_vec());
    let mut store = Store::open(&mut fs, 1, 0).unwrap();
    let detail = "z".repeat(700);
    for seq in 1..=60_000u64 {
        let topic = format!("system/health/s{}", seq % 40);
        store.append(seq, seq, &topic, &detail).unwrap();
        assert!(store.ledger().total() <= BUDGET);
    }
    store.sync(0).unwrap();
    drop(store);
    assert_eq!(fs.files["pkg.log"].len(), 3 * 1024 * 1024);
    assert_eq!(fs.files["notes.txt"], b"keep");
    let ours: usize = fs
        .files
        .iter()
        .filter(|(name, _)| name.starts_with('s'))
        .map(|(_, data)| data.len())
        .sum();
    assert!(ours as u64 <= BUDGET);
    for (name, data) in &fs.files {
        if name.starts_with('s') {
            assert!(data.len() as u64 <= FILE_CAP, "{name}");
            verify(std::str::from_utf8(data).unwrap()).unwrap();
        }
    }
}

#[test]
fn hostile_sources_are_capped() {
    let mut fs = MemFs::default();
    let mut store = Store::open(&mut fs, 1, 0).unwrap();
    for seq in 0..500u64 {
        store
            .append(seq + 1, 1, &format!("system/health/x{seq}"), "")
            .unwrap();
    }
    store.sync(0).unwrap();
    drop(store);
    assert_eq!(fs.files.len(), MAX_SOURCES + 1);
    assert!(fs.files.contains_key("system.log"));
}

#[test]
fn no_space_is_reported_and_nothing_is_counted() {
    let mut fs = MemFs {
        quota: Some(4096),
        ..MemFs::default()
    };
    let mut store = Store::open(&mut fs, 1, 0).unwrap();
    let mut failed = None;
    for seq in 1..=1000 {
        if let Err(error) = store.append(seq, seq, "system/health/a", "0123456789") {
            failed = Some(error);
            break;
        }
    }
    assert!(failed.is_some());
    let persisted = store.persisted();
    drop(store);
    assert!(persisted > 0);
    assert_eq!(verify(&fs.text("a.log")), Ok(persisted));
}

#[test]
fn tail_reads_the_newest_lines_across_rotation() {
    let mut fs = MemFs::default();
    let mut store = Store::open(&mut fs, 1, 0).unwrap();
    assert_eq!(store.tail("Bad Name", 5, 4096), Err(TailError::Invalid));
    assert_eq!(store.tail("nothing", 5, 4096), Err(TailError::Missing));
    let detail = "q".repeat(1000);
    for seq in 1..=300u64 {
        store.append(seq, seq, "system/health/t", &detail).unwrap();
    }
    // Buffered records are included.
    let lines = store.tail("t", 3, 1 << 20).unwrap();
    assert_eq!(lines.len(), 3);
    assert!(lines[2].starts_with("300\t"));
    // Reaching back into `.1`.
    let lines = store.tail("t", 290, 1 << 20).unwrap();
    assert!(lines.len() >= 280);
    assert!(lines.last().unwrap().starts_with("300\t"));
    // The byte cap keeps the newest.
    let lines = store.tail("t", 290, 4000).unwrap();
    assert_eq!(lines.len(), 3);
    assert!(store.tail("t", 0, 4000).unwrap().is_empty());
    assert_eq!(store.sources().unwrap(), ["t"]);
}

#[test]
fn an_oversized_file_from_an_earlier_boot_is_rotated() {
    let mut fs = MemFs::default();
    fs.files
        .insert("old.log".into(), vec![b'\n'; FILE_CAP as usize + 10]);
    let mut store = Store::open(&mut fs, 1, 0).unwrap();
    store.append(1, 1, "system/health/old", "x").unwrap();
    store.sync(0).unwrap();
    drop(store);
    assert_eq!(fs.files["old.log.1"].len(), FILE_CAP as usize + 10);
    assert_eq!(verify(&fs.text("old.log")), Ok(1));
}
