//! Whole-tree helpers for the host image build: `mkdir -p`, write or replace a
//! file in one call, remove a subtree, and read a file back.
//!
//! Each helper is built from the public single-node operations, so the build
//! and the kernel exercise the same code paths. None of them holds the volume
//! lock across calls; the caller owns the volume while it populates it.

use super::*;

/// Deepest directory nesting `remove_tree` follows. A real tree is nowhere
/// near it; a hostile image with a long chain of directories ends in an error
/// rather than a deep recursion.
const MAX_TREE_DEPTH: usize = 64;

impl Ext2 {
    /// Create every missing directory of `path` with `mode` and the owner
    /// `uid`/`gid`, like `mkdir -p`. A directory that already exists is left
    /// exactly as it is; a component that exists as a file is
    /// [`Ext2Error::NotDir`]. Returns the metadata of the final directory.
    pub fn mkdir_p(
        &self,
        path: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<InodeMeta, Ext2Error> {
        let owner = Owner { uid, gid };
        let mut current = String::new();
        let mut meta = self.lookup("/")?;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            current.push('/');
            current.push_str(part);
            meta = match self.lookup(&current) {
                Ok(found) if found.kind == FileKind::Dir => found,
                Ok(_) => return Err(Ext2Error::NotDir),
                Err(Ext2Error::NotFound) => self.mkdir(&current, mode, owner)?,
                Err(error) => return Err(error),
            };
        }
        Ok(meta)
    }

    /// Write `data` as the regular file `path` (its parent must exist), with
    /// `mode`, owner `uid`/`gid`, and all three timestamps set to `mtime`, so
    /// the same input always gives the same inode. An existing file is
    /// replaced (truncated, then written); an existing directory is
    /// [`Ext2Error::IsDir`].
    pub fn write_file(
        &self,
        path: &str,
        data: &[u8],
        mode: u16,
        uid: u32,
        gid: u32,
        mtime: i64,
    ) -> Result<InodeMeta, Ext2Error> {
        match self.lookup(path) {
            Ok(found) if found.kind == FileKind::File => self.truncate(path, 0)?,
            Ok(_) => return Err(Ext2Error::IsDir),
            Err(Ext2Error::NotFound) => {
                self.create(path, mode, Owner { uid, gid })?;
            }
            Err(error) => return Err(error),
        }
        if self.write(path, 0, data)? != data.len() {
            return Err(Ext2Error::NoSpace);
        }
        self.setattr(
            path,
            &AttrChange {
                mode: Some(mode),
                uid: Some(uid),
                gid: Some(gid),
                atime: Some(mtime),
                mtime: Some(mtime),
                ctime: Some(mtime),
            },
        )
    }

    /// The whole contents of the regular file `path`.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, Ext2Error> {
        let meta = self.lookup(path)?;
        if meta.kind != FileKind::File {
            return Err(Ext2Error::IsDir);
        }
        let mut data = zeroed(meta.size)?;
        let read = self.read(path, 0, &mut data)?;
        data.truncate(read);
        Ok(data)
    }

    /// Delete `path` and everything below it, children before parents. The
    /// root itself cannot be removed ([`Ext2Error::Invalid`]); empty it by
    /// removing its entries. A directory that is its own ancestor (a corrupt
    /// image) is [`Ext2Error::Invalid`], not an endless walk.
    pub fn remove_tree(&self, path: &str) -> Result<(), Ext2Error> {
        let path = path.trim_end_matches('/');
        if path.is_empty() {
            return Err(Ext2Error::Invalid);
        }
        let mut ancestors = Vec::new();
        self.remove_node(path, &mut ancestors)
    }

    /// Remove one node; `ancestors` holds the inodes of the directories above it.
    fn remove_node(&self, path: &str, ancestors: &mut Vec<u64>) -> Result<(), Ext2Error> {
        let meta = self.lookup(path)?;
        if meta.kind == FileKind::File {
            return self.unlink(path);
        }
        if ancestors.len() >= MAX_TREE_DEPTH || ancestors.contains(&meta.ino) {
            return Err(Ext2Error::Invalid);
        }
        ancestors.push(meta.ino);
        for entry in self.readdir(path)? {
            self.remove_node(&alloc::format!("{path}/{}", entry.name), ancestors)?;
        }
        ancestors.pop();
        self.rmdir(path)
    }
}
