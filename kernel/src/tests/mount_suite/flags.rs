//! Mount flags: `ro` enforcement before the backend, reporting, mount points
//! in `readdir`/`stat`, and `/proc/mounts`.

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{
    AttrRequest, DirEntry, FileKind, Filesystem, FsError, Id, Meta, MountFlags, SetAttr, Vfs,
};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

const RO: MountFlags = MountFlags {
    ro: true,
    noexec: false,
    nosuid: false,
};
const LOCKED: MountFlags = MountFlags {
    ro: true,
    noexec: true,
    nosuid: true,
};
const NOSUID: MountFlags = MountFlags {
    ro: false,
    noexec: false,
    nosuid: true,
};

fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

/// A ramfs that counts the mutating calls that reach it.
struct Counting {
    inner: RamFs,
    mutations: AtomicU32,
}

impl Counting {
    fn new() -> Arc<Counting> {
        Arc::new(Counting {
            inner: RamFs::new(),
            mutations: AtomicU32::new(0),
        })
    }

    fn bump(&self) {
        self.mutations.fetch_add(1, Ordering::Relaxed);
    }

    fn mutations(&self) -> u32 {
        self.mutations.load(Ordering::Relaxed)
    }
}

impl Filesystem for Counting {
    fn name(&self) -> &'static str {
        "counting"
    }
    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        self.inner.lookup(path)
    }
    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        self.inner.read(path, offset, buf)
    }
    fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        self.bump();
        self.inner.write(path, offset, data)
    }
    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        self.bump();
        self.inner.truncate(path, size)
    }
    fn setattr(&self, path: &str, attr: &SetAttr) -> Result<Meta, FsError> {
        self.bump();
        self.inner.setattr(path, attr)
    }
    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        self.bump();
        self.inner.create(path, mode, owner)
    }
    fn mkdir(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        self.bump();
        self.inner.mkdir(path, mode, owner)
    }
    fn unlink(&self, path: &str) -> Result<(), FsError> {
        self.bump();
        self.inner.unlink(path)
    }
    fn rmdir(&self, path: &str) -> Result<(), FsError> {
        self.bump();
        self.inner.rmdir(path)
    }
    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        self.bump();
        self.inner.rename(from, to)
    }
    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        self.inner.readdir(path)
    }
}

/// `--flags` parse as comma lists and round-trip through `proc_suffix`.
pub fn flag_lists() -> Result<(), String> {
    check!(
        MountFlags::parse_list("") == Ok(MountFlags::default()),
        "empty list"
    );
    check!(
        MountFlags::parse_list("ro,noexec,nosuid") == Ok(LOCKED),
        "all three"
    );
    check!(MountFlags::parse_list("nosuid") == Ok(NOSUID), "one flag");
    check!(MountFlags::parse_list("ro,rw") == Err("rw"), "unknown flag");
    check!(
        LOCKED.proc_suffix() == ",noexec,nosuid",
        "suffix {:?}",
        LOCKED.proc_suffix()
    );
    Ok(())
}

/// Every mutating entry point on a `ro` mount answers `ReadOnly` and leaves
/// the backend untouched, while an rw mount of the same type is called.
pub fn ro_refuses_every_mutation() -> Result<(), String> {
    let root = Id::ROOT;
    let backend = Counting::new();
    backend.inner.create("f", 0o644, root).map_err(fs_error)?;
    backend.inner.mkdir("d", 0o755, root).map_err(fs_error)?;
    let writable = Counting::new();

    let mut vfs = Vfs::new();
    vfs.mount("/", backend.clone(), RO).map_err(fs_error)?;
    vfs.mount("/rw", writable.clone(), MountFlags::default())
        .map_err(fs_error)?;
    let before = backend.mutations();

    let refusals: [(&str, Result<(), FsError>); 8] = [
        ("write", vfs.write(root, "/f", 0, b"x").map(|_| ())),
        ("truncate", vfs.truncate(root, "/f", 0)),
        ("create", vfs.create(root, "/g", 0o644).map(|_| ())),
        ("mkdir", vfs.mkdir(root, "/e", 0o755).map(|_| ())),
        ("unlink", vfs.unlink(root, "/f")),
        ("rmdir", vfs.rmdir(root, "/d")),
        ("rename", vfs.rename(root, "/f", "/h")),
        (
            "setattr",
            vfs.setattr(root, "/f", AttrRequest::Mode(0o600))
                .map(|_| ()),
        ),
    ];
    for (what, result) in refusals {
        check!(
            result == Err(FsError::ReadOnly),
            "{what} on a ro mount gave {result:?}"
        );
    }
    check!(
        backend.mutations() == before,
        "a refused call reached the backend"
    );
    check!(
        vfs.read_file(root, "/f").is_ok(),
        "reads on a ro mount failed"
    );
    check!(vfs.stat(root, "/d").is_ok(), "stat on a ro mount failed");

    vfs.create(root, "/rw/g", 0o644).map_err(fs_error)?;
    check!(writable.mutations() == 1, "the rw mount was not called");
    Ok(())
}

/// `mount_flags` follows the longest mount-point prefix, by whole components.
pub fn flags_are_reported() -> Result<(), String> {
    let mut vfs = Vfs::new();
    vfs.mount("/", Arc::new(RamFs::new()), MountFlags::default())
        .map_err(fs_error)?;
    vfs.mount("/boot", Arc::new(RamFs::new()), LOCKED)
        .map_err(fs_error)?;
    vfs.mount("/home", Arc::new(RamFs::new()), NOSUID)
        .map_err(fs_error)?;
    check!(
        vfs.mount_flags("/boot/INIT.ELF") == LOCKED,
        "/boot/INIT.ELF"
    );
    check!(vfs.mount_flags("/boot") == LOCKED, "/boot itself");
    check!(
        vfs.mount_flags("/bootx/a") == MountFlags::default(),
        "/bootx is not /boot"
    );
    check!(vfs.mount_flags("/home/me/x") == NOSUID, "/home/me/x");
    check!(
        vfs.mount_flags("/etc/passwd") == MountFlags::default(),
        "/etc/passwd"
    );
    check!(vfs.mount_flags("/boot/INIT.ELF").noexec, "noexec on /boot");
    Ok(())
}

/// Mount points show up in their parent's listing exactly once, even when the
/// backend lists the directory itself, and `stat` of one is the mounted root.
pub fn readdir_lists_mount_points() -> Result<(), String> {
    let root = Id::ROOT;
    let mut vfs = Vfs::new();
    let base = Arc::new(RamFs::new());
    base.mkdir("listed", 0o755, root).map_err(fs_error)?;
    base.create("file", 0o644, root).map_err(fs_error)?;
    let listed = Arc::new(RamFs::new());
    let bare = Arc::new(RamFs::new());
    vfs.mount("/", base, MountFlags::default())
        .map_err(fs_error)?;
    vfs.mount("/listed", listed.clone(), MountFlags::default())
        .map_err(fs_error)?;
    vfs.mount("/bare", bare.clone(), MountFlags::default())
        .map_err(fs_error)?;
    vfs.mount("/bare/deep", Arc::new(RamFs::new()), MountFlags::default())
        .map_err(fs_error)?;

    let names: Vec<String> = vfs
        .readdir(root, "/")
        .map_err(fs_error)?
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    for want in ["listed", "bare", "file"] {
        check!(
            names.iter().filter(|n| *n == want).count() == 1,
            "{want} in {names:?}"
        );
    }
    check!(
        !names.iter().any(|n| n == "deep"),
        "a nested mount leaked into /: {names:?}"
    );
    let bare_entries = vfs.readdir(root, "/bare").map_err(fs_error)?;
    check!(
        bare_entries.iter().any(|e| e.name == "deep"),
        "deep missing from /bare"
    );

    let meta = vfs.stat(root, "/bare").map_err(fs_error)?;
    check!(
        meta.kind == FileKind::Dir,
        "a mount point is not a directory"
    );
    check!(
        meta.ino == bare.stat("").map_err(fs_error)?.ino,
        "stat is not the mounted root's"
    );
    let entry = vfs
        .readdir(root, "/")
        .map_err(fs_error)?
        .into_iter()
        .find(|e| e.name == "bare")
        .ok_or("bare vanished")?;
    check!(
        entry.kind == FileKind::Dir,
        "mount point listed as {:?}",
        entry.kind
    );
    Ok(())
}

/// `/proc/mounts` and `mountinfo` carry the flags.
pub fn proc_mounts_shows_flags() -> Result<(), String> {
    let mut table = Vfs::new();
    table
        .mount("/", Arc::new(RamFs::new()), MountFlags::default())
        .map_err(fs_error)?;
    table
        .mount("/boot", Arc::new(RamFs::new()), LOCKED)
        .map_err(fs_error)?;
    table
        .mount("/home", Arc::new(RamFs::new()), NOSUID)
        .map_err(fs_error)?;
    let previous = crate::fs::install_abi_for_test(table);
    let read = |path| {
        crate::process::linux::proc_file_for_test(path)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default()
    };
    let mounts = read("/proc/mounts");
    let info = read("/proc/self/mountinfo");
    crate::fs::restore_abi_for_test(previous);
    check!(
        mounts.contains("ramfs / ramfs rw 0 0\n"),
        "root line: {mounts}"
    );
    check!(
        mounts.contains("ramfs /boot ramfs ro,noexec,nosuid 0 0\n"),
        "boot line: {mounts}"
    );
    check!(
        mounts.contains("ramfs /home ramfs rw,nosuid 0 0\n"),
        "home line: {mounts}"
    );
    check!(
        info.contains(" /boot ro,noexec,nosuid - ramfs ramfs ro\n"),
        "mountinfo: {info}"
    );
    Ok(())
}
