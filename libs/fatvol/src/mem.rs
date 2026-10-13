//! A RAM device for tests and for the daemon's host harness: shared bytes
//! (clones see the same disk), counters, a write log and fault injection.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use fatfs::{format_volume, FatType, FormatVolumeOptions};

use crate::io::{BlockIo, Cache, Fault, Shared, SECTOR};

#[derive(Default)]
struct Inner {
    bytes: Vec<u8>,
    reads: u64,
    writes: u64,
    flushes: u64,
    /// Every write: `(lba, sectors)`.
    log: Vec<(u64, usize)>,
    /// Fail with `Fault::Io` once this many more writes have landed.
    fail_writes_after: Option<u64>,
    gone: bool,
    read_only: bool,
}

/// A RAM disk; clones share one disk.
#[derive(Clone, Default)]
pub struct MemDisk {
    inner: Rc<RefCell<Inner>>,
}

impl MemDisk {
    pub fn new(sectors: u64) -> MemDisk {
        let disk = MemDisk::default();
        disk.inner.borrow_mut().bytes = alloc::vec![0; sectors as usize * SECTOR];
        disk
    }

    /// A disk holding a copy of `bytes`.
    pub fn from_image(bytes: &[u8]) -> MemDisk {
        let disk = MemDisk::default();
        disk.inner.borrow_mut().bytes = bytes.to_vec();
        disk
    }

    /// A formatted FAT volume (`fat`: 12, 16 or 32) of `sectors` sectors.
    pub fn formatted(sectors: u64, fat: u8, label: &[u8; 11]) -> MemDisk {
        let disk = MemDisk::new(sectors);
        let mut cache = Cache::new(disk.clone(), true);
        cache.rewind();
        let shared = Shared(Rc::new(RefCell::new(cache)));
        let kind = match fat {
            12 => FatType::Fat12,
            16 => FatType::Fat16,
            _ => FatType::Fat32,
        };
        let options = FormatVolumeOptions::new()
            .fat_type(kind)
            .volume_label(*label)
            .volume_id(0x1234_5678);
        let mut storage = shared.clone();
        format_volume(&mut storage, options).expect("format");
        shared.0.borrow_mut().flush_dirty().expect("flush");
        disk.reset_counters();
        disk
    }

    pub fn image(&self) -> Vec<u8> {
        self.inner.borrow().bytes.clone()
    }

    pub fn byte(&self, offset: usize) -> u8 {
        self.inner.borrow().bytes[offset]
    }

    pub fn reads(&self) -> u64 {
        self.inner.borrow().reads
    }

    pub fn writes(&self) -> u64 {
        self.inner.borrow().writes
    }

    pub fn flushes(&self) -> u64 {
        self.inner.borrow().flushes
    }

    pub fn write_log(&self) -> Vec<(u64, usize)> {
        self.inner.borrow().log.clone()
    }

    pub fn reset_counters(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.reads = 0;
        inner.writes = 0;
        inner.flushes = 0;
        inner.log.clear();
    }

    /// Let `count` more writes land, then fail every one.
    pub fn fail_writes_after(&self, count: u64) {
        self.inner.borrow_mut().fail_writes_after = Some(count);
    }

    pub fn unplug(&self) {
        self.inner.borrow_mut().gone = true;
    }

    pub fn set_read_only(&self, read_only: bool) {
        self.inner.borrow_mut().read_only = read_only;
    }
}

impl BlockIo for MemDisk {
    fn sectors(&self) -> u64 {
        (self.inner.borrow().bytes.len() / SECTOR) as u64
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Fault> {
        let mut inner = self.inner.borrow_mut();
        if inner.gone {
            return Err(Fault::Gone);
        }
        let from = lba as usize * SECTOR;
        let slice = inner.bytes.get(from..from + buf.len()).ok_or(Fault::Eof)?;
        buf.copy_from_slice(slice);
        inner.reads += 1;
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Fault> {
        let mut inner = self.inner.borrow_mut();
        if inner.gone {
            return Err(Fault::Gone);
        }
        if inner.read_only {
            return Err(Fault::ReadOnly);
        }
        if let Some(left) = inner.fail_writes_after {
            if left == 0 {
                return Err(Fault::Io);
            }
            inner.fail_writes_after = Some(left - 1);
        }
        let from = lba as usize * SECTOR;
        let slice = inner
            .bytes
            .get_mut(from..from + buf.len())
            .ok_or(Fault::Eof)?;
        slice.copy_from_slice(buf);
        inner.writes += 1;
        inner.log.push((lba, buf.len() / SECTOR));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Fault> {
        let mut inner = self.inner.borrow_mut();
        if inner.gone {
            return Err(Fault::Gone);
        }
        inner.flushes += 1;
        Ok(())
    }
}
