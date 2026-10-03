//! `MemDisk`: a [`BlockDevice`] over a region of memory, used for the
//! bootloader's ramdisk (issue #5).
//!
//! The ramdisk is a disk image the bootloader loads next to the kernel: a
//! bare FAT image (issue #5) or, on the USB stick, a whole disk with an MBR, a
//! FAT `/boot` holding `lazyos.cfg` and the ext2 OS volume
//! (docs/usb-stick.md). This device lets the filesystem layer mount it exactly
//! like a disk, so LazyOS boots with no disk driver for the medium it came
//! from. The region is exclusively owned by the device, so there is no
//! aliasing with other kernel memory, and it is writable: a root on it is a
//! RAM root, discarded at power-off.

use alloc::boxed::Box;
use spin::Mutex;

use super::{BlockDevice, BlockError, SECTOR_SIZE};

/// Registry name of the boot ramdisk.
pub const RAMDISK_NAME: &str = "ram0";

/// A block device backed by a byte region. A trailing partial sector is
/// ignored, so every sector the device reports is whole.
pub struct MemDisk {
    name: &'static str,
    data: Mutex<&'static mut [u8]>,
}

impl MemDisk {
    /// Wrap `region` (its length is rounded down to whole sectors).
    pub fn new(name: &'static str, region: &'static mut [u8]) -> MemDisk {
        let whole = region.len() / SECTOR_SIZE * SECTOR_SIZE;
        let (region, _) = region.split_at_mut(whole);
        MemDisk {
            name,
            data: Mutex::new(region),
        }
    }
}

impl BlockDevice for MemDisk {
    fn name(&self) -> &'static str {
        self.name
    }

    fn sector_count(&self) -> u64 {
        (self.data.lock().len() / SECTOR_SIZE) as u64
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        self.check_range(lba, buf.len())?;
        let data = self.data.lock();
        let start = lba as usize * SECTOR_SIZE;
        buf.copy_from_slice(&data[start..start + buf.len()]);
        Ok(())
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        self.check_range(lba, buf.len())?;
        let mut data = self.data.lock();
        let start = lba as usize * SECTOR_SIZE;
        data[start..start + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }
}

/// Register the bootloader's ramdisk as [`RAMDISK_NAME`], unless it is empty,
/// smaller than a sector, or the registry refuses it, and register its MBR
/// partitions (`ram0p<n>`). Returns whether it was registered. It never
/// becomes the block layer's boot device; `fs::mounts::build` looks at it and
/// its partitions before any disk, so the boot volume and the root come from
/// the ramdisk whenever it carries them.
pub fn register_ramdisk(addr: u64, len: u64) -> bool {
    let Ok(len) = usize::try_from(len) else {
        return false;
    };
    if addr == 0 || len < SECTOR_SIZE {
        return false;
    }
    // `addr` is already a *virtual* address: bootloader 0.11 maps the ramdisk
    // pages itself and stores the mapped start in `BootInfo::ramdisk_addr`
    // (bootloader-x86_64-common `Mappings::ramdisk_slice_start`), so no
    // physical-offset translation is needed.
    // SAFETY: the bootloader maps the ramdisk at `addr` for `len` bytes and
    // never hands the range to anything else; the kernel takes exclusive
    // ownership of it here and only ever reaches it through this device.
    let region = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, len) };
    let device: &'static MemDisk = Box::leak(Box::new(MemDisk::new(RAMDISK_NAME, region)));
    if super::register(device).is_err() {
        return false;
    }
    super::partition::scan_disk(device);
    true
}
