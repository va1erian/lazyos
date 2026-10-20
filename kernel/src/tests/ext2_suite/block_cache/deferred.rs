//! A provider disk's volume (`fs/ext2/deferred.rs`, issue #940): cached, but
//! written back by its users, never by the kernel task. The rule apart from any
//! state, the wiring (writes stay in memory, the flusher and a kernel-task hold
//! leave them, a user's hold writes them back once due, `sync` as everywhere),
//! the mapped-page path the kernel task must skip, and a soak against reboots.

use super::*;
use crate::fs::ext2::{decide, may_write_here, DeferredAction as Action};
use crate::fs::flusher::INTERVAL_TICKS;
use crate::fs::vfs::Filesystem;
use crate::task::KERNEL_TASK;

/// `due` with no schedule (the module's `CLEAN`).
const NONE: u64 = u64::MAX;

/// The rule: nothing dirty forgets the schedule, a first dirty set starts one
/// an interval away, and only a holder that may write does, once it is due.
pub fn deferred_rule() -> Result<(), String> {
    check!(decide(0, 700, 900, true) == Action::Clean, "clean volume");
    check!(
        decide(0, NONE, 900, false) == Action::Clean,
        "clean, no writer"
    );
    check!(
        decide(3, NONE, 900, true) == Action::Wait(900 + INTERVAL_TICKS),
        "a new dirty set waits an interval"
    );
    check!(
        decide(3, 1000, 999, true) == Action::Wait(1000),
        "not yet due"
    );
    check!(decide(3, 1000, 1000, true) == Action::WriteBack, "due");
    check!(
        decide(3, 1000, 5000, false) == Action::Wait(1000),
        "due, but no writer"
    );
    check!(
        decide(1, NONE, u64::MAX - 1, true) == Action::Wait(u64::MAX),
        "the schedule saturates"
    );
    task::register_kernel();
    task::harness::switch_current(KERNEL_TASK);
    check!(
        !may_write_here(),
        "the kernel task may write a provider back"
    );
    Ok(())
}

/// A user task: a fork of the kernel task, made current.
fn as_user() -> Result<usize, String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(KERNEL_TASK);
    let user = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
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
        // A user's hold before the interval leaves the blocks dirty.
        let _ = fs.lookup("/f").map_err(fs_error)?;
        check!(fs.dirty_blocks() > 30, "written back before it was due");
        // The kernel task's flusher, and its own holds, leave it alone even
        // once it is due.
        task::harness::switch_current(KERNEL_TASK);
        vfs.writeback_all(false).map_err(fs_error)?;
        crate::fs::flusher::service();
        fs.make_writeback_due();
        let _ = fs.lookup("/f").map_err(fs_error)?;
        check!(
            fs.dirty_blocks() > 30,
            "the kernel task wrote a provider volume back"
        );
        // Nor does it push mapped pages into such a volume.
        let node = fs.open_node("/f").map_err(fs_error)?.ok_or("no node")?;
        check!(
            fs.flush_page(node, 0, &data[..4096], false).is_none(),
            "the kernel task flushed a mapped page into a provider volume"
        );
        // The next user's hold, the writeback being due, writes it all back.
        task::harness::switch_current(user);
        let _ = fs.lookup("/f").map_err(fs_error)?;
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

/// Soak: hundreds of writes, rewrites and deletes with due writebacks between
/// them; after each checkpoint a reboot finds exactly the files written back,
/// the volume checks clean, and unmounting gives every cache frame back.
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
                // A call into the volume itself: the VFS may answer a `stat`
                // without one, and only a call into the volume ends a hold.
                let newest = &live.last().ok_or("no live file")?.0;
                let _ = fs.lookup(newest).map_err(fs_error)?;
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
