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

use super::pagecache::{DirtyPage, PAGE};
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
    /// size: a page past the end is not part of it), waiting for the volume.
    pub fn writeback(&self, first: u64, last: u64) -> Result<(), FsError> {
        self.inode.writeback(first, last, true)
    }

    /// A mapping of pages `first..=last` went away: write what it changed
    /// back and let go of the pages nothing maps any more. `wait` false is
    /// for teardown that must not park (what is skipped stays dirty and the
    /// flusher writes it).
    pub fn release_pages(&self, first: u64, last: u64, wait: bool) {
        if let Err(error) = self.inode.writeback(first, last, wait) {
            crate::serial_println!("mmap: writeback of {} failed: {error:?}", self.path());
        }
        self.inode.cache.trim(first, last);
    }

    /// Let go of page `index` if nothing maps it and it is clean (a fault
    /// that lost a race to another thread of the same mapping).
    pub fn trim_page(&self, index: u64) {
        self.inode.cache.trim(index, index);
    }

    /// An identity of the file shared by every description of it.
    pub fn inode_id(&self) -> usize {
        alloc::sync::Arc::as_ptr(&self.inode) as usize
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

/// Write back every dirty file page of every open file. The periodic
/// flusher's call (`wait` false) never parks: it runs on the kernel task, so
/// a busy volume, or a filesystem that cannot write without waiting (FUSE),
/// leaves the page dirty for the next round. `sync(2)` passes `wait`.
pub fn writeback_all(wait: bool) {
    static LOGGED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    let files: Vec<_> = OPEN
        .lock()
        .iter()
        .filter(|inode| inode.cache.has_dirty())
        .cloned()
        .collect();
    for inode in files {
        // A file with no node is path-addressed (ramfs): a write by name
        // takes the VFS lock, so only a waiting caller does it.
        if !wait && inode.node.is_none() {
            continue;
        }
        if let Err(error) = inode.writeback(0, LAST_PAGE, wait) {
            // Once: a failing volume would repeat this every round.
            if !LOGGED.swap(true, core::sync::atomic::Ordering::Relaxed) {
                crate::serial_println!(
                    "mmap: writeback of {} failed: {error:?} (reported once)",
                    inode.path()
                );
            }
        }
    }
}

/// A file changed behind the cache's back (a native write by name): refresh
/// the clean cached pages from the file, so mappings see it, and cut the
/// cache to the new size.
pub(super) fn refresh(inode: &Inode) {
    if inode.cache.is_empty() {
        return;
    }
    let Ok(size) = inode.size() else {
        return;
    };
    inode.cache.truncate(size);
    for (index, frame) in inode.cache.clean_pages() {
        let start = index * PAGE;
        let want = size.saturating_sub(start).min(PAGE) as usize;
        let mut buf = alloc::vec![0u8; PAGE as usize];
        let mut done = 0;
        while done < want {
            match inode.read_raw(start + done as u64, &mut buf[done..want]) {
                Ok(0) | Err(_) => break,
                Ok(count) => done += count,
            }
        }
        super::pagecache::copy_to_frame(frame.as_u64(), 0, &buf);
        free_frame(frame);
    }
}

/// The highest page index a range can name.
const LAST_PAGE: u64 = u64::MAX / PAGE;

impl Drop for Inode {
    /// The last description and mapping are gone: what a store through a
    /// shared mapping left unwritten (the exit-time flush skipped it, or the
    /// flusher never reached it) goes to the file now.
    fn drop(&mut self) {
        if self.cache.has_dirty() {
            if let Err(error) = self.writeback(0, LAST_PAGE, true) {
                crate::serial_println!("mmap: final writeback of {} failed: {error:?}", self.path());
            }
        }
    }
}

impl Inode {
    /// Write the dirty pages of `first..=last` to the file, up to its size
    /// at the time of the write. Every mapping of a page is write-protected
    /// first, so the next store through it faults and dirties it again. A
    /// page that could not be written (busy volume with `wait` false, an
    /// error) stays dirty.
    fn writeback(&self, first: u64, last: u64, wait: bool) -> Result<(), FsError> {
        if self.cache.is_empty() {
            return Ok(());
        }
        let dirty = self.cache.take_dirty(first, last);
        if dirty.is_empty() {
            return Ok(());
        }
        let indices: Vec<u64> = dirty.iter().map(|page| page.index).collect();
        crate::mem::filemap::write_protect(self as *const Inode as usize, &indices);
        let mut result = Ok(());
        for page in dirty {
            let written = self.flush_page(&page, wait);
            if let Some(Err(error)) = written {
                result = Err(error);
            }
            self.cache.finish(page, matches!(written, Some(Ok(_))));
        }
        result
    }

    /// One page to the file: `None` when it was not written for want of
    /// waiting.
    fn flush_page(&self, page: &DirtyPage, wait: bool) -> Option<Result<usize, FsError>> {
        // SAFETY: `page.frame` is a live frame we hold a reference to,
        // reachable through the physical window; it is read in place so the
        // bytes written are the frame's at write time, not at take time.
        let data = unsafe {
            core::slice::from_raw_parts(phys_to_virt(page.frame).as_ptr::<u8>(), PAGE as usize)
        };
        let offset = page.index * PAGE;
        match &self.node {
            Some(node) => node.flush_page(offset, data, wait),
            None if !wait => None,
            None => {
                let path = self.path();
                let size = match crate::fs::abi_stat(Id::ROOT, &path) {
                    Ok(meta) => meta.size,
                    Err(error) => return Some(Err(error)),
                };
                if offset >= size {
                    return Some(Ok(0));
                }
                let keep = data.len().min((size - offset) as usize);
                Some(crate::fs::abi_write(Id::ROOT, &path, offset, &data[..keep]))
            }
        }
    }

    fn read_raw(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        match &self.node {
            Some(node) => node.read(offset, buf),
            None => crate::fs::abi_read_at(Id::ROOT, &self.path(), offset, buf),
        }
    }

    pub(super) fn size(&self) -> Result<u64, FsError> {
        match &self.node {
            Some(node) => node.stat().map(|meta| meta.size),
            None => crate::fs::abi_stat(Id::ROOT, &self.path()).map(|meta| meta.size),
        }
    }
}
