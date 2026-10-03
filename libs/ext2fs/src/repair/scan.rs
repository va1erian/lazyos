//! One read-only pass over the volume: the bitmaps, the metadata, the tree
//! from the root, then whatever is allocated but unreachable. It refuses the
//! damage no crash leaves and lists the work a round of [`Ext2::repair`] does.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::vec::Vec;

use super::bits::Bits;
use super::*;

/// One directory record: where it is and which inode it names.
#[derive(Clone, Copy, Debug)]
pub(super) struct Record {
    pub dir: u32,
    pub block: u32,
    pub offset: usize,
    pub child: u32,
}

/// What the scan knows about one directory it walked.
#[derive(Default)]
pub(super) struct DirInfo {
    /// The entries (other than `.`/`..`) naming it.
    pub parents: Vec<Record>,
    /// Its `..` record.
    pub dotdot: Option<Record>,
    /// Its data blocks, in logical order.
    pub blocks: Vec<u32>,
    /// It was reached from no named directory: it is to be linked into
    /// `/lost+found`, so its own `..` and names are settled after that.
    pub orphan_root: bool,
}

pub(super) struct Scan {
    /// Blocks reached: metadata and everything a walked inode owns.
    pub claimed: Bits,
    /// The block bitmaps as found.
    pub block_used: Bits,
    /// The inode bitmaps as found (indexed by inode number).
    pub inode_used: Bits,
    /// Inodes walked (from the root, or from an orphan root).
    pub visited: Bits,
    /// Entries naming each inode, `.` and `..` included.
    pub entries: Vec<u32>,
    pub dirs: BTreeMap<u32, DirInfo>,
    /// Entries naming a dead inode, to remove.
    pub dead: Vec<Record>,
    /// Extra names of a directory, to remove.
    pub extra: Vec<Record>,
    /// `(directory, parent)`: `..` to repoint.
    pub dotdot: Vec<(u32, u32)>,
    /// `(directory, size)`: `i_size` to correct.
    pub sizes: Vec<(u32, u32)>,
    /// `(inode, sectors)`: `i_blocks` to correct.
    pub block_counts: Vec<(u32, u32)>,
    /// Unreachable inodes holding data, as `(inode, is_dir)`.
    pub orphans: Vec<(u32, bool)>,
    /// Unreachable inodes to free.
    pub to_free: Vec<u32>,
}

impl Scan {
    /// Whether this round fixes entries (which must land before any free).
    pub(super) fn has_entry_work(&self) -> bool {
        !(self.dead.is_empty()
            && self.extra.is_empty()
            && self.dotdot.is_empty()
            && self.sizes.is_empty())
    }
}

impl Ext2 {
    pub(super) fn scan(&self) -> Result<Scan, RepairError> {
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        let compat = le32(&raw, SB_FEATURE_COMPAT);
        if compat & !KNOWN_COMPAT != 0 {
            return Err(refuse(format!("compatible features {compat:#x}")));
        }
        let sparse = le32(&raw, SB_FEATURE_RO_COMPAT) & FEATURE_RO_SPARSE_SUPER != 0;
        let mut entries = Vec::new();
        let inode_slots = self.inodes_count as usize + 1;
        if entries.try_reserve_exact(inode_slots).is_err() {
            return Err(refuse("not enough memory for the link counts"));
        }
        entries.resize(inode_slots, 0);
        let mut scan = Scan {
            claimed: Bits::new(self.blocks_count)?,
            block_used: Bits::new(self.blocks_count)?,
            inode_used: Bits::new(self.inodes_count + 1)?,
            visited: Bits::new(self.inodes_count + 1)?,
            entries,
            dirs: BTreeMap::new(),
            dead: Vec::new(),
            extra: Vec::new(),
            dotdot: Vec::new(),
            sizes: Vec::new(),
            block_counts: Vec::new(),
            orphans: Vec::new(),
            to_free: Vec::new(),
        };
        self.load_bitmaps(&mut scan)?;
        self.claim_metadata(&mut scan, sparse)?;
        self.check_reserved()?;
        let root = self.read_inode(ROOT_INO)?;
        if live_kind(&root) != Some(FileKind::Dir) || !scan.inode_used.get(ROOT_INO) {
            return Err(refuse("the root is not a live directory"));
        }
        scan.visited.set(ROOT_INO);
        self.walk_from(&mut scan, ROOT_INO, false)?;
        self.find_orphans(&mut scan)?;
        settle_names(&mut scan)?;
        Ok(scan)
    }

    /// Copy the on-disk bitmaps (only the bits that name real blocks/inodes).
    fn load_bitmaps(&self, scan: &mut Scan) -> Result<(), RepairError> {
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        for group in 0..self.groups {
            let desc = self.read_group(group)?;
            // A hostile superblock can describe groups that start past the end
            // of the volume; no crash leaves that, so refuse rather than wrap.
            let start = group
                .checked_mul(self.blocks_per_group)
                .and_then(|offset| offset.checked_add(self.first_data_block))
                .filter(|start| *start < self.blocks_count)
                .ok_or_else(|| refuse(format!("block group {group} starts past the volume")))?;
            let base = group
                .checked_mul(self.inodes_per_group)
                .filter(|base| *base < self.inodes_count)
                .ok_or_else(|| {
                    refuse(format!("block group {group}'s inodes lie past the count"))
                })?;
            let span = self.blocks_per_group.min(self.blocks_count - start);
            self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
            for bit in 0..span {
                if Self::bitmap_test(&bitmap[..size], bit)? {
                    scan.block_used.set(start + bit);
                }
            }
            self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
            let count = self.inodes_per_group.min(self.inodes_count - base);
            for bit in 0..count {
                if Self::bitmap_test(&bitmap[..size], bit)? {
                    scan.inode_used.set(base + bit + 1);
                }
            }
        }
        Ok(())
    }

    /// Claim the superblock and descriptor copies, the bitmaps and the inode
    /// tables, which must be marked used and must not overlap.
    fn claim_metadata(&self, scan: &mut Scan, sparse: bool) -> Result<(), RepairError> {
        let gdt_blocks = (self.groups as usize * GD_SIZE).div_ceil(self.block_size as usize) as u32;
        let table_blocks = (self.inodes_per_group as usize * usize::from(self.inode_size))
            .div_ceil(self.block_size as usize) as u32;
        let mut spans = Vec::new();
        for group in 0..self.groups {
            let desc = self.read_group(group)?;
            let start = self.first_data_block + group * self.blocks_per_group;
            if !sparse || geometry::has_backup(group) {
                spans.push((start, 1 + gdt_blocks));
            }
            spans.push((desc.block_bitmap, 1));
            spans.push((desc.inode_bitmap, 1));
            spans.push((desc.inode_table, table_blocks));
        }
        if self.first_data_block == 1 {
            spans.push((0, 1)); // the boot block, outside every group
        }
        for (start, count) in spans {
            for block in start..start.saturating_add(count) {
                if block >= self.blocks_count || scan.claimed.set(block) {
                    return Err(refuse(format!(
                        "metadata block {block} out of range or doubly used"
                    )));
                }
                if block >= self.first_data_block && !scan.block_used.get(block) {
                    return Err(refuse(format!("metadata block {block} is marked free")));
                }
            }
        }
        Ok(())
    }

    /// The reserved inodes (bad blocks, journal, resize, ...) must own no
    /// blocks: the repair cannot tell what they are for, so it would free them.
    fn check_reserved(&self) -> Result<(), RepairError> {
        for ino in (1..self.first_ino).filter(|&ino| ino != ROOT_INO) {
            let inode = self.read_inode(ino)?;
            if (0..BLOCK_SLOTS).any(|slot| Self::direct_ptr(&inode, slot) != 0) {
                return Err(refuse(format!("reserved inode {ino} owns blocks")));
            }
        }
        Ok(())
    }

    /// Sort the allocated inodes the root walk did not reach: free the dead
    /// and the empty, walk the rest as orphans for `/lost+found`.
    fn find_orphans(&self, scan: &mut Scan) -> Result<(), RepairError> {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for ino in self.first_ino..=self.inodes_count {
            if !scan.inode_used.get(ino) || scan.visited.get(ino) {
                continue;
            }
            let inode = self.read_inode(ino)?;
            match live_kind(&inode) {
                None => scan.to_free.push(ino),
                Some(FileKind::Dir) => dirs.push(ino),
                Some(FileKind::File) => {
                    let pointers = (0..BLOCK_SLOTS).any(|slot| Self::direct_ptr(&inode, slot) != 0);
                    if self.file_size(&inode) == 0 && !pointers {
                        scan.to_free.push(ino);
                    } else {
                        files.push(ino);
                    }
                }
            }
        }
        self.walk_orphan_dirs(scan, &dirs)?;
        for ino in files {
            if !scan.visited.set(ino) {
                let inode = self.read_inode(ino)?;
                self.visit(scan, ino, &inode, false)?;
                scan.orphans.push((ino, false));
            }
        }
        Ok(())
    }

    /// Walk the unreachable directories from the tops of their subtrees (the
    /// ones no other unreachable directory names); an empty top is freed. A
    /// cycle has no top, so its lowest inode stands in.
    fn walk_orphan_dirs(&self, scan: &mut Scan, dirs: &[u32]) -> Result<(), RepairError> {
        let candidates: BTreeSet<u32> = dirs.iter().copied().collect();
        let mut named = BTreeSet::new();
        let mut empty = BTreeSet::new();
        for &dir in dirs {
            let (children, live) = self.peek_children(scan, dir)?;
            named.extend(
                children
                    .into_iter()
                    .filter(|&child| child != dir && candidates.contains(&child)),
            );
            if !live {
                empty.insert(dir);
            }
        }
        let mut roots: Vec<u32> = dirs
            .iter()
            .copied()
            .filter(|dir| !named.contains(dir))
            .collect();
        loop {
            for root in roots.drain(..) {
                if scan.visited.get(root) {
                    continue;
                }
                if empty.contains(&root) {
                    scan.to_free.push(root);
                    continue;
                }
                scan.visited.set(root);
                self.walk_from(scan, root, true)?;
                scan.orphans.push((root, true));
            }
            let left = dirs
                .iter()
                .copied()
                .find(|&dir| !scan.visited.get(dir) && !scan.to_free.contains(&dir));
            match left {
                Some(dir) => roots.push(dir),
                None => return Ok(()),
            }
        }
    }

    /// The inodes an unreachable directory names (besides `.`/`..`), and
    /// whether any of them is live. Nothing is claimed: an empty directory
    /// is freed, not walked. One whose blocks cannot be listed counts as
    /// holding something, so the walk (which refuses or repairs) sees it.
    fn peek_children(&self, scan: &Scan, dir: u32) -> Result<(Vec<u32>, bool), RepairError> {
        let inode = self.read_inode(dir)?;
        let Ok(blocks) = self.dir_blocks(&inode) else {
            return Ok((Vec::new(), true));
        };
        let mut children = Vec::new();
        let mut live = false;
        for (class, record) in self.dir_records(dir, &blocks)? {
            if class != walk::Class::Other || record.child == 0 {
                continue;
            }
            children.push(record.child);
            if record.child <= self.inodes_count && scan.inode_used.get(record.child) {
                live |= live_kind(&self.read_inode(record.child)?).is_some();
            }
        }
        Ok((children, live))
    }
}

/// Decide each walked directory's one name: with two (a rename cut short) the
/// one its `..` agrees with stays; with one, `..` follows it. Orphan roots
/// are settled after they are linked into `/lost+found`.
fn settle_names(scan: &mut Scan) -> Result<(), RepairError> {
    let (mut dotdots, mut extra) = (Vec::new(), Vec::new());
    for (&ino, info) in &scan.dirs {
        if info.orphan_root {
            continue;
        }
        let Some(dotdot) = info.dotdot else {
            return Err(refuse(format!("directory {ino} has no `..`")));
        };
        if ino == ROOT_INO {
            if !info.parents.is_empty() || dotdot.child != ROOT_INO {
                return Err(refuse("the root is named by another directory"));
            }
            continue;
        }
        match info.parents.as_slice() {
            [] => {
                return Err(refuse(format!(
                    "directory {ino} was reached without a name"
                )))
            }
            [only] => {
                if only.dir != dotdot.child {
                    dotdots.push((ino, only.dir));
                }
            }
            names => {
                let keep = names
                    .iter()
                    .position(|name| name.dir == dotdot.child)
                    .ok_or_else(|| {
                        refuse(format!(
                            "directory {ino} has {} names, none its `..`",
                            names.len()
                        ))
                    })?;
                extra.extend(
                    names
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| *index != keep)
                        .map(|(_, name)| *name),
                );
            }
        }
    }
    scan.dotdot = dotdots;
    scan.extra = extra;
    Ok(())
}
