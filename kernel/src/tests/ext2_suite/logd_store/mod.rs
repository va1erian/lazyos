//! `logd`'s persistent journals on ext2 (issue #508).
//!
//! `logd` is a ring-3 service; what it does with `/logs` is
//! `libs/logstore`'s [`Store`]. As `confd_store` does for `confd`, this suite
//! binds that same store to a real ext2 volume through the kernel's VFS (the
//! adapter the root filesystem is mounted with), the way the binary binds it
//! to its syscalls (`user/src/bin/logd/journal.rs`): appends survive a
//! remount, a rotation leaves `.log`, `.1` and `.2` only, the budget is never
//! exceeded, and a full disk fails an append without corrupting the volume.
//! The soak is in [`soak`].
//!
//! The test disks live in the kernel heap, so the volumes are a few MiB and
//! the cap and budget are scaled down with [`Limits`]; the full-scale soak
//! (8 MiB budget, 256 KiB files) runs on the host over the same ext2 driver
//! (`libs/logstore/tests/ext2_soak.rs`).

mod soak;

use super::*;
use crate::block::BlockDevice;
use logstore::rotate::{file_name, parse_name};
use logstore::store::{JournalFs, Store};
use logstore::Limits;

use soak::logd_store_soak_200k_records;

/// The journal directory, as on the OS volume.
const DIR: &str = fhs::state::LOGS_ROOT;
/// Block size of the suite's volumes.
const BLOCK: u32 = 1024;

pub(in crate::tests) const CASES: &[(&str, Test)] = &[
    ("logd_store_survives_remount", logd_store_survives_remount),
    (
        "logd_store_rotation_keeps_three_files",
        logd_store_rotation_keeps_three_files,
    ),
    (
        "logd_store_budget_never_exceeded",
        logd_store_budget_never_exceeded,
    ),
    ("logd_store_no_space_degrades", logd_store_no_space_degrades),
    ("logd_store_soak_200k_records", logd_store_soak_200k_records),
];

/// [`JournalFs`] over a VFS directory: what `logd` does with its syscalls.
struct VfsDir {
    vfs: Vfs,
}

fn path(name: &str) -> String {
    format!("{DIR}/{name}")
}

impl JournalFs for VfsDir {
    type Error = FsError;

    fn list(&mut self) -> Result<Vec<(String, u64)>, FsError> {
        let mut files = Vec::new();
        for entry in self.vfs.readdir(Id::ROOT, DIR)? {
            if entry.kind == FileKind::File {
                let size = self.vfs.stat(Id::ROOT, &path(&entry.name))?.size;
                files.push((entry.name, size));
            }
        }
        Ok(files)
    }

    /// The `append_file` syscall's steps (`process/fsops.rs`).
    fn append(&mut self, name: &str, data: &[u8]) -> Result<(), FsError> {
        let path = path(name);
        let end = match self.vfs.stat(Id::ROOT, &path) {
            Ok(meta) => meta.size,
            Err(FsError::NotFound) => {
                self.vfs.create(Id::ROOT, &path, 0o644)?;
                0
            }
            Err(error) => return Err(error),
        };
        match self.vfs.write(Id::ROOT, &path, end, data)? {
            written if written == data.len() => Ok(()),
            _ => Err(FsError::NoSpace),
        }
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), FsError> {
        self.vfs.rename(Id::ROOT, &path(from), &path(to))
    }

    fn remove(&mut self, name: &str) -> Result<(), FsError> {
        match self.vfs.unlink(Id::ROOT, &path(name)) {
            Ok(()) | Err(FsError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn sync(&mut self) -> Result<(), FsError> {
        self.vfs.flush(Id::ROOT, DIR)
    }

    fn read(&mut self, name: &str, out: &mut Vec<u8>) -> Result<bool, FsError> {
        match self.vfs.read_file(Id::ROOT, &path(name)) {
            Ok(data) => {
                *out = data;
                Ok(true)
            }
            Err(FsError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// The suite's disk, leaked once (the block layer wants `'static`) and
/// resized per test; [`release`] hands its memory back.
fn disk() -> &'static FakeDisk {
    static DISK: spin::Mutex<Option<&'static FakeDisk>> = spin::Mutex::new(None);
    let disk = *DISK
        .lock()
        .get_or_insert_with(|| FakeDisk::new("logd-store", 0));
    disk.fail_nth_write(u32::MAX);
    disk
}

fn release(disk: &FakeDisk) {
    *disk.data.lock() = Vec::new();
}

fn lib_error(error: ext2fs::Ext2Error) -> String {
    format!("ext2fs: {error:?}")
}

/// A fresh volume of `blocks` 1 KiB blocks, formatted by `libs/ext2fs` as the
/// image build does, with `/logs` made.
fn volume(blocks: u32) -> Result<&'static FakeDisk, String> {
    let disk = disk();
    *disk.data.lock() = vec![0u8; blocks as usize * BLOCK as usize];
    let geometry = ext2fs::Geometry {
        block_size: BLOCK,
        blocks_count: blocks,
        bytes_per_inode: 4096,
    };
    let device: &'static dyn BlockDevice = disk;
    ext2fs::format(&device, geometry, "lazyos-root", [0x5A; 16], 1_700_000_000)
        .map_err(lib_error)?;
    let (fs, mut vfs) = remount_disk(disk)?;
    vfs.mkdir(Id::ROOT, DIR, 0o755).map_err(fs_error)?;
    fs.flush().map_err(fs_error)?;
    Ok(disk)
}

/// Open the store on a fresh mount of `disk`, as a boot of `logd` would.
fn boot(
    disk: &'static FakeDisk,
    boot_id: u64,
    limits: Limits,
) -> Result<(Arc<Ext2>, Store<VfsDir>), String> {
    let (fs, vfs) = remount_disk(disk)?;
    let store = Store::open_with(VfsDir { vfs }, boot_id, 0, limits).map_err(fs_error)?;
    Ok((fs, store))
}

/// Flush the store and the volume, and drop both (a clean shutdown).
fn shut_down(fs: Arc<Ext2>, mut store: Store<VfsDir>) -> Result<(), String> {
    store.sync(0).map_err(fs_error)?;
    drop(store);
    fs.flush().map_err(fs_error)
}

/// Every file in `/logs` on a fresh mount: `(name, bytes)`.
fn journals(disk: &'static FakeDisk) -> Result<Vec<(String, Vec<u8>)>, String> {
    let (_fs, mut vfs) = remount_disk(disk)?;
    let mut files = Vec::new();
    for entry in vfs.readdir(Id::ROOT, DIR).map_err(fs_error)? {
        if entry.kind != FileKind::File {
            continue;
        }
        let data = vfs
            .read_file(Id::ROOT, &path(&entry.name))
            .map_err(fs_error)?;
        files.push((entry.name, data));
    }
    // Directory order is creation order; the tests compare sorted names.
    files.sort();
    Ok(files)
}

/// Records in one journal, failing on a broken chain.
fn verified(name: &str, data: &[u8]) -> Result<u64, String> {
    let text = core::str::from_utf8(data).map_err(|_| format!("{name} is not UTF-8"))?;
    logstore::verify(text).map_err(|broken| format!("{name}: {broken:?}"))
}

/// Records a boot appends and syncs are all there after a remount, a second
/// boot adds its own boot line and chain, and the denial samples and
/// unusable topics land in `kernel.log` and `system.log`.
pub fn logd_store_survives_remount() -> Result<(), String> {
    task::register_kernel();
    let disk = volume(1024)?;
    for boot_id in 1..=2u64 {
        let (fs, mut store) = boot(disk, boot_id, Limits::DEFAULT)?;
        for seq in 1..=50u64 {
            let topic = format!("system/health/svc{}", seq % 3);
            store
                .append(seq, seq, &topic, "status=ok")
                .map_err(fs_error)?;
        }
        store
            .append(51, 51, logstore::source::DENIAL_TOPIC, "denies=1")
            .map_err(fs_error)?;
        store
            .append(52, 52, "system/events/../x", "hostile")
            .map_err(fs_error)?;
        check!(store.persisted() > 0, "nothing flushed after 52 records");
        shut_down(fs, store)?;
    }
    let files = journals(disk)?;
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    check!(
        names
            == [
                "kernel.log",
                "svc0.log",
                "svc1.log",
                "svc2.log",
                "system.log"
            ],
        "/logs holds {names:?}"
    );
    let mut records = 0;
    for (name, data) in &files {
        records += verified(name, data)?;
        let text = core::str::from_utf8(data).unwrap_or("");
        check!(
            text.matches("\tboot\tid=").count() == 2,
            "{name} does not have one boot line per boot"
        );
    }
    check!(records == 104, "{records} records survived, not 104");
    release(disk);
    Ok(())
}

/// Driving one source far past the cap leaves exactly `.log`, `.log.1` and
/// `.log.2`, each within the cap and each verifying.
pub fn logd_store_rotation_keeps_three_files() -> Result<(), String> {
    task::register_kernel();
    let limits = Limits {
        file_cap: 8 * 1024,
        budget: 1024 * 1024,
    };
    let disk = volume(1024)?;
    let (fs, mut store) = boot(disk, 7, limits)?;
    let detail = "r".repeat(200);
    for seq in 1..=1000u64 {
        store
            .append(seq, seq, "system/events/net/link", &detail)
            .map_err(fs_error)?;
    }
    shut_down(fs, store)?;
    let files = journals(disk)?;
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    check!(
        names == ["net.log", "net.log.1", "net.log.2"],
        "rotation left {names:?}"
    );
    for (name, data) in &files {
        check!(
            data.len() as u64 <= limits.file_cap,
            "{name} is {} bytes",
            data.len()
        );
        check!(verified(name, data)? > 0, "{name} holds no records");
    }
    check_volume(disk, 1024)?;
    release(disk);
    Ok(())
}

/// Many sources against a small budget: after every append the store's
/// ledger, and after every flush the files themselves, stay within it, and
/// a file `logd` does not own (`pkg.log`) is neither counted nor touched.
pub fn logd_store_budget_never_exceeded() -> Result<(), String> {
    task::register_kernel();
    let limits = Limits {
        file_cap: 8 * 1024,
        budget: 48 * 1024,
    };
    let disk = volume(1024)?;
    {
        let (_fs, mut vfs) = remount_disk(disk)?;
        let pkg = path("pkg.log");
        vfs.create(Id::ROOT, &pkg, 0o644).map_err(fs_error)?;
        vfs.write(Id::ROOT, &pkg, 0, &[b'p'; 20 * 1024])
            .map_err(fs_error)?;
        vfs.flush(Id::ROOT, DIR).map_err(fs_error)?;
    }
    let (fs, mut store) = boot(disk, 3, limits)?;
    let detail = "b".repeat(120);
    for seq in 1..=3000u64 {
        let topic = format!("system/health/s{}", seq % 12);
        store.append(seq, seq, &topic, &detail).map_err(fs_error)?;
        check!(
            store.ledger().total() <= limits.budget,
            "record {seq}: the ledger holds {}",
            store.ledger().total()
        );
    }
    shut_down(fs, store)?;
    let files = journals(disk)?;
    let mut ours = 0u64;
    for (name, data) in &files {
        if name == "pkg.log" {
            check!(
                data.len() == 20 * 1024 && data.iter().all(|&b| b == b'p'),
                "pkg.log was touched"
            );
            continue;
        }
        check!(parse_name(name).is_some(), "stray file {name}");
        check!(
            data.len() as u64 <= limits.file_cap,
            "{name} is over the cap"
        );
        verified(name, data)?;
        ours += data.len() as u64;
    }
    check!(ours <= limits.budget, "/logs holds {ours} bytes");
    check_volume(disk, 1024)?;
    release(disk);
    Ok(())
}

/// A 1 MiB volume (the smallest `libs/ext2fs` formats) fills up long before
/// the default budget: the append that hits `NoSpace` returns the error (so `logd` falls back to its ring, which still
/// holds every record), the store does not claim more than it wrote, and the
/// volume is consistent with every complete line verifying after a remount.
pub fn logd_store_no_space_degrades() -> Result<(), String> {
    task::register_kernel();
    let disk = volume(1024)?;
    let (fs, mut store) = boot(disk, 5, Limits::DEFAULT)?;
    let detail = "n".repeat(300);
    let mut ring = 0u64;
    let mut failure = None;
    for seq in 1..=5000u64 {
        // The caller's ring takes the record first, as `logd` does.
        ring += 1;
        let topic = format!("system/health/s{}", seq % 6);
        if let Err(error) = store.append(seq, seq, &topic, &detail) {
            failure = Some(error);
            break;
        }
    }
    check!(
        failure == Some(FsError::NoSpace),
        "the full volume reported {failure:?}"
    );
    let persisted = store.persisted();
    check!(
        persisted > 0 && persisted < ring,
        "persisted {persisted} of {ring} records"
    );
    // The store is abandoned here, as `logd` abandons it; the ring is intact.
    drop(store);
    fs.flush().map_err(fs_error)?;
    drop(fs);
    check_volume(disk, 1024)?;
    let mut on_disk = 0;
    for (name, data) in journals(disk)? {
        on_disk += verified(&name, &data)?;
        check!(
            parse_name(&name).is_some_and(|(source, _)| file_name(source, 0) == name),
            "a full volume left {name}"
        );
    }
    check!(
        on_disk >= persisted,
        "{on_disk} records on disk, {persisted} reported persisted"
    );
    release(disk);
    Ok(())
}
