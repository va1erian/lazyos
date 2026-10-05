//! In-kernel drivers (issue #239): a `Driver` trait and a static table.
//!
//! Drivers are statically linked; nothing is hot-loaded. At boot the device
//! core walks the enumerated devices and offers each to the table; the first
//! driver whose [`Driver::matches`] accepts it gets to [`Driver::attach`] it
//! (claiming it for the kernel owner first). A failed attach is rolled back so
//! the device stays claimable.
//!
//! The legacy block drivers register here with **no behavior change**: their
//! `attach` calls the same probe code the block layer always used.

use super::{BusId, DevError, DeviceHandle, DeviceInfo, DeviceTable, TaskSlot};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

/// A statically linked kernel driver.
pub trait Driver: Sync {
    /// Short name for logs and the driver table.
    fn name(&self) -> &'static str;

    /// Whether this driver can drive `info`.
    fn matches(&self, info: &DeviceInfo) -> bool;

    /// Bring the device up. Called after a successful `matches`, with the
    /// device already claimed for the kernel owner.
    fn attach(&self, handle: DeviceHandle) -> Result<(), DevError>;

    /// Tear the device down. Optional; default is a no-op.
    fn detach(&self, handle: DeviceHandle) {
        let _ = handle;
    }
}

/// The ATA PIO controller is a platform (ISA) device, not a PCI function; the
/// core seeds it at boot with its fixed ports/IRQ.
struct AtaDriver;

impl Driver for AtaDriver {
    fn name(&self) -> &'static str {
        "ata"
    }

    fn matches(&self, info: &DeviceInfo) -> bool {
        matches!(info.bus, BusId::Platform) && info.class == 0x01
    }

    fn attach(&self, _handle: DeviceHandle) -> Result<(), DevError> {
        crate::block::install_ata()
            .map(|_| ())
            .ok_or(DevError::NoDriver)
    }
}

/// Legacy (0.9.5 / transitional) virtio-blk, one block device per function.
/// Modern-only functions match too, but their attach is the documented `None`
/// until the modern transport lands.
struct VirtioBlkDriver;

impl Driver for VirtioBlkDriver {
    fn name(&self) -> &'static str {
        "virtio-blk"
    }

    fn matches(&self, info: &DeviceInfo) -> bool {
        matches!(info.bus, BusId::Pci(_))
            && info.vendor == super::pci::VIRTIO_VENDOR
            && matches!(info.device, 0x1001 | 0x1042)
    }

    fn attach(&self, handle: DeviceHandle) -> Result<(), DevError> {
        // Each matching function gets its own block device; the table entry
        // says which function this handle is.
        let info = super::table()
            .lock()
            .get(handle.id())
            .ok_or(DevError::NoDriver)?;
        let BusId::Pci(address) = info.bus else {
            return Err(DevError::NoDriver);
        };
        let function = super::pci::Function {
            address,
            vendor: info.vendor,
            id: info.device,
        };
        crate::block::install_virtio(function)
            .map(|_| ())
            .ok_or(DevError::NoDriver)
    }
}

/// NVMe controllers (class 01:08:02), one block device per function
/// (docs/nvme-install-plan.md N1). After virtio in the table, so it never
/// displaces an earlier boot device.
struct NvmeDriver;

impl Driver for NvmeDriver {
    fn name(&self) -> &'static str {
        "nvme"
    }

    fn matches(&self, info: &DeviceInfo) -> bool {
        matches!(info.bus, BusId::Pci(_))
            && (info.class, info.subclass, info.prog_if) == (0x01, 0x08, 0x02)
    }

    fn attach(&self, handle: DeviceHandle) -> Result<(), DevError> {
        let info = super::table()
            .lock()
            .get(handle.id())
            .ok_or(DevError::NoDriver)?;
        let BusId::Pci(address) = info.bus else {
            return Err(DevError::NoDriver);
        };
        let Some(bar) = info
            .resources
            .bar(0)
            .filter(|bar| bar.kind == super::BarKind::Mem)
        else {
            serial_println!("nvme: {:?} has no memory BAR0", address);
            return Err(DevError::NoDriver);
        };
        let function = super::pci::Function {
            address,
            vendor: info.vendor,
            id: info.device,
        };
        crate::block::install_nvme(function, bar.base, bar.len)
            .map(|_| ())
            .ok_or(DevError::NoDriver)
    }
}

/// The static in-kernel driver table. Order matters only for which driver wins
/// a device both accept; drivers do not overlap today.
pub static DRIVERS: &[&dyn Driver] = &[&AtaDriver, &VirtioBlkDriver, &NvmeDriver];

/// Guards the one-shot boot probe.
static PROBED: AtomicBool = AtomicBool::new(false);

/// Offer every device in `table` to `drivers`, claiming for the kernel first
/// and rolling the claim back when `attach` fails. Returns how many attached.
///
/// The table lock is taken in short, separate statements and is **never held
/// across `attach`**: a driver may call back into the device core, and the
/// failed-attach rollback below re-locks the table. (Locking inside an
/// `if let` scrutinee would keep the guard alive for the whole body and
/// self-deadlock on that rollback.)
pub(crate) fn attach_all(table: &Mutex<DeviceTable>, drivers: &[&dyn Driver]) -> usize {
    let devices: Vec<DeviceInfo> = table.lock().iter().collect();
    let mut attached = 0;
    for info in devices {
        let Some(driver) = drivers.iter().find(|driver| driver.matches(&info)) else {
            continue;
        };
        let claim = table.lock().claim(info.id, TaskSlot::KERNEL);
        let Ok(handle) = claim else { continue };
        match driver.attach(handle) {
            Ok(()) => {
                attached += 1;
                silence_intx(&info);
                serial_println!(
                    "dev: {} attached device {} (vendor {:04x}:{:04x})",
                    driver.name(),
                    info.id.0,
                    info.vendor,
                    info.device
                );
            }
            Err(_) => {
                let _ = table.lock().release(handle);
            }
        }
    }
    attached
}

/// Every in-kernel driver polls, so a PCI function it owns must never assert
/// its INTx line: on a line shared with a userspace claimant (QEMU has four
/// PIRQs for everything) a level-asserted, never-serviced interrupt would look
/// like the claimant's own and keep its line busy.
fn silence_intx(info: &DeviceInfo) {
    if let BusId::Pci(address) = info.bus {
        super::pci::set_command(address, super::pci::COMMAND_INTX_DISABLE);
    }
}

/// Offer every enumerated device to the driver table. Idempotent; returns how
/// many devices were attached (0 if probing already ran).
pub fn probe() -> usize {
    if PROBED.swap(true, Ordering::SeqCst) {
        return 0;
    }
    attach_all(super::table(), DRIVERS)
}
