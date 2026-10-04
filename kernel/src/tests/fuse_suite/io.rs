//! The working path: bytes and trees round-trip through both mount tables,
//! open files are read by node, unmounting, a read-only mount, and a soak.

use super::*;
use crate::fs::openfile::OpenFile;
use crate::fs::vfs::{FileKind, READ};
use crate::fs::{self as vfsapi, nodes};
use fused::wire::Op;

const ROOT: Id = Id::ROOT;

pub fn roundtrip_both_tables() -> Result<(), String> {
    run(Mode::Normal, |_| {
        let mounts = vfsapi::abi_mounts();
        check!(
            mounts.iter().any(|(point, name)| point == "/mnt/t" && *name == "fuse"),
            "not in the ABI table: {mounts:?}"
        );
        check!(
            vfsapi::abi_mount_flags("/mnt/t/x").nosuid,
            "the mount is not nosuid"
        );
        vfsapi::abi_create(ROOT, "/mnt/t/data.bin", 0o644).map_err(|e| format!("create: {e:?}"))?;
        // Longer than three requests' worth: split, then reassembled.
        let body = pattern(200_000, 7);
        let wrote = vfsapi::abi_write(ROOT, "/mnt/t/data.bin", 0, &body);
        check!(wrote == Ok(body.len()), "write: {wrote:?}");
        let meta = vfsapi::abi_stat(ROOT, "/mnt/t/data.bin").map_err(|e| format!("stat: {e:?}"))?;
        check!(
            meta.size == body.len() as u64 && meta.kind == FileKind::File,
            "stat after write: {meta:?}"
        );
        let back = vfsapi::abi_read(ROOT, "/mnt/t/data.bin").map_err(|e| format!("read: {e:?}"))?;
        check!(back == body, "ABI read-back differs ({} bytes)", back.len());
        // The native table sees the same daemon.
        let native = vfsapi::vfs_read(ROOT, "/mnt/t/data.bin").map_err(|e| format!("native: {e:?}"))?;
        check!(native == body, "native read-back differs");
        let mut mid = [0u8; 5000];
        let got = vfsapi::abi_read_at(ROOT, "/mnt/t/data.bin", 65_000, &mut mid);
        check!(got == Ok(5000), "read across a chunk edge: {got:?}");
        check!(mid[..] == body[65_000..70_000], "chunk-edge bytes differ");
        let past = vfsapi::abi_read_at(ROOT, "/mnt/t/data.bin", 1 << 40, &mut mid);
        check!(past == Ok(0), "read past the end: {past:?}");
        // `/mnt` lists the mount point itself.
        let listed = vfsapi::abi_readdir(ROOT, "/mnt").map_err(|e| format!("ls /mnt: {e:?}"))?;
        check!(listed.iter().any(|e| e.name == "t" && e.kind == FileKind::Dir), "ls /mnt: {listed:?}");
        check!(with_fake(|f| f.fs.used()) == body.len(), "the daemon holds other bytes");
        Ok(())
    })
}

pub fn tree_operations() -> Result<(), String> {
    run(Mode::Normal, |_| {
        let at = |path: &str| alloc::format!("/mnt/t/{path}");
        vfsapi::abi_mkdir(ROOT, &at("dir"), 0o750).map_err(|e| format!("mkdir: {e:?}"))?;
        vfsapi::abi_create(ROOT, &at("dir/a.txt"), 0o600).map_err(|e| format!("create: {e:?}"))?;
        vfsapi::abi_write(ROOT, &at("dir/a.txt"), 0, b"hello").map_err(|e| format!("write: {e:?}"))?;
        let meta = vfsapi::abi_stat(ROOT, &at("dir")).map_err(|e| format!("stat dir: {e:?}"))?;
        check!(meta.kind == FileKind::Dir && meta.mode & 0o777 == 0o750, "dir: {meta:?}");
        let file = vfsapi::abi_stat(ROOT, &at("dir/a.txt")).map_err(|e| format!("stat: {e:?}"))?;
        check!(file.uid == 0 && file.mode & 0o777 == 0o600, "owner/mode: {file:?}");
        expect(vfsapi::abi_rmdir(ROOT, &at("dir")), FsError::NotEmpty, "rmdir non-empty")?;
        expect(vfsapi::abi_mkdir(ROOT, &at("dir"), 0o755), FsError::Exists, "mkdir twice")?;
        vfsapi::abi_rename(ROOT, &at("dir/a.txt"), &at("b.txt")).map_err(|e| format!("rename: {e:?}"))?;
        expect(vfsapi::abi_stat(ROOT, &at("dir/a.txt")), FsError::NotFound, "old name")?;
        let moved = vfsapi::abi_read(ROOT, &at("b.txt")).map_err(|e| format!("read moved: {e:?}"))?;
        check!(moved == b"hello", "moved bytes: {moved:?}");
        vfsapi::abi_truncate(ROOT, &at("b.txt"), 2).map_err(|e| format!("truncate: {e:?}"))?;
        check!(vfsapi::abi_read(ROOT, &at("b.txt")) == Ok(b"he".to_vec()), "truncated bytes");
        let names: Vec<String> = vfsapi::abi_readdir(ROOT, "/mnt/t")
            .map_err(|e| format!("readdir: {e:?}"))?
            .into_iter()
            .map(|e| e.name)
            .collect();
        check!(names == ["b.txt", "dir"], "listing: {names:?}");
        let figures = vfsapi::abi_statfs(ROOT, "/mnt/t").map_err(|e| format!("statfs: {e:?}"))?;
        check!(
            u64::from(figures.magic) == fused::memfs::MAGIC && figures.block_size == 4096,
            "statfs: {figures:?}"
        );
        // A chmod reaches the daemon and comes back in the metadata. (Each
        // mount table caches on its own: a change made through the native
        // table is not seen by the ABI table's cache, docs/smb-plan.md §3.1.)
        vfsapi::abi_setattr(ROOT, &at("b.txt"), crate::fs::vfs::AttrRequest::Mode(0o640))
            .map_err(|e| format!("chmod: {e:?}"))?;
        let mode = vfsapi::abi_stat(ROOT, &at("b.txt")).map(|m| m.mode & 0o7777);
        check!(mode == Ok(0o640), "mode after chmod: {mode:?}");
        vfsapi::abi_flush(ROOT, &at("b.txt")).map_err(|e| format!("flush: {e:?}"))?;
        check!(
            with_fake(|f| f.ops.contains(&(Op::Flush as u64))),
            "the flush did not reach the daemon"
        );
        vfsapi::abi_unlink(ROOT, &at("b.txt")).map_err(|e| format!("unlink: {e:?}"))?;
        vfsapi::abi_rmdir(ROOT, &at("dir")).map_err(|e| format!("rmdir: {e:?}"))?;
        check!(with_fake(|f| f.fs.node_count()) == 1, "nodes left behind");
        // Mount roots do not move or go.
        // The daemon refuses to remove its root.
        expect(vfsapi::abi_rmdir(ROOT, "/mnt/t"), FsError::NotPermitted, "rmdir the mount")?;
        Ok(())
    })
}

pub fn nodes_follow_renames() -> Result<(), String> {
    run(Mode::Normal, |_| {
        vfsapi::abi_create(ROOT, "/mnt/t/f", 0o644).map_err(|e| format!("create: {e:?}"))?;
        vfsapi::abi_write(ROOT, "/mnt/t/f", 0, b"node bytes").map_err(|e| format!("write: {e:?}"))?;
        let node = nodes::abi_open_node(ROOT, "/mnt/t/f", READ)
            .map_err(|e| format!("open: {e:?}"))?
            .ok_or("the mount has no nodes")?;
        vfsapi::abi_rename(ROOT, "/mnt/t/f", "/mnt/t/g").map_err(|e| format!("rename: {e:?}"))?;
        let ops_before = with_fake(|f| f.served);
        let mut buf = [0u8; 32];
        let got = node.read(0, &mut buf);
        check!(got == Ok(10) && &buf[..10] == b"node bytes", "read by node: {got:?}");
        // By node is one request: no lookup of every ancestor.
        check!(with_fake(|f| f.served) == ops_before + 1, "a node read took several requests");
        check!(node.write(10, b"!") == Ok(1), "write by node");
        check!(node.stat().map(|m| m.size) == Ok(11), "stat by node");
        check!(node.truncate(4).is_ok(), "truncate by node");
        vfsapi::abi_unlink(ROOT, "/mnt/t/g").map_err(|e| format!("unlink: {e:?}"))?;
        expect(node.read(0, &mut buf), FsError::NotFound, "a node of a deleted file")?;
        Ok(())
    })
}

pub fn open_file_in_place() -> Result<(), String> {
    run(Mode::Normal, |_| {
        check!(vfsapi::abi_persistent("/mnt/t/x"), "a FUSE file would be snapshotted");
        vfsapi::abi_create(ROOT, "/mnt/t/log", 0o644).map_err(|e| format!("create: {e:?}"))?;
        let file = OpenFile::open("/mnt/t/log", true, true, false).map_err(|e| format!("open: {e:?}"))?;
        let body = pattern(100_000, 3);
        check!(file.write_at(0, &body) == Ok(body.len()), "write_at");
        // A second opener sees the first one's bytes at once (no snapshot).
        let other = OpenFile::open("/mnt/t/log", true, false, false).map_err(|e| format!("reopen: {e:?}"))?;
        let mut back = vec![0u8; body.len()];
        check!(other.read_at(0, &mut back) == Ok(body.len()), "read_at");
        check!(back == body, "the second opener saw other bytes");
        check!(file.stat().map(|m| m.size) == Ok(body.len() as u64), "fstat");
        check!(file.flush().is_ok(), "fsync");
        drop(file);
        drop(other);
        Ok(())
    })
}

pub fn large_directory() -> Result<(), String> {
    run(Mode::Normal, |_| {
        // 21 bytes per entry: 4000 entries take two batches and an empty one.
        const COUNT: usize = 4000;
        with_fake(|fake| -> Result<(), u64> {
            for n in 0..COUNT {
                let name = alloc::format!("entry-{n:05}");
                fused::daemon::FuseFs::create(&mut fake.fs, &name, 0o644, 0, 0)?;
            }
            Ok(())
        })
        .map_err(|e| format!("filling the tree: errno {e}"))?;
        let entries = vfsapi::abi_readdir(ROOT, "/mnt/t").map_err(|e| format!("readdir: {e:?}"))?;
        check!(entries.len() == COUNT, "{} entries listed", entries.len());
        check!(
            entries.first().map(|e| e.name.as_str()) == Some("entry-00000")
                && entries.last().map(|e| e.name.as_str()) == Some("entry-03999"),
            "listing out of order"
        );
        let readdirs = with_fake(|f| f.ops.iter().filter(|&&op| op == Op::ReadDir as u64).count());
        check!(readdirs >= 3, "the listing took {readdirs} requests");
        Ok(())
    })
}

pub fn read_only_mount() -> Result<(), String> {
    let ro = MountFlags {
        ro: true,
        ..MountFlags::default()
    };
    setup_named("ro", Mode::Normal, ro)?;
    let result = (|| {
        let served = with_fake(|f| f.served);
        expect(vfsapi::abi_create(ROOT, "/mnt/ro/x", 0o644), FsError::ReadOnly, "create")?;
        expect(vfsapi::abi_mkdir(ROOT, "/mnt/ro/d", 0o755), FsError::ReadOnly, "mkdir")?;
        // Refused by the VFS before any request beyond the lookups.
        let ops = with_fake(|f| f.ops[served as usize..].to_vec());
        check!(
            ops.iter().all(|&op| op == Op::Lookup as u64),
            "a mutation reached the daemon: {ops:?}"
        );
        check!(vfsapi::abi_readdir(ROOT, "/mnt/ro").is_ok(), "reads still work");
        Ok(())
    })();
    teardown();
    result
}

pub fn unregister_and_remount() -> Result<(), String> {
    run(Mode::Normal, |index| {
        vfsapi::abi_create(ROOT, "/mnt/t/f", 0o644).map_err(|e| format!("create: {e:?}"))?;
        let node = nodes::abi_open_node(ROOT, "/mnt/t/f", READ)
            .map_err(|e| format!("open: {e:?}"))?
            .ok_or("no node")?;
        let owner = with_fake(|f| f.owner);
        check!(fuse::unregister(index, owner).is_ok(), "unregister");
        expect(vfsapi::abi_stat(ROOT, "/mnt/t/f"), FsError::NotFound, "ABI after unmount")?;
        expect(vfsapi::vfs_stat(ROOT, "/mnt/t"), FsError::NotFound, "native after unmount")?;
        check!(fuse::unregister(index, owner) == Err(FuseError::NotOwner), "unregister twice");
        // A new provider under the same name, likely in the same slot: the
        // old node must not reach it.
        let again = fuse::register(owner, "t", MountFlags::default()).map_err(|e| format!("{e:?}"))?;
        with_fake(|f| f.index = again);
        let mut buf = [0u8; 4];
        expect(node.read(0, &mut buf), FsError::Io, "an old node after remount")?;
        check!(vfsapi::abi_stat(ROOT, "/mnt/t").is_ok(), "the new mount answers");
        Ok(())
    })
}

pub fn stress() -> Result<(), String> {
    run(Mode::Normal, |index| {
        let owner = with_fake(|f| f.owner);
        for round in 0..1500usize {
            let path = alloc::format!("/mnt/t/s{}", round % 7);
            let len = (round * 7919) % 70_000 + 1;
            let body = pattern(len, round as u8);
            if vfsapi::abi_stat(ROOT, &path).is_err() {
                vfsapi::abi_create(ROOT, &path, 0o644).map_err(|e| format!("{round} create: {e:?}"))?;
            }
            vfsapi::abi_truncate(ROOT, &path, 0).map_err(|e| format!("{round} truncate: {e:?}"))?;
            check!(vfsapi::abi_write(ROOT, &path, 0, &body) == Ok(len), "{round}: write");
            let back = vfsapi::abi_read(ROOT, &path).map_err(|e| format!("{round} read: {e:?}"))?;
            check!(back == body, "{round}: bytes differ");
            if round % 5 == 4 {
                vfsapi::abi_unlink(ROOT, &path).map_err(|e| format!("{round} unlink: {e:?}"))?;
            }
        }
        for n in 0..7 {
            let _ = vfsapi::abi_unlink(ROOT, &alloc::format!("/mnt/t/s{n}"));
        }
        check!(with_fake(|f| (f.fs.used(), f.fs.node_count())) == (0, 1), "the tree leaked");
        let (stats, alive) = fuse::stats(index).ok_or("no stats")?;
        check!(alive && stats.timeouts == 0 && stats.stale == 0, "after the soak: {stats:?}");
        // Mount and unmount many generations of providers.
        check!(fuse::unregister(index, owner).is_ok(), "unregister");
        for generation in 0..200usize {
            let name = if generation % 2 == 0 { "a" } else { "b" };
            let id = fuse::register(owner, name, MountFlags::default())
                .map_err(|e| format!("generation {generation}: {e:?}"))?;
            with_fake(|f| f.index = id);
            let path = alloc::format!("/mnt/{name}");
            check!(vfsapi::abi_stat(ROOT, &path).is_ok(), "{generation}: no answer");
            check!(fuse::unregister(id, owner).is_ok(), "{generation}: unregister");
        }
        let live = (0..fuse::MAX_PROVIDERS).filter(|&id| fuse::stats(id).is_some()).count();
        check!(live == 0, "{live} slots still taken");
        let mounts = vfsapi::abi_mounts();
        check!(mounts.len() == 1, "mounts left: {mounts:?}");
        Ok(())
    })
}
