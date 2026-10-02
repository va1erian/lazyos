//! Creating and removing names: `create`, `mkdir` and `unlink`.

use super::*;

impl Ext2 {
    /// Create an empty regular file owned by `owner`.
    pub fn create(&self, path: &str, mode: u16, owner: Owner) -> Result<InodeMeta, Ext2Error> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(Ext2Error::NotDir);
        }
        if self.find_entry(parent_ino, name).is_ok() {
            return Err(Ext2Error::Exists);
        }
        check_owner(owner)?;
        let ino = self.alloc_inode(false)?;
        let mut inode = [0u8; INODE_CORE_SIZE];
        put16(&mut inode, INO_MODE, S_IFREG | (mode & 0o7777));
        put16(&mut inode, INO_UID, owner.uid as u16);
        put16(&mut inode, INO_GID, owner.gid as u16);
        put16(&mut inode, INO_LINKS, 1);
        let time = self.now();
        put32(&mut inode, INO_ATIME, time);
        touch(&mut inode, time);
        if let Err(error) = self.write_inode(ino, &inode) {
            let _ = self.free_inode(ino, false);
            return Err(error);
        }
        match self.add_entry(parent_ino, &mut parent, name, ino, FT_REGULAR) {
            Ok(()) => self.meta_of(ino),
            Err(error) => {
                // Roll the fresh inode back; the parent was not written.
                let _ = self.free_inode(ino, false);
                Err(error)
            }
        }
    }

    /// Create an empty directory owned by `owner`.
    pub fn mkdir(&self, path: &str, mode: u16, owner: Owner) -> Result<InodeMeta, Ext2Error> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(Ext2Error::NotDir);
        }
        if self.find_entry(parent_ino, name).is_ok() {
            return Err(Ext2Error::Exists);
        }
        check_owner(owner)?;
        // The new child makes the parent worth one more link; check before
        // allocating so a full link count leaks nothing.
        let parent_links = le16(&parent, INO_LINKS)
            .checked_add(1)
            .ok_or(Ext2Error::Invalid)?;
        let ino = self.alloc_inode(true)?;
        let block = match self.alloc_block() {
            Ok(block) => block,
            Err(error) => {
                let _ = self.free_inode(ino, true);
                return Err(error);
            }
        };
        let size = self.block_size as usize;
        let mut dir = [0u8; MAX_BLOCK_SIZE];
        put32(&mut dir, DE_INO, ino);
        put16(&mut dir, DE_REC_LEN, 12);
        dir[DE_NAME_LEN] = 1;
        let dir_type = if self.has_file_type { FT_DIRECTORY } else { 0 };
        dir[DE_FILE_TYPE] = dir_type;
        dir[DE_HEADER] = b'.';
        let dotdot = DE_HEADER + 4; // aligned start of the `..` record
        put32(&mut dir, dotdot + DE_INO, parent_ino);
        put16(&mut dir, dotdot + DE_REC_LEN, (size - dotdot) as u16);
        dir[dotdot + DE_NAME_LEN] = 2;
        dir[dotdot + DE_FILE_TYPE] = dir_type;
        dir[dotdot + DE_HEADER] = b'.';
        dir[dotdot + DE_HEADER + 1] = b'.';
        if let Err(error) = self.write_block(u64::from(block), &dir[..size]) {
            let _ = self.free_block(block);
            let _ = self.free_inode(ino, true);
            return Err(error);
        }

        let mut inode = [0u8; INODE_CORE_SIZE];
        put16(&mut inode, INO_MODE, S_IFDIR | (mode & 0o7777));
        put16(&mut inode, INO_UID, owner.uid as u16);
        put16(&mut inode, INO_GID, owner.gid as u16);
        put32(&mut inode, INO_SIZE, self.block_size);
        put16(&mut inode, INO_LINKS, 2); // `.` and the parent's entry
        put32(&mut inode, INO_BLOCKS, self.block_size / SECTOR_SIZE as u32);
        put32(&mut inode, INO_BLOCK, block);
        let time = self.now();
        put32(&mut inode, INO_ATIME, time);
        touch(&mut inode, time);
        if let Err(error) = self.write_inode(ino, &inode) {
            let _ = self.free_block(block);
            let _ = self.free_inode(ino, true);
            return Err(error);
        }

        put16(&mut parent, INO_LINKS, parent_links);
        match self.add_entry(parent_ino, &mut parent, name, ino, FT_DIRECTORY) {
            Ok(()) => self.meta_of(ino),
            Err(error) => {
                let _ = self.free_block(block);
                let _ = self.free_inode(ino, true);
                Err(error)
            }
        }
    }

    /// Remove the file at `path` (its blocks and inode go with the last link).
    pub fn unlink(&self, path: &str) -> Result<(), Ext2Error> {
        self.unlink_inner(path, false)
    }

    /// Remove a file the host parked under its reserved orphan name. A file
    /// on its last link is deleted inode-first, so a stop part-way can be
    /// resumed by [`Ext2::reclaim_orphans`] (see `orphans.rs`); one with more
    /// links is unlinked like any other.
    pub fn unlink_parked(&self, path: &str) -> Result<(), Ext2Error> {
        self.unlink_inner(path, true)
    }

    fn unlink_inner(&self, path: &str, parked: bool) -> Result<(), Ext2Error> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(Ext2Error::NotDir);
        }
        let (child_ino, _) = self.find_entry(parent_ino, name)?;
        let mut child = self.read_inode(child_ino)?;
        if kind_from_mode(le16(&child, INO_MODE)) != Some(FileKind::File) {
            return Err(Ext2Error::IsDir);
        }
        if parked && le16(&child, INO_LINKS) <= 1 {
            // A parked orphan is deleted inode-first so a stop can be resumed.
            return self.discard_orphan(parent_ino, &mut parent, name, child_ino, &mut child);
        }
        self.remove_entry(parent_ino, &mut parent, name)?;
        let links = le16(&child, INO_LINKS);
        if links <= 1 {
            // Last link: release the data blocks, then the inode itself.
            put16(&mut child, INO_LINKS, 0);
            put32(&mut child, INO_DTIME, self.now());
            self.free_inode_blocks(child_ino, &mut child)?;
            self.free_inode(child_ino, false)?;
        } else {
            put16(&mut child, INO_LINKS, links - 1);
            touch(&mut child, self.now());
            self.write_inode(child_ino, &child)?;
        }
        Ok(())
    }
}
