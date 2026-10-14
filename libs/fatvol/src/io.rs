//! The device under a volume and the sector cache between them.
//!
//! [`BlockIo`] is the raw device: 512-byte sectors, at most [`MAX_IO`] bytes a
//! request (a USB stick is served 64 KiB at a time). [`Cache`] stands between
//! it and `rust-fatfs`, which reads and writes a few bytes at a time (a FAT
//! entry is four bytes, a directory entry thirty-two) and wants a byte
//! stream.
//!
//! # Write-back within an operation, write-through between them
//!
//! A write only dirties cached sectors. [`Cache::flush_dirty`] sends them to
//! the device in ascending order, adjacent sectors as one request, and the
//! volume calls it before it answers every operation that changed anything,
//! so a power cut loses at most the operation in flight, as with a
//! write-through cache, while a 64 KiB file write costs a handful of device
//! requests instead of thousands. Nothing is ever held back past an
//! operation: a stick that is pulled has nothing in this process to lose.
//!
//! A failed **write** is sticky: every later call fails with the same
//! [`Fault`], because the sectors that did not land are still dirty and the
//! volume above no longer knows what is on the medium. So is a medium that
//! is gone. A failed **read** is only that call's error (a slow stick that
//! timed out once answers the next request) unless changes are waiting in
//! the cache, which would then be committed half done; and the device's own
//! flush is only sent after something was written.
//!
//! Large aligned transfers (a file's data) bypass the pages: they would only
//! be copied through and evict the metadata that makes the next lookup fast.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use fatfs::IoError;

mod direct;
mod stream;

pub use stream::Shared;

/// Bytes in a sector.
pub const SECTOR: usize = 512;
/// The most a [`BlockIo`] request carries.
pub const MAX_IO: usize = 64 * 1024;

const PAGE_SECTORS: u64 = 8;
const PAGE_BYTES: usize = SECTOR * PAGE_SECTORS as usize;
/// Pages kept (2 MiB).
const MAX_PAGES: usize = 512;
/// Pages a miss reads at once (16 KiB): the next FAT entry or directory
/// entry is almost always in the next sector.
const READ_AHEAD: u64 = 4;
/// Transfers at least this long go straight to the device.
const BYPASS: usize = 32 * 1024;
/// Dirty bytes that force a flush inside one long operation.
const DIRTY_LIMIT: usize = 1024 * 1024;

/// Why a device request failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The device reported an error.
    Io,
    /// The medium is gone (unplugged): the volume is over.
    Gone,
    /// A write to a read-only medium.
    ReadOnly,
    /// A read or write ran past the end.
    Eof,
}

impl IoError for Fault {
    fn is_interrupted(&self) -> bool {
        false
    }

    fn new_unexpected_eof_error() -> Self {
        Fault::Eof
    }

    fn new_write_zero_error() -> Self {
        Fault::Io
    }
}

/// A raw device of 512-byte sectors.
pub trait BlockIo {
    /// Sectors on the device (the volume's window of the disk).
    fn sectors(&self) -> u64;
    /// Read `buf.len() / 512` sectors from `lba`; `buf` is a whole number of
    /// sectors, at most [`MAX_IO`].
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Fault>;
    /// Write whole sectors from `lba`; at most [`MAX_IO`].
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Fault>;
    /// Make everything written so far durable on the device.
    fn flush(&mut self) -> Result<(), Fault>;
}

/// Counters, for tests and the daemon's log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub reads: u64,
    pub writes: u64,
    pub hits: u64,
    pub misses: u64,
}

struct Page {
    data: Box<[u8; PAGE_BYTES]>,
    /// Bit `i`: sector `i` of the page holds what the device holds (or what
    /// is waiting to be written).
    valid: u8,
    /// Bit `i`: sector `i` is waiting to be written.
    dirty: u8,
    used: u64,
}

impl Page {
    fn new() -> Page {
        Page {
            data: Box::new([0; PAGE_BYTES]),
            valid: 0,
            dirty: 0,
            used: 0,
        }
    }
}

/// A sector cache over a [`BlockIo`], read, written and seeked as a byte
/// stream by `rust-fatfs` (through [`Shared`]).
pub struct Cache<D> {
    dev: D,
    pages: BTreeMap<u64, Page>,
    tick: u64,
    pos: u64,
    dirty_bytes: usize,
    /// Something was written since the device was last flushed.
    wrote: bool,
    failed: Option<Fault>,
    writable: bool,
    stats: Stats,
}

impl<D: BlockIo> Cache<D> {
    pub fn new(dev: D, writable: bool) -> Cache<D> {
        Cache {
            dev,
            pages: BTreeMap::new(),
            tick: 0,
            pos: 0,
            dirty_bytes: 0,
            wrote: false,
            failed: None,
            writable,
            stats: Stats::default(),
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The device, for a test to look at.
    pub fn device(&self) -> &D {
        &self.dev
    }

    /// The device, mutable (a test injects faults through it).
    pub fn device_mut(&mut self) -> &mut D {
        &mut self.dev
    }

    /// Put the stream position back at the start (a filesystem object is
    /// opened on it from byte 0).
    pub fn rewind(&mut self) {
        self.pos = 0;
    }

    /// The sticky failure, if a request ever failed.
    pub fn failure(&self) -> Option<Fault> {
        self.failed
    }

    fn len_bytes(&self) -> u64 {
        self.dev.sectors() * SECTOR as u64
    }

    fn check(&self) -> Result<(), Fault> {
        self.failed.map_or(Ok(()), Err)
    }

    fn fail(&mut self, fault: Fault) -> Fault {
        self.failed = Some(fault);
        fault
    }

    /// A failed read: the medium being gone ends the volume, anything else
    /// is this request's error alone.
    fn read_failed(&mut self, fault: Fault) -> Fault {
        // With changes waiting the operation is half done: committing them
        // (the volume writes back after every operation, failed or not)
        // would leave a deleted target and a rename that never happened.
        if fault == Fault::Gone || self.dirty_bytes > 0 {
            self.fail(fault)
        } else {
            fault
        }
    }

    /// Read `buf` at byte offset `at`.
    pub fn read_at(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Fault> {
        self.check()?;
        let end = at.checked_add(buf.len() as u64).ok_or(Fault::Eof)?;
        if end > self.len_bytes() {
            return Err(Fault::Eof);
        }
        if buf.len() >= BYPASS
            && at.is_multiple_of(SECTOR as u64)
            && buf.len().is_multiple_of(SECTOR)
        {
            return self.read_direct(at / SECTOR as u64, buf);
        }
        let mut done = 0;
        while done < buf.len() {
            let offset = at + done as u64;
            let sector = offset / SECTOR as u64;
            let within = (offset % SECTOR as u64) as usize;
            let take = (SECTOR - within).min(buf.len() - done);
            let page = self.valid_sector(sector)?;
            let slot = (sector % PAGE_SECTORS) as usize;
            let from = slot * SECTOR + within;
            buf[done..done + take].copy_from_slice(&page.data[from..from + take]);
            done += take;
        }
        Ok(())
    }

    /// Write `data` at byte offset `at` into the cache (or straight through
    /// for a long aligned run).
    pub fn write_at(&mut self, at: u64, data: &[u8]) -> Result<(), Fault> {
        self.check()?;
        if !self.writable {
            return Err(Fault::ReadOnly);
        }
        let end = at.checked_add(data.len() as u64).ok_or(Fault::Eof)?;
        if end > self.len_bytes() {
            return Err(Fault::Eof);
        }
        if data.len() >= BYPASS
            && at.is_multiple_of(SECTOR as u64)
            && data.len().is_multiple_of(SECTOR)
        {
            return self.write_direct(at / SECTOR as u64, data);
        }
        let mut done = 0;
        while done < data.len() {
            let offset = at + done as u64;
            let sector = offset / SECTOR as u64;
            let within = (offset % SECTOR as u64) as usize;
            let take = (SECTOR - within).min(data.len() - done);
            let whole = within == 0 && take == SECTOR;
            let tick = self.touch();
            let index = sector / PAGE_SECTORS;
            let slot = (sector % PAGE_SECTORS) as usize;
            if whole {
                self.page_for_write(index)?;
            } else {
                // A part of a sector: its other bytes must be the device's.
                self.valid_sector(sector)?;
            }
            let page = self.pages.get_mut(&index).ok_or(Fault::Io)?;
            let from = slot * SECTOR + within;
            page.data[from..from + take].copy_from_slice(&data[done..done + take]);
            page.valid |= 1 << slot;
            if page.dirty & (1 << slot) == 0 {
                page.dirty |= 1 << slot;
                self.dirty_bytes += SECTOR;
            }
            page.used = tick;
            done += take;
        }
        if self.dirty_bytes >= DIRTY_LIMIT {
            self.flush_dirty()?;
        }
        Ok(())
    }

    /// Send every dirty sector to the device: ascending, adjacent sectors as
    /// one request of at most [`MAX_IO`] bytes.
    pub fn flush_dirty(&mut self) -> Result<(), Fault> {
        self.check()?;
        if self.dirty_bytes == 0 {
            return Ok(());
        }
        let mut dirty: Vec<u64> = Vec::new();
        for (&index, page) in &self.pages {
            for slot in 0..PAGE_SECTORS {
                if page.dirty & (1 << slot) != 0 {
                    dirty.push(index * PAGE_SECTORS + slot);
                }
            }
        }
        let mut start = 0;
        let mut staging: Vec<u8> = Vec::new();
        while start < dirty.len() {
            let mut end = start + 1;
            while end < dirty.len()
                && dirty[end] == dirty[end - 1] + 1
                && (end - start + 1) * SECTOR <= MAX_IO
            {
                end += 1;
            }
            staging.clear();
            for &sector in &dirty[start..end] {
                let page = &self.pages[&(sector / PAGE_SECTORS)];
                let from = (sector % PAGE_SECTORS) as usize * SECTOR;
                staging.extend_from_slice(&page.data[from..from + SECTOR]);
            }
            self.stats.writes += 1;
            if let Err(fault) = self.dev.write(dirty[start], &staging) {
                return Err(self.fail(fault));
            }
            self.wrote = true;
            for &sector in &dirty[start..end] {
                if let Some(page) = self.pages.get_mut(&(sector / PAGE_SECTORS)) {
                    page.dirty &= !(1 << (sector % PAGE_SECTORS));
                }
            }
            self.dirty_bytes -= (end - start) * SECTOR;
            start = end;
        }
        Ok(())
    }

    /// [`Cache::flush_dirty`], then the device's own flush.
    pub fn sync(&mut self) -> Result<(), Fault> {
        self.flush_dirty()?;
        if !self.wrote {
            return Ok(());
        }
        // The device's own flush failing loses no sector this cache holds:
        // it is that call's error, and the next sync tries again.
        self.dev.flush().map_err(|fault| self.read_failed(fault))?;
        self.wrote = false;
        Ok(())
    }

    fn touch(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Make room for one more page: drop the least recently used clean one,
    /// writing the dirty ones back first when there is none.
    fn make_room(&mut self) -> Result<(), Fault> {
        if self.pages.len() < MAX_PAGES {
            return Ok(());
        }
        let victim = |pages: &BTreeMap<u64, Page>| {
            pages
                .iter()
                .filter(|(_, page)| page.dirty == 0)
                .min_by_key(|(_, page)| page.used)
                .map(|(&index, _)| index)
        };
        let index = match victim(&self.pages) {
            Some(index) => index,
            None => {
                self.flush_dirty()?;
                victim(&self.pages).ok_or(Fault::Io)?
            }
        };
        self.pages.remove(&index);
        Ok(())
    }

    /// The page of `index`, present (not necessarily valid), for a write of
    /// whole sectors.
    fn page_for_write(&mut self, index: u64) -> Result<(), Fault> {
        if !self.pages.contains_key(&index) {
            self.make_room()?;
            let mut page = Page::new();
            page.used = self.touch();
            self.pages.insert(index, page);
        }
        Ok(())
    }

    /// The page holding `sector`, with that sector valid, reading from the
    /// device (a read-ahead window) when it is not.
    fn valid_sector(&mut self, sector: u64) -> Result<&Page, Fault> {
        let index = sector / PAGE_SECTORS;
        let slot = sector % PAGE_SECTORS;
        let tick = self.touch();
        let have = self
            .pages
            .get(&index)
            .is_some_and(|page| page.valid & (1 << slot) != 0);
        if have {
            self.stats.hits += 1;
        } else {
            self.stats.misses += 1;
            self.fill(index)?;
        }
        let page = self.pages.get_mut(&index).ok_or(Fault::Io)?;
        page.used = tick;
        Ok(page)
    }

    /// Read pages `index..` (up to [`READ_AHEAD`], stopping at the end of
    /// the device or at a page already held whole) in one request.
    fn fill(&mut self, index: u64) -> Result<(), Fault> {
        let total_pages = self.dev.sectors().div_ceil(PAGE_SECTORS);
        let mut count = 0;
        while count < READ_AHEAD && index + count < total_pages {
            let held = self
                .pages
                .get(&(index + count))
                .is_some_and(|page| page.valid == 0xFF);
            if held && count > 0 {
                break;
            }
            count += 1;
        }
        let first = index * PAGE_SECTORS;
        let sectors = (count * PAGE_SECTORS).min(self.dev.sectors() - first);
        let mut staging = alloc::vec![0u8; sectors as usize * SECTOR];
        self.stats.reads += 1;
        if let Err(fault) = self.dev.read(first, &mut staging) {
            return Err(self.read_failed(fault));
        }
        for n in 0..count {
            let page_index = index + n;
            if !self.pages.contains_key(&page_index) {
                self.make_room()?;
                self.pages.insert(page_index, Page::new());
            }
            let tick = self.touch();
            let page = self.pages.get_mut(&page_index).ok_or(Fault::Io)?;
            page.used = tick;
            for slot in 0..PAGE_SECTORS {
                let sector = n * PAGE_SECTORS + slot;
                if sector >= sectors {
                    break;
                }
                // Never overwrite a sector that is newer than the device.
                if page.valid & (1 << slot) == 0 {
                    let from = sector as usize * SECTOR;
                    let to = slot as usize * SECTOR;
                    page.data[to..to + SECTOR].copy_from_slice(&staging[from..from + SECTOR]);
                    page.valid |= 1 << slot;
                }
            }
        }
        Ok(())
    }
}
