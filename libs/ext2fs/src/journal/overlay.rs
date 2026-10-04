//! Replaying a log onto a device that cannot be written.
//!
//! A read-only mount of a volume whose log holds committed work must still
//! show that work (the home blocks are stale until it is applied), and must
//! not touch the device. The replay therefore writes into an in-memory
//! overlay of sectors, and every later read sees the overlay over the device.
//! The overlay reports itself unwritable, so the volume mounted over it is
//! read-only and nothing writes after the replay.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use spin::Mutex;

use crate::{BlockIo, IoError, SECTOR_SIZE};

/// `inner` with the sectors written since [`Overlay::new`] laid over it.
pub(crate) struct Overlay {
    inner: Box<dyn BlockIo>,
    sectors: Mutex<BTreeMap<u64, [u8; SECTOR_SIZE]>>,
}

impl Overlay {
    pub fn new(inner: Box<dyn BlockIo>) -> Overlay {
        Overlay {
            inner,
            sectors: Mutex::new(BTreeMap::new()),
        }
    }
}

// The sector size is the unit of the overlay; chunking by it is the point.
#[allow(clippy::chunks_exact_to_as_chunks)]
impl BlockIo for Overlay {
    fn sector_count(&self) -> u64 {
        self.inner.sector_count()
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.inner.read_sectors(lba, buf)?;
        let sectors = self.sectors.lock();
        for (index, sector) in buf.chunks_exact_mut(SECTOR_SIZE).enumerate() {
            if let Some(saved) = sectors.get(&(lba + index as u64)) {
                sector.copy_from_slice(saved);
            }
        }
        Ok(())
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        if !buf.len().is_multiple_of(SECTOR_SIZE)
            || lba + (buf.len() / SECTOR_SIZE) as u64 > self.sector_count()
        {
            return Err(IoError::Failed);
        }
        let mut sectors = self.sectors.lock();
        for (index, sector) in buf.chunks_exact(SECTOR_SIZE).enumerate() {
            let mut saved = [0u8; SECTOR_SIZE];
            saved.copy_from_slice(sector);
            sectors.insert(lba + index as u64, saved);
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), IoError> {
        Ok(())
    }

    fn is_writable(&self) -> bool {
        false
    }
}

/// Stands in for the device while it is moved into an [`Overlay`].
pub(crate) struct NoDevice;

impl BlockIo for NoDevice {
    fn sector_count(&self) -> u64 {
        0
    }

    fn read_sectors(&self, _lba: u64, _buf: &mut [u8]) -> Result<(), IoError> {
        Err(IoError::Failed)
    }

    fn write_sectors(&self, _lba: u64, _buf: &[u8]) -> Result<(), IoError> {
        Err(IoError::Failed)
    }

    fn flush(&self) -> Result<(), IoError> {
        Ok(())
    }

    fn is_writable(&self) -> bool {
        false
    }
}
