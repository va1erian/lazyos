//! Block devices: the [`BlockDevice`] trait, a small registry, and the
//! drivers behind it (issue #100).
//!
//! Filesystems talk to storage through [`BlockDevice::read_sectors`] and never
//! to a driver directly. Drivers register themselves here at probe time
//! ([`init`]) as `'static` singletons, so the registry is a fixed-size array
//! and needs no heap. A mount can then name a device ([`device`]) or use the
//! active boot device ([`boot_device`]).
//!
//! Sector I/O takes a caller-provided buffer. The heap maps scattered
//! physical frames and a stack slice can straddle pages, so virtio-blk hands
//! the device one descriptor per page piece of the caller's buffers,
//! translated with [`virt_to_phys`]; ATA PIO has no such constraint. A caller
//! that holds no plain spin lock passes [`Wait::MaySleep`] and gives the CPU
//! away while its request is in flight ([`iowait`]). The
//! registry keeps a `'static` reference per device, so someone must own the
//! driver instances for the whole kernel life, which the statics in
//! [`ata`]/[`virtio`] provide.
//!
//! Filesystems open the device they are handed and keep that handle, so every
//! read is tied to one disk regardless of which driver won the probe and which
//! device is the active boot device.

pub mod ahci;
pub mod ata;
mod dma;
pub mod iowait;
pub mod mem;
pub mod nvme;
pub mod partition;
pub mod provider;
pub mod stats;
pub mod virtio;
mod virtio_diag;

use alloc::vec::Vec;
use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::{PhysAddr, VirtAddr};

/// Sector size every device in this tree speaks.
pub const SECTOR_SIZE: usize = 512;

/// How many devices the registry can hold: whole disks, their partitions
/// ([`partition::MAX_PARTITIONS`]), eight provider disks and a few test
/// doubles (the NVMe test image alone attaches four disks and their
/// partitions).
const MAX_DEVICES: usize = 32;

/// Block-layer failures. Filesystems map these to their own errors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockError {
    /// Registration collided with an existing device name.
    Exists,
    /// The registry has no free slot.
    Full,
    /// The range lies outside the device.
    Bounds,
    /// The length is not a whole number of sectors, or a driver rejects it.
    Unsupported,
    /// Device-level I/O failure (no device, timeout, or an error status).
    Io,
    /// The device is read-only and the caller tried to write.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // write path is tested only
    ReadOnly,
}

/// How a request may wait for its device (docs/performance-plan.md P5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wait {
    /// Busy-wait with interrupts as the caller has them (off in a syscall):
    /// for callers that may hold a spin lock another task could want.
    Spin,
    /// The caller holds no lock but ones whose contenders yield
    /// (`task::relax`), so it may give the CPU away while the device works
    /// ([`iowait`] decides whether the context really can).
    MaySleep,
}

/// Validate a sector range against a geometry. Shared by drivers so bounds
/// handling is identical everywhere: `bytes` must be a whole number of
/// `sector_size`-byte sectors and `[lba, lba + sectors)` must lie inside the
/// device. Returns the sector count on success.
pub fn check_range(
    sector_size: usize,
    sector_count: u64,
    lba: u64,
    bytes: usize,
) -> Result<usize, BlockError> {
    if sector_size == 0 || !bytes.is_multiple_of(sector_size) {
        return Err(BlockError::Unsupported);
    }
    let sectors = (bytes / sector_size) as u64;
    if sectors == 0 {
        return Ok(0); // an empty transfer is a legal no-op
    }
    if lba
        .checked_add(sectors)
        .is_none_or(|end| end > sector_count)
    {
        return Err(BlockError::Bounds);
    }
    Ok(sectors as usize)
}

/// A random-access block device. Implementations are `Send + Sync`: the
/// scheduler can have several tasks reading the filesystem at once, and the
/// driver must serialise them (a mutex around the hardware).
pub trait BlockDevice: Send + Sync {
    /// Short registry name (`"ata0"`, `"virtio0"`, `"test-fake0"`).
    fn name(&self) -> &'static str;

    /// Bytes per addressable sector (512 for every driver today).
    fn sector_size(&self) -> usize {
        SECTOR_SIZE
    }

    /// Number of sectors on the device.
    fn sector_count(&self) -> u64;

    /// Read whole sectors starting at `lba` into `buf`. `buf.len()` must be a
    /// multiple of the sector size and the range must fit the device.
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError>;

    /// Write whole sectors starting at `lba` from `buf`. Read-only devices
    /// (the default) answer [`BlockError::ReadOnly`].
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // no kernel writer yet
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        let _ = (lba, buf);
        Err(BlockError::ReadOnly)
    }

    /// Read consecutive sectors from `lba` into `bufs`, back to back. A driver
    /// that can makes this one request (virtio-blk); the default reads each
    /// buffer in turn. The ext2 block cache reads ahead this way.
    fn read_sectors_vectored(&self, lba: u64, bufs: &mut [&mut [u8]]) -> Result<(), BlockError> {
        let mut at = lba;
        for buf in bufs.iter_mut() {
            self.read_sectors(at, buf)?;
            at += (buf.len() / self.sector_size().max(1)) as u64;
        }
        Ok(())
    }

    /// Write `bufs` back to back as consecutive sectors from `lba`, as one
    /// request where the driver can. The ext2 block cache coalesces
    /// contiguous dirty blocks this way.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // no kernel writer yet
    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), BlockError> {
        let mut at = lba;
        for buf in bufs {
            self.write_sectors(at, buf)?;
            at += (buf.len() / self.sector_size().max(1)) as u64;
        }
        Ok(())
    }

    /// [`BlockDevice::read_sectors_vectored`], saying how the caller may wait.
    /// A driver that can sleep (virtio-blk) honours [`Wait::MaySleep`]; the
    /// default ignores it.
    fn read_sectors_vectored_with(
        &self,
        lba: u64,
        bufs: &mut [&mut [u8]],
        wait: Wait,
    ) -> Result<(), BlockError> {
        let _ = wait;
        self.read_sectors_vectored(lba, bufs)
    }

    /// [`BlockDevice::write_sectors_vectored`], saying how the caller may wait.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // no kernel writer yet
    fn write_sectors_vectored_with(
        &self,
        lba: u64,
        bufs: &[&[u8]],
        wait: Wait,
    ) -> Result<(), BlockError> {
        let _ = wait;
        self.write_sectors_vectored(lba, bufs)
    }

    /// Flush any write cache so earlier writes are durable. Devices without a
    /// cache complete immediately.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // no kernel writer yet
    fn flush(&self) -> Result<(), BlockError> {
        Ok(())
    }

    /// Request counters, for a driver that keeps them ([`stats`]). Partitions
    /// answer `None`: their requests are counted once, on the whole disk.
    fn stats(&self) -> Option<&stats::IoStats> {
        None
    }

    /// Whether this is a window onto another device ([`partition`]). Boot-time
    /// probing looks at whole disks only.
    fn is_partition(&self) -> bool {
        false
    }

    /// Whether [`BlockDevice::write_sectors`] can succeed.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // no kernel writer yet
    fn is_writable(&self) -> bool {
        false
    }

    /// Range check against this device's geometry; drivers should call it
    /// before touching the hardware.
    fn check_range(&self, lba: u64, bytes: usize) -> Result<usize, BlockError> {
        check_range(self.sector_size(), self.sector_count(), lba, bytes)
    }
}

/// The device table: a fixed array behind one lock, so registration needs no
/// heap and probing cannot fail for lack of memory.
struct Registry {
    devices: [Option<&'static dyn BlockDevice>; MAX_DEVICES],
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    devices: [None; MAX_DEVICES],
});

/// The device filesystems read from. Set during driver attach.
static BOOT: Mutex<Option<&'static dyn BlockDevice>> = Mutex::new(None);

/// Add `device` under its [`BlockDevice::name`]. A duplicate name is
/// [`BlockError::Exists`]; a full table is [`BlockError::Full`].
pub fn register(device: &'static dyn BlockDevice) -> Result<(), BlockError> {
    let mut registry = REGISTRY.lock();
    if registry
        .devices
        .iter()
        .flatten()
        .any(|entry| entry.name() == device.name())
    {
        return Err(BlockError::Exists);
    }
    for slot in registry.devices.iter_mut() {
        if slot.is_none() {
            *slot = Some(device);
            return Ok(());
        }
    }
    Err(BlockError::Full)
}

/// Look a device up by its registry name.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/mount_device
pub fn device(name: &str) -> Option<&'static dyn BlockDevice> {
    REGISTRY
        .lock()
        .devices
        .iter()
        .flatten()
        .copied()
        .find(|entry| entry.name() == name)
}

/// Every registered device, in registration order.
pub fn devices() -> Vec<&'static dyn BlockDevice> {
    REGISTRY.lock().devices.iter().flatten().copied().collect()
}

/// The active boot device, if one was selected.
pub fn boot_device() -> Option<&'static dyn BlockDevice> {
    *BOOT.lock()
}

/// Make `device` the device filesystems read through.
pub fn set_boot_device(device: &'static dyn BlockDevice) {
    *BOOT.lock() = Some(device);
}

/// Forget the boot device, restoring the "none selected" state a test may have
/// replaced (issue #274).
#[cfg(lazyos_tests)]
pub fn clear_boot_device() {
    *BOOT.lock() = None;
}

/// Probe and register the built-in drivers. Idempotent; a thin wrapper over the
/// device core (issue #239): [`crate::dev::init`] enumerates the buses and runs
/// the in-kernel driver table, whose ATA and virtio-blk entries call
/// [`install_ata`]/[`install_virtio`] below with the same probe order and logs
/// as before. Safe to call from [`crate::fs::init`] on every boot path.
pub fn init() {
    crate::dev::init();
    partition::scan_all();
    if boot_device().is_none() {
        serial_println!("block: no block device found");
    }
}

/// Attach the ATA primary master (first, so it stays the fallback boot device)
/// and register it. Called by the device core's ATA driver entry.
pub(crate) fn install_ata() -> Option<&'static dyn BlockDevice> {
    let device = ata::probe()?;
    let _ = register(device);
    set_boot_device(device);
    serial_println!(
        "block: {} ready, {} sectors",
        device.name(),
        device.sector_count()
    );
    Some(device)
}

/// Attach one virtio-blk function and register it. The first virtio
/// disk takes over the boot slot from ATA (the QEMU preference); later ones
/// (a data disk) are registered but never displace it. Called by the device
/// core's driver entry once per matching function.
pub(crate) fn install_virtio(
    function: crate::dev::pci::Function,
) -> Option<&'static dyn BlockDevice> {
    let device = virtio::attach_function(function)?;
    let _ = register(device);
    if !boot_device().is_some_and(|boot| boot.name().starts_with("virtio")) {
        set_boot_device(device);
    }
    serial_println!(
        "block: {} ready, {} sectors",
        device.name(),
        device.sector_count()
    );
    Some(device)
}

/// Attach one NVMe controller and register it. It takes the boot slot only
/// when no earlier driver found a disk (docs/nvme-install-plan.md N1): the
/// root is chosen by UUID from `lazyos.cfg` on any device, so the boot slot
/// is only the legacy fallback. Called by the device core's driver entry
/// once per matching function.
pub(crate) fn install_nvme(
    function: crate::dev::pci::Function,
    bar: u64,
    len: u64,
) -> Option<&'static dyn BlockDevice> {
    let device = nvme::attach_function(function, bar, len)?;
    let _ = register(device);
    if boot_device().is_none() {
        set_boot_device(device);
    }
    serial_println!(
        "block: {} ready, {} sectors",
        device.name(),
        device.sector_count()
    );
    Some(device)
}

/// Attach one AHCI controller and register a block device for every port with
/// a disk. The first takes the boot slot only when no earlier driver found a
/// disk (docs/ahci-plan.md A2): the root is chosen by UUID from `lazyos.cfg`
/// on any device, so the boot slot is only the legacy fallback. Called by the
/// device core's driver entry once per matching function; returns how many
/// disks it registered.
pub(crate) fn install_ahci(function: crate::dev::pci::Function, bar: u64, len: u64) -> usize {
    let disks = ahci::attach_function(function, bar, len);
    for &device in &disks {
        let _ = register(device);
        if boot_device().is_none() {
            set_boot_device(device);
        }
        serial_println!(
            "block: {} ready, {} sectors",
            device.name(),
            device.sector_count()
        );
    }
    disks.len()
}

/// Translate a kernel virtual address to its physical address by walking the
/// active page table. DMA drivers need physical addresses, and the bootloader
/// maps the kernel and the physical-memory window at unrelated dynamic
/// offsets, so subtracting one from the other is not enough.
pub(crate) fn virt_to_phys(virt: VirtAddr) -> Option<PhysAddr> {
    const PRESENT: u64 = 1 << 0;
    const HUGE: u64 = 1 << 7;
    const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

    let mut table = Cr3::read().0.start_address().as_u64();
    for shift in [39u64, 30, 21, 12] {
        let pointer = crate::mem::phys_to_virt(PhysAddr::new(table)).as_ptr::<u64>();
        // Safety: `table` is a live page-table frame reachable through the
        // physical-memory mapping.
        let entry = unsafe {
            pointer
                .add(((virt.as_u64() >> shift) & 0x1FF) as usize)
                .read_volatile()
        };
        if entry & PRESENT == 0 {
            return None;
        }
        // A 1 GiB (PDPTE) or 2 MiB (PDE) huge mapping covers the range whole.
        if (shift == 30 || shift == 21) && entry & HUGE != 0 {
            let size = 1u64 << shift;
            return Some(PhysAddr::new((entry & ADDR) | (virt.as_u64() & (size - 1))));
        }
        table = entry & ADDR;
    }
    Some(PhysAddr::new(table | (virt.as_u64() & 0xFFF)))
}
