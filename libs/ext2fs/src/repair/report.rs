//! What [`Ext2::repair`](crate::Ext2::repair) changed, and why it refused.
//!
//! The library has no log of its own, so the report is the log: every repair
//! is counted, and the first [`LISTED`] inode or block numbers of each kind
//! are kept for the host to print (the image build turns [`RepairReport`]'s
//! `Display` into one `cargo:warning`; the kernel can log the same line).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::Ext2Error;

/// How many numbers of each kind a [`Listed`] keeps.
pub const LISTED: usize = 16;

/// How many of them the one-line summary quotes.
const QUOTED: usize = 6;

/// A count of repairs of one kind, with the first [`LISTED`] of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed<T> {
    pub count: usize,
    pub first: Vec<T>,
}

impl<T> Default for Listed<T> {
    fn default() -> Self {
        Listed {
            count: 0,
            first: Vec::new(),
        }
    }
}

impl<T> Listed<T> {
    pub(crate) fn push(&mut self, item: T) {
        self.count += 1;
        if self.first.len() < LISTED {
            self.first.push(item);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Every repair one [`Ext2::repair`](crate::Ext2::repair) made. Only the
/// inconsistencies an interrupted writeback can leave are repaired
/// (`docs/architecture/block-cache.md`, "Crash semantics").
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RepairReport {
    /// Blocks marked used that nothing reaches, returned to the bitmap.
    pub leaked_blocks: Listed<u32>,
    /// Unreachable inodes freed: a delete or a create cut short (no name and
    /// nothing in it), or one whose initialisation never landed.
    pub freed_inodes: Listed<u32>,
    /// Unreachable inodes that still hold data, named `/lost+found/#<ino>`.
    pub lost_found: Listed<u32>,
    /// Entries removed because the inode they name was being deleted or was
    /// never initialised, as `(directory, inode)`.
    pub dead_entries: Listed<(u32, u32)>,
    /// A directory's second name (a rename cut short), removed: the name its
    /// `..` agrees with stays, as `(parent, directory)`.
    pub extra_dir_names: Listed<(u32, u32)>,
    /// `..` pointed at the one directory that names it, as `(directory, parent)`.
    pub dotdot: Listed<(u32, u32)>,
    /// Directory sizes set to the blocks the directory owns.
    pub dir_sizes: Listed<u32>,
    /// Link counts set to the number of entries naming the inode, as
    /// `(inode, was, now)`.
    pub link_counts: Listed<(u32, u16, u16)>,
    /// `i_blocks` set to the blocks the inode owns.
    pub block_counts: Listed<u32>,
    /// Groups whose free or directory counts were recomputed from the bitmaps.
    pub group_counters: Listed<u32>,
    /// Whether the superblock's free counters were recomputed.
    pub super_counters: bool,
}

impl RepairReport {
    /// Whether nothing needed repairing.
    pub fn is_empty(&self) -> bool {
        self.leaked_blocks.is_empty()
            && self.freed_inodes.is_empty()
            && self.lost_found.is_empty()
            && self.dead_entries.is_empty()
            && self.extra_dir_names.is_empty()
            && self.dotdot.is_empty()
            && self.dir_sizes.is_empty()
            && self.link_counts.is_empty()
            && self.block_counts.is_empty()
            && self.group_counters.is_empty()
            && !self.super_counters
    }
}

/// Why [`Ext2::repair`](crate::Ext2::repair) did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairError {
    /// The damage is not a shape a crash can leave (or the repair cannot
    /// prove it safe): the volume is left for a real fsck. Nothing was freed.
    Refused(String),
    /// The device or the volume failed underneath the repair.
    Fs(Ext2Error),
}

impl From<Ext2Error> for RepairError {
    fn from(error: Ext2Error) -> Self {
        RepairError::Fs(error)
    }
}

impl fmt::Display for RepairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepairError::Refused(reason) => f.write_str(reason),
            RepairError::Fs(error) => write!(f, "{error:?}"),
        }
    }
}

/// Write `count noun (a b ...)` for one kind of repair; `noun` is
/// `(singular, plural)`.
fn part<T>(
    f: &mut fmt::Formatter<'_>,
    first: &mut bool,
    listed: &Listed<T>,
    noun: (&str, &str),
    item: impl Fn(&T, &mut fmt::Formatter<'_>) -> fmt::Result,
) -> fmt::Result {
    if listed.is_empty() {
        return Ok(());
    }
    if !*first {
        f.write_str(", ")?;
    }
    *first = false;
    let noun = if listed.count == 1 { noun.0 } else { noun.1 };
    write!(f, "{} {noun} (", listed.count)?;
    for (index, value) in listed.first.iter().take(QUOTED).enumerate() {
        if index > 0 {
            f.write_str(" ")?;
        }
        item(value, f)?;
    }
    if listed.count > QUOTED {
        f.write_str(" ...")?;
    }
    f.write_str(")")
}

impl fmt::Display for RepairReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("nothing");
        }
        let first = &mut true;
        let number = |n: &u32, f: &mut fmt::Formatter<'_>| write!(f, "{n}");
        let pair = |(a, b): &(u32, u32), f: &mut fmt::Formatter<'_>| write!(f, "{a}/{b}");
        part(
            f,
            first,
            &self.leaked_blocks,
            ("leaked block", "leaked blocks"),
            number,
        )?;
        part(
            f,
            first,
            &self.freed_inodes,
            ("leaked inode", "leaked inodes"),
            number,
        )?;
        part(
            f,
            first,
            &self.lost_found,
            ("inode moved to lost+found", "inodes moved to lost+found"),
            |n, f| write!(f, "#{n}"),
        )?;
        part(
            f,
            first,
            &self.dead_entries,
            ("dead entry", "dead entries"),
            pair,
        )?;
        part(
            f,
            first,
            &self.extra_dir_names,
            ("extra directory name", "extra directory names"),
            pair,
        )?;
        part(f, first, &self.dotdot, ("`..` entry", "`..` entries"), pair)?;
        part(
            f,
            first,
            &self.dir_sizes,
            ("directory size", "directory sizes"),
            number,
        )?;
        part(
            f,
            first,
            &self.link_counts,
            ("link count", "link counts"),
            |(n, was, now), f| write!(f, "{n}: {was}->{now}"),
        )?;
        part(
            f,
            first,
            &self.block_counts,
            ("block count", "block counts"),
            number,
        )?;
        part(
            f,
            first,
            &self.group_counters,
            ("group's counters", "groups' counters"),
            number,
        )?;
        if self.super_counters {
            if !*first {
                f.write_str(", ")?;
            }
            f.write_str("the superblock counters")?;
        }
        Ok(())
    }
}
