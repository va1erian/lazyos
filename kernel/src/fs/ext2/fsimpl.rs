//! The [`Filesystem`] implementation over the ext2 primitives.

use super::*;

impl Filesystem for Ext2 {
    fn name(&self) -> &'static str {
        "ext2 (rw)"
    }

    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        self.meta_of(ino)
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(FsError::IsDir);
        }
        let size = self.file_size(&inode);
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let count = min(size - offset, buf.len() as u64) as usize;
        let block_size = u64::from(self.block_size);
        let size_usize = self.block_size as usize;
        let mut done = 0usize;
        while done < count {
            let position = offset + done as u64;
            let index = self.block_index(position)?;
            let inner = (position % block_size) as usize;
            let chunk = min(size_usize - inner, count - done);
            let block = self.block_map(&inode, index)?;
            if block == 0 {
                buf[done..done + chunk].fill(0); // a sparse hole reads as zero
            } else {
                let mut tmp = [0u8; MAX_BLOCK_SIZE];
                self.read_block(u64::from(block), &mut tmp[..size_usize])?;
                buf[done..done + chunk].copy_from_slice(&tmp[inner..inner + chunk]);
            }
            done += chunk;
        }
        Ok(done)
    }

    fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let mut inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(FsError::IsDir);
        }
        if data.is_empty() {
            return Ok(0);
        }
        // Sizes are 32-bit, so a write past the cap is short (or refused); it
        // must never wrap a huge offset onto a low block.
        let data = &data[..indirect::writable_len(offset, data.len())?];
        let block_size = u64::from(self.block_size);
        let size_usize = self.block_size as usize;
        let mut done = 0usize;
        let mut failure = None;
        while done < data.len() {
            let position = offset + done as u64;
            let index = self.block_index(position)?;
            let inner = (position % block_size) as usize;
            let chunk = min(size_usize - inner, data.len() - done);
            let (block, fresh) = match self.ensure_block(&mut inode, index) {
                Ok(mapped) => mapped,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            };
            let mut tmp = [0u8; MAX_BLOCK_SIZE];
            if !fresh {
                if let Err(error) = self.read_block(u64::from(block), &mut tmp[..size_usize]) {
                    failure = Some(error);
                    break;
                }
            }
            // A fresh block is written from zeros, so a short write can never
            // expose stale bytes from the block's previous owner.
            tmp[inner..inner + chunk].copy_from_slice(&data[done..done + chunk]);
            if let Err(error) = self.write_block(u64::from(block), &tmp[..size_usize]) {
                failure = Some(error);
                break;
            }
            done += chunk;
        }
        // `ensure_block` allocated blocks and edited the in-memory inode as it
        // went. Persist the inode whatever happened, or every block allocated
        // before a failure (out of space, an I/O error) stays marked used in
        // the bitmap while no inode owns it: a permanent leak, and the bytes
        // already written vanish. The size covers exactly what landed.
        let landed = offset + done as u64;
        if done > 0 && landed > self.file_size(&inode) {
            put32(&mut inode, INO_SIZE, landed as u32);
        }
        touch(&mut inode, now());
        let persisted = self.write_inode(ino, &inode);
        match failure {
            // A short write reports the bytes that landed; the caller's next
            // write sees the failure again with nothing written.
            Some(error) if done == 0 => Err(error),
            _ => persisted.map(|()| done),
        }
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        self.truncate_file(path, size)
    }

    fn setattr(&self, path: &str, attr: &crate::fs::vfs::SetAttr) -> Result<Meta, FsError> {
        self.set_attributes(path, attr)
    }

    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        if self.find_entry(parent_ino, name).is_ok() {
            return Err(FsError::Exists);
        }
        check_owner(owner)?;
        let ino = self.alloc_inode(false)?;
        let mut inode = [0u8; INODE_CORE_SIZE];
        put16(&mut inode, INO_MODE, S_IFREG | (mode & 0o7777));
        put16(&mut inode, INO_UID, owner.uid as u16);
        put16(&mut inode, INO_GID, owner.gid as u16);
        put16(&mut inode, INO_LINKS, 1);
        let time = now();
        put32(&mut inode, INO_ATIME, time);
        touch(&mut inode, time);
        self.write_inode(ino, &inode)?;
        match self.add_entry(parent_ino, &mut parent, name, ino, FT_REGULAR) {
            Ok(()) => self.meta_of(ino),
            Err(error) => {
                // Roll the fresh inode back; the parent was not written.
                let _ = self.free_inode(ino, false);
                Err(error)
            }
        }
    }

    fn mkdir(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        if self.find_entry(parent_ino, name).is_ok() {
            return Err(FsError::Exists);
        }
        check_owner(owner)?;
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
        dir[DE_FILE_TYPE] = FT_DIRECTORY;
        dir[DE_HEADER] = b'.';
        let dotdot = DE_HEADER + 4; // aligned start of the `..` record
        put32(&mut dir, dotdot + DE_INO, parent_ino);
        put16(&mut dir, dotdot + DE_REC_LEN, (size - dotdot) as u16);
        dir[dotdot + DE_NAME_LEN] = 2;
        dir[dotdot + DE_FILE_TYPE] = FT_DIRECTORY;
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
        let time = now();
        put32(&mut inode, INO_ATIME, time);
        touch(&mut inode, time);
        self.write_inode(ino, &inode)?;

        // The new child makes the parent worth one more link.
        let links = le16(&parent, INO_LINKS)
            .checked_add(1)
            .ok_or(FsError::Invalid)?;
        put16(&mut parent, INO_LINKS, links);
        match self.add_entry(parent_ino, &mut parent, name, ino, FT_DIRECTORY) {
            Ok(()) => self.meta_of(ino),
            Err(error) => {
                let _ = self.free_block(block);
                let _ = self.free_inode(ino, true);
                Err(error)
            }
        }
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        let (child_ino, _) = self.find_entry(parent_ino, name)?;
        let mut child = self.read_inode(child_ino)?;
        if kind_from_mode(le16(&child, INO_MODE)) != Some(FileKind::File) {
            return Err(FsError::IsDir);
        }
        if crate::fs::hidden::is_reserved(name) && le16(&child, INO_LINKS) <= 1 {
            // A parked orphan is deleted inode-first so a stop can be resumed.
            return self.discard_orphan(parent_ino, &mut parent, name, child_ino, &mut child);
        }
        self.remove_entry(parent_ino, &mut parent, name)?;
        let links = le16(&child, INO_LINKS);
        if links <= 1 {
            // Last link: release the data blocks, then the inode itself.
            put16(&mut child, INO_LINKS, 0);
            put32(&mut child, INO_DTIME, now());
            self.free_inode_blocks(child_ino, &mut child)?;
            self.free_inode(child_ino, false)?;
        } else {
            put16(&mut child, INO_LINKS, links - 1);
            touch(&mut child, now());
            self.write_inode(child_ino, &child)?;
        }
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let _guard = self.lock.lock();
        if from == to {
            return Ok(());
        }
        let (from_parent_path, from_name) = split_parent(from)?;
        let (to_parent_path, to_name) = split_parent(to)?;
        let from_parent_ino = self.resolve(from_parent_path)?;
        let to_parent_ino = self.resolve(to_parent_path)?;
        let mut from_parent = self.read_inode(from_parent_ino)?;
        if kind_from_mode(le16(&from_parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        let mut to_parent = self.read_inode(to_parent_ino)?;
        if kind_from_mode(le16(&to_parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        let (child_ino, _) = self.find_entry(from_parent_ino, from_name)?;
        let mut child = self.read_inode(child_ino)?;
        let child_kind = kind_from_mode(le16(&child, INO_MODE)).ok_or(FsError::NotSupported)?;
        // Moving a directory below itself would make a cycle.
        if child_kind == FileKind::Dir && self.is_within(child_ino, to_parent_ino)? {
            return Err(FsError::Invalid);
        }

        // The destination may exist: a file replaces a file, a directory may
        // replace only an empty directory (the same rules as ramfs).
        if let Ok((existing, _)) = self.find_entry(to_parent_ino, to_name) {
            if existing == child_ino {
                return Ok(()); // already linked there
            }
            let mut victim = self.read_inode(existing)?;
            let victim_kind =
                kind_from_mode(le16(&victim, INO_MODE)).ok_or(FsError::NotSupported)?;
            match (child_kind, victim_kind) {
                (FileKind::File, FileKind::File) => {}
                (FileKind::Dir, FileKind::Dir) => {
                    if !self.dir_is_empty(existing)? {
                        return Err(FsError::NotEmpty);
                    }
                }
                (FileKind::File, FileKind::Dir) => return Err(FsError::IsDir),
                (FileKind::Dir, FileKind::File) => return Err(FsError::NotDir),
            }
            self.remove_entry(to_parent_ino, &mut to_parent, to_name)?;
            let links = le16(&victim, INO_LINKS);
            // An empty directory holds two links (`.` and its parent's entry),
            // so removing the parent's entry drops its last reference; only a
            // file with further hard links survives the replacement.
            let last_reference = victim_kind == FileKind::Dir || links <= 1;
            if last_reference {
                put16(&mut victim, INO_LINKS, 0);
                put32(&mut victim, INO_DTIME, now());
                self.free_inode_blocks(existing, &mut victim)?;
                self.free_inode(existing, victim_kind == FileKind::Dir)?;
                if victim_kind == FileKind::Dir {
                    // The victim's `..` pointed at `to_parent`; that link goes
                    // with it. Persist it now, not via the `add_entry` at the
                    // end: a later step can fail, and the victim is already
                    // gone, so the on-disk count must not keep its link.
                    let parent_links = le16(&to_parent, INO_LINKS).saturating_sub(1);
                    put16(&mut to_parent, INO_LINKS, parent_links);
                    self.write_inode(to_parent_ino, &to_parent)?;
                    if from_parent_ino == to_parent_ino {
                        // Two in-memory copies of one inode: refresh the other
                        // so its later writes (and the rollback) carry the new
                        // count instead of overwriting it with a stale one.
                        from_parent = self.read_inode(from_parent_ino)?;
                    }
                }
            } else {
                put16(&mut victim, INO_LINKS, links - 1);
                touch(&mut victim, now());
                self.write_inode(existing, &victim)?;
            }
        }

        // Directory bookkeeping: the old parent loses a child directory, the
        // new parent gains one, and the moved directory's `..` follows.
        if child_kind == FileKind::Dir && from_parent_ino != to_parent_ino {
            let from_links = le16(&from_parent, INO_LINKS).saturating_sub(1);
            put16(&mut from_parent, INO_LINKS, from_links);
            let to_links = le16(&to_parent, INO_LINKS)
                .checked_add(1)
                .ok_or(FsError::Invalid)?;
            put16(&mut to_parent, INO_LINKS, to_links);
            self.set_dotdot(child_ino, &mut child, to_parent_ino)?;
        }
        if child_kind == FileKind::File {
            touch(&mut child, now());
            self.write_inode(child_ino, &child)?;
        }

        self.remove_entry(from_parent_ino, &mut from_parent, from_name)?;
        let file_type = if child_kind == FileKind::Dir {
            FT_DIRECTORY
        } else {
            FT_REGULAR
        };
        if let Err(error) =
            self.add_entry(to_parent_ino, &mut to_parent, to_name, child_ino, file_type)
        {
            // Put the source entry back so a failure leaves the tree intact.
            let _ = self.add_entry(
                from_parent_ino,
                &mut from_parent,
                from_name,
                child_ino,
                file_type,
            );
            return Err(error);
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), FsError> {
        self.sync_volume()
    }

    fn statfs(&self) -> Result<crate::fs::vfs::StatFs, FsError> {
        self.capacity()
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        let blocks = self.dir_blocks(&inode)?;
        let size = self.block_size as usize;
        let mut entries = Vec::new();
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
                    return Err(FsError::Invalid);
                }
                if entry_ino != 0 && name_len > 0 {
                    let name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                    if name != b"." && name != b".." {
                        // Revision-0 entries carry no type byte: read the
                        // child inode instead. Types the VFS cannot hold
                        // (symlink, device, ...) are skipped, never guessed.
                        let file_type = buf[offset + DE_FILE_TYPE];
                        let kind = if self.has_file_type && file_type != 0 {
                            match file_type {
                                FT_REGULAR => Some(FileKind::File),
                                FT_DIRECTORY => Some(FileKind::Dir),
                                _ => None,
                            }
                        } else {
                            let child = self.read_inode(entry_ino)?;
                            kind_from_mode(le16(&child, INO_MODE))
                        };
                        if let Some(kind) = kind {
                            entries.push(DirEntry {
                                name: String::from_utf8_lossy(name).into_owned(),
                                ino: u64::from(entry_ino),
                                kind,
                            });
                        }
                    }
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Ok(entries)
    }
}
