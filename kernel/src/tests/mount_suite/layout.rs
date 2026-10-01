//! The two mount layouts chosen by `fs::mounts::build`.

use super::*;
use crate::block::BlockDevice;
use crate::fs::mounts::{self, Tables};
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{FsError, Id, MountFlags, Vfs};
use alloc::sync::Arc;
use core::sync::atomic::Ordering;

fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

fn points(vfs: &Vfs) -> Vec<String> {
    vfs.mounts().into_iter().map(|(point, _)| point).collect()
}

fn build(devices: &[&'static dyn BlockDevice]) -> Tables {
    mounts::build(devices)
}

/// Boot, root, home: the order a probe would meet them.
fn devices(rig: &Rig) -> [&'static dyn BlockDevice; 3] {
    [rig.boot, rig.root, rig.home]
}

fn config(extra: &str) -> String {
    format!("root=UUID={}\n{extra}", uuid(0xA1).1)
}

const LEGACY: [&str; 3] = ["/", "/tmp", "/data"];

/// No `lazyos.cfg`: FAT at `/`, ramfs at `/tmp`, ext2 at `/data`, with the ABI
/// root an overlay, exactly as before F1.
pub fn legacy_without_config() -> Result<(), String> {
    let rig = rig(None);
    let tables = build(&[rig.boot, rig.home]);
    check!(tables.mounted, "no root mounted");
    check!(
        points(&tables.native) == LEGACY,
        "native {:?}",
        points(&tables.native)
    );
    check!(
        points(&tables.abi) == LEGACY,
        "abi {:?}",
        points(&tables.abi)
    );
    let abi_root = tables.abi.mounts()[0].1;
    check!(abi_root == "overlay (abi rw)", "abi root is {abi_root}");
    check!(
        tables.native.mount_flags("/tmp/x") == MountFlags::default(),
        "legacy mounts carry flags"
    );
    Ok(())
}

/// A configured root that is not on any device falls back to the same layout,
/// as does a malformed config.
pub fn legacy_with_unknown_root() -> Result<(), String> {
    let rig = rig(Some(&format!("root=UUID={}\n", uuid(9).1)));
    let tables = build(&[rig.boot, rig.home]);
    check!(tables.mounted, "no root mounted");
    check!(
        points(&tables.native) == LEGACY,
        "native {:?}",
        points(&tables.native)
    );
    check!(
        points(&tables.abi) == LEGACY,
        "abi {:?}",
        points(&tables.abi)
    );
    let rig = self::rig(Some("root=UUID=short\n"));
    let tables = build(&[rig.boot, rig.home]);
    check!(
        points(&tables.native) == LEGACY,
        "malformed: {:?}",
        points(&tables.native)
    );
    Ok(())
}

/// The configured layout: ext2 `/`, read-only `/boot`, one ramfs at `/tmp` and
/// `/transient`, and a home volume, in both tables, with no `/data`.
pub fn configured_layout() -> Result<(), String> {
    let rig = rig(Some(&config("home=LABEL=home\nroot_flags=noexec\n")));
    let mut tables = build(&devices(rig));
    let want = ["/", "/boot", "/transient", "/tmp", "/home"];
    check!(tables.mounted, "no root mounted");
    check!(
        points(&tables.native) == want,
        "native {:?}",
        points(&tables.native)
    );
    check!(points(&tables.abi) == want, "abi {:?}", points(&tables.abi));
    check!(
        tables.abi.mounts()[0].1.starts_with("ext2"),
        "abi root {:?}",
        tables.abi.mounts()[0]
    );

    let locked = MountFlags {
        ro: true,
        noexec: true,
        nosuid: true,
    };
    let rootf = MountFlags {
        ro: false,
        noexec: true,
        nosuid: false,
    };
    let homef = MountFlags {
        ro: false,
        noexec: false,
        nosuid: true,
    };
    for table in [&tables.native, &tables.abi] {
        check!(table.mount_flags("/boot/INIT.ELF") == locked, "boot flags");
        check!(table.mount_flags("/etc/x") == rootf, "root flags");
        check!(table.mount_flags("/home/a") == homef, "home flags");
        check!(
            table.mount_flags("/tmp/a") == MountFlags::default(),
            "tmp flags"
        );
    }

    let id = Id::ROOT;
    let native = &mut tables.native;
    check!(
        native.create(id, "/boot/x", 0o644).map(|_| ()) == Err(FsError::ReadOnly),
        "/boot is writable"
    );
    native.create(id, "/hello", 0o644).map_err(fs_error)?;
    native.create(id, "/home/note", 0o644).map_err(fs_error)?;
    native.create(id, "/tmp/shared", 0o644).map_err(fs_error)?;
    check!(
        native.stat(id, "/transient/shared").is_ok(),
        "/tmp and /transient differ"
    );
    let mode = native.stat(id, "/tmp").map_err(fs_error)?.mode & 0o7777;
    check!(mode == 0o1777, "scratch root mode {mode:o}");
    let listing: Vec<String> = native
        .readdir(id, "/")
        .map_err(fs_error)?
        .into_iter()
        .map(|e| e.name)
        .collect();
    for name in ["boot", "transient", "tmp", "home", "hello"] {
        check!(
            listing.iter().filter(|n| *n == name).count() == 1,
            "/ lacks {name}: {listing:?}"
        );
    }
    check!(
        !listing.iter().any(|n| n == "data"),
        "a /data appeared: {listing:?}"
    );

    // Shutdown flushes every volume that was written.
    let (root_flushes, home_flushes) = (
        rig.root.flushes.load(Ordering::Relaxed),
        rig.home.flushes.load(Ordering::Relaxed),
    );
    native.sync_all().map_err(fs_error)?;
    check!(
        rig.root.flushes.load(Ordering::Relaxed) > root_flushes,
        "root not flushed"
    );
    check!(
        rig.home.flushes.load(Ordering::Relaxed) > home_flushes,
        "home not flushed"
    );
    Ok(())
}

/// A missing home volume is only a log line: `/home` stays a plain directory,
/// and the root volume is never taken for it.
pub fn missing_home() -> Result<(), String> {
    let want = ["/", "/boot", "/transient", "/tmp"];
    for home in ["LABEL=nothere", "LABEL=rootfs"] {
        let rig = rig(Some(&config(&format!("home={home}\n"))));
        let tables = build(&devices(rig));
        check!(
            points(&tables.native) == want,
            "home={home}: {:?}",
            points(&tables.native)
        );
    }
    Ok(())
}

/// An unwritable root device mounts `ro` at the VFS, not just inside ext2.
pub fn readonly_device_root() -> Result<(), String> {
    let rig = rig(Some(&config("")));
    rig.root.set_read_only(true);
    let mut tables = build(&devices(rig));
    let flagged = tables.native.mount_flags("/x").ro;
    let created = tables.native.create(Id::ROOT, "/x", 0o644).map(|_| ());
    rig.root.set_read_only(false);
    check!(flagged, "a read-only device mounted rw");
    check!(
        created == Err(FsError::ReadOnly),
        "created a file on a read-only root"
    );
    Ok(())
}

/// 10k mount, lookup and readdir cycles across mount boundaries leave the heap
/// where they found it.
pub fn mount_cycles_soak() -> Result<(), String> {
    let id = Id::ROOT;
    let cycle = || -> Result<(), String> {
        let mut vfs = Vfs::new();
        let ro = MountFlags {
            ro: true,
            noexec: true,
            nosuid: true,
        };
        vfs.mount("/", Arc::new(RamFs::new()), MountFlags::default())
            .map_err(fs_error)?;
        let boot = Arc::new(RamFs::new());
        crate::fs::vfs::Filesystem::create(&*boot, "INIT.ELF", 0o755, id).map_err(fs_error)?;
        vfs.mount("/boot", boot, ro).map_err(fs_error)?;
        vfs.mount("/home", Arc::new(RamFs::new()), MountFlags::default())
            .map_err(fs_error)?;
        check!(
            vfs.stat(id, "/boot/INIT.ELF").is_ok(),
            "stat across the boundary"
        );
        check!(
            vfs.readdir(id, "/").map_err(fs_error)?.len() == 2,
            "listing of /"
        );
        check!(vfs.mount_flags("/boot/INIT.ELF").noexec, "flags");
        check!(
            vfs.create(id, "/boot/y", 0o644).map(|_| ()) == Err(FsError::ReadOnly),
            "ro create"
        );
        Ok(())
    };
    cycle()?; // warm any lazily grown allocations
    let measure = || crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used;
    let before = measure();
    for _ in 0..10_000 {
        cycle()?;
    }
    let after = measure();
    check!(
        after <= before + 16 * 1024,
        "heap grew from {before} to {after}"
    );
    Ok(())
}
