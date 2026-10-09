//! The registry-facing face of an AHCI port: [`BlockDevice`] over the
//! transfer, flush and statistics code in the parent module.

use core::sync::atomic::Ordering;

use super::{iowait, spans, AhciDisk, NAMES, TIMEOUT_NS};
use crate::block::stats::IoStats;
use crate::block::{BlockDevice, BlockError, Wait};

impl BlockDevice for AhciDisk {
    fn name(&self) -> &'static str {
        NAMES[self.index]
    }

    fn sector_count(&self) -> u64 {
        self.sectors.load(Ordering::Relaxed)
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        self.read_sectors_vectored_with(lba, &mut [buf], Wait::Spin)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        self.write_sectors_vectored_with(lba, &[buf], Wait::Spin)
    }

    fn read_sectors_vectored(&self, lba: u64, bufs: &mut [&mut [u8]]) -> Result<(), BlockError> {
        self.read_sectors_vectored_with(lba, bufs, Wait::Spin)
    }

    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), BlockError> {
        self.write_sectors_vectored_with(lba, bufs, Wait::Spin)
    }

    /// The HBA writes straight into `bufs`, which stay borrowed until every
    /// command completed (or the port was stopped).
    fn read_sectors_vectored_with(
        &self,
        lba: u64,
        bufs: &mut [&mut [u8]],
        wait: Wait,
    ) -> Result<(), BlockError> {
        let segments = spans(
            bufs.iter_mut()
                .map(|buf| (buf.as_mut_ptr() as *const u8, buf.len())),
        )?;
        self.transfer(false, lba, &segments, wait)
    }

    fn write_sectors_vectored_with(
        &self,
        lba: u64,
        bufs: &[&[u8]],
        wait: Wait,
    ) -> Result<(), BlockError> {
        let segments = spans(bufs.iter().map(|buf| (buf.as_ptr(), buf.len())))?;
        self.transfer(true, lba, &segments, wait)
    }

    fn flush(&self) -> Result<(), BlockError> {
        let mut guard = self.state.lock();
        let live = guard.as_mut().ok_or(BlockError::Io)?;
        let expect = &self.expect_write;
        let mut waiter =
            |ready: &dyn Fn() -> bool| iowait::wait_until(Wait::Spin, expect, TIMEOUT_NS, ready);
        let result = live.port.flush(&live.hw, &mut waiter);
        if live.port.is_detached() {
            *guard = None;
        }
        match result {
            Ok(()) => {
                self.stats.count_flush();
                Ok(())
            }
            Err(error) => {
                serial_println!("{}: flush failed: {error}", NAMES[self.index]);
                Err(BlockError::Io)
            }
        }
    }

    fn is_writable(&self) -> bool {
        self.state.lock().is_some()
    }

    fn stats(&self) -> Option<&IoStats> {
        Some(&self.stats)
    }
}
