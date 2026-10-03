//! The scan's tree walk: claim every block a live inode owns (refusing a
//! block that is out of range, free, metadata, or already claimed), parse
//! every directory record (refusing a garbled one), and count the names.

use alloc::format;
use alloc::vec::Vec;

use super::scan::{Record, Scan};
use super::*;

/// What a directory record is, by name.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Dot,
    DotDot,
    Other,
}

impl Ext2 {
    /// Walk the directory tree under `top` (already marked visited).
    pub(super) fn walk_from(
        &self,
        scan: &mut Scan,
        top: u32,
        orphan_root: bool,
    ) -> Result<(), RepairError> {
        let inode = self.read_inode(top)?;
        let blocks = self.visit(scan, top, &inode, true)?;
        let info = scan.dirs.entry(top).or_default();
        info.blocks = blocks;
        info.orphan_root = orphan_root;
        let mut stack = alloc::vec![top];
        while let Some(dir) = stack.pop() {
            let blocks = scan
                .dirs
                .get(&dir)
                .map(|info| info.blocks.clone())
                .unwrap_or_default();
            for (class, record) in self.dir_records(dir, &blocks)? {
                if let Some(child) = self.take_record(scan, class, record)? {
                    stack.push(child);
                }
            }
        }
        Ok(())
    }

    /// Account for one record of a walked directory; returns a directory to
    /// walk next.
    fn take_record(
        &self,
        scan: &mut Scan,
        class: Class,
        record: Record,
    ) -> Result<Option<u32>, RepairError> {
        let (dir, child) = (record.dir, record.child);
        match class {
            Class::Dot if child != dir => {
                Err(refuse(format!("`.` of directory {dir} names {child}")))
            }
            Class::DotDot if child == 0 || child > self.inodes_count => Err(refuse(format!(
                "`..` of directory {dir} names inode {child}"
            ))),
            Class::Dot | Class::DotDot => {
                if class == Class::DotDot {
                    scan.dirs.entry(dir).or_default().dotdot = Some(record);
                }
                scan.entries[child as usize] = scan.entries[child as usize].saturating_add(1);
                Ok(None)
            }
            Class::Other if child == 0 => Ok(None),
            Class::Other => self.take_entry(scan, record),
        }
    }

    /// A named entry: dead inodes lose it, live ones are counted and walked.
    fn take_entry(&self, scan: &mut Scan, record: Record) -> Result<Option<u32>, RepairError> {
        let (dir, child) = (record.dir, record.child);
        if child > self.inodes_count {
            return Err(refuse(format!(
                "an entry in directory {dir} names inode {child}, past the table"
            )));
        }
        if !scan.inode_used.get(child) {
            return Err(refuse(format!(
                "inode {child} is reachable from directory {dir} but marked free"
            )));
        }
        let inode = self.read_inode(child)?;
        let Some(kind) = live_kind(&inode) else {
            if child < self.first_ino {
                return Err(refuse(format!(
                    "directory {dir} names reserved inode {child}"
                )));
            }
            scan.dead.push(record);
            return Ok(None);
        };
        scan.entries[child as usize] = scan.entries[child as usize].saturating_add(1);
        let is_dir = kind == FileKind::Dir;
        if is_dir {
            scan.dirs.entry(child).or_default().parents.push(record);
        }
        if scan.visited.set(child) {
            return Ok(None);
        }
        let blocks = self.visit(scan, child, &inode, is_dir)?;
        if !is_dir {
            return Ok(None);
        }
        scan.dirs.entry(child).or_default().blocks = blocks;
        Ok(Some(child))
    }

    /// Claim what `inode` owns, note an `i_blocks` (or, for a directory, an
    /// `i_size`) that disagrees, and return a directory's data blocks.
    pub(super) fn visit(
        &self,
        scan: &mut Scan,
        ino: u32,
        inode: &[u8; INODE_CORE_SIZE],
        is_dir: bool,
    ) -> Result<Vec<u32>, RepairError> {
        if le32(inode, INO_FILE_ACL) != 0 {
            return Err(refuse(format!(
                "inode {ino} has an extended attribute block"
            )));
        }
        let mut data = Vec::new();
        let mut owned = 0u32;
        for slot in 0..BLOCK_SLOTS {
            let root = Self::direct_ptr(inode, slot);
            if root == 0 {
                continue;
            }
            let depth = slot.saturating_sub(SINGLE_INDIRECT_SLOT - 1) as usize;
            if is_dir && depth > 1 {
                return Err(refuse(format!(
                    "directory {ino} uses a double-indirect block"
                )));
            }
            let mut walk = Claim {
                scan,
                ino,
                owned: &mut owned,
                data: &mut data,
                collect: is_dir,
            };
            self.claim_tree(&mut walk, root, depth, slot)?;
        }
        let sectors = owned.saturating_mul(self.block_size / SECTOR_SIZE as u32);
        if le32(inode, INO_BLOCKS) != sectors {
            scan.block_counts.push((ino, sectors));
        }
        if !is_dir {
            return Ok(Vec::new());
        }
        // Directories are dense: a hole is not a crash shape.
        if data.is_empty()
            || data
                .iter()
                .enumerate()
                .any(|(index, &(logical, _))| logical != index as u32)
        {
            return Err(refuse(format!("directory {ino} has a hole or no blocks")));
        }
        let size = (data.len() as u32).saturating_mul(self.block_size);
        if le32(inode, INO_SIZE) != size {
            scan.sizes.push((ino, size));
        }
        Ok(data.into_iter().map(|(_, block)| block).collect())
    }

    /// Claim `block` and, below it, `depth` levels of pointer tables. `index`
    /// is the logical block of a data block (only tracked to depth one, all a
    /// directory may use).
    fn claim_tree(
        &self,
        walk: &mut Claim<'_>,
        block: u32,
        depth: usize,
        index: u32,
    ) -> Result<(), RepairError> {
        let ino = walk.ino;
        if block < self.first_data_block || block >= self.blocks_count {
            return Err(refuse(format!("inode {ino}: block {block} out of range")));
        }
        if walk.scan.claimed.set(block) {
            return Err(refuse(format!(
                "block {block} of inode {ino} is claimed twice or is metadata"
            )));
        }
        if !walk.scan.block_used.get(block) {
            return Err(refuse(format!(
                "block {block} of inode {ino} is in use but marked free"
            )));
        }
        *walk.owned += 1;
        if depth == 0 {
            if walk.collect {
                walk.data.push((index, block));
            }
            return Ok(());
        }
        for (offset, child) in self.read_table(block)?.into_iter().enumerate() {
            if child != 0 {
                self.claim_tree(walk, child, depth - 1, index.wrapping_add(offset as u32))?;
            }
        }
        Ok(())
    }

    /// Every record of directory `dir`, validated as the driver validates
    /// them (a garbled one is refused).
    pub(super) fn dir_records(
        &self,
        dir: u32,
        blocks: &[u32],
    ) -> Result<Vec<(Class, Record)>, RepairError> {
        let size = self.block_size as usize;
        let mut buf = zeroed(u64::from(self.block_size))?;
        let mut out = Vec::new();
        for &block in blocks {
            self.read_block(u64::from(block), &mut buf)?;
            let mut offset = 0usize;
            while offset < size {
                let (rec_len, name_len) = record_shape(&buf, offset).ok_or_else(|| {
                    refuse(format!(
                        "bad directory record in block {block} of directory {dir}"
                    ))
                })?;
                let name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                let class = match name {
                    b"." => Class::Dot,
                    b".." => Class::DotDot,
                    _ => Class::Other,
                };
                let child = le32(&buf, offset + DE_INO);
                out.push((
                    class,
                    Record {
                        dir,
                        block,
                        offset,
                        child,
                    },
                ));
                offset += rec_len;
            }
        }
        Ok(out)
    }
}

/// The record at `offset` as `(rec_len, name_len)`, if it is well formed.
pub(super) fn record_shape(buf: &[u8], offset: usize) -> Option<(usize, usize)> {
    if offset + DE_HEADER > buf.len() {
        return None;
    }
    let rec_len = usize::from(le16(buf, offset + DE_REC_LEN));
    let name_len = usize::from(buf[offset + DE_NAME_LEN]);
    let fits = rec_len >= DE_HEADER
        && rec_len.is_multiple_of(4)
        && offset + rec_len <= buf.len()
        && name_len <= rec_len - DE_HEADER;
    fits.then_some((rec_len, name_len))
}

/// The state one inode's block walk carries.
struct Claim<'a> {
    scan: &'a mut Scan,
    ino: u32,
    owned: &'a mut u32,
    data: &'a mut Vec<(u32, u32)>,
    collect: bool,
}
