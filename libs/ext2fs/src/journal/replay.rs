//! Mount-time recovery: find the committed transactions in the log and write
//! their blocks to their home locations.
//!
//! Three walks over the log, as jbd2 does: the first finds where the
//! committed transactions end, the second collects revoke records, the
//! third copies every logged block that was not revoked later. Every loop is
//! bounded by the journal's length, so a hostile log cannot hang the mount.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::*;

use super::*;

/// One descriptor or revoke block met while walking the log.
enum Event<'a> {
    /// A descriptor: transaction id, log index of the first data block, and
    /// its tags (home block, flags).
    Descriptor(u32, usize, Vec<(u32, u16)>),
    /// A revoke block: transaction id and the block itself.
    Revoke(u32, &'a [u8]),
}

/// Whether transaction id `a` is the same as or later than `b` (ids wrap).
fn at_or_after(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) < u32::MAX / 2
}

impl Ext2 {
    /// The physical blocks of the journal inode, in log order, validated to
    /// lie inside the volume.
    pub(crate) fn journal_blocks(&self) -> Result<Vec<u64>, Ext2Error> {
        let inode = self.read_inode(JOURNAL_INO)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(Ext2Error::Invalid);
        }
        let count = u64::from(le32(&inode, INO_SIZE)) / u64::from(self.block_size);
        if count < 16 || count > u64::from(self.blocks_count) {
            return Err(Ext2Error::Invalid);
        }
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(count as usize)
            .map_err(|_| Ext2Error::NoSpace)?;
        for index in 0..count as u32 {
            let block = self.block_map(&inode, index)?;
            if block < self.first_data_block || block >= self.blocks_count {
                return Err(Ext2Error::Invalid);
            }
            blocks.push(u64::from(block));
        }
        Ok(blocks)
    }

    /// The journal as the cache commit path drives it. Recovery has
    /// already emptied the log, so a log that still holds a transaction is
    /// refused rather than overwritten.
    pub(crate) fn load_journal(&self) -> Result<Journal, Ext2Error> {
        let mut blocks = self.journal_blocks()?;
        let mut sb_block = vec![0u8; self.block_size as usize];
        self.read_block(blocks[0], &mut sb_block)?;
        let sb = JournalSb::parse(&sb_block, self.block_size, blocks.len() as u32)?;
        if sb.start != 0 {
            return Err(Ext2Error::Invalid);
        }
        blocks.truncate(sb.maxlen as usize);
        Ok(Journal {
            blocks,
            first: sb.first,
            sequence: sb.sequence,
            uuid: self.uuid,
            sb: sb_block,
            aborted: false,
        })
    }

    /// Check the journal fields of the superblock and replay the log when
    /// the last mount left work in it. Called by [`Ext2::open`] with the
    /// cache not yet installed, so everything goes straight to the device.
    pub(crate) fn recover_journal(&mut self, superblock: &[u8; 1024]) -> Result<(), Ext2Error> {
        let external = le32(superblock, SB_JOURNAL_DEV) != 0
            || superblock[SB_JOURNAL_UUID..SB_JOURNAL_UUID + 16]
                .iter()
                .any(|&byte| byte != 0);
        if le32(superblock, SB_JOURNAL_INUM) != JOURNAL_INO || external {
            return Err(Ext2Error::NotSupported);
        }
        let blocks = self.journal_blocks()?;
        let size = self.block_size as usize;
        let mut buf = vec![0u8; size];
        self.read_block(blocks[0], &mut buf)?;
        let sb = JournalSb::parse(&buf, self.block_size, blocks.len() as u32)?;
        let flagged = le32(superblock, SB_FEATURE_INCOMPAT) & FEATURE_INCOMPAT_RECOVER != 0;
        if sb.start == 0 && !flagged {
            return Ok(());
        }
        // A device that cannot be written still shows the committed state:
        // the replay lands in a memory overlay and the device is untouched.
        let overlay = self.read_only.then(|| {
            let device = core::mem::replace(&mut self.io, Box::new(overlay::NoDevice));
            Box::new(overlay::Overlay::new(device))
        });
        if let Some(overlay) = overlay {
            self.io = overlay;
        }
        let end = if sb.start != 0 {
            self.recovered = true;
            self.replay(&blocks, &sb)?
        } else {
            sb.sequence
        };
        self.io.flush().map_err(io_error)?;
        // Empty the log, then clear the flag: a crash between the two leaves
        // a mount that replays nothing and finishes the job.
        // Skip the id of an attempt that never committed: its blocks may still
        // be in the log, and a new transaction must not share their id.
        put_be32(&mut buf, JS_SEQUENCE, end.wrapping_add(1));
        put_be32(&mut buf, JS_START, 0);
        self.write_direct(blocks[0], &buf)?;
        self.io.flush().map_err(io_error)?;
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?; // the replay may have rewritten it
        let incompat = le32(&raw, SB_FEATURE_INCOMPAT) & !FEATURE_INCOMPAT_RECOVER;
        put32(&mut raw, SB_FEATURE_INCOMPAT, incompat);
        // The log restored a state the driver committed: consistent.
        let state = le16(&raw, SB_STATE) | STATE_VALID;
        put16(&mut raw, SB_STATE, state);
        self.write_super_direct(&raw)?;
        self.io.flush().map_err(io_error)?;
        self.mount_state = state;
        *self.clean.get_mut() = true;
        Ok(())
    }

    /// Walk the log and copy the committed transactions home. Returns the
    /// id the next transaction takes.
    fn replay(&self, blocks: &[u64], sb: &JournalSb) -> Result<u32, Ext2Error> {
        let end = self.walk_log(blocks, sb, None, &mut |_| Ok(()))?;
        let mut revoked: BTreeMap<u32, u32> = BTreeMap::new();
        self.walk_log(blocks, sb, Some(end), &mut |event| {
            if let Event::Revoke(sequence, buf) = event {
                let used = (be32(buf, HEADER) as usize).clamp(HEADER + 4, buf.len());
                for at in (HEADER + 4..used).step_by(4) {
                    if at + 4 <= buf.len() {
                        let entry = revoked.entry(be32(buf, at)).or_insert(sequence);
                        if at_or_after(sequence, *entry) {
                            *entry = sequence;
                        }
                    }
                }
            }
            Ok(())
        })?;
        let size = self.block_size as usize;
        self.walk_log(blocks, sb, Some(end), &mut |event| {
            let Event::Descriptor(sequence, data, tags) = event else {
                return Ok(());
            };
            for (index, (home, flags)) in tags.into_iter().enumerate() {
                if revoked
                    .get(&home)
                    .is_some_and(|&revoke| at_or_after(revoke, sequence))
                {
                    continue;
                }
                if home >= self.blocks_count || blocks.contains(&u64::from(home)) {
                    return Err(Ext2Error::Invalid);
                }
                let mut buf = vec![0u8; size];
                self.read_block(blocks[wrap(data + index, sb)], &mut buf)?;
                if flags & TAG_ESCAPE != 0 {
                    put_be32(&mut buf, 0, MAGIC);
                }
                self.write_direct(u64::from(home), &buf)?;
            }
            Ok(())
        })?;
        Ok(end)
    }

    /// Walk the committed transactions (all of them, or those before
    /// `limit`), calling `visit` for each descriptor and revoke block.
    /// Returns the id of the first transaction without a commit block.
    fn walk_log(
        &self,
        blocks: &[u64],
        sb: &JournalSb,
        limit: Option<u32>,
        visit: &mut dyn FnMut(Event<'_>) -> Result<(), Ext2Error>,
    ) -> Result<u32, Ext2Error> {
        let size = self.block_size as usize;
        let mut buf = vec![0u8; size];
        let mut expect = sb.sequence;
        let mut pos = sb.start as usize;
        let mut steps = 0usize;
        while steps < blocks.len() {
            if limit == Some(expect) {
                break;
            }
            self.read_block(blocks[pos], &mut buf)?;
            match header(&buf) {
                Some((BLOCK_DESCRIPTOR, sequence)) if sequence == expect => {
                    let tags = parse_tags(&buf, size)?;
                    let count = tags.len();
                    visit(Event::Descriptor(sequence, wrap(pos + 1, sb), tags))?;
                    pos = wrap(pos + 1 + count, sb);
                    steps += 1 + count;
                }
                Some((BLOCK_REVOKE, sequence)) if sequence == expect => {
                    visit(Event::Revoke(sequence, &buf))?;
                    pos = wrap(pos + 1, sb);
                    steps += 1;
                }
                Some((BLOCK_COMMIT, sequence)) if sequence == expect => {
                    expect = expect.wrapping_add(1);
                    pos = wrap(pos + 1, sb);
                    steps += 1;
                }
                _ => break,
            }
        }
        Ok(expect)
    }

    /// Write one block straight to the device (no cache, no dirty marker).
    fn write_direct(&self, block: u64, buf: &[u8]) -> Result<(), Ext2Error> {
        self.io
            .write_sectors(block * u64::from(self.sectors_per_block), buf)
            .map_err(io_error)
    }

    fn write_super_direct(&self, raw: &[u8; 1024]) -> Result<(), Ext2Error> {
        let size = self.block_size as usize;
        let block = SUPER_OFFSET / u64::from(self.block_size);
        let mut buf = vec![0u8; size];
        self.read_block(block, &mut buf)?;
        let start = (SUPER_OFFSET % u64::from(self.block_size)) as usize;
        buf[start..start + 1024].copy_from_slice(raw);
        self.write_direct(block, &buf)
    }
}

/// The log index after `index`, wrapping from the end back to `first`.
fn wrap(index: usize, sb: &JournalSb) -> usize {
    let max = sb.maxlen as usize;
    if index >= max {
        index - max + sb.first as usize
    } else {
        index
    }
}

/// The tags of a descriptor block, refusing one that runs off the block.
fn parse_tags(buf: &[u8], size: usize) -> Result<Vec<(u32, u16)>, Ext2Error> {
    let mut tags = Vec::new();
    let mut at = HEADER;
    loop {
        if at + TAG_BYTES > size {
            return Err(Ext2Error::Invalid);
        }
        let flags = be16(buf, at + 6);
        tags.push((be32(buf, at), flags));
        at += TAG_BYTES;
        if flags & TAG_SAME_UUID == 0 {
            at += UUID_BYTES;
        }
        if flags & TAG_LAST != 0 {
            return Ok(tags);
        }
    }
}
