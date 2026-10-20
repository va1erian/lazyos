//! A provider disk's volume (`fs/ext2/deferred.rs`, issue #940): cached, but
//! written back by user tasks at a syscall's end, never by the kernel task and
//! never inside a hold of the gate. The rules apart from any state (the
//! schedule, who may write and when, the global claim), the wiring (writes
//! stay in memory, holds and the kernel task leave them, a user's due
//! writeback commits them, an interactive one waits, a dead disk refuses
//! changes, `sync` as everywhere), and a soak against reboots.

use super::*;
use crate::fs::ext2::{
    claim_deferred as claim, may_write_here, note_deferred_due as note_due, schedule, writer_due,
    DEFERRED_CLEAN as CLEAN,
};
use crate::fs::flusher::INTERVAL_TICKS;
use crate::fs::vfs::Filesystem;
use crate::task::{PriorityClass, KERNEL_TASK};

/// The rules: the schedule starts an interval after a volume first needs a
/// writeback and ends when it needs none; a writer that may not never
/// commits, an interactive one an interval late; the global claim goes to
/// exactly one caller, and only to one that may write once it is due.
pub fn deferred_rule() -> Result<(), String> {
    check!(schedule(false, 700, 900) == CLEAN, "clean volume");
    check!(
        schedule(true, CLEAN, 900) == 900 + INTERVAL_TICKS,
        "a newly dirty volume waits an interval"
    );
    check!(schedule(true, 1000, 5000) == 1000, "a standing schedule");
    check!(
        schedule(true, CLEAN, u64::MAX - 1) == u64::MAX,
        "the schedule saturates"
    );
    check!(writer_due(1000, false, false).is_none(), "no writer");
    check!(writer_due(CLEAN, true, false).is_none(), "nothing due");
    check!(
        writer_due(1000, true, false) == Some(1000),
        "a normal writer"
    );
    check!(
        writer_due(1000, true, true) == Some(1000 + INTERVAL_TICKS),
        "an interactive writer waits an interval"
    );
    let user = as_user()?;
    let result = (|| {
        // Whatever an earlier test left scheduled is taken first.
        let _ = claim(u64::MAX - 1);
        note_due(100);
        check!(!claim(99), "claimed before it was due");
        task::harness::switch_current(KERNEL_TASK);
        check!(
            !may_write_here(),
            "the kernel task may write a provider back"
        );
        check!(!claim(100), "the kernel task claimed");
        task::harness::switch_current(user);
        check!(claim(100), "a user task did not claim");
        check!(!claim(100), "claimed twice");
        task::set_priority(user, PriorityClass::Interactive);
        note_due(100);
        check!(
            !claim(100 + INTERVAL_TICKS - 1),
            "an interactive task claimed early"
        );
        check!(
            claim(100 + INTERVAL_TICKS),
            "an interactive task never claims"
        );
        task::set_priority(user, PriorityClass::Normal);
        Ok(())
    })();
    back_to_kernel();
    result
}

/// A user task: a fork of the kernel task, made current.
fn as_user() -> Result<usize, String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(KERNEL_TASK);
    let user = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    // A fork inherits the kernel task's class, `Interactive`: a user program
    // runs `Normal`.
    task::set_priority(user, PriorityClass::Normal);
    task::harness::switch_current(user);
    check!(may_write_here(), "a user task may not write back");
    Ok(user)
}

fn back_to_kernel() {
    task::harness::switch_current(KERNEL_TASK);
    task::harness::reset();
}

/// `disk` mounted as a provider disk would be, as a private VFS root.
fn deferred(disk: &'static FakeDisk) -> Result<(Arc<Ext2>, Vfs), String> {
    let fs = Arc::new(Ext2::open_deferred_with_pages(disk, 256).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    Ok((fs, vfs))
}

/// Whether a reboot right now finds `path` holding `data`.
fn on_disk(disk: &FakeDisk, path: &str, data: &[u8]) -> Result<bool, String> {
    let (_, _, mut after) = reboot_copy(disk)?;
    Ok(read_file(&mut after, path).is_ok_and(|bytes| bytes == data))
}

pub fn deferred_writeback_by_users() -> Result<(), String> {
    let user = as_user()?;
    let result = (|| {
        let disk = formatted(0)?;
        let (fs, mut vfs) = deferred(disk)?;
        let data = pattern_bytes(11, 40_000);
        write_file(&mut vfs, "/f", &data)?;
        check!(fs.dirty_blocks() > 30, "the write did not stay in memory");
        check!(
            !on_disk(disk, "/f", &data)?,
            "the write reached the disk at once"
        );
        // A hold never writes back, even once due: it may sit inside a mount
        // table, which every other volume's callers need.
        fs.make_writeback_due();
        let _ = fs.lookup("/f").map_err(fs_error)?;
        check!(fs.dirty_blocks() > 30, "a hold wrote back");
        // Nor the kernel task: not its flusher, not a due deferred writeback.
        task::harness::switch_current(KERNEL_TASK);
        vfs.writeback_all(false).map_err(fs_error)?;
        fs.make_writeback_due();
        fs.writeback_deferred();
        check!(
            fs.dirty_blocks() > 30,
            "the kernel task wrote a provider volume back"
        );
        // A mapped page still reaches the file (into the cache, no I/O).
        let node = fs.open_node("/f").map_err(fs_error)?.ok_or("no node")?;
        check!(
            matches!(fs.flush_page(node, 0, &data[..4096], false), Some(Ok(4096))),
            "a mapped page did not reach the file"
        );
        // An interactive task waits a whole interval past the due tick.
        task::harness::switch_current(user);
        task::set_priority(user, PriorityClass::Interactive);
        fs.make_writeback_due();
        fs.writeback_deferred();
        check!(
            fs.dirty_blocks() > 30,
            "an interactive task wrote back at once"
        );
        // A normal user task writes everything back once due.
        task::set_priority(user, PriorityClass::Normal);
        fs.writeback_deferred();
        check!(
            fs.dirty_blocks() == 0,
            "the due writeback left dirty blocks"
        );
        check!(on_disk(disk, "/f", &data)?, "the written-back file");
        check!(
            raw_state(disk) & 1 == 0,
            "a writeback marked the volume clean"
        );
        check_volume(pooled(1), BLOCKS)?;
        // A dead disk refuses changes instead of caching and losing them.
        disk.set_read_only(true);
        check!(
            matches!(vfs.write(Id::ROOT, "/f", 0, b"x"), Err(FsError::Invalid)),
            "a dead disk took a write"
        );
        check!(
            read_file(&mut vfs, "/f")? == data,
            "a dead disk's cached read"
        );
        disk.set_read_only(false);
        // `sync` is unchanged: everything, then the clean marker.
        write_file(&mut vfs, "/g", &data[..9_000])?;
        vfs.sync_all().map_err(fs_error)?;
        check!(fs.dirty_blocks() == 0, "sync left dirty blocks");
        check!(
            raw_state(disk) & 1 == 1,
            "sync did not mark the volume clean"
        );
        check_volume(disk, BLOCKS)?;
        drop((fs, vfs));
        release(disk);
        release(pooled(1));
        Ok(())
    })();
    back_to_kernel();
    result
}

/// Soak: hundreds of writes, rewrites, truncates and deletes with due
/// writebacks between them; after each checkpoint a reboot finds exactly the
/// files written back, the volume checks clean, and unmounting gives every
/// cache frame back.
pub fn soak_deferred_writeback() -> Result<(), String> {
    let _user = as_user()?;
    let result = (|| {
        let frames_before = crate::fs::ext2::cache_frames();
        let disk = formatted(0)?;
        let (fs, mut vfs) = deferred(disk)?;
        let mut live: Vec<(String, Vec<u8>)> = Vec::new();
        for round in 0..240u32 {
            let path = format!("/s{}", round % 17);
            let data = pattern_bytes(round, 300 + (round as usize * 977) % 9_000);
            // A rewrite is a whole new content: cut the old one first.
            match vfs.truncate(Id::ROOT, &path, 0) {
                Ok(()) | Err(FsError::NotFound) => {}
                Err(error) => return Err(fs_error(error)),
            }
            write_file(&mut vfs, &path, &data)?;
            live.retain(|(name, _)| *name != path);
            live.push((path, data));
            if round % 5 == 4 {
                let (gone, _) = live.remove(0);
                vfs.unlink(Id::ROOT, &gone).map_err(fs_error)?;
            }
            if round % 7 == 6 || round % 60 == 59 {
                fs.make_writeback_due();
                fs.writeback_deferred();
                check!(
                    fs.dirty_blocks() == 0,
                    "round {round}: dirty after a due writeback"
                );
            }
            if round % 60 == 59 {
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
    back_to_kernel();
    result
}
