//! memfd: an anonymous file whose bytes live in page frames of its own
//! (`memfd_create`, `docs/architecture/processes.md`).
//!
//! The scratch ramfs keeps file bytes in a growable `Vec<u8>` on the kernel
//! heap, which cannot be mapped into a second address space: a page of heap
//! has no stable frame and a `Vec` moves when it grows. A [`Memfd`] therefore
//! owns a vector of frames from the frame allocator, so `mmap(MAP_SHARED)`
//! can map the very same frames into every process that holds the descriptor
//! (`process::linux::mem`), and a `fork` child keeps them shared.
//!
//! # Accounting
//!
//! A page is charged when it is first materialised, once, whoever maps it:
//! `Resource::UserMemory` of the uid that created the memfd (released when the
//! page goes), and a global ceiling of `limit.scratch_max` bytes, the budget
//! `/tmp` is held to as well. A mapping of a memfd charges nothing more.
//! Files are sparse: `ftruncate` only moves the size, a page appears on the
//! first write or mapping, and a read of a hole is zeros.
//!
//! # Lifetime
//!
//! The frames carry one reference for the memfd and one per mapping (the page
//! tables), so a mapping outlives the descriptor and the descriptor outlives
//! a `munmap`; the frame returns to the pool with the last of them.
//! Shrinking a memfd drops its own reference to the pages past the new size
//! (and their charge); a process that still maps one keeps the frame alive
//! until it unmaps it, where Linux would raise `SIGBUS` on the next touch.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;

use super::{alloc_zeroed_frame, free_frame, phys_to_virt, share_frame, FRAME_SIZE};
use crate::quota::{self, Resource};

/// Longest name `memfd_create` accepts (Linux's `NAME_MAX - 6`).
pub const NAME_MAX: usize = 249;

/// Frames held by every memfd right now (test and soak observable).
static LIVE_PAGES: AtomicUsize = AtomicUsize::new(0);

/// Why a memfd operation could not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemfdError {
    /// The memfd budget (`limit.scratch_max`) is spent (`ENOSPC`).
    NoSpace,
    /// The uid's memory quota or the frame allocator refused (`ENOMEM`).
    NoMemory,
    /// The size is past what one memfd may hold (`EFBIG`).
    TooBig,
}

/// The pages and the size, under one lock.
struct State {
    /// The materialised pages by index. A hole has no entry, so a sparse
    /// file (`ftruncate` to the budget, a mapping past the end) costs no
    /// kernel heap until a page is written or mapped: one small node per
    /// resident page, each already paid for as a frame.
    pages: BTreeMap<usize, u64>,
    size: u64,
}

/// An open memfd: what `memfd_create` returns, shared by `dup` and `fork`
/// (one open file description, so one file offset).
pub struct Memfd {
    name: String,
    /// The uid that pays for the pages.
    uid: u32,
    /// The descriptor's file offset.
    offset: AtomicU64,
    state: Mutex<State>,
}

/// The most pages all memfds together may hold (`limit.scratch_max`).
fn page_budget() -> usize {
    usize::try_from(crate::limits::scratch_max() / FRAME_SIZE).unwrap_or(usize::MAX)
}

/// Frames held by every memfd right now.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn live_pages() -> usize {
    LIVE_PAGES.load(Ordering::Acquire)
}

impl Memfd {
    /// A new empty memfd named `name` (without the `/memfd:` prefix) whose
    /// pages are charged to `uid`.
    pub fn new(name: &str, uid: u32) -> Option<alloc::sync::Arc<Memfd>> {
        let mut owned = String::new();
        owned.try_reserve_exact(name.len()).ok()?;
        owned.push_str(name);
        Some(alloc::sync::Arc::new(Memfd {
            name: owned,
            uid,
            offset: AtomicU64::new(0),
            state: Mutex::new(State {
                pages: BTreeMap::new(),
                size: 0,
            }),
        }))
    }

    /// The name given at creation.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The uid the pages are charged to (the file's owner).
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// The file size in bytes.
    pub fn size(&self) -> u64 {
        self.state.lock().size
    }

    /// The descriptor's file offset.
    pub fn offset(&self) -> u64 {
        self.offset.load(Ordering::Relaxed)
    }

    pub fn set_offset(&self, offset: u64) {
        self.offset.store(offset, Ordering::Relaxed);
    }

    /// Pages materialised in this memfd (a hole holds none).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn resident_pages(&self) -> usize {
        self.state.lock().pages.len()
    }

    /// Whether page `index` lies within the budget one memfd may hold.
    fn within_budget(index: usize) -> Result<(), MemfdError> {
        if index >= page_budget() {
            return Err(MemfdError::TooBig);
        }
        Ok(())
    }

    /// Materialise page `index` (a zeroed frame, charged once) unless it
    /// exists, and return its frame. Growing the map is the one allocation
    /// here, one small node per resident page.
    fn page_locked(&self, state: &mut State, index: usize) -> Result<u64, MemfdError> {
        Self::within_budget(index)?;
        if let Some(&frame) = state.pages.get(&index) {
            return Ok(frame);
        }
        LIVE_PAGES
            .try_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                (live < page_budget()).then_some(live + 1)
            })
            .map_err(|_| MemfdError::NoSpace)?;
        if quota::charge(self.uid, Resource::UserMemory, FRAME_SIZE).is_err() {
            LIVE_PAGES.fetch_sub(1, Ordering::AcqRel);
            return Err(MemfdError::NoMemory);
        }
        let Some(frame) = alloc_zeroed_frame() else {
            quota::release(self.uid, Resource::UserMemory, FRAME_SIZE);
            LIVE_PAGES.fetch_sub(1, Ordering::AcqRel);
            return Err(MemfdError::NoMemory);
        };
        state.pages.insert(index, frame.as_u64());
        Ok(frame.as_u64())
    }

    /// Page `index`'s frame with one extra reference taken for the caller (a
    /// mapping that will own it), materialising the page if it is a hole.
    pub fn page_for_mapping(&self, index: u64) -> Result<PhysAddr, MemfdError> {
        let index = usize::try_from(index).map_err(|_| MemfdError::TooBig)?;
        let mut state = self.state.lock();
        let frame = PhysAddr::new(self.page_locked(&mut state, index)?);
        if !share_frame(frame) {
            return Err(MemfdError::NoMemory);
        }
        Ok(frame)
    }

    /// `ftruncate`: set the size. Growing adds a hole; shrinking drops the
    /// pages wholly past the new end and zeroes the rest of the last page, so
    /// a later growth reads zeros.
    pub fn set_size(&self, size: u64) -> Result<(), MemfdError> {
        let pages = size.div_ceil(FRAME_SIZE);
        if pages > page_budget() as u64 {
            return Err(MemfdError::TooBig);
        }
        let pages = pages as usize;
        let mut freed: Vec<u64> = Vec::new();
        {
            let mut state = self.state.lock();
            if size <= state.size {
                let tail = state.pages.split_off(&pages);
                freed.extend(tail.into_values());
                zero_tail(&state, size);
            }
            state.size = size;
        }
        for frame in freed {
            self.release_page(frame);
        }
        Ok(())
    }

    /// Return one page to the pool and give its charge back.
    fn release_page(&self, frame: u64) {
        free_frame(PhysAddr::new(frame));
        quota::release(self.uid, Resource::UserMemory, FRAME_SIZE);
        LIVE_PAGES.fetch_sub(1, Ordering::AcqRel);
    }

    /// Copy up to `dst.len()` bytes from `offset`; short at the end of the
    /// file, and holes read as zeros.
    pub fn read_at(&self, offset: u64, dst: &mut [u8]) -> usize {
        let state = self.state.lock();
        if offset >= state.size {
            return 0;
        }
        let count = dst.len().min((state.size - offset) as usize);
        let mut done = 0;
        while done < count {
            let at = offset + done as u64;
            let (index, within) = ((at / FRAME_SIZE) as usize, (at % FRAME_SIZE) as usize);
            let piece = (count - done).min(FRAME_SIZE as usize - within);
            match state.pages.get(&index).copied() {
                Some(frame) => copy_from_frame(frame, within, &mut dst[done..done + piece]),
                None => dst[done..done + piece].fill(0),
            }
            done += piece;
        }
        count
    }

    /// Write `src` at `offset`, growing the file when it ends past the size.
    /// Every page the write touches is materialised first, so a refusal
    /// changes neither the size nor the bytes.
    pub fn write_at(&self, offset: u64, src: &[u8]) -> Result<usize, MemfdError> {
        if src.is_empty() {
            return Ok(0);
        }
        let end = offset
            .checked_add(src.len() as u64)
            .filter(|end| *end <= i64::MAX as u64)
            .ok_or(MemfdError::TooBig)?;
        let mut state = self.state.lock();
        let first = (offset / FRAME_SIZE) as usize;
        let last = ((end - 1) / FRAME_SIZE) as usize;
        for index in first..=last {
            self.page_locked(&mut state, index)?;
        }
        let mut done = 0;
        while done < src.len() {
            let at = offset + done as u64;
            let (index, within) = ((at / FRAME_SIZE) as usize, (at % FRAME_SIZE) as usize);
            let piece = (src.len() - done).min(FRAME_SIZE as usize - within);
            if let Some(&frame) = state.pages.get(&index) {
                copy_to_frame(frame, within, &src[done..done + piece]);
            }
            done += piece;
        }
        state.size = state.size.max(end);
        Ok(src.len())
    }
}

/// Zero the bytes of the last kept page past `size` (shrinking).
fn zero_tail(state: &State, size: u64) {
    let within = (size % FRAME_SIZE) as usize;
    if within == 0 {
        return;
    }
    if let Some(&frame) = state.pages.get(&((size / FRAME_SIZE) as usize)) {
        let zeros = [0u8; FRAME_SIZE as usize];
        copy_to_frame(frame, within, &zeros[within..]);
    }
}

fn copy_from_frame(frame: u64, within: usize, dst: &mut [u8]) {
    // SAFETY: `frame` is a live frame this memfd owns, mapped by the
    // physical-memory window; `within + dst.len()` stays inside the 4 KiB
    // frame (callers split at page boundaries) and the kernel buffer `dst` is
    // distinct from it.
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt(PhysAddr::new(frame))
                .as_ptr::<u8>()
                .add(within),
            dst.as_mut_ptr(),
            dst.len(),
        );
    }
}

fn copy_to_frame(frame: u64, within: usize, src: &[u8]) {
    // SAFETY: as `copy_from_frame`: a live frame owned by the memfd, a range
    // inside it, and a kernel buffer that does not overlap it.
    unsafe {
        core::ptr::copy_nonoverlapping(
            src.as_ptr(),
            phys_to_virt(PhysAddr::new(frame))
                .as_mut_ptr::<u8>()
                .add(within),
            src.len(),
        );
    }
}

impl Drop for Memfd {
    /// The last description is gone: the memfd's reference to each page
    /// goes (a mapping that outlives it keeps the frame), with its charge.
    fn drop(&mut self) {
        let frames: Vec<u64> = core::mem::take(&mut self.state.get_mut().pages)
            .into_values()
            .collect();
        for frame in frames {
            self.release_page(frame);
        }
    }
}
