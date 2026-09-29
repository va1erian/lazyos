//! `rmdir` for the ext2 driver: remove an empty directory, release its block
//! and inode, and drop the link its `..` entry held on the parent.

use super::*;

impl Ext2 {
    /// Remove the empty directory at `path`. Refused with `NotEmpty` while it
    /// has entries, `NotDir` for a file, and `Invalid` for the root or a `.`/`..`
    /// name (the root has no parent entry to remove).
    pub(super) fn remove_dir(&self, path: &str) -> Result<(), FsError> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        if name == "." || name == ".." {
            return Err(FsError::Invalid);
        }
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        let (child_ino, _) = self.find_entry(parent_ino, name)?;
        let mut child = self.read_inode(child_ino)?;
        if kind_from_mode(le16(&child, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        if !self.dir_is_empty(child_ino)? {
            return Err(FsError::NotEmpty);
        }
        self.remove_entry(parent_ino, &mut parent, name)?;
        // An empty directory holds two links (`.` and the parent's entry); the
        // parent's entry is gone, so the inode dies and takes the child's `..`
        // link on the parent with it.
        put16(&mut child, INO_LINKS, 0);
        put32(&mut child, INO_DTIME, now());
        self.free_inode_blocks(child_ino, &mut child)?;
        self.free_inode(child_ino, true)?;
        let links = le16(&parent, INO_LINKS).saturating_sub(1);
        put16(&mut parent, INO_LINKS, links);
        self.write_inode(parent_ino, &parent)
    }
}
