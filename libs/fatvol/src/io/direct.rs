//! Transfers that bypass the pages: long aligned runs (a file's data) go
//! straight to the device and back, with the cache only kept coherent.

use super::{BlockIo, Cache, Fault, MAX_IO, PAGE_SECTORS, SECTOR};

impl<D: BlockIo> Cache<D> {
    pub(super) fn read_direct(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Fault> {
        for (index, chunk) in buf.chunks_mut(MAX_IO).enumerate() {
            let at = lba + (index * MAX_IO / SECTOR) as u64;
            self.stats.reads += 1;
            if let Err(fault) = self.dev.read(at, chunk) {
                return Err(self.read_failed(fault));
            }
        }
        // What the device returned may predate sectors still waiting here.
        self.overlay_dirty(lba, buf);
        Ok(())
    }

    pub(super) fn write_direct(&mut self, lba: u64, data: &[u8]) -> Result<(), Fault> {
        for (index, chunk) in data.chunks(MAX_IO).enumerate() {
            let at = lba + (index * MAX_IO / SECTOR) as u64;
            self.stats.writes += 1;
            if let Err(fault) = self.dev.write(at, chunk) {
                return Err(self.fail(fault));
            }
            self.wrote = true;
        }
        // Cached copies of these sectors now hold the new bytes, clean.
        for (n, sector_bytes) in data.chunks(SECTOR).enumerate() {
            let sector = lba + n as u64;
            let index = sector / PAGE_SECTORS;
            let slot = (sector % PAGE_SECTORS) as usize;
            if let Some(page) = self.pages.get_mut(&index) {
                page.data[slot * SECTOR..(slot + 1) * SECTOR].copy_from_slice(sector_bytes);
                page.valid |= 1 << slot;
                if page.dirty & (1 << slot) != 0 {
                    page.dirty &= !(1 << slot);
                    page.filedata &= !(1 << slot);
                    self.dirty_bytes -= SECTOR;
                }
            }
        }
        Ok(())
    }

    pub(super) fn overlay_dirty(&self, lba: u64, buf: &mut [u8]) {
        for (n, sector_bytes) in buf.chunks_mut(SECTOR).enumerate() {
            let sector = lba + n as u64;
            if let Some(page) = self.pages.get(&(sector / PAGE_SECTORS)) {
                let slot = (sector % PAGE_SECTORS) as usize;
                if page.dirty & (1 << slot) != 0 {
                    sector_bytes.copy_from_slice(&page.data[slot * SECTOR..(slot + 1) * SECTOR]);
                }
            }
        }
    }
}
