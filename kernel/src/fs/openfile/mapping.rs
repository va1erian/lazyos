//! What a file-backed `mmap` needs from an open file: its page frames,
//! filled on demand, and their writeback (`pagecache`).
//!
//! The frames are page-cache pages shared by every mapping and description of
//! the file. A fault asks for one with [`OpenFile::page_frame`]; a store
//! through a writable `MAP_SHARED` mapping dirties it, and
//! [`OpenFile::writeback`] (`msync`, `munmap`, exit, `fsync`, the flusher)
//! writes it to the file.

use alloc::vec::Vec;
use x86_64::PhysAddr;

use super::pagecache::{copy_from_frame, DirtyPage, PAGE};
use super::{Inode, OpenFile, OPEN};
use crate::fs::vfs::{FsError, Id};
use crate::mem::{alloc_zeroed_frame, free_frame, phys_to_virt};

/// Why a page of a file cannot be mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// The page lies wholly past the end of the file (`SIGBUS` on Linux).
    Beyond,
    /// No frame to hold it.
    NoMemory,
    /// The file could not be read.
    Io,
}

/// How many times a fill retries when a write raced with its read.
const FILL_TRIES: usize = 8;

impl OpenFile {
    /// The frame of page `index`, with one reference taken for the mapping
    /// that will own it. Reads the page from the file the first time; bytes
    /// past the end of the file read as zeros.
    pub fn page_frame(&self, index: u64) -> Result<PhysAddr, MapError> {
        let cache = &self.inode.cache;
        for _ in 0..FILL_TRIES {
            if let Some(frame) = cache.lookup(index) {
                return Ok(frame);
            }
            let generation = cache.generation();
            let frame = self.read_page(index)?;
            match cache.insert(index, frame, generation) {
                Ok(frame) => return Ok(frame),
                // A write slipped in between the read and the insert: the
                // page may be stale, read it again.
                Err(()) => {
                    free_frame(frame);
                }
            }
        }
        Err(MapError::Io)
    }

    /// A new frame holding page `index` of the file as it is now (not
    /// cached), zero-filled past the end.
    fn read_page(&self, index: u64) -> Result<PhysAddr, MapError> {
        let start = index.checked_mul(PAGE).ok_or(MapError::Beyond)?;
        let size = self.stat().map_err(|_| MapError::Io)?.size;
        if start >= size {
            return Err(MapError::Beyond);
        }
        let frame = alloc_zeroed_frame().ok_or(MapError::NoMemory)?;
        let want = (size - start).min(PAGE) as usize;
        let mut buf = alloc::vec![0u8; want];
        let mut done = 0;
        while done < want {
            match self.read_raw(start + done as u64, &mut buf[done..]) {
                Ok(0) => break,
                Ok(count) => done += count,
                Err(_) => {
                    free_frame(frame);
                    return Err(MapError::Io);
                }
            }
        }
        super::pagecache::copy_to_frame(frame.as_u64(), 0, &buf[..done]);
        Ok(frame)
    }

    /// Copy page `index` into `dst` for a private mapping that is written
    /// before it was ever read: from the cache when the page is there (it may
    /// hold stores of a shared mapping), otherwise straight from the file.
    pub fn copy_page(&self, index: u64, dst: PhysAddr) -> Result<(), MapError> {
        if let Some(frame) = self.inode.cache.lookup(index) {
            // SAFETY: both are live frames reachable through the physical
            // window; `frame` is pinned by the reference `lookup` took, and
            // `dst` is the caller's own fresh frame, so they do not overlap.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    phys_to_virt(frame).as_ptr::<u8>(),
                    phys_to_virt(dst).as_mut_ptr::<u8>(),
                    PAGE as usize,
                );
            }
            free_frame(frame);
            return Ok(());
        }
        let frame = self.read_page(index)?;
        // SAFETY: as above; `frame` is a private frame of ours.
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(frame).as_ptr::<u8>(),
                phys_to_virt(dst).as_mut_ptr::<u8>(),
                PAGE as usize,
            );
        }
        free_frame(frame);
        Ok(())
    }

    /// Pages `first..=last` may now be changed through a writable shared
    /// mapping: reads must take their bytes from the frames from here on.
    pub fn mark_dirty(&self, first: u64, last: u64) {
        self.inode.cache.mark_dirty(first, last);
    }

    /// Write the dirty pages of `first..=last` to the file (clipped to its
    /// size: a page past the end is not part of it).
    pub fn writeback(&self, first: u64, last: u64) -> Result<(), FsError> {
        self.inode.writeback(first, last)
    }

    /// A mapping of pages `first..=last` went away: write what it changed
    /// back and let go of the pages nothing maps any more.
    pub fn release_pages(&self, first: u64, last: u64) {
        if let Err(error) = self.writeback(first, last) {
            crate::serial_println!("mmap: writeback of {} failed: {error:?}", self.path());
        }
        self.inode.cache.trim(first, last);
    }

    /// Cached page count (tests).
    #[cfg(lazyos_tests)]
    pub fn cached_pages(&self) -> usize {
        self.inode.cache.len()
    }

    /// Whether any cached page is dirty (tests).
    #[cfg(lazyos_tests)]
    pub fn has_dirty_pages(&self) -> bool {
        self.inode.cache.has_dirty()
    }
}

/// Write back every dirty file page of every open file: the periodic
/// flusher's job, so a store through a shared mapping reaches the file within
/// the flush interval even if the mapping lives on.
pub fn writeback_all() {
    let files: Vec<_> = OPEN
        .lock()
        .iter()
        .filter(|inode| inode.cache.has_dirty())
        .cloned()
        .collect();
    for inode in files {
        if let Err(error) = inode.writeback(0, LAST_PAGE) {
            crate::serial_println!("mmap: writeback of {} failed: {error:?}", inode.path());
        }
    }
}

/// The highest page index a range can name.
const LAST_PAGE: u64 = u64::MAX / PAGE;

impl Inode {
    /// Write the dirty pages of `first..=last` to the file, up to its size,
    /// with the file's own write (no cache update: the frame is the newer
    /// copy). A page that could not be written stays dirty.
    fn writeback(&self, first: u64, last: u64) -> Result<(), FsError> {
        if self.cache.is_empty() {
            return Ok(());
        }
        let dirty = self.cache.take_dirty(first, last);
        if dirty.is_empty() {
            return Ok(());
        }
        let size = self.size().ok();
        let mut result = Ok(());
        for page in dirty {
            let written = match size {
                Some(size) => self.write_page(&page, size),
                None => Err(FsError::Io),
            };
            if let Err(error) = written {
                result = Err(error);
            }
            self.cache.finish(page, written.is_ok());
        }
        result
    }

    fn size(&self) -> Result<u64, FsError> {
        match &self.node {
            Some(node) => node.stat().map(|meta| meta.size),
            None => crate::fs::abi_stat(Id::ROOT, &self.path()).map(|meta| meta.size),
        }
    }

    /// Write one frame to the file at its place, up to `size`.
    fn write_page(&self, page: &DirtyPage, size: u64) -> Result<(), FsError> {
        let start = page.index * PAGE;
        if start >= size {
            return Ok(());
        }
        let len = (size - start).min(PAGE) as usize;
        let mut buf = alloc::vec![0u8; len];
        copy_from_frame(page.frame.as_u64(), 0, &mut buf);
        let mut done = 0;
        while done < len {
            let at = start + done as u64;
            let count = match &self.node {
                Some(node) => node.write(at, &buf[done..])?,
                None => crate::fs::abi_write(Id::ROOT, &self.path(), at, &buf[done..])?,
            };
            if count == 0 {
                return Err(FsError::Io);
            }
            done += count;
        }
        Ok(())
    }
}
