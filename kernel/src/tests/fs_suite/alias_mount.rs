//! One filesystem mounted at two points (`/tmp` and `/transient` are one
//! ramfs, audit H6): the VFS caches and the open-file registry treat the two
//! names as one file. Keyed by mount, a `chmod 600 /tmp/x` left `/transient/x`
//! cached as 0644 forever (ramfs never expires a cache entry), and a file open
//! under one name was a different file from the same one open under the other.

use super::*;
use crate::fs::openfile::{self, OpenFile};
use crate::fs::vfs::{AttrRequest, Filesystem, MountFlags};

const BOB: Id = Id::new(2000, 200);

/// Ramfs at `/`, and one more ramfs instance mounted at both `/tmp` and
/// `/transient` (the layout `fs::mounts` builds).
fn aliased_vfs() -> Result<Vfs, String> {
    let mut vfs = ram_vfs();
    let scratch: Arc<dyn Filesystem> = Arc::new(RamFs::new());
    for point in ["/transient", "/tmp"] {
        vfs.mount(point, Arc::clone(&scratch), MountFlags::default())
            .map_err(fs_error)?;
    }
    Ok(vfs)
}

/// Names of one file give one answer, whichever name asked first: attributes
/// changed through one name show through the other at once, and so do sizes,
/// renames and removals.
pub fn alias_mount_caches_agree() -> Result<(), String> {
    let root = Id::ROOT;
    let mut vfs = aliased_vfs()?;
    vfs.create(root, "/tmp/x", 0o644).map_err(fs_error)?;

    // Warm the second name's cache while the file is world-readable.
    check!(
        vfs.check(BOB, "/transient/x", vfs::READ).is_ok(),
        "another user cannot read a 0644 file"
    );
    vfs.setattr(root, "/tmp/x", AttrRequest::Mode(0o600))
        .map_err(fs_error)?;
    check!(
        vfs.check(BOB, "/transient/x", vfs::READ).err() == Some(FsError::Access),
        "chmod 600 through /tmp left /transient/x readable by another user"
    );
    check!(
        vfs.stat(root, "/transient/x").map_err(fs_error)?.mode & 0o777 == 0o600,
        "the mode seen through /transient is stale"
    );

    vfs.write(root, "/transient/x", 0, b"12345")
        .map_err(fs_error)?;
    check!(
        vfs.stat(root, "/tmp/x").map_err(fs_error)?.size == 5,
        "a write through /transient is not in the size seen through /tmp"
    );

    vfs.rename(root, "/tmp/x", "/tmp/y").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/transient/x").err() == Some(FsError::NotFound),
        "the old name still resolves through the other mount"
    );
    vfs.unlink(root, "/transient/y").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/tmp/y").err() == Some(FsError::NotFound),
        "an unlinked file still resolves through the other mount"
    );
    check!(
        vfs.canonical_path("/tmp/dir/f") == "/transient/dir/f"
            && vfs.canonical_path("/transient") == "/transient"
            && vfs.canonical_path("/tmp") == "/transient"
            && vfs.canonical_path("/other") == "/other",
        "canonical names: {:?} {:?} {:?}",
        vfs.canonical_path("/tmp/dir/f"),
        vfs.canonical_path("/tmp"),
        vfs.canonical_path("/other")
    );
    Ok(())
}

/// Soak: 2000 chmod flips through one name are never stale through the other.
pub fn alias_mount_cache_soak() -> Result<(), String> {
    let root = Id::ROOT;
    let mut vfs = aliased_vfs()?;
    vfs.create(root, "/tmp/f", 0o644).map_err(fs_error)?;
    for round in 0..2000u32 {
        let (through, asked) = if round % 2 == 0 {
            ("/tmp/f", "/transient/f")
        } else {
            ("/transient/f", "/tmp/f")
        };
        let mode = if round % 4 < 2 { 0o600 } else { 0o644 };
        vfs.setattr(root, through, AttrRequest::Mode(mode))
            .map_err(fs_error)?;
        let readable = vfs.check(BOB, asked, vfs::READ).is_ok();
        check!(
            readable == (mode == 0o644),
            "round {round}: mode {mode:o} through {through}, readable {readable} through {asked}"
        );
    }
    Ok(())
}

/// The open-file registry keys a file by one name: opening it under both
/// names is one open file, an unlink under either name parks it for every
/// descriptor, and a name recreated afterwards is not the old descriptors'.
pub fn alias_mount_open_files_are_one_file() -> Result<(), String> {
    let mut vfs = aliased_vfs()?;
    vfs.create(Id::ROOT, "/tmp/h6", 0o644).map_err(fs_error)?;
    vfs.write(Id::ROOT, "/tmp/h6", 0, b"first")
        .map_err(fs_error)?;
    let previous = crate::fs::install_abi_for_test(vfs);
    let outcome = open_files_case();
    crate::fs::restore_abi_for_test(previous);
    outcome
}

fn open_files_case() -> Result<(), String> {
    let before = openfile::open_files();
    let by_tmp = OpenFile::open("/tmp/h6", true, true, false).map_err(fs_error)?;
    let by_transient = OpenFile::open("/transient/h6", true, true, false).map_err(fs_error)?;
    check!(
        openfile::open_files() == before + 1,
        "one file open under two names is {} registry entries",
        openfile::open_files() - before
    );
    check!(
        by_tmp.path() == by_transient.path(),
        "the two descriptions disagree on the name: {:?} {:?}",
        by_tmp.path(),
        by_transient.path()
    );

    // A rename onto its own other name is not a replacement: the file stays
    // where it is instead of being parked as if it were being overwritten.
    let _ = crate::fs::abi_rename(Id::ROOT, "/tmp/h6", "/transient/h6");
    check!(
        crate::fs::abi_stat(Id::ROOT, "/tmp/h6").is_ok() && !by_tmp.path().contains(".unlinked"),
        "a rename onto the file's other name took the file away: {:?}",
        by_tmp.path()
    );

    // Unlink under one name while it is open under the other.
    crate::fs::abi_unlink(Id::ROOT, "/transient/h6").map_err(fs_error)?;
    check!(
        crate::fs::abi_stat(Id::ROOT, "/tmp/h6").err() == Some(FsError::NotFound),
        "the unlinked name still resolves"
    );
    // The name is reused by a new file: the open descriptors keep the old one.
    crate::fs::abi_create(Id::ROOT, "/tmp/h6", 0o600).map_err(fs_error)?;
    crate::fs::abi_write(Id::ROOT, "/tmp/h6", 0, b"other").map_err(fs_error)?;
    let mut buf = [0u8; 8];
    let read = by_tmp.read_at(0, &mut buf).map_err(fs_error)?;
    check!(
        &buf[..read] == b"first",
        "a descriptor follows a recycled name: read {:?}",
        &buf[..read]
    );
    let read = by_transient.read_at(0, &mut buf).map_err(fs_error)?;
    check!(
        &buf[..read] == b"first",
        "the other description follows a recycled name: read {:?}",
        &buf[..read]
    );
    drop(by_tmp);
    drop(by_transient);
    crate::fs::abi_unlink(Id::ROOT, "/tmp/h6").map_err(fs_error)?;
    check!(
        openfile::open_files() == before,
        "the registry kept {} files",
        openfile::open_files() - before
    );
    Ok(())
}

/// Unmounting shifts the later mounts' indices: the cache keys follow the
/// instances, not a stored index (a stored one made `canonical_path` index
/// past the table and let one mount's cache answer for another).
pub fn alias_mount_survives_unmount() -> Result<(), String> {
    let root = Id::ROOT;
    let mut vfs = ram_vfs();
    let fresh = || -> Arc<dyn Filesystem> { Arc::new(RamFs::new()) };
    vfs.mkdir(root, "/mnt", 0o755).map_err(fs_error)?;
    for point in ["/mnt/a", "/mnt/b"] {
        vfs.mkdir(root, point, 0o755).map_err(fs_error)?;
        vfs.mount(point, fresh(), MountFlags::default())
            .map_err(fs_error)?;
    }
    vfs.create(root, "/mnt/b/x", 0o644).map_err(fs_error)?;
    vfs.unmount("/mnt/a").map_err(fs_error)?;
    check!(
        vfs.canonical_path("/mnt/b/x") == "/mnt/b/x",
        "after an unmount a mount is named {:?}",
        vfs.canonical_path("/mnt/b/x")
    );
    check!(
        vfs.stat(root, "/mnt/b/x").is_ok(),
        "a surviving mount lost its file"
    );
    vfs.mkdir(root, "/mnt/c", 0o755).map_err(fs_error)?;
    vfs.mount("/mnt/c", fresh(), MountFlags::default())
        .map_err(fs_error)?;
    vfs.create(root, "/mnt/c/y", 0o600).map_err(fs_error)?;
    check!(
        vfs.stat(root, "/mnt/c/x").err() == Some(FsError::NotFound)
            && vfs.stat(root, "/mnt/b/y").err() == Some(FsError::NotFound),
        "two mounts answer for each other's names"
    );
    check!(
        vfs.canonical_path("/mnt/c/y") == "/mnt/c/y",
        "a new mount is named {:?}",
        vfs.canonical_path("/mnt/c/y")
    );

    // The same with the alias: the first name goes, the second keeps working.
    let mut vfs = aliased_vfs()?;
    vfs.create(root, "/tmp/x", 0o644).map_err(fs_error)?;
    vfs.unmount("/transient").map_err(fs_error)?;
    check!(
        vfs.canonical_path("/tmp/x") == "/tmp/x" && vfs.stat(root, "/tmp/x").is_ok(),
        "the surviving name of an alias is {:?}",
        vfs.canonical_path("/tmp/x")
    );
    Ok(())
}
