//! Directory blocks: lookup, add/remove entries and path resolution.

use super::*;

/// Most `..` hops [`Ext2::is_within`] follows before calling the image corrupt.
const MAX_DOTDOT_STEPS: u32 = 4096;

impl Ext2 {
    /// The physical blocks a directory owns, in logical order. Directories are
    /// dense: a hole is corruption (there is no path that creates one).
    pub(super) fn dir_blocks(&self, inode: &[u8; INODE_CORE_SIZE]) -> Result<Vec<u32>, Ext2Error> {
        if kind_from_mode(le16(inode, INO_MODE)) != Some(FileKind::Dir) {
            return Err(Ext2Error::NotDir);
        }
        let block_size = u64::from(self.block_size);
        let size = u64::from(le32(inode, INO_SIZE));
        if size == 0 || size % block_size != 0 {
            return Err(Ext2Error::Invalid);
        }
        let count = size / block_size;
        let capacity = u64::from(DIRECT_BLOCKS + self.ptrs_per_block);
        if count > capacity {
            return Err(Ext2Error::NotSupported);
        }
        let mut blocks = Vec::new();
        for index in 0..count as u32 {
            let block = self.block_map(inode, index)?;
            if block == 0 {
                return Err(Ext2Error::Invalid);
            }
            blocks.push(block);
        }
        Ok(blocks)
    }

    /// Refuse to change a directory that carries an htree index: this driver
    /// edits entries linearly and would leave the index stale.
    pub(super) fn check_not_indexed(&self, dir: &[u8; INODE_CORE_SIZE]) -> Result<(), Ext2Error> {
        if le32(dir, INO_FLAGS) & INDEX_FL != 0 {
            return Err(Ext2Error::NotSupported);
        }
        Ok(())
    }

    /// Resolve `path` within the volume to an inode number, one directory
    /// entry per component from the root.
    pub(super) fn resolve(&self, path: &str) -> Result<u32, Ext2Error> {
        let mut ino = ROOT_INO;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if part == "." || part == ".." {
                return Err(Ext2Error::Invalid);
            }
            if part.len() > MAX_NAME {
                return Err(Ext2Error::NameTooLong);
            }
            ino = self.find_entry(ino, part)?.0;
        }
        Ok(ino)
    }

    /// Find `name` in directory `dir_ino` as `(inode, file type byte)`.
    pub(super) fn find_entry(&self, dir_ino: u32, name: &str) -> Result<(u32, u8), Ext2Error> {
        let inode = self.read_inode(dir_ino)?;
        let blocks = self.dir_blocks(&inode)?;
        let size = self.block_size as usize;
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || !rec_len.is_multiple_of(4)
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(Ext2Error::Invalid);
                }
                let entry_name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                if entry_ino != 0 && entry_name == name.as_bytes() {
                    return Ok((entry_ino, buf[offset + DE_FILE_TYPE]));
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Err(Ext2Error::NotFound)
    }

    /// Whether directory `dir_ino` holds no entries besides `.` and `..`.
    pub(super) fn dir_is_empty(&self, dir_ino: u32) -> Result<bool, Ext2Error> {
        let inode = self.read_inode(dir_ino)?;
        let blocks = self.dir_blocks(&inode)?;
        let size = self.block_size as usize;
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || !rec_len.is_multiple_of(4)
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(Ext2Error::Invalid);
                }
                if entry_ino != 0 {
                    let name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                    if name != b"." && name != b".." {
                        return Ok(false);
                    }
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Ok(true)
    }

    /// Add `name` -> `child_ino` to directory `dir_ino`, splitting a free
    /// record or appending a fresh block. `dir` is the caller's in-memory
    /// inode; it is written back with a fresh `ctime`/`mtime`.
    pub(super) fn add_entry(
        &self,
        dir_ino: u32,
        dir: &mut [u8; INODE_CORE_SIZE],
        name: &str,
        child_ino: u32,
        file_type: u8,
    ) -> Result<(), Ext2Error> {
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(Ext2Error::NameTooLong);
        }
        self.check_not_indexed(dir)?;
        let size = self.block_size as usize;
        let needed = (DE_HEADER + name.len() + 3) & !3;
        if needed > size {
            return Err(Ext2Error::NameTooLong);
        }
        // Without the FILETYPE feature the type byte must stay zero.
        let file_type = if self.has_file_type { file_type } else { 0 };
        let blocks = self.dir_blocks(dir)?;
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || !rec_len.is_multiple_of(4)
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(Ext2Error::Invalid);
                }
                // An entry can take a free record, or be carved out of a
                // record that has slack after its own name (the trailing `..`
                // in a fresh directory is exactly that case). This is Linux's
                // `ext2_add_link` rule.
                let own = (DE_HEADER + name_len + 3) & !3;
                if entry_ino == 0 && rec_len >= needed {
                    put32(&mut buf, offset + DE_INO, child_ino);
                    put16(&mut buf, offset + DE_REC_LEN, rec_len as u16);
                    buf[offset + DE_NAME_LEN] = name.len() as u8;
                    buf[offset + DE_FILE_TYPE] = file_type;
                    let name_offset = offset + DE_HEADER;
                    buf[name_offset..name_offset + name.len()].copy_from_slice(name.as_bytes());
                    self.write_block(u64::from(block), &buf[..size])?;
                    touch(dir, self.now());
                    return self.write_inode(dir_ino, dir);
                }
                if rec_len >= own + needed {
                    let new_offset = offset + own;
                    put16(&mut buf, offset + DE_REC_LEN, own as u16);
                    put32(&mut buf, new_offset + DE_INO, child_ino);
                    put16(&mut buf, new_offset + DE_REC_LEN, (rec_len - own) as u16);
                    buf[new_offset + DE_NAME_LEN] = name.len() as u8;
                    buf[new_offset + DE_FILE_TYPE] = file_type;
                    let name_offset = new_offset + DE_HEADER;
                    buf[name_offset..name_offset + name.len()].copy_from_slice(name.as_bytes());
                    self.write_block(u64::from(block), &buf[..size])?;
                    touch(dir, self.now());
                    return self.write_inode(dir_ino, dir);
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }

        // No free slot: append a block and make it one whole-block entry.
        let index = le32(dir, INO_SIZE) / self.block_size;
        if index >= DIRECT_BLOCKS + self.ptrs_per_block {
            return Err(Ext2Error::NoSpace);
        }
        // Check the size update first: a failure after allocating would leak.
        let new_size = le32(dir, INO_SIZE)
            .checked_add(self.block_size)
            .ok_or(Ext2Error::NoSpace)?;
        let (block, fresh) = self.ensure_block(dir, index)?;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        if !fresh {
            self.read_block(u64::from(block), &mut buf[..size])?;
        }
        put32(&mut buf, DE_INO, child_ino);
        put16(&mut buf, DE_REC_LEN, size as u16);
        buf[DE_NAME_LEN] = name.len() as u8;
        buf[DE_FILE_TYPE] = file_type;
        buf[DE_HEADER..DE_HEADER + name.len()].copy_from_slice(name.as_bytes());
        self.write_block(u64::from(block), &buf[..size])?;
        put32(dir, INO_SIZE, new_size);
        touch(dir, self.now());
        self.write_inode(dir_ino, dir)
    }

    /// Remove `name` from directory `dir_ino` (merging the record into its
    /// predecessor when possible) and return the child's inode number.
    pub(super) fn remove_entry(
        &self,
        dir_ino: u32,
        dir: &mut [u8; INODE_CORE_SIZE],
        name: &str,
    ) -> Result<u32, Ext2Error> {
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        self.check_not_indexed(dir)?;
        let size = self.block_size as usize;
        let blocks = self.dir_blocks(dir)?;
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            let mut previous = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || !rec_len.is_multiple_of(4)
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(Ext2Error::Invalid);
                }
                let entry_name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                if entry_ino != 0 && entry_name == name.as_bytes() {
                    if offset == 0 {
                        put32(&mut buf, DE_INO, 0); // first record: leave a hole
                    } else {
                        let previous_len = le16(&buf, previous + DE_REC_LEN) as usize;
                        put16(
                            &mut buf,
                            previous + DE_REC_LEN,
                            (previous_len + rec_len) as u16,
                        );
                    }
                    self.write_block(u64::from(block), &buf[..size])?;
                    touch(dir, self.now());
                    self.write_inode(dir_ino, dir)?;
                    return Ok(entry_ino);
                }
                previous = offset;
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Err(Ext2Error::NotFound)
    }

    /// Point the `..` entry of directory `child` at `parent_ino`.
    pub(super) fn set_dotdot(
        &self,
        child_ino: u32,
        child: &mut [u8; INODE_CORE_SIZE],
        parent_ino: u32,
    ) -> Result<(), Ext2Error> {
        self.check_not_indexed(child)?;
        let size = self.block_size as usize;
        let block = self.block_map(child, 0)?;
        if block == 0 {
            return Err(Ext2Error::Invalid);
        }
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(block), &mut buf[..size])?;
        let mut offset = 0usize;
        for _ in 0..(size / DE_HEADER) {
            if offset + DE_HEADER > size {
                break;
            }
            let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
            let name_len = buf[offset + DE_NAME_LEN] as usize;
            if rec_len < DE_HEADER
                || !rec_len.is_multiple_of(4)
                || offset + rec_len > size
                || name_len > rec_len - DE_HEADER
            {
                return Err(Ext2Error::Invalid);
            }
            if &buf[offset + DE_HEADER..offset + DE_HEADER + name_len] == b".." {
                put32(&mut buf, offset + DE_INO, parent_ino);
                self.write_block(u64::from(block), &buf[..size])?;
                touch(child, self.now());
                return self.write_inode(child_ino, child);
            }
            offset += rec_len;
            if offset == size {
                break;
            }
        }
        Err(Ext2Error::Invalid) // every directory must carry `.` and `..`
    }

    /// Whether `node` lies inside directory `ancestor` (walking `..` up).
    /// Used to refuse moving a directory into itself.
    pub(super) fn is_within(&self, ancestor: u32, node: u32) -> Result<bool, Ext2Error> {
        let mut current = node;
        let mut steps = 0u32;
        while current != ROOT_INO {
            if current == ancestor {
                return Ok(true);
            }
            let inode = self.read_inode(current)?;
            if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::Dir) {
                return Ok(false);
            }
            current = self.find_entry(current, "..")?.0;
            steps += 1;
            if steps > MAX_DOTDOT_STEPS {
                return Err(Ext2Error::Invalid); // a `..` cycle (or an absurdly deep tree)
            }
        }
        Ok(false)
    }

    /// The size of a regular file, honouring the large-file high bits.
    pub(super) fn file_size(&self, inode: &[u8; INODE_CORE_SIZE]) -> u64 {
        let low = u64::from(le32(inode, INO_SIZE));
        if self.has_large_file {
            low | (u64::from(le32(inode, INO_DIR_ACL)) << 32)
        } else {
            low
        }
    }

    /// Build [`InodeMeta`] for an inode number.
    pub(super) fn meta_of(&self, ino: u32) -> Result<InodeMeta, Ext2Error> {
        let inode = self.read_inode(ino)?;
        let mode = le16(&inode, INO_MODE);
        let kind = kind_from_mode(mode).ok_or(Ext2Error::NotSupported)?;
        let size = if kind == FileKind::File {
            self.file_size(&inode)
        } else {
            u64::from(le32(&inode, INO_SIZE))
        };
        Ok(InodeMeta {
            ino: u64::from(ino),
            mode,
            uid: u32::from(le16(&inode, INO_UID)),
            gid: u32::from(le16(&inode, INO_GID)),
            size,
            kind,
            times: attr::times_of(&inode),
        })
    }
}
