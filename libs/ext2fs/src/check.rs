//! An fsck-style consistency checker over raw image bytes, for tests and the
//! fuzz entry point.
//!
//! It deliberately shares no code with the driver: it re-reads the superblock,
//! descriptors, bitmaps and inodes from the bytes and walks the tree from the
//! root, so a bug in the driver's bookkeeping cannot hide behind the same bug
//! in the checker. [`fsck`] returns one line per violated invariant:
//!
//! * every block reachable from an inode is marked used, claimed once, and
//!   every used block is reachable or metadata (no leaks);
//! * every inode reachable from the root is marked used, and every used inode
//!   past the reserved ones is reachable;
//! * `i_links_count` equals the number of directory entries naming the inode
//!   (`.` and `..` included), and `i_blocks` matches the blocks owned;
//! * the group free counters, directory counts and the superblock totals match
//!   the bitmaps.

use std::collections::BTreeMap;
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::{S_IFDIR, S_IFMT, S_IFREG};

fn le16(d: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([d[at], d[at + 1]]))
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

/// What block `n` is used for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Free,
    Metadata,
    Owned(u32),
}

struct Group {
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    free_blocks: u32,
    free_inodes: u32,
    used_dirs: u32,
}

struct Checker<'a> {
    d: &'a [u8],
    bs: usize,
    blocks: u32,
    first_data: u32,
    bpg: u32,
    ipg: u32,
    inodes: u32,
    first_ino: u32,
    inode_size: usize,
    groups: Vec<Group>,
    uses: Vec<Use>,
    problems: Vec<String>,
}

/// Check the volume in `image`; an empty result means it is consistent.
pub fn fsck(image: &[u8]) -> Vec<String> {
    let mut checker = match Checker::new(image) {
        Ok(checker) => checker,
        Err(problem) => return std::vec![problem],
    };
    checker.run();
    checker.problems
}

impl<'a> Checker<'a> {
    fn new(d: &'a [u8]) -> Result<Self, String> {
        if d.len() < 2048 || le16(d, 1024 + 0x38) != 0xEF53 {
            return Err("no ext2 superblock".to_string());
        }
        let sb = |at: usize| le32(d, 1024 + at);
        let bs = 1024usize << sb(0x18);
        let (blocks, first_data, bpg, ipg) = (sb(0x04), sb(0x14), sb(0x20), sb(0x28));
        let group_count = (blocks - first_data).div_ceil(bpg) as usize;
        let gdt = (first_data as usize + 1) * bs;
        let groups = (0..group_count)
            .map(|g| {
                let at = gdt + g * 32;
                Group {
                    block_bitmap: le32(d, at),
                    inode_bitmap: le32(d, at + 4),
                    inode_table: le32(d, at + 8),
                    free_blocks: le16(d, at + 0x0C),
                    free_inodes: le16(d, at + 0x0E),
                    used_dirs: le16(d, at + 0x10),
                }
            })
            .collect();
        Ok(Checker {
            d,
            bs,
            blocks,
            first_data,
            bpg,
            ipg,
            inodes: sb(0x00),
            first_ino: sb(0x54),
            inode_size: sb(0x58) as usize & 0xFFFF,
            groups,
            uses: std::vec![Use::Free; blocks as usize],
            problems: Vec::new(),
        })
    }

    fn run(&mut self) {
        self.mark_metadata();
        let links = self.walk_tree();
        self.check_inode_bitmaps(&links);
        self.check_block_bitmaps();
        self.check_counters();
    }

    fn problem(&mut self, text: String) {
        if self.problems.len() < 50 {
            self.problems.push(text);
        }
    }

    fn block(&self, n: u32) -> &'a [u8] {
        &self.d[n as usize * self.bs..(n as usize + 1) * self.bs]
    }

    fn bit(bitmap: &[u8], index: u32) -> bool {
        bitmap[(index / 8) as usize] & (1 << (index % 8)) != 0
    }

    /// Superblock, descriptor copies, bitmaps and inode tables.
    fn mark_metadata(&mut self) {
        let gdt_blocks = (self.groups.len() * 32).div_ceil(self.bs) as u32;
        let mut spans = Vec::new();
        for (index, group) in self.groups.iter().enumerate() {
            let start = self.first_data + index as u32 * self.bpg;
            if crate::geometry::has_backup(index as u32) {
                spans.push((start, 1 + gdt_blocks));
            }
            let table = (self.ipg as usize * self.inode_size).div_ceil(self.bs) as u32;
            spans.push((group.block_bitmap, 1));
            spans.push((group.inode_bitmap, 1));
            spans.push((group.inode_table, table));
        }
        if self.first_data == 1 {
            spans.push((0, 1)); // the boot block
        }
        for (start, count) in spans {
            for n in start..start + count {
                match self.uses.get_mut(n as usize) {
                    Some(slot) if *slot == Use::Free => *slot = Use::Metadata,
                    _ => self.problem(format!("metadata block {n} out of range or doubly used")),
                }
            }
        }
    }

    fn inode(&self, ino: u32) -> &'a [u8] {
        let index = ino - 1;
        let group = &self.groups[(index / self.ipg) as usize];
        let at =
            group.inode_table as usize * self.bs + (index % self.ipg) as usize * self.inode_size;
        &self.d[at..at + 128]
    }

    /// Walk the tree from the root; returns the directory-entry count per inode.
    fn walk_tree(&mut self) -> BTreeMap<u32, u32> {
        let mut links: BTreeMap<u32, u32> = BTreeMap::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut queue = std::vec![2u32];
        links.insert(2, 0);
        while let Some(ino) = queue.pop() {
            if !seen.insert(ino) {
                continue;
            }
            let inode = self.inode(ino);
            let kind = le16(inode, 0) & u32::from(S_IFMT);
            let blocks = self.claim_blocks(ino, inode);
            if kind == u32::from(S_IFREG) {
                continue;
            }
            if kind != u32::from(S_IFDIR) {
                self.problem(format!("inode {ino} has type {kind:o}"));
                continue;
            }
            if le32(inode, 4) as usize != blocks.len() * self.bs {
                self.problem(format!("directory {ino} size does not match its blocks"));
            }
            for block in blocks {
                for (name, child) in self.entries(block) {
                    *links.entry(child).or_insert(0) += 1;
                    if name != "." && name != ".." {
                        queue.push(child);
                    }
                }
            }
        }
        links
    }

    /// The live entries of one directory block.
    fn entries(&mut self, block: u32) -> Vec<(String, u32)> {
        let data = self.block(block);
        let (mut offset, mut out) = (0usize, Vec::new());
        while offset < self.bs {
            let rec_len = le16(data, offset + 4) as usize;
            let name_len = data[offset + 6] as usize;
            if rec_len < 8
                || !rec_len.is_multiple_of(4)
                || offset + rec_len > self.bs
                || 8 + name_len > rec_len
            {
                self.problem(format!("bad directory record in block {block}"));
                break;
            }
            let ino = le32(data, offset);
            if ino != 0 {
                let name = String::from_utf8_lossy(&data[offset + 8..offset + 8 + name_len]);
                if ino > self.inodes {
                    self.problem(format!("entry {name} names inode {ino} past the table"));
                } else {
                    out.push((name.into_owned(), ino));
                }
            }
            offset += rec_len;
        }
        out
    }

    /// Claim every block `inode` owns (data and tables), checking `i_blocks`.
    /// Returns the data blocks in logical order (holes skipped).
    fn claim_blocks(&mut self, ino: u32, inode: &'a [u8]) -> Vec<u32> {
        let mut data = Vec::new();
        let mut owned = 0u32;
        for slot in 0..15usize {
            let root = le32(inode, 0x28 + slot * 4);
            if root != 0 {
                let depth = slot.saturating_sub(11);
                self.claim_tree(ino, root, depth, &mut data, &mut owned);
            }
        }
        let expected = owned * (self.bs as u32 / 512);
        if le32(inode, 0x1C) != expected {
            self.problem(format!(
                "inode {ino}: i_blocks {} but owns {expected} sectors",
                le32(inode, 0x1C)
            ));
        }
        data
    }

    fn claim_tree(
        &mut self,
        ino: u32,
        block: u32,
        depth: usize,
        data: &mut Vec<u32>,
        owned: &mut u32,
    ) {
        if block < self.first_data || block >= self.blocks {
            self.problem(format!("inode {ino}: block {block} out of range"));
            return;
        }
        match self.uses[block as usize] {
            Use::Free => self.uses[block as usize] = Use::Owned(ino),
            Use::Metadata => {
                return self.problem(format!("inode {ino} claims metadata block {block}"))
            }
            Use::Owned(other) => {
                return self.problem(format!("block {block} claimed by inodes {other} and {ino}"));
            }
        }
        *owned += 1;
        if depth == 0 {
            data.push(block);
            return;
        }
        let table = self.block(block);
        for index in 0..self.bs / 4 {
            let child = le32(table, index * 4);
            if child != 0 {
                self.claim_tree(ino, child, depth - 1, data, owned);
            }
        }
    }

    fn check_inode_bitmaps(&mut self, links: &BTreeMap<u32, u32>) {
        for ino in 1..=self.inodes {
            let group = &self.groups[((ino - 1) / self.ipg) as usize];
            let bitmap = self.block(group.inode_bitmap);
            let used = Self::bit(bitmap, (ino - 1) % self.ipg);
            let reserved = ino < self.first_ino;
            let reachable = links.contains_key(&ino);
            if reachable && !used {
                self.problem(format!("inode {ino} is reachable but marked free"));
            }
            if used && !reachable && !reserved {
                self.problem(format!("inode {ino} is marked used but unreachable"));
            }
            if reachable {
                let have = le16(self.inode(ino), 0x1A);
                if have != links[&ino] {
                    self.problem(format!(
                        "inode {ino}: links {have}, entries {}",
                        links[&ino]
                    ));
                }
            }
        }
    }

    fn check_block_bitmaps(&mut self) {
        for n in self.first_data..self.blocks {
            let index = n - self.first_data;
            let bitmap = self.block(self.groups[(index / self.bpg) as usize].block_bitmap);
            let used = Self::bit(bitmap, index % self.bpg);
            match (self.uses[n as usize], used) {
                (Use::Free, true) => self.problem(format!("block {n} marked used but unreachable")),
                (Use::Free, false) => {}
                (_, false) => self.problem(format!("block {n} is in use but marked free")),
                (_, true) => {}
            }
        }
    }

    fn check_counters(&mut self) {
        let (mut free_blocks, mut free_inodes) = (0u32, 0u32);
        for index in 0..self.groups.len() as u32 {
            let start = self.first_data + index * self.bpg;
            let span = self.bpg.min(self.blocks - start);
            let blocks = self.block(self.groups[index as usize].block_bitmap);
            let blocks_free = (0..span).filter(|&bit| !Self::bit(blocks, bit)).count() as u32;
            let inodes = self.block(self.groups[index as usize].inode_bitmap);
            let inodes_free = (0..self.ipg).filter(|&bit| !Self::bit(inodes, bit)).count() as u32;
            let dirs = (1..=self.ipg)
                .map(|local| index * self.ipg + local)
                .filter(|&ino| Self::bit(inodes, ino - 1 - index * self.ipg))
                .filter(|&ino| le16(self.inode(ino), 0) & u32::from(S_IFMT) == u32::from(S_IFDIR))
                .count() as u32;
            let group = &self.groups[index as usize];
            let (gb, gi, gd) = (group.free_blocks, group.free_inodes, group.used_dirs);
            if gb != blocks_free {
                self.problem(format!(
                    "group {index}: free_blocks {gb}, bitmap says {blocks_free}"
                ));
            }
            if gi != inodes_free {
                self.problem(format!(
                    "group {index}: free_inodes {gi}, bitmap says {inodes_free}"
                ));
            }
            if gd != dirs {
                self.problem(format!("group {index}: used_dirs {gd}, found {dirs}"));
            }
            free_blocks += blocks_free;
            free_inodes += inodes_free;
        }
        let (sb_blocks, sb_inodes) = (le32(self.d, 1024 + 0x0C), le32(self.d, 1024 + 0x10));
        if sb_blocks != free_blocks {
            self.problem(format!(
                "superblock free_blocks {sb_blocks}, groups say {free_blocks}"
            ));
        }
        if sb_inodes != free_inodes {
            self.problem(format!(
                "superblock free_inodes {sb_inodes}, groups say {free_inodes}"
            ));
        }
    }
}
