//! Bringing a volume that stopped uncleanly back to clean: what a boot-time
//! `e2fsck -p` does elsewhere, for a host that can afford a full check.
//!
//! The kernel never launders an unclean volume (`state.rs`): it has no fsck,
//! so a clean shutdown restores the state it found at mount. Without this
//! module that state would stick forever: one closed QEMU window and every
//! later boot of the same image says "was not cleanly unmounted". The image
//! build runs [`Ext2::recover`] when it updates a volume in place, so a
//! rebuild is the check.
//!
//! The order matters. The orphans an unlink-while-open left behind are
//! reclaimed first, because a mount of a clean volume never looks for them.
//! Then the whole volume is read back and judged by [`check::fsck`], which
//! shares no code with the driver. A volume with problems gets
//! [`Ext2::repair`] (`repair/`), which fixes exactly what an interrupted
//! writeback can leave (leaks, counters, link counts, dead entries, a rename
//! cut short) and refuses anything else, and is then judged again. Only a
//! volume the checker passes is marked checked; the clean marker itself is
//! written by the next [`Ext2::flush`], under the usual ordering.

use std::format;
use std::string::String;
use std::vec::Vec;

use super::*;
use crate::check;
use crate::{RepairError, RepairReport};

/// What [`Ext2::recover`] did.
// One per recovery, so the report travels by value.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    /// The volume was clean at mount: nothing to do.
    WasClean,
    /// It was unclean, `reclaimed` orphans were deleted, the crash damage in
    /// `repairs` (often nothing) was repaired, the check passed and the next
    /// flush marks it clean.
    Recovered {
        reclaimed: usize,
        repairs: RepairReport,
    },
    /// It stays flagged unclean, for the reason given.
    StillUnclean(String),
}

/// How many fsck problems a [`Recovery::StillUnclean`] reason quotes.
const QUOTED_PROBLEMS: usize = 3;

/// Bytes read from the device per request while loading the volume.
const READ_CHUNK: usize = 1 << 20;

impl Ext2 {
    /// Check a volume that stopped uncleanly and, when it is consistent, let
    /// the next flush mark it clean. `reserved_prefix` names the orphans to
    /// reclaim (the kernel's [`ORPHAN_PREFIX`]).
    ///
    /// The whole volume is read into memory for the checker, so this is for
    /// the host (the image build), not the kernel. Recorded filesystem errors
    /// are never cleared here: such a volume is left for a real fsck.
    pub fn recover(&mut self, reserved_prefix: &str) -> Result<Recovery, Ext2Error> {
        if self.was_clean_at_mount() {
            return Ok(Recovery::WasClean);
        }
        if self.read_only {
            return Ok(still("the device is read-only"));
        }
        if self.had_errors_at_mount() {
            return Ok(still("it has recorded filesystem errors"));
        }
        let report = self.reclaim_orphans(reserved_prefix);
        if report.scan_truncated || !report.failed.is_empty() {
            return Ok(still("not every orphaned file could be reclaimed"));
        }
        // The checker reads the raw device, so everything the reclaim did
        // must be there first: through a block cache (`open_cached`) that is a
        // commit (dirty blocks and the deferred frees), uncached a flush.
        {
            let _guard = self.lock.lock();
            self.commit_locked()?;
        }
        let problems = match self.problems()? {
            Ok(problems) => problems,
            Err(reason) => return Ok(reason),
        };
        let mut repairs = RepairReport::default();
        if !problems.is_empty() {
            repairs = match self.repair() {
                Ok(repairs) => repairs,
                Err(RepairError::Refused(reason)) => {
                    return Ok(unclean(&problems, &format!("; not repaired: {reason}")));
                }
                Err(RepairError::Fs(error)) => return Err(error),
            };
            {
                let _guard = self.lock.lock();
                self.commit_locked()?;
            }
            let after = match self.problems()? {
                Ok(problems) => problems,
                Err(reason) => return Ok(reason),
            };
            if !after.is_empty() {
                return Ok(unclean(
                    &after,
                    &format!(" (left after repairing {repairs})"),
                ));
            }
        }
        self.mark_checked();
        Ok(Recovery::Recovered {
            reclaimed: report.reclaimed,
            repairs,
        })
    }

    /// What the checker finds on the device, or the reason it cannot run.
    fn problems(&self) -> Result<Result<Vec<String>, Recovery>, Ext2Error> {
        let Some(volume) = self.read_volume()? else {
            return Ok(Err(still(
                "the volume does not fit in memory for the check",
            )));
        };
        Ok(Ok(check::fsck(&volume)))
    }

    /// Every byte of the volume (`s_blocks_count` blocks), as the checker
    /// wants it, or `None` when the host cannot allocate that much.
    ///
    /// The checker takes the whole image on purpose: it re-reads raw bytes
    /// and shares no code with the driver. This runs on the build host, and
    /// only for a volume that stopped uncleanly, so the allocation is fallible
    /// rather than streamed: a volume too large for the host stays flagged
    /// (the build says why) instead of aborting the build.
    fn read_volume(&self) -> Result<Option<Vec<u8>>, Ext2Error> {
        let _guard = self.lock.lock();
        let Some(total) = (self.blocks_count as usize).checked_mul(self.block_size as usize) else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(total).is_err() {
            return Ok(None);
        }
        bytes.resize(total, 0);
        for (index, chunk) in bytes.chunks_mut(READ_CHUNK).enumerate() {
            let lba = (index * READ_CHUNK / SECTOR_SIZE) as u64;
            self.io.read_sectors(lba, chunk).map_err(io_error)?;
        }
        Ok(Some(bytes))
    }
}

/// "fsck found N problem(s): the first few", then `detail`.
fn unclean(problems: &[String], detail: &str) -> Recovery {
    let quoted: Vec<&str> = problems
        .iter()
        .take(QUOTED_PROBLEMS)
        .map(String::as_str)
        .collect();
    Recovery::StillUnclean(format!(
        "fsck found {} problem(s): {}{detail}",
        problems.len(),
        quoted.join("; ")
    ))
}

fn still(reason: &str) -> Recovery {
    Recovery::StillUnclean(String::from(reason))
}
