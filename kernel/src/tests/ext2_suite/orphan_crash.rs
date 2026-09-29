//! Crash safety of deleting a parked file (issue #346): a power cut at every
//! write of the last-close delete, and of the mount-time reclaim itself, must
//! be recoverable by the next mount with nothing leaked and no block both free
//! and reachable. Plus a soak with random cut points.

use super::*;
use crate::fs::vfs::Filesystem;

const ORPHAN: &str = "/.unlinked-7";

/// Blocks of the parked file (12 direct plus a single-indirect table).
const BODY: usize = 40 * 1024;

/// Bytes of the live file beside the parked one.
const KEEP: usize = 8 * 1024;

/// What one cut leaves to judge.
struct Fixture {
    disk: &'static FakeDisk,
    /// The dirty image: one parked file, as after an unlink-while-open.
    parked: Vec<u8>,
    /// Free `(blocks, inodes)` of the same volume without the parked file.
    baseline: (u32, u32),
}

fn fixture() -> Result<Fixture, String> {
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    // A live neighbour, so a wrong free would show up as damage to it.
    vfs.create(root, "/keep", 0o644).map_err(fs_error)?;
    vfs.write(root, "/keep", 0, &pattern_bytes(4, KEEP))
        .map_err(fs_error)?;
    let baseline = bitmap_free(disk, 512);
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    vfs.write(root, "/f", 0, &pattern_bytes(9, BODY))
        .map_err(fs_error)?;
    vfs.rename(root, "/f", ORPHAN).map_err(fs_error)?;
    drop((fs, vfs));
    check!(raw_state(disk) & 1 == 0, "the fixture is not dirty");
    let parked = disk.data.lock().clone();
    Ok(Fixture {
        disk,
        parked,
        baseline,
    })
}

/// Cut power at write `k` of `op`, then check that a fresh mount finishes the
/// job. Returns whether `op` completed without reaching the cut.
fn cut_at(fixture: &Fixture, k: u32, op: impl Fn(&Ext2, &mut Vfs) -> bool) -> Result<bool, String> {
    let disk = fixture.disk;
    disk.data.lock().copy_from_slice(&fixture.parked);
    let (fs, mut vfs) = remount_disk(disk)?;
    disk.cut_power_at(k);
    let finished = op(&fs, &mut vfs);
    disk.fail_nth_write(u32::MAX);
    drop((fs, vfs));
    check_recovery(fixture, k)?;
    Ok(finished)
}

/// Judge the image a stop at cut `k` left behind, as the next boot finds it.
fn check_recovery(fixture: &Fixture, k: u32) -> Result<(), String> {
    let disk = fixture.disk;
    let (fs, mut vfs) = remount_disk(disk)?;
    let still_named = fs.lookup(ORPHAN).is_ok();
    check!(!fs.was_clean_at_mount(), "cut {k}: the volume looks clean");
    let reclaimed = fs.reclaim_orphans();
    check!(
        reclaimed == usize::from(still_named),
        "cut {k}: reclaimed {reclaimed}, name present: {still_named}"
    );
    check!(
        fs.lookup(ORPHAN).err() == Some(FsError::NotFound),
        "cut {k}: the entry survived the reclaim"
    );
    check!(
        bitmap_free(disk, 512) == fixture.baseline,
        "cut {k}: a block or inode leaked"
    );
    // The neighbour kept its data, and its blocks are still allocated.
    check!(
        vfs.read_file(Id::ROOT, "/keep").map_err(fs_error)? == pattern_bytes(4, KEEP),
        "cut {k}: the live file was damaged"
    );
    for index in 0..8 {
        let block = fs.mapped_block("/keep", index).map_err(fs_error)?;
        check!(
            block_allocated(disk, block),
            "cut {k}: a live block was freed"
        );
    }
    Ok(())
}

/// Run `op` with the cut moving through every write until it completes.
fn sweep(op: impl Fn(&Ext2, &mut Vfs) -> bool + Copy) -> Result<u32, String> {
    let fixture = fixture()?;
    for k in 1..2000 {
        if cut_at(&fixture, k, op)? {
            return Ok(k);
        }
    }
    Err(String::from("the operation never completed in 2000 writes"))
}

/// The last close of an unlinked file deletes its hidden name; a cut at any
/// write of that delete leaves something the next mount reclaims.
pub fn orphan_delete_crash_sweep() -> Result<(), String> {
    task::register_kernel();
    let cuts = sweep(|_, vfs| vfs.unlink(Id::ROOT, ORPHAN).is_ok())?;
    check!(cuts > 20, "the sweep saw only {cuts} write points");
    Ok(())
}

/// The reclaim is itself interruptible: a cut at any of its writes is finished
/// by the next mount.
pub fn orphan_reclaim_crash_sweep() -> Result<(), String> {
    task::register_kernel();
    let cuts = sweep(|fs, _| fs.reclaim_orphans() == 1)?;
    check!(cuts > 20, "the sweep saw only {cuts} write points");
    Ok(())
}

/// Soak: generations of several parked files of random sizes in two
/// directories, a power cut at a random write while deleting them (by reclaim
/// or one by one), then recovery. The allocated blocks and inodes always
/// return to baseline (judged by the bitmaps: see `bitmap_free`).
pub fn soak_orphan_generations() -> Result<(), String> {
    task::register_kernel();
    let root = Id::ROOT;
    let mut rng = Rng(0x0ddc_0ffe);
    for generation in 0..150u32 {
        let (fs, mut vfs, disk) = mounted(1024, 512)?;
        vfs.mkdir(root, "/d", 0o755).map_err(fs_error)?;
        vfs.create(root, "/keep", 0o644).map_err(fs_error)?;
        vfs.write(root, "/keep", 0, &pattern_bytes(generation, 2500))
            .map_err(fs_error)?;
        let baseline = bitmap_free(disk, 512);
        let count = 1 + rng.below(4);
        for n in 0..count {
            let dir = if rng.below(2) == 0 { "" } else { "/d" };
            let size = rng.below(30 * 1024) as usize;
            let live = format!("{dir}/f{n}");
            vfs.create(root, &live, 0o644).map_err(fs_error)?;
            vfs.write(root, &live, 0, &pattern_bytes(n, size))
                .map_err(fs_error)?;
            vfs.rename(root, &live, &format!("{dir}/.unlinked-{n}"))
                .map_err(fs_error)?;
        }
        drop((fs, vfs));

        let (fs, mut vfs) = remount_disk(disk)?;
        disk.cut_power_at(1 + rng.below(120));
        if rng.below(2) == 0 {
            fs.reclaim_orphans();
        } else {
            for n in 0..count {
                let _ = vfs.unlink(root, &format!("/.unlinked-{n}"));
                let _ = vfs.unlink(root, &format!("/d/.unlinked-{n}"));
            }
        }
        disk.fail_nth_write(u32::MAX);
        drop((fs, vfs));

        let (fs, mut vfs) = remount_disk(disk)?;
        fs.reclaim_orphans();
        for dir in ["/", "/d"] {
            let names = vfs.readdir(root, dir).map_err(fs_error)?;
            check!(
                names
                    .iter()
                    .all(|entry| !entry.name.starts_with(".unlinked-")),
                "generation {generation}: a hidden entry survived in {dir}"
            );
        }
        check!(
            bitmap_free(disk, 512) == baseline,
            "generation {generation}: a block or inode leaked"
        );
        check!(
            vfs.read_file(root, "/keep").map_err(fs_error)? == pattern_bytes(generation, 2500),
            "generation {generation}: a live file was damaged"
        );
    }
    Ok(())
}
