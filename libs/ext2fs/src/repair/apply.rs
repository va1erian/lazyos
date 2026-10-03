//! The writes of one repair round: entry fixes, `/lost+found` links, and the
//! final frees and counts.

use alloc::format;

use super::scan::{Record, Scan};
use super::walk::record_shape;
use super::*;

impl Ext2 {
    /// Remove dead entries and extra directory names, repoint `..`, and set
    /// directory sizes. Nothing is freed here.
    pub(super) fn fix_entries(
        &self,
        scan: &Scan,
        report: &mut RepairReport,
    ) -> Result<(), RepairError> {
        for record in &scan.dead {
            self.drop_record(record)?;
            report.dead_entries.push((record.dir, record.child));
        }
        for record in &scan.extra {
            self.drop_record(record)?;
            report.extra_dir_names.push((record.dir, record.child));
        }
        for &(dir, parent) in &scan.dotdot {
            let mut inode = self.read_inode(dir)?;
            self.set_dotdot(dir, &mut inode, parent)?;
            report.dotdot.push((dir, parent));
        }
        for &(dir, size) in &scan.sizes {
            let mut inode = self.read_inode(dir)?;
            put32(&mut inode, INO_SIZE, size);
            self.write_inode(dir, &inode)?;
            report.dir_sizes.push(dir);
        }
        Ok(())
    }

    /// Remove one record, merging it into its predecessor (or clearing its
    /// inode when it opens the block), as `remove_entry` does.
    fn drop_record(&self, record: &Record) -> Result<(), RepairError> {
        let mut buf = zeroed(u64::from(self.block_size))?;
        self.read_block(u64::from(record.block), &mut buf)?;
        let mut offset = 0usize;
        let mut previous = None;
        while offset < buf.len() {
            let (rec_len, _) = record_shape(&buf, offset)
                .ok_or_else(|| refuse(format!("bad directory record in block {}", record.block)))?;
            if offset == record.offset {
                if le32(&buf, offset + DE_INO) != record.child {
                    break;
                }
                match previous {
                    None => put32(&mut buf, offset + DE_INO, 0),
                    Some(before) => {
                        let merged = usize::from(le16(&buf, before + DE_REC_LEN)) + rec_len;
                        put16(&mut buf, before + DE_REC_LEN, merged as u16);
                    }
                }
                self.write_block(u64::from(record.block), &buf)?;
                return Ok(());
            }
            previous = Some(offset);
            offset += rec_len;
        }
        Err(refuse(format!(
            "the entry for inode {} moved under the repair",
            record.child
        )))
    }

    /// Name every orphan `/lost+found/#<ino>` (a directory's `..` follows).
    pub(super) fn attach_orphans(
        &self,
        scan: &Scan,
        report: &mut RepairReport,
    ) -> Result<(), RepairError> {
        let lost_found = match self.find_entry(ROOT_INO, LOST_FOUND) {
            Ok((ino, _)) => ino,
            Err(Ext2Error::NotFound) => {
                return Err(refuse(
                    "unreachable inodes hold data and there is no /lost+found",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let mut dir = self.read_inode(lost_found)?;
        let reachable =
            scan.visited.get(lost_found) && !scan.orphans.iter().any(|&(ino, _)| ino == lost_found);
        if live_kind(&dir) != Some(FileKind::Dir) || !reachable {
            return Err(refuse("/lost+found is not a directory"));
        }
        for &(ino, is_dir) in &scan.orphans {
            let name = self.free_name(lost_found, ino)?;
            let file_type = if is_dir { FT_DIRECTORY } else { FT_REGULAR };
            self.add_entry(lost_found, &mut dir, &name, ino, file_type)
                .map_err(|error| match error {
                    Ext2Error::NoSpace => refuse("/lost+found is full"),
                    error => RepairError::Fs(error),
                })?;
            if is_dir {
                let mut child = self.read_inode(ino)?;
                self.set_dotdot(ino, &mut child, lost_found)?;
            }
            report.lost_found.push(ino);
        }
        Ok(())
    }

    /// `#<ino>`, or `#<ino>.<n>` if a previous repair already used it.
    fn free_name(&self, dir: u32, ino: u32) -> Result<alloc::string::String, RepairError> {
        for attempt in 0..16u32 {
            let name = match attempt {
                0 => format!("#{ino}"),
                n => format!("#{ino}.{n}"),
            };
            match self.find_entry(dir, &name) {
                Err(Ext2Error::NotFound) => return Ok(name),
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
        }
        Err(refuse(format!(
            "no free name for inode {ino} in /lost+found"
        )))
    }

    /// The last round: free the unreachable inodes and the leaked blocks, set
    /// link counts and `i_blocks`, and recompute the counters.
    pub(super) fn finish(&self, scan: &Scan, report: &mut RepairReport) -> Result<(), RepairError> {
        for &ino in &scan.to_free {
            self.free_unreachable(ino)?;
            report.freed_inodes.push(ino);
        }
        self.free_leaked_blocks(scan, report)?;
        for ino in 1..=self.inodes_count {
            if !scan.visited.get(ino) {
                continue;
            }
            let mut inode = self.read_inode(ino)?;
            let links = le16(&inode, INO_LINKS);
            let entries = u16::try_from(scan.entries[ino as usize]).unwrap_or(u16::MAX);
            if links != entries {
                put16(&mut inode, INO_LINKS, entries);
                self.write_inode(ino, &inode)?;
                report.link_counts.push((ino, links, entries));
            }
        }
        for &(ino, sectors) in &scan.block_counts {
            let mut inode = self.read_inode(ino)?;
            put32(&mut inode, INO_BLOCKS, sectors);
            self.write_inode(ino, &inode)?;
            report.block_counts.push(ino);
        }
        self.recount(report)
    }

    /// Return an unreachable inode to its bitmap, cleared like a deleted one
    /// (its blocks are leaked blocks, freed by the bitmap pass).
    fn free_unreachable(&self, ino: u32) -> Result<(), RepairError> {
        let mut inode = self.read_inode(ino)?;
        put16(&mut inode, INO_LINKS, 0);
        if le32(&inode, INO_DTIME) == 0 {
            put32(&mut inode, INO_DTIME, self.now().max(1));
        }
        put32(&mut inode, INO_SIZE, 0);
        put32(&mut inode, INO_BLOCKS, 0);
        put32(&mut inode, INO_DIR_ACL, 0);
        inode[INO_BLOCK..INO_BLOCK + BLOCK_SLOTS as usize * 4].fill(0);
        self.write_inode(ino, &inode)?;
        let index = ino - 1;
        let desc = self.read_group(index / self.inodes_per_group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
        Self::bitmap_clear(&mut bitmap[..size], index % self.inodes_per_group)?;
        self.write_block(u64::from(desc.inode_bitmap), &bitmap[..size])?;
        Ok(())
    }

    /// Clear the bitmap bit of every block marked used that nothing claimed.
    fn free_leaked_blocks(
        &self,
        scan: &Scan,
        report: &mut RepairReport,
    ) -> Result<(), RepairError> {
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        for group in 0..self.groups {
            let start = self.first_data_block + group * self.blocks_per_group;
            let span = self.blocks_per_group.min(self.blocks_count - start);
            let leaked = (0..span)
                .filter(|&bit| scan.block_used.get(start + bit) && !scan.claimed.get(start + bit));
            let mut changed = false;
            let desc = self.read_group(group)?;
            for bit in leaked {
                if !changed {
                    self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
                    changed = true;
                }
                Self::bitmap_clear(&mut bitmap[..size], bit)?;
                report.leaked_blocks.push(start + bit);
            }
            if changed {
                self.write_block(u64::from(desc.block_bitmap), &bitmap[..size])?;
            }
        }
        Ok(())
    }

    /// Recompute every group's free and directory counts, and the
    /// superblock's totals, from the bitmaps.
    pub(super) fn recount(&self, report: &mut RepairReport) -> Result<(), RepairError> {
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        let (mut free_blocks, mut free_inodes) = (0u32, 0u32);
        for group in 0..self.groups {
            let mut desc = self.read_group(group)?;
            let start = self.first_data_block + group * self.blocks_per_group;
            let span = self.blocks_per_group.min(self.blocks_count - start);
            self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
            let blocks = count_clear(&bitmap[..size], span)?;
            let base = group * self.inodes_per_group;
            let count = self.inodes_per_group.min(self.inodes_count - base);
            self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
            let inodes = count_clear(&bitmap[..size], count)?;
            let mut dirs = 0u32;
            for bit in 0..count {
                if Self::bitmap_test(&bitmap[..size], bit)? {
                    let mode = le16(&self.read_inode(base + bit + 1)?, INO_MODE);
                    dirs += u32::from(kind_from_mode(mode) == Some(FileKind::Dir));
                }
            }
            free_blocks += blocks;
            free_inodes += inodes;
            let wanted = (to_u16(blocks)?, to_u16(inodes)?, to_u16(dirs)?);
            if (desc.free_blocks, desc.free_inodes, desc.used_dirs) != wanted {
                (desc.free_blocks, desc.free_inodes, desc.used_dirs) = wanted;
                self.write_group(group, &desc)?;
                if !report.group_counters.first.contains(&group) {
                    report.group_counters.push(group);
                }
            }
        }
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        if le32(&raw, SB_FREE_BLOCKS) != free_blocks || le32(&raw, SB_FREE_INODES) != free_inodes {
            put32(&mut raw, SB_FREE_BLOCKS, free_blocks);
            put32(&mut raw, SB_FREE_INODES, free_inodes);
            self.write_super_raw(&raw)?;
            report.super_counters = true;
        }
        Ok(())
    }
}

/// Clear bits among the first `bits` of a bitmap.
fn count_clear(bitmap: &[u8], bits: u32) -> Result<u32, RepairError> {
    let mut clear = 0;
    for bit in 0..bits {
        clear += u32::from(!Ext2::bitmap_test(bitmap, bit)?);
    }
    Ok(clear)
}

fn to_u16(count: u32) -> Result<u16, RepairError> {
    u16::try_from(count).map_err(|_| refuse("a group counter does not fit its field"))
}
