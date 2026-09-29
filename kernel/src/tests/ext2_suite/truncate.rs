//! ext2 `truncate`: shrink, grow, sparse files, bad inputs, persistence, and
//! the crash-ordering property (nothing an inode points at is ever freed).

use super::*;
use crate::fs::vfs::Filesystem;

/// Blocks of a 1 KiB volume this file has consumed so far.
fn used_since(fs: &Ext2, baseline: u32) -> Result<u32, String> {
    Ok(baseline - fs.free_blocks().map_err(fs_error)?)
}

/// Shrink within a block, across blocks, to zero, and grow again: sizes,
/// contents, and the free-block count all track, and a grow reads zeros where
/// the shrink cut (the old bytes must never come back).
pub fn truncate_shrink_grow_zero() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let baseline = fs.free_blocks().map_err(fs_error)?;
    let body = pattern_bytes(1, 5000);
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    vfs.write(root, "/f", 0, &body).map_err(fs_error)?;
    check!(
        used_since(&fs, baseline)? == 5,
        "5000 bytes should use 5 blocks"
    );

    vfs.truncate(root, "/f", 4200).map_err(fs_error)?; // same block count
    check!(
        vfs.stat(root, "/f").map_err(fs_error)?.size == 4200
            && vfs.read_file(root, "/f").map_err(fs_error)? == body[..4200]
            && used_since(&fs, baseline)? == 5,
        "an in-block shrink changed more than the size"
    );

    vfs.truncate(root, "/f", 100).map_err(fs_error)?;
    check!(
        vfs.read_file(root, "/f").map_err(fs_error)? == body[..100]
            && used_since(&fs, baseline)? == 1,
        "the shrink to 100 bytes kept the wrong blocks"
    );

    vfs.truncate(root, "/f", 3000).map_err(fs_error)?; // grow: a hole
    let grown = vfs.read_file(root, "/f").map_err(fs_error)?;
    check!(
        grown.len() == 3000
            && grown[..100] == body[..100]
            && grown[100..].iter().all(|&byte| byte == 0),
        "the grown range is not zeros (stale bytes survived the shrink)"
    );
    check!(used_since(&fs, baseline)? == 1, "growing allocated blocks");

    vfs.truncate(root, "/f", 0).map_err(fs_error)?;
    check!(
        vfs.stat(root, "/f").map_err(fs_error)?.size == 0
            && fs.mapped_block("/f", 0).map_err(fs_error)? == 0
            && used_since(&fs, baseline)? == 0,
        "truncate to zero left blocks behind"
    );
    check_volume(disk, 512)
}

/// Cuts on both sides of every block-map boundary (direct/indirect) keep
/// exactly the right blocks, and the indirect table goes with its last block.
pub fn truncate_across_indirect() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let baseline = fs.free_blocks().map_err(fs_error)?;
    let body = pattern_bytes(2, 40 * 1024);
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    vfs.write(root, "/f", 0, &body).map_err(fs_error)?;
    check!(used_since(&fs, baseline)? == 41, "40 data blocks + 1 table");

    // (new size, blocks that must remain)
    let steps: [(usize, u32); 6] = [
        (40 * 1024, 41),
        (13 * 1024, 14),
        (12 * 1024 + 1, 14), // one byte into the indirect range keeps the table
        (12 * 1024, 12),     // the boundary itself frees it
        (5 * 1024 + 10, 6),
        (1, 1),
    ];
    for (size, blocks) in steps {
        vfs.truncate(root, "/f", size as u64).map_err(fs_error)?;
        check!(
            vfs.read_file(root, "/f").map_err(fs_error)? == body[..size],
            "size {size}: contents changed"
        );
        let used = used_since(&fs, baseline)?;
        check!(
            used == blocks,
            "size {size}: {used} blocks, expected {blocks}"
        );
    }
    check_volume(disk, 512)
}

/// Sparse files: growing allocates nothing, shrinking into a hole frees the
/// blocks past it, and holes read as zeros throughout.
pub fn truncate_sparse_files() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let baseline = fs.free_blocks().map_err(fs_error)?;
    vfs.create(root, "/s", 0o644).map_err(fs_error)?;
    vfs.write(root, "/s", 5000, b"tail").map_err(fs_error)?;
    check!(used_since(&fs, baseline)? == 1, "one block for the tail");

    vfs.truncate(root, "/s", 100_000).map_err(fs_error)?;
    let grown = vfs.read_file(root, "/s").map_err(fs_error)?;
    check!(
        grown.len() == 100_000
            && &grown[5000..5004] == b"tail"
            && grown
                .iter()
                .enumerate()
                .all(|(at, &byte)| (5000..5004).contains(&at) || byte == 0)
            && used_since(&fs, baseline)? == 1,
        "growing a sparse file allocated blocks or corrupted the hole"
    );

    vfs.truncate(root, "/s", 2000).map_err(fs_error)?; // into the hole
    check!(
        vfs.read_file(root, "/s").map_err(fs_error)? == vec![0u8; 2000]
            && used_since(&fs, baseline)? == 0,
        "shrinking into a hole kept the tail block"
    );
    check_volume(disk, 512)
}

/// Bad inputs fail cleanly and change nothing: directories, missing paths,
/// sizes past the cap, and a caller without write permission.
pub fn truncate_bad_inputs() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    vfs.mkdir(root, "/d", 0o755).map_err(fs_error)?;
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    vfs.write(root, "/f", 0, b"keep me").map_err(fs_error)?;
    let image = disk.data.lock().clone();

    check!(
        Filesystem::truncate(&*fs, "d", 0) == Err(FsError::IsDir),
        "truncating a directory was not IsDir"
    );
    check!(
        Filesystem::truncate(&*fs, "", 0) == Err(FsError::IsDir),
        "truncating the root was not IsDir"
    );
    check!(
        Filesystem::truncate(&*fs, "nope", 0) == Err(FsError::NotFound),
        "truncating a missing file was not NotFound"
    );
    for size in [0x8000_0000u64, 1 << 32, u64::MAX] {
        check!(
            vfs.truncate(root, "/f", size) == Err(FsError::NoSpace),
            "size {size:#x} past the cap was accepted"
        );
    }
    let user = Id::new(1000, 1000);
    check!(
        vfs.truncate(user, "/f", 0) == Err(FsError::Access),
        "a user without write permission truncated the file"
    );
    check!(
        vfs.truncate(root, "/f", 7).is_ok() && *disk.data.lock() == image,
        "truncating to the current size wrote to the disk"
    );
    check!(
        vfs.read_file(root, "/f").map_err(fs_error)? == b"keep me".to_vec(),
        "a rejected truncate damaged the file"
    );
    check_volume(disk, 512)
}

/// A truncate survives a remount, and the cut bytes stay gone after a grow.
pub fn truncate_survives_remount() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let body = pattern_bytes(3, 30 * 1024);
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    vfs.write(root, "/f", 0, &body).map_err(fs_error)?;
    vfs.truncate(root, "/f", 7000).map_err(fs_error)?;
    fs.flush().map_err(fs_error)?;
    drop((fs, vfs));

    let (fs, mut vfs) = remount_disk(disk)?;
    check!(
        vfs.read_file(root, "/f").map_err(fs_error)? == body[..7000],
        "the shrunk file changed across the remount"
    );
    vfs.truncate(root, "/f", 20_000).map_err(fs_error)?;
    let grown = vfs.read_file(root, "/f").map_err(fs_error)?;
    check!(
        grown[..7000] == body[..7000] && grown[7000..].iter().all(|&byte| byte == 0),
        "old bytes resurfaced after shrink, remount and grow"
    );
    fs.flush().map_err(fs_error)?;
    check_volume(disk, 512)
}

/// The block indices the crash sweep plants data at: every region of the map
/// (direct, single, double across several tables, triple).
const CRASH_BLOCKS: [u32; 12] = [0, 1, 11, 12, 13, 267, 268, 269, 524, 600, 65_804, 66_104];

/// One crash point: cut power (lose every write from the `k`th on) while
/// `op` runs, then judge the raw image as a fresh mount would find it.
fn crash_point(
    disk: &'static FakeDisk,
    pristine: &[u8],
    k: u32,
    keep_bytes: usize,
    vanish_ok: bool,
    op: impl Fn(&mut Vfs) -> Result<(), FsError>,
) -> Result<bool, String> {
    disk.data.lock().copy_from_slice(pristine);
    let (fs, mut vfs) = remount_disk(disk)?;
    disk.fail_nth_write(k);
    let finished = op(&mut vfs).is_ok();
    disk.fail_nth_write(u32::MAX); // disarm if the op ended before the fault
    drop((fs, vfs)); // the interrupted mount is gone; only the disk remains

    // A volume that still claims to be clean must be byte-for-byte untouched.
    check!(
        finished || raw_state(disk) & 1 == 0 || *disk.data.lock() == pristine,
        "crash point {k}: the volume is flagged clean but changed"
    );
    let (fs, mut vfs) = remount_disk(disk)?;
    let root = Id::ROOT;
    if vanish_ok && vfs.stat(root, "/f").err() == Some(FsError::NotFound) {
        return Ok(finished); // an unlink that got as far as the directory entry
    }
    for index in CRASH_BLOCKS {
        let block = fs.mapped_block("/f", index).map_err(fs_error)?;
        check!(
            block == 0 || block_allocated(disk, block),
            "crash point {k}: block {index} is reachable from the inode but free"
        );
        let offset = u64::from(index) * 1024;
        if (offset as usize) + 16 <= keep_bytes {
            let mut marker = [0u8; 16];
            vfs.read(root, "/f", offset, &mut marker)
                .map_err(fs_error)?;
            check!(
                marker == pattern_bytes(index, 16)[..],
                "crash point {k}: kept block {index} lost its data"
            );
        }
    }
    Ok(finished)
}

/// The ordering property behind truncate and unlink (detach, then free): at
/// *every* write a power cut could land on, no block an inode still reaches is
/// marked free, everything below the cut survives, and a volume left flagged
/// clean is identical to how it started.
pub fn truncate_crash_sweep() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    for index in CRASH_BLOCKS {
        let marker = pattern_bytes(index, 16);
        vfs.write(root, "/f", u64::from(index) * 1024, &marker)
            .map_err(fs_error)?;
    }
    fs.flush().map_err(fs_error)?;
    drop((fs, vfs));
    let pristine = disk.data.lock().clone();

    // Cuts inside the single, double and triple trees, and to zero.
    for target in [
        13 * 1024 + 10u64,
        300 * 1024 + 1,
        40_000_000,
        65_904 * 1024 + 5,
        0,
    ] {
        let keep = target as usize;
        let mut finished = false;
        for k in 1..400 {
            finished = crash_point(disk, &pristine, k, keep, false, |vfs| {
                vfs.truncate(Id::ROOT, "/f", target)
            })?;
            if finished {
                break;
            }
        }
        check!(
            finished,
            "truncate to {target} never completed within 400 writes"
        );
    }
    // Unlink frees through the same path; the file is either fully there
    // (all data intact) or gone, and never half-freed while reachable.
    let mut done = false;
    for k in 1..400 {
        let unlink = |vfs: &mut Vfs| vfs.unlink(Id::ROOT, "/f");
        if crash_point(disk, &pristine, k, usize::MAX, true, unlink)? {
            done = true;
            break;
        }
    }
    check!(done, "unlink never completed within 400 writes");
    Ok(())
}
