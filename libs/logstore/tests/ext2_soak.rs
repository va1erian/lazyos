//! The store on a real ext2 volume made and checked by `libs/ext2fs` (the
//! driver the kernel mounts `/` with), at full scale: the default 256 KiB cap
//! and 8 MiB budget, 200 000 records over 40 sources, then the independent
//! fsck-style checker. The kernel suite (`ext2_suite::logd_store`) runs the
//! same store through the VFS adapter on a scaled-down volume.

use ext2fs::check::fsck;
use ext2fs::memio::MemIo;
use ext2fs::{Ext2, Ext2Error, FileKind, Geometry, Owner};
use logstore::rotate::{file_name, parse_name};
use logstore::store::{JournalFs, Store};
use logstore::{verify, BUDGET, FILE_CAP};

const DIR: &str = "/logs";

fn clock() -> i64 {
    1_700_000_000
}

/// A formatted volume of `bytes` with `/logs` made, as the image build does.
fn volume(bytes: u64) -> MemIo {
    let io = MemIo::new(bytes as usize);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: (bytes / 4096) as u32,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&io, geometry, "lazyos-root", [7; 16], clock()).unwrap();
    let fs = mount(&io);
    fs.mkdir_p(DIR, 0o755, 0, 0).unwrap();
    fs.flush().unwrap();
    io
}

fn mount(io: &MemIo) -> Ext2 {
    Ext2::open(Box::new(io.clone()), clock).unwrap()
}

/// [`JournalFs`] over the library, the way the kernel adapter drives it.
struct Dir<'a>(&'a Ext2);

fn path(name: &str) -> String {
    format!("{DIR}/{name}")
}

impl JournalFs for Dir<'_> {
    type Error = Ext2Error;

    fn list(&mut self) -> Result<Vec<(String, u64)>, Ext2Error> {
        let mut files = Vec::new();
        for entry in self.0.readdir(DIR)? {
            if entry.kind == FileKind::File {
                files.push((entry.name.clone(), self.0.lookup(&path(&entry.name))?.size));
            }
        }
        Ok(files)
    }

    fn append(&mut self, name: &str, data: &[u8]) -> Result<(), Ext2Error> {
        let path = path(name);
        let end = match self.0.lookup(&path) {
            Ok(meta) => meta.size,
            Err(Ext2Error::NotFound) => {
                self.0.create(&path, 0o100644, Owner { uid: 0, gid: 0 })?;
                0
            }
            Err(error) => return Err(error),
        };
        let written = self.0.write(&path, end, data)?;
        if written == data.len() {
            Ok(())
        } else {
            Err(Ext2Error::NoSpace)
        }
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Ext2Error> {
        self.0.rename(&path(from), &path(to))
    }

    fn remove(&mut self, name: &str) -> Result<(), Ext2Error> {
        match self.0.unlink(&path(name)) {
            Ok(()) | Err(Ext2Error::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn sync(&mut self) -> Result<(), Ext2Error> {
        self.0.flush()
    }

    fn read(&mut self, name: &str, out: &mut Vec<u8>) -> Result<bool, Ext2Error> {
        match self.0.read_file(&path(name)) {
            Ok(data) => {
                *out = data;
                Ok(true)
            }
            Err(Ext2Error::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// Every journal on the volume: `(name, bytes)`.
fn journals(fs: &Ext2) -> Vec<(String, Vec<u8>)> {
    fs.readdir(DIR)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == FileKind::File)
        .map(|entry| {
            (
                entry.name.clone(),
                fs.read_file(&path(&entry.name)).unwrap(),
            )
        })
        .collect()
}

fn assert_clean(io: &MemIo) {
    let problems = fsck(&io.snapshot());
    assert!(problems.is_empty(), "fsck: {problems:#?}");
}

#[test]
fn soak_200k_records_over_40_sources() {
    let io = volume(24 * 1024 * 1024);
    let mut appended = 0u64;
    // Two boots, so the second starts from the first one's files.
    for boot in 1..=2u64 {
        let fs = mount(&io);
        let mut store = Store::open(Dir(&fs), boot, 0).unwrap();
        for index in 0..100_000u64 {
            appended += 1;
            let topic = format!("system/events/svc{:02}/state", index % 40);
            let detail = format!(
                "boot={boot} index={index} state=running pid={}",
                index % 977
            );
            store.append(appended, index, &topic, &detail).unwrap();
            assert!(store.ledger().total() <= BUDGET);
            store.tick(index).unwrap();
        }
        store.sync(0).unwrap();
        drop(store);
        fs.flush().unwrap();
    }
    assert_clean(&io);
    let fs = mount(&io);
    let files = journals(&fs);
    let total: usize = files.iter().map(|(_, data)| data.len()).sum();
    assert!(total as u64 <= BUDGET, "/logs holds {total} bytes");
    assert!(files.len() <= 40 * 3);
    for (name, data) in &files {
        assert!(parse_name(name).is_some(), "stray file {name}");
        assert!(
            data.len() as u64 <= FILE_CAP,
            "{name} is {} bytes",
            data.len()
        );
        verify(std::str::from_utf8(data).unwrap()).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
    // The newest records of the last boot are on disk after the remount.
    let live = fs.read_file(&path(&file_name("svc39", 0))).unwrap();
    assert!(String::from_utf8(live)
        .unwrap()
        .contains("boot=2 index=99999 "));
}

#[test]
fn a_full_volume_fails_the_append_and_stays_consistent() {
    let io = volume(1024 * 1024);
    let fs = mount(&io);
    let mut store = Store::open(Dir(&fs), 1, 0).unwrap();
    let detail = "x".repeat(300);
    let mut failed = false;
    for seq in 1..=20_000u64 {
        let topic = format!("system/health/s{}", seq % 8);
        if let Err(error) = store.append(seq, seq, &topic, &detail) {
            assert_eq!(error, Ext2Error::NoSpace);
            failed = true;
            break;
        }
    }
    assert!(failed, "the volume never filled");
    let persisted = store.persisted();
    assert!(persisted > 0);
    drop(store);
    fs.flush().unwrap();
    drop(fs);
    assert_clean(&io);
    let fs = mount(&io);
    let mut on_disk = 0;
    for (name, data) in journals(&fs) {
        // A line cut by the full disk has no newline; the verifier ignores it.
        on_disk +=
            verify(std::str::from_utf8(&data).unwrap()).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
    assert!(
        on_disk >= persisted,
        "{on_disk} on disk, {persisted} persisted"
    );
}
