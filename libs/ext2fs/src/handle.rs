//! Files opened by inode: resolve a path once, then read and write the inode
//! directly (docs/performance-plan.md P5).
//!
//! A path lookup walks every directory from the root with linear scans; an
//! open descriptor that re-resolved its path on every read paid that walk per
//! call. A [`FileHandle`] names the inode instead. An inode number alone is
//! not an identity, though: once the file is deleted its inode is freed and
//! the next `create` may reuse it, and a handle that still read it would read
//! someone else's file. Every inode therefore carries a generation
//! (`i_generation`), which [`Ext2::next_generation`] advances each time the
//! inode is allocated, and a handle only answers while the inode is still a
//! linked regular file of the generation it was opened at.

use super::*;

/// A regular file opened by [`Ext2::open_file`]: its inode number and the
/// generation the inode had then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileHandle {
    ino: u32,
    generation: u32,
}

impl FileHandle {
    /// The inode number.
    pub fn ino(&self) -> u32 {
        self.ino
    }

    /// The inode's generation when the handle was opened.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// A handle from its parts, for a host that keeps them as numbers. A
    /// forged or stale one is refused by every handle call
    /// ([`Ext2Error::NotFound`]), never misread.
    pub fn from_parts(ino: u32, generation: u32) -> FileHandle {
        FileHandle { ino, generation }
    }
}

impl Ext2 {
    /// Resolve the regular file at `path` once, for the handle calls below.
    pub fn open_file(&self, path: &str) -> Result<FileHandle, Ext2Error> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(Ext2Error::IsDir);
        }
        Ok(FileHandle {
            ino,
            generation: le32(&inode, INO_GENERATION),
        })
    }

    /// [`Ext2::read`] through a handle.
    pub fn read_handle(
        &self,
        handle: FileHandle,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Ext2Error> {
        let _guard = self.lock.lock();
        let inode = self.handle_inode(handle)?;
        self.read_data(&inode, offset, buf)
    }

    /// [`Ext2::write`] through a handle.
    pub fn write_handle(
        &self,
        handle: FileHandle,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, Ext2Error> {
        let _guard = self.lock.lock();
        let inode = self.handle_inode(handle)?;
        self.write_data(handle.ino, inode, offset, data)
    }

    /// The metadata of the file behind a handle.
    pub fn handle_meta(&self, handle: FileHandle) -> Result<InodeMeta, Ext2Error> {
        let _guard = self.lock.lock();
        self.handle_inode(handle)?;
        self.meta_of(handle.ino)
    }

    /// [`Ext2::truncate`] through a handle.
    pub fn truncate_handle(&self, handle: FileHandle, size: u64) -> Result<(), Ext2Error> {
        let _guard = self.lock.lock();
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        self.handle_inode(handle)?;
        self.truncate_inode(handle.ino(), size)
    }

    /// The inode behind `handle` while it is still the file the handle
    /// opened: freed (no links), reused (another generation) or no longer a
    /// regular file is [`Ext2Error::NotFound`].
    fn handle_inode(&self, handle: FileHandle) -> Result<[u8; INODE_CORE_SIZE], Ext2Error> {
        let inode = self.read_inode(handle.ino)?;
        let linked = le16(&inode, INO_LINKS) != 0;
        let same = le32(&inode, INO_GENERATION) == handle.generation;
        let file = kind_from_mode(le16(&inode, INO_MODE)) == Some(FileKind::File);
        if linked && same && file {
            Ok(inode)
        } else {
            Err(Ext2Error::NotFound)
        }
    }

    /// The generation a freshly allocated `ino` gets: one past its previous
    /// occupant's, so no handle on that occupant matches the new file.
    pub(super) fn next_generation(&self, ino: u32) -> u32 {
        self.read_inode(ino)
            .map_or(0, |old| le32(&old, INO_GENERATION))
            .wrapping_add(1)
    }
}
