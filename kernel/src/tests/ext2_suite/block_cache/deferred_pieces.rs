//! A provider volume's commit in pieces (`fs/ext2/deferred.rs`, issue #940):
//! each call takes the gate for a bounded stretch and gives it back, other
//! tasks change the volume between the pieces, and in the end the disk holds
//! exactly what was written, nothing half written, and checks clean.

use super::deferred::{as_user, back_to_kernel, deferred, on_disk};
use super::*;
use crate::fs::ext2::set_piece_requests;
use crate::fs::vfs::Filesystem;

/// Calls of `writeback_deferred` that end the commit begun by the first.
const DRAIN_LIMIT: usize = 4000;

/// Make the writeback due and call until the volume has nothing dirty and no
/// commit half written; returns the calls it took. Between two calls the gate
/// must be free.
pub(super) fn drain(fs: &Ext2) -> Result<usize, String> {
    for calls in 1..=DRAIN_LIMIT {
        fs.make_writeback_due();
        fs.writeback_deferred();
        check!(fs.gate_is_free(), "the gate stayed held after a piece");
        if fs.dirty_blocks() == 0 && !fs.commit_in_progress() {
            return Ok(calls);
        }
    }
    Err(format!("{DRAIN_LIMIT} pieces did not finish the commit"))
}

/// One request per piece: a commit of many files takes many calls, none of
/// which holds the gate afterwards; files written between the pieces are
/// whole after a reboot (or wait for the next commit, whole or absent).
pub fn commit_in_pieces() -> Result<(), String> {
    let _user = as_user()?;
    set_piece_requests(1);
    let result = (|| {
        let disk = formatted(0)?;
        let (fs, mut vfs) = deferred(disk)?;
        let mut files = Vec::new();
        for n in 0..24u32 {
            let path = format!("/file{n}");
            let data = pattern_bytes(n, 1_500 + n as usize * 700);
            write_file(&mut vfs, &path, &data)?;
            files.push((path, data));
        }
        fs.make_writeback_due();
        fs.writeback_deferred();
        check!(
            fs.commit_in_progress(),
            "a one-request piece finished a 24-file commit"
        );
        check!(fs.gate_is_free(), "the first piece kept the gate");
        // A reader and a writer take the gate between the pieces.
        let mut late = Vec::new();
        let mut pieces = 1;
        while fs.commit_in_progress() && pieces < DRAIN_LIMIT {
            let path = format!("/late{pieces}");
            let data = pattern_bytes(100 + pieces as u32, 700 + pieces * 311);
            write_file(&mut vfs, &path, &data)?;
            late.push((path, data));
            let _ = fs.lookup("/file0").map_err(fs_error)?;
            fs.make_writeback_due();
            fs.writeback_deferred();
            check!(fs.gate_is_free(), "piece {pieces} kept the gate");
            pieces += 1;
        }
        check!(pieces > 3, "the commit took only {pieces} pieces");
        // What the commit began with is on the disk, whole.
        for (path, data) in &files {
            check!(on_disk(disk, path, data)?, "{path} after the commit");
        }
        let calls = drain(&fs)?;
        for (path, data) in files.iter().chain(&late) {
            check!(on_disk(disk, path, data)?, "{path} after the drain");
        }
        vfs.sync_all().map_err(fs_error)?;
        check_volume(disk, BLOCKS)?;
        crate::trace::test!("bcache_deferred_commit_in_pieces:INFO:pieces={pieces} drain={calls}");
        drop((fs, vfs));
        release(disk);
        release(pooled(1));
        Ok(())
    })();
    set_piece_requests(32);
    back_to_kernel();
    result
}

/// Soak: writes, rewrites and deletes with a two-request piece between each,
/// reboots at checkpoints. A deleted file's blocks return to the bitmaps only
/// by a finished commit, so the volume must also check clean at the end with
/// every free applied.
pub fn soak_in_pieces() -> Result<(), String> {
    let _user = as_user()?;
    set_piece_requests(2);
    let result = (|| {
        let frames_before = crate::fs::ext2::cache_frames();
        let disk = formatted(0)?;
        let (fs, mut vfs) = deferred(disk)?;
        let mut live: Vec<(String, Vec<u8>)> = Vec::new();
        let mut pieces = 0u32;
        for round in 0..300u32 {
            let path = format!("/p{}", round % 13);
            let data = pattern_bytes(round, 200 + (round as usize * 1013) % 12_000);
            match vfs.truncate(Id::ROOT, &path, 0) {
                Ok(()) | Err(FsError::NotFound) => {}
                Err(error) => return Err(fs_error(error)),
            }
            write_file(&mut vfs, &path, &data)?;
            live.retain(|(name, _)| *name != path);
            live.push((path, data));
            if round % 4 == 3 {
                let (gone, _) = live.remove(0);
                vfs.unlink(Id::ROOT, &gone).map_err(fs_error)?;
            }
            fs.make_writeback_due();
            fs.writeback_deferred();
            pieces += 1;
            check!(fs.gate_is_free(), "round {round}: the gate stayed held");
            if round % 75 == 74 {
                drain(&fs)?;
                check_volume(disk, BLOCKS).map_err(|e| format!("round {round}: {e}"))?;
                for (name, data) in &live {
                    check!(
                        on_disk(disk, name, data)?,
                        "round {round}: {name} after a reboot"
                    );
                }
                check_volume(pooled(1), BLOCKS).map_err(|e| format!("round {round}: copy: {e}"))?;
            }
        }
        vfs.sync_all().map_err(fs_error)?;
        check_volume(disk, BLOCKS)?;
        crate::trace::test!("bcache_soak_deferred_pieces:INFO:calls={pieces}");
        drop((fs, vfs));
        check!(
            crate::fs::ext2::cache_frames() == frames_before,
            "cache frames leaked: {} before, {} after",
            frames_before,
            crate::fs::ext2::cache_frames()
        );
        release(disk);
        release(pooled(1));
        Ok(())
    })();
    set_piece_requests(32);
    back_to_kernel();
    result
}
