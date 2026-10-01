//! `rename`: move a node, replacing a compatible destination.

use super::*;

impl Ext2 {
    /// Rename `from` to `to`. A file replaces a file, a directory may replace only an
    /// empty directory, and a directory cannot move below itself.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), Ext2Error> {
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
            return Err(Ext2Error::NotDir);
        }
        let mut to_parent = self.read_inode(to_parent_ino)?;
        if kind_from_mode(le16(&to_parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(Ext2Error::NotDir);
        }
        let (child_ino, _) = self.find_entry(from_parent_ino, from_name)?;
        let mut child = self.read_inode(child_ino)?;
        let child_kind = kind_from_mode(le16(&child, INO_MODE)).ok_or(Ext2Error::NotSupported)?;
        // Moving a directory below itself would make a cycle.
        if child_kind == FileKind::Dir && self.is_within(child_ino, to_parent_ino)? {
            return Err(Ext2Error::Invalid);
        }

        // The destination may exist: a file replaces a file, a directory may
        // replace only an empty directory (the same rules as ramfs).
        if let Ok((existing, _)) = self.find_entry(to_parent_ino, to_name) {
            if existing == child_ino {
                return Ok(()); // already linked there
            }
            let mut victim = self.read_inode(existing)?;
            let victim_kind =
                kind_from_mode(le16(&victim, INO_MODE)).ok_or(Ext2Error::NotSupported)?;
            match (child_kind, victim_kind) {
                (FileKind::File, FileKind::File) => {}
                (FileKind::Dir, FileKind::Dir) => {
                    if !self.dir_is_empty(existing)? {
                        return Err(Ext2Error::NotEmpty);
                    }
                }
                (FileKind::File, FileKind::Dir) => return Err(Ext2Error::IsDir),
                (FileKind::Dir, FileKind::File) => return Err(Ext2Error::NotDir),
            }
            self.remove_entry(to_parent_ino, &mut to_parent, to_name)?;
            let links = le16(&victim, INO_LINKS);
            // An empty directory holds two links (`.` and its parent's entry),
            // so removing the parent's entry drops its last reference; only a
            // file with further hard links survives the replacement.
            let last_reference = victim_kind == FileKind::Dir || links <= 1;
            if last_reference {
                put16(&mut victim, INO_LINKS, 0);
                put32(&mut victim, INO_DTIME, self.now());
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
                touch(&mut victim, self.now());
                self.write_inode(existing, &victim)?;
            }
        }

        // Directory bookkeeping: the old parent loses a child directory, the
        // new parent gains one, and the moved directory's `..` follows.
        //
        // The fallible `add_entry` runs first (it can run out of space), and
        // it persists the new parent's link count with the entry. Only then
        // does the `..` move, and the old entry go: a failure at either step
        // undoes the add, so no link count or `..` is left half-changed.
        let moves_dir = child_kind == FileKind::Dir && from_parent_ino != to_parent_ino;
        let to_links_before = le16(&to_parent, INO_LINKS);
        if moves_dir {
            let from_links = le16(&from_parent, INO_LINKS).saturating_sub(1);
            put16(&mut from_parent, INO_LINKS, from_links);
            let to_links = to_links_before.checked_add(1).ok_or(Ext2Error::Invalid)?;
            put16(&mut to_parent, INO_LINKS, to_links);
        }
        let file_type = if child_kind == FileKind::Dir {
            FT_DIRECTORY
        } else {
            FT_REGULAR
        };
        self.add_entry(to_parent_ino, &mut to_parent, to_name, child_ino, file_type)?;
        if from_parent_ino == to_parent_ino {
            // One inode, two in-memory copies: the add may have grown it.
            from_parent = to_parent;
        }
        let moved = self.finish_move(
            from_parent_ino,
            &mut from_parent,
            from_name,
            (child_ino, &mut child, child_kind),
            moves_dir.then_some(to_parent_ino),
        );
        if let Err(error) = moved {
            put16(&mut to_parent, INO_LINKS, to_links_before);
            let _ = self.remove_entry(to_parent_ino, &mut to_parent, to_name);
            return Err(error);
        }
        Ok(())
    }

    /// The steps of a rename that follow adding the new entry: repoint `..`
    /// (when a directory changes parent), then drop the old entry.
    fn finish_move(
        &self,
        from_parent_ino: u32,
        from_parent: &mut [u8; INODE_CORE_SIZE],
        from_name: &str,
        child: (u32, &mut [u8; INODE_CORE_SIZE], FileKind),
        new_dotdot: Option<u32>,
    ) -> Result<(), Ext2Error> {
        let (child_ino, child, kind) = child;
        if let Some(parent) = new_dotdot {
            self.set_dotdot(child_ino, child, parent)?;
        }
        if kind == FileKind::File {
            touch(child, self.now());
            self.write_inode(child_ino, child)?;
        }
        if let Err(error) = self.remove_entry(from_parent_ino, from_parent, from_name) {
            if new_dotdot.is_some() {
                let _ = self.set_dotdot(child_ino, child, from_parent_ino);
            }
            return Err(error);
        }
        Ok(())
    }
}
