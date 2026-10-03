//! Where an executable's bytes come from when the loader maps it.
//!
//! The loader used to take the whole ELF as one `Vec<u8>`: `spawn` and
//! `execve` read the file into the kernel heap first, so a 40 MiB program
//! needed 40 MiB of heap for the instant it took to copy it into its frames.
//! The loader now asks an [`Image`] for the header, the program headers and
//! then each segment's bytes in bounded chunks, so a file is streamed from its
//! volume straight into the new address space ([`VfsFile`]). Embedded images
//! and the test suite's synthetic ELFs are plain byte slices.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{FileKind, FsError, Id};

/// The reason a load fails when the filesystem could not read the image: an
/// I/O error (`EIO`), neither a bad image nor frame exhaustion.
pub const READ_FAILED: &str = "failed to read the executable";

/// A random-access, read-only executable.
pub trait Image {
    /// Size of the file in bytes.
    fn len(&self) -> u64;
    /// Fill all of `buf` from `offset`; an error when the file ends early or
    /// cannot be read.
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), &'static str>;
}

impl Image for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        let start = usize::try_from(offset).map_err(|_| "read past the end of the image")?;
        let end = start
            .checked_add(buf.len())
            .filter(|&end| end <= <[u8]>::len(self))
            .ok_or("read past the end of the image")?;
        buf.copy_from_slice(&self[start..end]);
        Ok(())
    }
}

impl<const N: usize> Image for [u8; N] {
    fn len(&self) -> u64 {
        N as u64
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        self.as_slice().read_exact_at(offset, buf)
    }
}

impl Image for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        self.as_slice().read_exact_at(offset, buf)
    }
}

/// Which virtual filesystem a [`VfsFile`] is read through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tree {
    /// The Linux ABI VFS (`fs::abi_*`): what `execve` and Linux spawns see.
    Abi,
    /// The native VFS (`fs::vfs_*`): native spawns and the kernel's own.
    Native,
}

/// A regular file streamed through a VFS, as the caller `id` may read it.
///
/// The file is named by path and each read walks it again, like the
/// descriptor layer's in-place files (`fs::openfile`). A file replaced while
/// it loads yields a broken image, never kernel memory: every read is bounded
/// by the buffer and checked against the size recorded at open.
pub struct VfsFile {
    tree: Tree,
    id: Id,
    path: String,
    len: u64,
}

impl VfsFile {
    /// The regular file at `path` in the Linux ABI VFS, if `id` may read it.
    pub fn abi(id: Id, path: &str) -> Result<VfsFile, FsError> {
        let meta = crate::fs::abi_check(id, path, crate::fs::vfs::READ)?;
        VfsFile::new(Tree::Abi, id, path, meta.kind, meta.size)
    }

    /// The regular file at `path` in the native VFS, if `id` may read it.
    pub fn native(id: Id, path: &str) -> Result<VfsFile, FsError> {
        let meta = crate::fs::vfs_check(id, path, crate::fs::vfs::READ)?;
        VfsFile::new(Tree::Native, id, path, meta.kind, meta.size)
    }

    fn new(tree: Tree, id: Id, path: &str, kind: FileKind, len: u64) -> Result<VfsFile, FsError> {
        if kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        Ok(VfsFile {
            tree,
            id,
            path: String::from(path),
            len,
        })
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        match self.tree {
            Tree::Abi => crate::fs::abi_read_at(self.id, &self.path, offset, buf),
            Tree::Native => crate::fs::vfs_read_at(self.id, &self.path, offset, buf),
        }
    }
}

impl Image for VfsFile {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&end| end <= self.len)
            .ok_or("read past the end of the image")?;
        let mut filled = 0usize;
        while filled < buf.len() {
            let at = end - (buf.len() - filled) as u64;
            match self.read_at(at, &mut buf[filled..]) {
                Ok(0) => return Err("executable shrank while loading"),
                Ok(read) => filled += read.min(buf.len() - filled),
                Err(_) => return Err(READ_FAILED),
            }
        }
        Ok(())
    }
}
