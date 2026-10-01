//! Renaming a regular file, ordered for crash safety.
//!
//! The write-temp-then-rename pattern (`confd`'s store, issue #407) relies on
//! a rename leaving the file reachable after a power cut at *any* write. ext2
//! has no journal, so that comes from ordering alone, as in Linux's
//! `ext2_rename`:
//!
//! 1. the file gains a link, so it may briefly have two names;
//! 2. the new name is made: an existing destination entry is retargeted in
//!    place (one block write: the commit point), otherwise a new entry is added;
//! 3. the old name is removed and the extra link dropped;
//! 4. only then is the replaced file released.
//!
//! A cut before 2 leaves the old state, after 2 the file under both names with
//! a link count of two, so unlinking either leaves the other intact. The worst
//! case is a leaked link count or an unreferenced victim inode (space, never
//! data). Directories keep the general path in `fsimpl` (their link counts
//! carry `..` bookkeeping).

use super::*;

/// One side of a rename: the parent directory and the name in it.
pub(super) struct Side<'a> {
    pub(super) parent_ino: u32,
    pub(super) parent: &'a mut [u8; INODE_CORE_SIZE],
    pub(super) name: &'a str,
}

impl Ext2 {
    /// Move regular file `child_ino` from `from` to `to`, replacing a regular
    /// file already there. The caller holds the volume lock and has checked
    /// both parents are directories.
    pub(super) fn rename_file(
        &self,
        from: Side<'_>,
        to: Side<'_>,
        child_ino: u32,
        child: &mut [u8; INODE_CORE_SIZE],
    ) -> Result<(), FsError> {
        let same_dir = from.parent_ino == to.parent_ino;
        let victim = match self.find_entry(to.parent_ino, to.name) {
            Ok((existing, _)) if existing == child_ino => return Ok(()), // already linked there
            Ok((existing, _)) => {
                let inode = self.read_inode(existing)?;
                match kind_from_mode(le16(&inode, INO_MODE)) {
                    Some(FileKind::File) => Some((existing, inode)),
                    Some(FileKind::Dir) => return Err(FsError::IsDir),
                    None => return Err(FsError::NotSupported),
                }
            }
            Err(FsError::NotFound) => None,
            Err(error) => return Err(error),
        };

        // 1. A second link for the moment the file has two names.
        let links = le16(child, INO_LINKS);
        put16(
            child,
            INO_LINKS,
            links.checked_add(1).ok_or(FsError::Invalid)?,
        );
        touch(child, now());
        self.write_inode(child_ino, child)?;

        // 2. The new name: the commit point.
        let linked = match victim {
            Some(_) => self.retarget_entry(to.parent_ino, to.parent, to.name, child_ino),
            None => self.add_entry(to.parent_ino, to.parent, to.name, child_ino, FT_REGULAR),
        };
        if let Err(error) = linked {
            put16(child, INO_LINKS, links);
            let _ = self.write_inode(child_ino, child);
            return Err(error);
        }
        if same_dir {
            // Two in-memory copies of one inode: `add_entry` may have grown
            // the directory, which the stale copy must not write back over.
            *from.parent = *to.parent;
        }

        // 3. Drop the old name and the extra link. If the old name cannot be
        // removed, the file keeps both names (and both links), but the new
        // name is committed: the victim lost its entry, so it is released
        // before the error is returned instead of leaking until an fsck.
        if let Err(error) = self.remove_entry(from.parent_ino, from.parent, from.name) {
            if let Some((existing, mut inode)) = victim {
                let _ = self.release_link(existing, &mut inode);
            }
            return Err(error);
        }
        put16(child, INO_LINKS, links);
        self.write_inode(child_ino, child)?;

        // 4. Release what the new name replaced.
        if let Some((existing, mut inode)) = victim {
            self.release_link(existing, &mut inode)?;
        }
        Ok(())
    }

    /// Drop one link of file `ino`, freeing it with its last name.
    fn release_link(&self, ino: u32, inode: &mut [u8; INODE_CORE_SIZE]) -> Result<(), FsError> {
        let links = le16(inode, INO_LINKS);
        if links <= 1 {
            put16(inode, INO_LINKS, 0);
            put32(inode, INO_DTIME, now());
            self.free_inode_blocks(ino, inode)?;
            self.free_inode(ino, false)
        } else {
            put16(inode, INO_LINKS, links - 1);
            touch(inode, now());
            self.write_inode(ino, inode)
        }
    }

    /// Point the existing entry `name` in directory `dir_ino` at `child_ino`,
    /// in place: one block write, so the name never disappears.
    fn retarget_entry(
        &self,
        dir_ino: u32,
        dir: &mut [u8; INODE_CORE_SIZE],
        name: &str,
        child_ino: u32,
    ) -> Result<(), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let size = self.block_size as usize;
        for block in self.dir_blocks(dir)? {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            while offset + DE_HEADER <= size {
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
                let entry_name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                if entry_ino != 0 && entry_name == name.as_bytes() {
                    put32(&mut buf, offset + DE_INO, child_ino);
                    buf[offset + DE_FILE_TYPE] = FT_REGULAR;
                    self.write_block(u64::from(block), &buf[..size])?;
                    touch(dir, now());
                    return self.write_inode(dir_ino, dir);
                }
                offset += rec_len;
            }
        }
        Err(FsError::NotFound)
    }
}
