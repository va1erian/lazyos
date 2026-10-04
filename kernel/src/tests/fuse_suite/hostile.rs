//! Daemons that lie, stall or die: every failure is a clean error, the
//! caller's buffers are untouched, nothing hangs past its deadline, and a
//! dead provider leaves the mount tables.

use super::*;
use crate::fs as vfsapi;
use fused::wire::errno;

const ROOT: Id = Id::ROOT;

/// Make `/mnt/t/f` holding `bytes` while the daemon behaves.
fn file_with(bytes: &[u8]) -> Result<(), String> {
    mode(Mode::Normal);
    vfsapi::abi_create(ROOT, "/mnt/t/f", 0o644).map_err(|e| format!("create: {e:?}"))?;
    vfsapi::abi_write(ROOT, "/mnt/t/f", 0, bytes).map_err(|e| format!("write: {e:?}"))?;
    Ok(())
}

pub fn error_statuses() -> Result<(), String> {
    run(Mode::Normal, |_| {
        for (code, want) in [
            (errno::ENOENT, FsError::NotFound),
            (errno::ESTALE, FsError::NotFound),
            (errno::EACCES, FsError::Access),
            (errno::EPERM, FsError::NotPermitted),
            (errno::ENOSPC, FsError::NoSpace),
            (errno::EOPNOTSUPP, FsError::NotSupported),
            (errno::EIO, FsError::Io),
            (9999, FsError::Io),
            (u64::MAX, FsError::Io),
        ] {
            mode(Mode::Status(code));
            expect(vfsapi::abi_stat(ROOT, "/mnt/t/x"), want, &alloc::format!("errno {code}"))?;
        }
        // An error is an answer: the provider lives on.
        mode(Mode::Normal);
        check!(vfsapi::abi_stat(ROOT, "/mnt/t").is_ok(), "the daemon did not recover");
        Ok(())
    })
}

pub fn silent_daemon_dies() -> Result<(), String> {
    run(Mode::Normal, |index| {
        check!(vfsapi::abi_stat(ROOT, "/mnt/t").is_ok(), "first lookup");
        mode(Mode::Silent);
        // `/mnt/t/a` is not cached: each stat is a request that times out.
        expect(vfsapi::abi_stat(ROOT, "/mnt/t/a"), FsError::Io, "first timeout")?;
        let (stats, alive) = fuse::stats(index).ok_or("no stats")?;
        check!(alive && stats.timeouts == 1, "after one timeout: {stats:?}");
        expect(vfsapi::abi_stat(ROOT, "/mnt/t/b"), FsError::Io, "second timeout")?;
        let (stats, alive) = fuse::stats(index).ok_or("no stats")?;
        check!(!alive && stats.timeouts == 2, "after two timeouts: {stats:?} alive={alive}");
        // Dead: the next call fails without a request.
        let served = with_fake(|f| f.served);
        expect(vfsapi::abi_stat(ROOT, "/mnt/t/c"), FsError::Io, "a dead provider")?;
        check!(with_fake(|f| f.served) == served, "a dead provider was asked");
        // The flusher's reap takes it out of both tables.
        fuse::reap();
        expect(vfsapi::abi_stat(ROOT, "/mnt/t"), FsError::NotFound, "ABI after reap")?;
        expect(vfsapi::vfs_stat(ROOT, "/mnt/t"), FsError::NotFound, "native after reap")?;
        check!(fuse::stats(index).is_none(), "the slot was not freed");
        Ok(())
    })
}

pub fn death_mid_request() -> Result<(), String> {
    run(Mode::Normal, |index| {
        file_with(b"before")?;
        let node = vfsapi::nodes::abi_open_node(ROOT, "/mnt/t/f", crate::fs::vfs::READ)
            .map_err(|e| format!("open: {e:?}"))?
            .ok_or("no node")?;
        mode(Mode::DieAfterTake);
        let mut buf = [0x5Au8; 8];
        expect(node.read(0, &mut buf), FsError::Io, "read as the daemon dies")?;
        check!(buf == [0x5A; 8], "a failed read changed the buffer");
        check!(fuse::stats(index).map(|(_, alive)| alive) == Some(false), "still alive");
        // A re-registration of the name replaces the dead mount at once.
        let owner = with_fake(|f| f.owner);
        let again = fuse::register(owner, "t", MountFlags::default()).map_err(|e| format!("{e:?}"))?;
        with_fake(|f| {
            f.index = again;
            f.mode = Mode::Normal;
        });
        check!(vfsapi::abi_stat(ROOT, "/mnt/t").is_ok(), "the new provider answers");
        Ok(())
    })
}

pub fn stale_reply() -> Result<(), String> {
    run(Mode::Normal, |index| {
        mode(Mode::WrongTag);
        expect(vfsapi::abi_stat(ROOT, "/mnt/t/x"), FsError::Io, "a wrong tag")?;
        let (stats, alive) = fuse::stats(index).ok_or("no stats")?;
        check!(alive && stats.stale == 1 && stats.timeouts == 1, "{stats:?}");
        // A reply with nothing in flight is stale too.
        let owner = with_fake(|f| f.owner);
        let late = fuse::reply(index, owner, &Reply::default(), &mut |_| Ok(()));
        check!(late == Err(FuseError::Stale), "a late reply: {late:?}");
        // Another task cannot answer for the provider.
        let other = fuse::reply(index, owner + 1, &Reply::default(), &mut |_| Ok(()));
        check!(other == Err(FuseError::NotOwner), "another task replied: {other:?}");
        mode(Mode::Normal);
        check!(vfsapi::abi_stat(ROOT, "/mnt/t").is_ok(), "recovery");
        Ok(())
    })
}

pub fn oversized_reply() -> Result<(), String> {
    run(Mode::Normal, |index| {
        file_with(b"0123456789")?;
        mode(Mode::Oversize);
        let mut buf = [0xA5u8; 4];
        let got = vfsapi::abi_read_at(ROOT, "/mnt/t/f", 0, &mut buf);
        check!(got == Err(FsError::Io), "an oversized read reply: {got:?}");
        check!(buf == [0xA5; 4], "the caller's buffer changed");
        let (_, alive) = fuse::stats(index).ok_or("no stats")?;
        check!(alive, "a refused reply killed the provider");
        Ok(())
    })
}

pub fn faulting_reply_data() -> Result<(), String> {
    run(Mode::Normal, |index| {
        file_with(b"0123456789")?;
        mode(Mode::FaultData);
        let mut buf = [0x3Cu8; 4];
        let got = vfsapi::abi_read_at(ROOT, "/mnt/t/f", 0, &mut buf);
        check!(got == Err(FsError::Io), "faulting reply data: {got:?}");
        check!(buf == [0x3C; 4], "the caller's buffer changed");
        let (stats, alive) = fuse::stats(index).ok_or("no stats")?;
        check!(alive && stats.timeouts == 0, "after a faulting reply: {stats:?}");
        mode(Mode::Normal);
        check!(vfsapi::abi_read_at(ROOT, "/mnt/t/f", 0, &mut buf) == Ok(4), "recovery");
        Ok(())
    })
}

pub fn lying_replies() -> Result<(), String> {
    run(Mode::Normal, |_| {
        file_with(b"0123456789")?;
        // Made behind the VFS's back, so no lookup of them is cached yet.
        with_fake(|fake| -> Result<(), u64> {
            fused::daemon::FuseFs::create(&mut fake.fs, "g", 0o644, 0, 0)?;
            fused::daemon::FuseFs::create(&mut fake.fs, "h", 0o644, 0, 0)?;
            Ok(())
        })
        .map_err(|e| format!("creating g and h: errno {e}"))?;
        for (lie, path, what) in [
            (Lie::Symlink, "/mnt/t/g", "a symlink"),
            (Lie::HugeUid, "/mnt/t/h", "a 33-bit uid"),
        ] {
            mode(Mode::Lie(lie));
            expect(vfsapi::abi_stat(ROOT, path), FsError::Io, what)?;
        }
        mode(Mode::Lie(Lie::ReadCount));
        let mut buf = [0u8; 4];
        expect(
            vfsapi::abi_read_at(ROOT, "/mnt/t/f", 0, &mut buf),
            FsError::Io,
            "a read count above its data",
        )?;
        mode(Mode::Lie(Lie::Dirents));
        expect(vfsapi::abi_readdir(ROOT, "/mnt/t"), FsError::Io, "garbage entries")?;
        mode(Mode::Normal);
        check!(vfsapi::abi_readdir(ROOT, "/mnt/t").is_ok(), "recovery");
        Ok(())
    })
}

pub fn endless_directory() -> Result<(), String> {
    run(Mode::Normal, |_| {
        mode(Mode::Lie(Lie::EndlessDir));
        expect(vfsapi::abi_readdir(ROOT, "/mnt/t"), FsError::Io, "a listing that never ends")?;
        let asked = with_fake(|f| f.served);
        check!(
            asked as usize <= fuse::MAX_DIR_ENTRIES / 5000 + 2,
            "{asked} requests for one listing"
        );
        Ok(())
    })
}
