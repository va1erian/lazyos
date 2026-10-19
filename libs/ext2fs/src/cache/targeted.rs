//! Writeback of part of the dirty set: one file's blocks (`fsync`), or the
//! blocks one operation changed (a rename's ordering points), without
//! writing what unrelated work left dirty.
//!
//! A subset may only be written if the order of [`super::roles`] still holds
//! for every byte that reaches the disk, so the blocks asked for are widened
//! by what they depend on:
//!
//! * the group descriptors and bitmaps (a few blocks): a block or inode is
//!   marked used before anything durable references it. Writing them early
//!   costs a leak at worst, which `repair` knows;
//! * the *fresh* blocks of every inode that is written: a pointer must not
//!   reach the disk ahead of the bytes it points to. A fresh block is
//!   attributed to the inode whose operation wrote it ([`BlockCache::owner`]);
//!   one with no known owner is always included.
//!
//! An inode table block holds many inodes, so an inode is written by patching
//! its slot into the block as the disk has it: the neighbours' newer inodes
//! (and the pointers in them) stay in the cache with their data. The cached
//! block stays dirty until a full writeback.
//!
//! The superblock (free counters, `s_state`) is left for the next commit
//! unless it is asked for by number: counters that disagree with the bitmaps
//! are a state `repair` already handles. Frees stay deferred to the commit.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use super::roles::Phase;
use super::{page, BlockCache};
use crate::{BlockIo, IoError};

/// What a subset writeback is asked for. `owners` and `blocks` are sorted.
pub(crate) struct Targets<'a> {
    /// Inodes whose dirty blocks (data, indirect tables) are wanted.
    pub owners: &'a [u32],
    /// Inodes whose table entries are wanted.
    pub inodes: &'a [u32],
    /// Other blocks wanted by number (a clean one is skipped).
    pub blocks: &'a [u64],
    /// Also dirty content blocks with no known owner (directories, anything
    /// not tagged): not knowing whose they are, they may be the caller's.
    pub unknown: bool,
}

impl BlockCache {
    /// Enter an operation that is attributed to inode `ino`; returns the
    /// previous owner for [`BlockCache::set_owner`].
    pub fn set_owner(&mut self, ino: u32) -> u32 {
        core::mem::replace(&mut self.owner, ino)
    }

    /// Start recording the blocks written ([`BlockCache::take_touched`]).
    /// Scopes nest; the record starts empty when the outermost begins.
    pub fn track_begin(&mut self) {
        if self.tracking == 0 {
            self.touched.clear();
            self.touched_inodes.clear();
        }
        self.tracking += 1;
    }

    pub fn track_end(&mut self) {
        self.tracking = self.tracking.saturating_sub(1);
        if self.tracking == 0 {
            self.touched = BTreeSet::new();
            self.touched_inodes = BTreeSet::new();
        }
    }

    /// Whether a scope is recording.
    pub fn is_tracking(&self) -> bool {
        self.tracking > 0
    }

    /// Record that inode `ino` was written, when a scope is recording.
    pub fn touch_inode(&mut self, ino: u32) {
        if self.tracking > 0 {
            self.touched_inodes.insert(ino);
        }
    }

    /// The recorded blocks and inodes, each sorted and unique, and start a
    /// new record.
    pub fn take_touched(&mut self) -> (Vec<u64>, Vec<u32>) {
        let blocks = core::mem::take(&mut self.touched);
        let inodes = core::mem::take(&mut self.touched_inodes);
        (blocks.into_iter().collect(), inodes.into_iter().collect())
    }

    /// Write the dirty blocks `targets` names, and what they depend on (see
    /// the module docs), in phase order. Everything else stays dirty.
    pub fn flush_targeted(&mut self, io: &dyn BlockIo, targets: &Targets) -> Result<(), IoError> {
        if self.dirty == 0 {
            return Ok(());
        }
        if self.journal.is_some() {
            return self.flush_journaled(io);
        }
        let order = self.targeted_order(io, targets);
        if order.is_empty() {
            return Ok(());
        }
        self.stats.writebacks += 1;
        let mut start = 0;
        while start < order.len() {
            io.pace();
            let (phase, block, index) = order[start];
            if phase == Phase::Inodes {
                self.write_inode_slots(io, block, index, targets.inodes)?;
                start += 1;
                continue;
            }
            let end = self.run_end(&order, start);
            self.write_run(io, &order[start..end])?;
            start = end;
        }
        Ok(())
    }

    /// Write inode-table block `block`, carrying only the entries of
    /// `wanted` from the cache and the rest as the disk has them. When that
    /// is the cached block exactly, it is an ordinary write and the block
    /// becomes clean.
    fn write_inode_slots(
        &mut self,
        io: &dyn BlockIo,
        block: u64,
        index: usize,
        wanted: &[u32],
    ) -> Result<(), IoError> {
        let range = self.roles.inodes_of(block).unwrap_or(0..0);
        let size = self.block_size;
        let entry = self.roles.inode_size as usize;
        let mut image = alloc::vec![0u8; size];
        if let Err(error) = io.read_sectors(block * self.sectors_per_block, &mut image) {
            self.failed = true;
            return Err(error);
        }
        let cached = &page(&self.slots[index])[..size];
        for &ino in wanted.iter().filter(|ino| range.contains(ino)) {
            let at = (ino - range.start) as usize * entry;
            image[at..at + entry].copy_from_slice(&cached[at..at + entry]);
        }
        if image[..] == *cached {
            return self.write_run(io, &[(Phase::Inodes, block, index)]);
        }
        if let Err(error) = io.write_sectors(block * self.sectors_per_block, &image) {
            self.failed = true;
            return Err(error);
        }
        self.stats.write_requests += 1;
        self.stats.written += 1;
        Ok(())
    }

    /// The blocks to write, sorted by (phase, block).
    fn targeted_order(&self, io: &dyn BlockIo, targets: &Targets) -> Vec<(Phase, u64, usize)> {
        let asked = |block: u64| targets.blocks.binary_search(&block).is_ok();
        let mut picked: BTreeSet<u64> = BTreeSet::new();
        // Inodes whose fresh blocks must precede what is written.
        let mut owners: BTreeSet<u32> = targets.owners.iter().copied().collect();
        owners.extend(targets.inodes);
        for (seen, (&block, &index)) in self.map.iter().enumerate() {
            if seen % super::flush::PACE_EVERY == 0 {
                io.pace();
            }
            let slot = &self.slots[index];
            if !slot.dirty {
                continue;
            }
            let phase = self.roles.phase(block, slot.fresh);
            let wanted = match phase {
                Phase::Alloc => true,
                Phase::Super => asked(block),
                Phase::Inodes => self.roles.inodes_of(block).is_some_and(|range| {
                    targets.inodes.iter().any(|ino| range.contains(ino))
                }),
                Phase::Fresh | Phase::Content => {
                    let owned = slot.owner != 0 && targets.owners.binary_search(&slot.owner).is_ok();
                    let unknown = targets.unknown && slot.owner == 0 && phase == Phase::Content;
                    asked(block) || owned || unknown
                }
            };
            if !wanted {
                continue;
            }
            picked.insert(block);
            if phase == Phase::Content && slot.owner != 0 {
                owners.insert(slot.owner);
            }
        }
        // The fresh blocks any of those inodes could point at.
        for (&block, &index) in &self.map {
            let slot = &self.slots[index];
            if slot.dirty
                && self.roles.phase(block, slot.fresh) == Phase::Fresh
                && (slot.owner == 0 || owners.contains(&slot.owner))
            {
                picked.insert(block);
            }
        }
        let mut buckets: [Vec<(Phase, u64, usize)>; Phase::ALL.len()] = Default::default();
        for block in picked {
            let index = self.map[&block];
            let phase = self.roles.phase(block, self.slots[index].fresh);
            buckets[phase as usize].push((phase, block, index));
        }
        buckets.into_iter().flatten().collect()
    }
}
