//! The in-place update's check of a volume that stopped uncleanly (#512, and
//! the repair of what the block cache's crash semantics allow).

use ext2fs::{Ext2, Recovery};

use crate::os_image::{volume_error, RESET_HINT};

const DAMAGED_HINT: &str = "copy your files off the image first, then set \
    LAZYOS_RESET_OS=1 to recreate it, or set LAZYOS_UPDATE_DAMAGED_OS=1 to update it \
    anyway (an updated file may overwrite a damaged user file)";

/// Check a volume that stopped uncleanly (a QEMU window closed, a crash) so
/// the update's closing flush can mark it clean again. The kernel never does:
/// it has no fsck, and restores the state it found at every shutdown, so
/// without this one unclean stop would flag the image for good. What a crash
/// can leave (leaks, stale counters, link counts, dead entries) is repaired
/// first and summarised. Any other damage fails the build, since the update's
/// allocator could hand a block a user file still uses to an updated file,
/// unless `update_damaged` is set: then the volume is updated, stays flagged,
/// and nothing of it is freed.
pub fn recover(volume: &mut Ext2, update_damaged: bool) -> Result<(), String> {
    match volume
        .recover(ext2fs::ORPHAN_PREFIX)
        .map_err(|e| volume_error("check", e))?
    {
        Recovery::WasClean => {}
        Recovery::Recovered { reclaimed, repairs } if repairs.is_empty() => println!(
            "cargo:warning=the OS volume was not cleanly unmounted; checked it, \
             reclaimed {reclaimed} orphaned file(s), and it is clean again"
        ),
        Recovery::Recovered { reclaimed, repairs } => println!(
            "cargo:warning=the OS volume was not cleanly unmounted; re-certified it \
             after repairing {repairs} (reclaimed {reclaimed} orphaned file(s))"
        ),
        Recovery::StillUnclean(reason) if update_damaged => println!(
            "cargo:warning=LAZYOS_UPDATE_DAMAGED_OS=1: updating a damaged OS volume, \
             which stays flagged: {reason}; {RESET_HINT}"
        ),
        Recovery::StillUnclean(reason) => {
            return Err(format!(
                "the OS volume has damage the repair cannot fix ({reason}); {DAMAGED_HINT}"
            ))
        }
    }
    Ok(())
}
