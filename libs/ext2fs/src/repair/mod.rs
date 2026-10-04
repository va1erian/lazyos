//! Repairing what an interrupted writeback leaves behind, and nothing else.
//!
//! ext2 has no journal. The block cache's phase order (`cache/roles.rs`), its
//! deferred frees and its barriers bound what a power cut between two commits
//! can leave (`docs/architecture/block-cache.md`, "Crash semantics"): leaked
//! blocks and inodes, stale free counters, link counts and `i_blocks` out of
//! step, a directory size that disagrees with its blocks, an entry naming an
//! inode whose initialisation (or deletion) was cut short, and a renamed file
//! or directory under both names. [`Ext2::repair`] fixes exactly those, the
//! way `e2fsck -p` would, and refuses everything a crash cannot produce (a
//! block or inode that is reachable and free, a block claimed twice or
//! claimed by metadata, a garbled directory, an out-of-range pointer, a
//! directory with a hole): that volume is someone else's problem, and the
//! repair writes nothing to it.
//!
//! # Rules
//!
//! * **Dead entries.** An entry naming an inode that is allocated but dead (an
//!   unsupported type, no links, or a deletion time: a create whose inode
//!   never landed, or a delete whose name removal did not) is removed. An entry
//!   naming a *free* inode is not a crash shape and is refused.
//! * **A directory under two names** (a rename cut short) keeps the name its
//!   `..` agrees with; with one name, `..` is pointed at it.
//! * **Unreachable inodes**: one that is dead, or holds nothing (an empty file,
//!   a directory with only `.` and `..`), is freed. One that still holds data
//!   is linked into `/lost+found` as `#<ino>`, as fsck does: the name was
//!   lost, the data need not be. A subtree of unreachable directories is
//!   linked by its top directory only, with `..` repointed.
//! * **Leaked blocks** (marked used, reached by nothing) are freed.
//! * **Link counts** are set to the number of entries naming the inode, in
//!   both directions (fsck's rule: a count below the entries would free a
//!   named inode at its next unlink; one above it leaks it).
//! * **`i_blocks`**, directory sizes, and every group and superblock counter
//!   are recomputed from what the inode owns and from the bitmaps.
//!
//! # Order
//!
//! The work runs in rounds, each one a fresh scan: entry fixes first, then the
//! `/lost+found` links, then the frees and counts, with a barrier between
//! rounds. Nothing is freed while any entry still needs fixing, and a frees'
//! bitmap change never lands ahead of the entry change that made the inode
//! unreachable, so a power cut during the repair leaves a volume the next
//! repair accepts. The volume stays flagged unclean throughout; certifying it
//! is the caller's business (`recover.rs`, which re-runs the independent
//! checker on the host).
//!
//! It is `no_std` and works through the driver's own block I/O (and cache),
//! so the kernel can run it at mount too; the memory it needs is two bits per
//! block, two per inode and a counter per inode, allocated fallibly.

use alloc::string::String;

use super::*;

mod apply;
mod bits;
mod report;
mod scan;
mod walk;

pub use report::{Listed, RepairError, RepairReport, LISTED};

/// More rounds than any crash shape needs (entries, links, frees, plus one
/// per cycle of unreachable directories); past this the volume is refused.
const MAX_ROUNDS: usize = 16;

/// `i_file_acl`: an extended-attribute block the block map does not name.
const INO_FILE_ACL: usize = 0x68;

/// `s_feature_compat`, and the compatible features the repair understands:
/// directory preallocation changes nothing it looks at. Anything else (a
/// journal, a resize inode, extended attributes) may own blocks the repair
/// cannot see, so it would call them leaked.
const SB_FEATURE_COMPAT: usize = 0x5C;
const KNOWN_COMPAT: u32 = 0x0001 | FEATURE_COMPAT_HAS_JOURNAL;

/// The directory under the root that fsck links unreachable inodes into
/// (`format` creates it).
const LOST_FOUND: &str = "lost+found";

pub(crate) fn refuse(reason: impl Into<String>) -> RepairError {
    RepairError::Refused(reason.into())
}

/// The kind of a live inode, or `None` for a dead one: an unsupported type
/// (never initialised), or no links or a deletion time (being deleted).
fn live_kind(inode: &[u8; INODE_CORE_SIZE]) -> Option<FileKind> {
    let kind = kind_from_mode(le16(inode, INO_MODE))?;
    (le16(inode, INO_LINKS) > 0 && le32(inode, INO_DTIME) == 0).then_some(kind)
}

impl Ext2 {
    /// Repair the inconsistencies an unclean stop can leave (see the module
    /// docs for the rules), returning what was changed.
    ///
    /// [`RepairError::Refused`] means the volume has damage no crash leaves;
    /// nothing was freed and it is left for a real fsck. A consistent volume
    /// comes back with an empty report and nothing written. The volume's
    /// clean flag is not touched.
    pub fn repair(&self) -> Result<RepairReport, RepairError> {
        let _guard = self.lock.lock();
        if self.read_only {
            return Err(RepairError::Fs(Ext2Error::ReadOnly));
        }
        let mut report = RepairReport::default();
        for _ in 0..MAX_ROUNDS {
            let scan = self.scan()?;
            if scan.has_entry_work() {
                self.fix_entries(&scan, &mut report)?;
            } else if !scan.orphans.is_empty() {
                // The allocator trusts the counters to find a group with room.
                self.recount(&mut report)?;
                self.attach_orphans(&scan, &mut report)?;
            } else {
                self.finish(&scan, &mut report)?;
                return Ok(report);
            }
            self.barrier()?;
        }
        Err(refuse("the repair did not settle"))
    }
}
