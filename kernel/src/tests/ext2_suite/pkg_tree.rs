//! `pkgd`'s install, upgrade and remove trees on ext2 (issue #508).
//!
//! `pkgd` is a ring-3 service; what it does to `/apps`, `/docs/apps` and
//! `/logs/pkg.log` is `libs/pkgstore`'s `tree` and `audit`. As `logd_store`
//! does for `logd`, this suite binds that same code to a real ext2 volume
//! through the kernel's VFS (the adapter `/` is mounted with), the way the
//! binary binds it to its syscalls (`user/src/bin/pkgd/store.rs`). The
//! package is a stand-in shaped like the sample (the Counter demo: manifest,
//! program, three icons, documentation), built in memory because the kernel
//! has no zip writer; `libs/pkgstore/tests/ext2_soak.rs` runs the same cycles
//! on the host over `libs/ext2fs` with the independent fsck-style checker.

use super::*;
use crate::block::BlockDevice;
use crate::fs::vfs::AttrRequest;
use messenger_generated::os_lazy_pkgd_v1::{encode_pkg_event, PkgEvent};
use pkgstore::audit::{verify_from, Chain};
use pkgstore::layout;
use pkgstore::tree::{self, Node, Source, TreeError, TreeFs};

pub(in crate::tests) const CASES: &[(&str, Test)] = &[
    (
        "pkg_tree_install_upgrade_remove",
        pkg_tree_install_upgrade_remove,
    ),
    (
        "pkg_tree_docs_repair_after_remount",
        pkg_tree_docs_repair_after_remount,
    ),
    ("pkg_tree_soak_1000_cycles", pkg_tree_soak_1000_cycles),
];

const SYSTEM_NAME: &str = "org.lazy.counter";
const DOCS: &str = "/docs/apps/org.lazy.counter";
/// Volume size in 1 KiB blocks: one group. The test disks live in the 16 MiB
/// kernel heap, so this is the `logd_store` soak's 2 MiB, and the audit events
/// are trimmed to fit 3 000 of them (about 600 KiB); the host soak writes full
/// events on a 32 MiB volume.
const BLOCKS: u32 = 2048;
const CYCLES: u32 = 1000;

/// The sample's shape at `version`.
struct Sample(u32);

impl Sample {
    fn install_dir(&self) -> String {
        format!("{SYSTEM_NAME}/1.0.{}-{:08x}", self.0, 0x0c0 + self.0)
    }

    fn install_path(&self) -> String {
        layout::install_path(&self.install_dir()).unwrap_or_default()
    }
}

impl Source for Sample {
    fn entries(&self) -> Vec<(&str, bool)> {
        vec![
            ("manifest.toml", false),
            ("bin/", true),
            ("bin/counter.elf", false),
            ("icons/app-16.png", false),
            ("icons/app-32.png", false),
            ("icons/app-128.png", false),
            ("docs/", true),
            ("docs/README.md", false),
            ("docs/guide/usage.md", false),
        ]
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        let size = if name == "bin/counter.elf" { 6000 } else { 200 };
        let mut data = format!("{name} v{}\n", self.0).into_bytes();
        data.resize(size + self.0 as usize % 7, b'x');
        Ok(data)
    }
}

/// [`TreeFs`] over the VFS, as root: what `pkgd` does with its syscalls.
struct VfsTree {
    vfs: Vfs,
}

impl TreeFs for VfsTree {
    type Error = FsError;

    fn stat(&mut self, path: &str) -> Result<Option<Node>, FsError> {
        match self.vfs.stat(Id::ROOT, path) {
            Ok(meta) if meta.kind == FileKind::Dir => Ok(Some(Node::Dir)),
            Ok(meta) => Ok(Some(Node::File(meta.size))),
            Err(FsError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        self.vfs.mkdir(Id::ROOT, path, 0o755).map(|_| ())
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), FsError> {
        match self.vfs.create(Id::ROOT, path, 0o644) {
            Ok(_) => {}
            Err(FsError::Exists) => self.vfs.truncate(Id::ROOT, path, 0)?,
            Err(error) => return Err(error),
        }
        match self.vfs.write(Id::ROOT, path, 0, data)? {
            written if written == data.len() => Ok(()),
            _ => Err(FsError::NoSpace),
        }
    }

    fn chmod(&mut self, path: &str, mode: u16) -> Result<(), FsError> {
        self.vfs
            .setattr(Id::ROOT, path, AttrRequest::Mode(mode))
            .map(|_| ())
    }

    fn list(&mut self, path: &str) -> Result<Vec<String>, FsError> {
        Ok(self
            .vfs
            .readdir(Id::ROOT, path)?
            .into_iter()
            .map(|entry| entry.name)
            .filter(|name| name != "." && name != "..")
            .collect())
    }

    fn remove(&mut self, path: &str) -> Result<(), FsError> {
        match self.vfs.stat(Id::ROOT, path)?.kind {
            FileKind::Dir => self.vfs.rmdir(Id::ROOT, path),
            _ => self.vfs.unlink(Id::ROOT, path),
        }
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), FsError> {
        self.vfs.rename(Id::ROOT, from, to)
    }
}

fn tree_error(error: TreeError<FsError>) -> String {
    match error {
        TreeError::Fs { step, error } => format!("{step}: {error:?}"),
        TreeError::Bad(step) => step,
    }
}

/// The suite's disk, leaked once and resized per test; [`release`] hands its
/// memory back.
fn disk() -> &'static FakeDisk {
    static DISK: spin::Mutex<Option<&'static FakeDisk>> = spin::Mutex::new(None);
    let disk = *DISK
        .lock()
        .get_or_insert_with(|| FakeDisk::new("pkg-tree", 0));
    disk.fail_nth_write(u32::MAX);
    disk
}

fn release(disk: &FakeDisk) {
    *disk.data.lock() = Vec::new();
}

/// A fresh volume formatted by `libs/ext2fs` with the directories the image
/// build makes for `pkgd`.
fn volume() -> Result<&'static FakeDisk, String> {
    let disk = disk();
    *disk.data.lock() = vec![0u8; BLOCKS as usize * 1024];
    let geometry = ext2fs::Geometry {
        block_size: 1024,
        blocks_count: BLOCKS,
        bytes_per_inode: 4096,
    };
    let device: &'static dyn BlockDevice = disk;
    ext2fs::format(&device, geometry, "lazyos-root", [0x6B; 16], 1_700_000_000)
        .map_err(|error| format!("ext2fs: {error:?}"))?;
    let (fs, mut vfs) = remount_disk(disk)?;
    for dir in ["/apps", "/docs", "/docs/apps", "/logs"] {
        vfs.mkdir(Id::ROOT, dir, 0o755).map_err(fs_error)?;
    }
    fs.flush().map_err(fs_error)?;
    Ok(disk)
}

/// `pkgd`'s filesystem steps for an install (`install.rs`): extract, stage the
/// docs, (record and activate), publish the docs, delete the old version.
fn install(fs: &mut VfsTree, sample: &Sample, previous: Option<&Sample>) -> Result<(), String> {
    let path = sample.install_path();
    tree::remove_tree(fs, &path).map_err(tree_error)?;
    tree::extract(fs, sample, &path).map_err(tree_error)?;
    let staged = tree::stage_docs(fs, sample, SYSTEM_NAME).map_err(tree_error)?;
    tree::commit_docs(fs, SYSTEM_NAME, staged).map_err(tree_error)?;
    if let Some(old) = previous {
        tree::remove_tree(fs, &old.install_path()).map_err(tree_error)?;
    }
    Ok(())
}

fn remove(fs: &mut VfsTree, sample: &Sample) -> Result<(), String> {
    tree::remove_tree(fs, &sample.install_path()).map_err(tree_error)?;
    tree::withdraw_docs(fs, SYSTEM_NAME).map_err(tree_error)?;
    if let Ok(app_dir) = layout::app_dir(SYSTEM_NAME) {
        tree::remove_if_empty(fs, &app_dir);
    }
    Ok(())
}

/// `pkgd`'s audit append (`pkgd/audit.rs`): one chained line.
fn audit(vfs: &mut Vfs, chain: &mut Chain, op: &str, sample: &Sample) -> Result<(), String> {
    let event = PkgEvent {
        op: op.into(),
        system_name: SYSTEM_NAME.into(),
        version: format!("1.0.{}", sample.0),
        install_dir: String::new(),
        digest: String::new(),
        actor_uid: 1000,
        ok: true,
        detail: String::new(),
    };
    let bytes = encode_pkg_event(&event).map_err(|_| String::from("encode"))?;
    let line = chain.append(&bytes);
    let end = match vfs.stat(Id::ROOT, layout::LOG_FILE) {
        Ok(meta) => meta.size,
        Err(FsError::NotFound) => {
            vfs.create(Id::ROOT, layout::LOG_FILE, 0o644)
                .map_err(fs_error)?;
            0
        }
        Err(error) => return Err(fs_error(error)),
    };
    let written = vfs
        .write(Id::ROOT, layout::LOG_FILE, end, line.as_bytes())
        .map_err(fs_error)?;
    check!(written == line.len(), "a short audit append");
    Ok(())
}

fn empty(fs: &mut VfsTree, dir: &str) -> Result<bool, String> {
    Ok(fs.list(dir).map_err(fs_error)?.is_empty())
}

fn readme(vfs: &mut Vfs) -> Result<Vec<u8>, String> {
    vfs.read_file(Id::ROOT, &format!("{DOCS}/README.md"))
        .map_err(fs_error)
}

/// An install puts the program 0755 and the docs 0644 under `/docs/apps`;
/// an upgrade replaces both; a remove leaves `/apps` and `/docs/apps` empty
/// and the volume consistent.
pub fn pkg_tree_install_upgrade_remove() -> Result<(), String> {
    task::register_kernel();
    let disk = volume()?;
    let (fs, vfs) = remount_disk(disk)?;
    let mut tree_fs = VfsTree { vfs };
    install(&mut tree_fs, &Sample(1), None)?;
    let program = format!("{}/bin/counter.elf", Sample(1).install_path());
    let mode = tree_fs.vfs.stat(Id::ROOT, &program).map_err(fs_error)?.mode & 0o777;
    check!(mode == 0o755, "the program is {mode:o}");
    check!(
        readme(&mut tree_fs.vfs)?.starts_with(b"docs/README.md v1"),
        "v1 docs missing"
    );
    install(&mut tree_fs, &Sample(2), Some(&Sample(1)))?;
    check!(
        readme(&mut tree_fs.vfs)?.starts_with(b"docs/README.md v2"),
        "v2 docs missing"
    );
    check!(
        tree_fs
            .vfs
            .stat(Id::ROOT, &Sample(1).install_path())
            .is_err(),
        "the old version stayed"
    );
    remove(&mut tree_fs, &Sample(2))?;
    check!(empty(&mut tree_fs, "/apps")?, "/apps is not empty");
    check!(
        empty(&mut tree_fs, "/docs/apps")?,
        "/docs/apps is not empty"
    );
    fs.flush().map_err(fs_error)?;
    drop((fs, tree_fs));
    check_volume(disk, BLOCKS)?;
    release(disk);
    Ok(())
}

/// A stop between the two renames of a docs replacement (live already set
/// aside, the complete copy not yet renamed) is repaired after a remount: the
/// new copy becomes live and no `~` copy remains.
pub fn pkg_tree_docs_repair_after_remount() -> Result<(), String> {
    task::register_kernel();
    let disk = volume()?;
    let (fs, vfs) = remount_disk(disk)?;
    let mut tree_fs = VfsTree { vfs };
    install(&mut tree_fs, &Sample(1), None)?;
    tree::stage_docs(&mut tree_fs, &Sample(2), SYSTEM_NAME).map_err(tree_error)?;
    let retired = pkgstore::docs::retired_dir(SYSTEM_NAME).map_err(|e| format!("{e}"))?;
    tree_fs
        .vfs
        .rename(Id::ROOT, DOCS, &retired)
        .map_err(fs_error)?;
    fs.flush().map_err(fs_error)?;
    drop((fs, tree_fs));

    let (fs, vfs) = remount_disk(disk)?;
    let mut tree_fs = VfsTree { vfs };
    let repairs = tree::repair_docs(&mut tree_fs).map_err(tree_error)?;
    check!(repairs == 2, "{repairs} repairs");
    check!(
        readme(&mut tree_fs.vfs)?.starts_with(b"docs/README.md v2"),
        "the new docs were lost"
    );
    let names = tree_fs.list("/docs/apps").map_err(fs_error)?;
    check!(names == [SYSTEM_NAME], "left in /docs/apps: {names:?}");
    fs.flush().map_err(fs_error)?;
    drop((fs, tree_fs));
    check_volume(disk, BLOCKS)?;
    release(disk);
    Ok(())
}

/// Soak: 1 000 install / upgrade / remove cycles with a remount every 100.
/// `/apps` and `/docs/apps` end empty, `pkg.log` verifies with all 3 000
/// records, the inode count returns to its baseline after every cycle (no
/// orphan or leaked inode), the orphan list is empty, and the bitmaps and
/// counters agree at every remount.
pub fn pkg_tree_soak_1000_cycles() -> Result<(), String> {
    task::register_kernel();
    let disk = volume()?;
    // The disk lives in the kernel heap: give it back on failure too.
    let outcome = soak(disk);
    release(disk);
    outcome
}

fn soak(disk: &'static FakeDisk) -> Result<(), String> {
    let mut chain = Chain::default();
    let mut baseline = None;
    for first in (0..CYCLES).step_by(100) {
        let (fs, vfs) = remount_disk(disk)?;
        let mut tree_fs = VfsTree { vfs };
        for cycle in first..first + 100 {
            let (old, new) = (Sample(cycle * 2 + 1), Sample(cycle * 2 + 2));
            install(&mut tree_fs, &old, None)?;
            audit(&mut tree_fs.vfs, &mut chain, "install", &old)?;
            install(&mut tree_fs, &new, Some(&old))?;
            audit(&mut tree_fs.vfs, &mut chain, "install", &new)?;
            remove(&mut tree_fs, &new)?;
            audit(&mut tree_fs.vfs, &mut chain, "remove", &new)?;
            fs.flush().map_err(fs_error)?;
            let inodes = bitmap_free(disk, BLOCKS).1;
            match baseline {
                None => baseline = Some(inodes),
                Some(first) => check!(
                    inodes == first,
                    "cycle {cycle}: free inodes {first} -> {inodes}"
                ),
            }
        }
        drop((fs, tree_fs));
        check_volume(disk, BLOCKS)?;
        check!(
            last_orphan(disk) == 0,
            "cycle {first}: the orphan list is not empty"
        );
    }
    let (_fs, vfs) = remount_disk(disk)?;
    let mut tree_fs = VfsTree { vfs };
    check!(empty(&mut tree_fs, "/apps")?, "/apps is not empty");
    check!(
        empty(&mut tree_fs, "/docs/apps")?,
        "/docs/apps is not empty"
    );
    let verified = verify_log(&mut tree_fs.vfs)?;
    check!(
        verified.count == u64::from(CYCLES) * 3 && verified == chain,
        "pkg.log holds {} records",
        verified.count
    );
    Ok(())
}

/// Verify `pkg.log` in 16 KiB pieces of whole lines (`audit::verify_from`):
/// the suite's heap is too fragmented after the other soaks to hold the
/// whole ~600 KiB log in one allocation.
fn verify_log(vfs: &mut Vfs) -> Result<Chain, String> {
    let mut chain = Chain::default();
    let mut offset = 0u64;
    let mut pending: Vec<u8> = Vec::new();
    let mut piece = vec![0u8; 16 * 1024];
    loop {
        let read = vfs
            .read(Id::ROOT, layout::LOG_FILE, offset, &mut piece)
            .map_err(fs_error)?;
        if read == 0 {
            break;
        }
        offset += read as u64;
        pending.extend_from_slice(&piece[..read]);
        let Some(end) = pending.iter().rposition(|byte| *byte == b'\n') else {
            continue;
        };
        let text = core::str::from_utf8(&pending[..=end])
            .map_err(|_| String::from("pkg.log is not UTF-8"))?;
        chain = verify_from(chain, text).map_err(|error| format!("pkg.log: {error:?}"))?;
        pending.drain(..=end);
    }
    check!(pending.is_empty(), "pkg.log ends in a torn record");
    Ok(chain)
}
