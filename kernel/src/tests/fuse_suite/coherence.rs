//! The VFS cache over a daemon's tree (docs/smb-plan.md §3.1, F3): metadata
//! a daemon answered is believed for `fuse::ATTR_TICKS`, so a change made
//! behind the VFS (another client of a share) shows once that passes, and a
//! change through one mount table reaches the other's cache the same way.

use super::*;
use crate::fs::vfs::FileKind;
use crate::fs::{self as vfsapi};
use fused::daemon::{FuseFs as _, Target};

const ROOT: Id = Id::ROOT;

/// Change the daemon's tree directly, as another client of the share would.
fn behind(change: impl FnOnce(&mut MemFs) -> Result<(), u64>) -> Result<(), String> {
    with_fake(|fake| change(&mut fake.fs)).map_err(|e| format!("behind the VFS: errno {e}"))
}

fn size(path: &str) -> Result<u64, String> {
    vfsapi::abi_stat(ROOT, path)
        .map(|meta| meta.size)
        .map_err(|e| format!("stat {path}: {e:?}"))
}

pub fn remote_change_expires() -> Result<(), String> {
    run(Mode::Normal, |_| {
        vfsapi::abi_create(ROOT, "/mnt/t/f", 0o644).map_err(|e| format!("create: {e:?}"))?;
        check!(
            vfsapi::abi_write(ROOT, "/mnt/t/f", 0, b"abc") == Ok(3),
            "write"
        );
        check!(size("/mnt/t/f")? == 3, "size after write");
        behind(|fs| fs.write(Target::Path("f"), 0, b"0123456789").map(drop))?;
        // Within the lifetime the cache answers: the gap is bounded, not gone.
        check!(size("/mnt/t/f")? == 3, "the cache was not used");
        test_hook::advance(fuse::ATTR_TICKS);
        check!(size("/mnt/t/f")? == 10, "a remote write never showed");
        let back = vfsapi::abi_read(ROOT, "/mnt/t/f").map_err(|e| format!("read: {e:?}"))?;
        check!(back == b"0123456789", "read after expiry: {back:?}");

        // A file removed behind the VFS stops existing, and one added appears.
        behind(|fs| fs.unlink("f"))?;
        behind(|fs| fs.create("g", 0o600, DAEMON_UID, DAEMON_UID).map(drop))?;
        test_hook::advance(fuse::ATTR_TICKS);
        expect(
            vfsapi::abi_stat(ROOT, "/mnt/t/f"),
            FsError::NotFound,
            "a remote unlink",
        )?;
        check!(size("/mnt/t/g")? == 0, "a remote create");

        // A directory replaced by a file: nothing below it survives.
        behind(|fs| fs.mkdir("d", 0o755, DAEMON_UID, DAEMON_UID).map(drop))?;
        behind(|fs| {
            fs.create("d/inner", 0o644, DAEMON_UID, DAEMON_UID)
                .map(drop)
        })?;
        check!(size("/mnt/t/d/inner")? == 0, "the inner file");
        behind(|fs| fs.unlink("d/inner"))?;
        behind(|fs| fs.rmdir("d"))?;
        behind(|fs| fs.create("d", 0o644, DAEMON_UID, DAEMON_UID).map(drop))?;
        test_hook::advance(fuse::ATTR_TICKS);
        let meta = vfsapi::abi_stat(ROOT, "/mnt/t/d").map_err(|e| format!("stat d: {e:?}"))?;
        check!(meta.kind == FileKind::File, "d is still a directory");
        check!(
            vfsapi::abi_stat(ROOT, "/mnt/t/d/inner").is_err(),
            "a name below a file"
        );

        // A change through the native table reaches the ABI table's cache.
        check!(size("/mnt/t/g")? == 0, "g cached");
        vfsapi::vfs_setattr(ROOT, "/mnt/t/g", crate::fs::vfs::AttrRequest::Mode(0o640))
            .map_err(|e| format!("native chmod: {e:?}"))?;
        test_hook::advance(fuse::ATTR_TICKS);
        let mode = vfsapi::abi_stat(ROOT, "/mnt/t/g").map(|m| m.mode & 0o7777);
        check!(
            mode == Ok(0o640),
            "the ABI table missed a native chmod: {mode:?}"
        );
        let stats = vfsapi::abi_cache_stats_for_test().ok_or("no ABI table")?;
        check!(stats.expired > 0, "no entry expired: {stats:?}");
        Ok(())
    })
}

/// Other filesystems keep their entries until a change invalidates them: a
/// ramfs path stays cached however much time passes.
pub fn local_cache_does_not_expire() -> Result<(), String> {
    run(Mode::Normal, |_| {
        vfsapi::abi_mkdir(ROOT, "/local", 0o755).map_err(|e| format!("mkdir: {e:?}"))?;
        vfsapi::abi_stat(ROOT, "/local").map_err(|e| format!("stat: {e:?}"))?;
        let before = vfsapi::abi_cache_stats_for_test().ok_or("no ABI table")?;
        test_hook::advance(100 * fuse::ATTR_TICKS);
        vfsapi::abi_stat(ROOT, "/local").map_err(|e| format!("stat: {e:?}"))?;
        let after = vfsapi::abi_cache_stats_for_test().ok_or("no ABI table")?;
        check!(
            after.expired == before.expired && after.dentry_hits > before.dentry_hits,
            "a ramfs entry expired: {before:?} -> {after:?}"
        );
        Ok(())
    })
}

/// Thousands of remote changes, each seen once its lifetime passes, while
/// the cache keeps answering in between (and holds no more than it did).
pub fn expiry_soak() -> Result<(), String> {
    run(Mode::Normal, |index| {
        for n in 0..8 {
            vfsapi::abi_create(ROOT, &alloc::format!("/mnt/t/f{n}"), 0o644)
                .map_err(|e| format!("create f{n}: {e:?}"))?;
        }
        let served = with_fake(|f| f.served);
        for round in 0..2000usize {
            let name = alloc::format!("f{}", round % 8);
            let path = alloc::format!("/mnt/t/{name}");
            let len = (round * 131) % 4000 + 1;
            behind(|fs| {
                fs.truncate(Target::Path(&name), 0)?;
                fs.write(Target::Path(&name), 0, &pattern(len, round as u8))
                    .map(drop)
            })?;
            test_hook::advance(fuse::ATTR_TICKS);
            check!(size(&path)? == len as u64, "{round}: stale size");
            // Cached now: these ask the daemon nothing.
            let asked = with_fake(|f| f.served);
            for _ in 0..3 {
                check!(size(&path)? == len as u64, "{round}: cached size");
            }
            check!(with_fake(|f| f.served) == asked, "{round}: cache bypassed");
        }
        let stats = vfsapi::abi_cache_stats_for_test().ok_or("no ABI table")?;
        check!(stats.expired >= 2000, "too few expiries: {stats:?}");
        check!(
            with_fake(|f| f.served) - served < 2000 * 4,
            "the soak asked the daemon too often"
        );
        let (fuse_stats, alive) = fuse::stats(index).ok_or("no stats")?;
        check!(
            alive && fuse_stats.timeouts == 0,
            "after the soak: {fuse_stats:?}"
        );
        Ok(())
    })
}
