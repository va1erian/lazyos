//! Kernel device core (issue #239, driver-plan stages D0/D1).
//!
//! The kernel knows buses, resources and IRQs — never what a "NIC" or a
//! "sound card" is. Class semantics live in each driver's Messenger interface
//! (see `docs/driver-plan.md`). This module is the shared foundation:
//!
//! - [`DeviceId`]/[`DeviceInfo`] and typed [`Resources`] ([`Bar`], [`Irq`]);
//! - the [`Bus`] enumeration seam, with [`PciBus`] the only implementation;
//! - [`DeviceTable`], a fixed-capacity array with owner + generation;
//! - [`Driver`] and the static [`DRIVERS`] table for in-kernel drivers.
//!
//! On top of that core, issue #240 adds the userspace tier: interrupt
//! dispatch ([`irq`], [`intx`]), device claims ([`claims`], [`grant`]) and the
//! `dev_*` syscall ([`syscall`], [`ops`], [`teardown`]).
//!
//! [`init`] seeds platform devices, enumerates PCI, runs the driver table and
//! prints the `DEV:ENUM` boot line. With no heap on the hot path, enumeration
//! is boot-time only and handles fail closed once a device is released.

// The core is deliberately wider than today's callers: the IRQ (D2), `dev_*`
// syscall (D3) and DMA (D4) stages consume the command-register, capability
// and resource accessors below, and the kernel suite exercises them. Keep the
// surface documented rather than deleting it to silence the lint.
#![allow(dead_code)]
#![allow(unused_imports)]

mod bus;
pub mod claims;
pub mod class;
pub mod dma;
mod driver;
pub mod errno;
pub mod grant;
pub mod intx;
pub mod irq;
pub mod ops;
pub mod pci;
pub mod report;
mod resources;
mod selfcheck;
pub mod syscall;
pub mod table;
mod teardown;

pub use bus::{Bus, Enumerated, PciBus};
pub(crate) use driver::attach_all;
pub use driver::{probe, Driver, DRIVERS};
pub use resources::{Bar, BarKind, Irq, Resource, Resources, MAX_BARS};
pub use selfcheck::selfcheck;
pub use table::{DevError, DeviceHandle, DeviceTable, MAX_DEVICES};
pub use teardown::{
    dma_buffer_freed, dma_quarantine, note_task_exited, silence_exited, teardown_task,
};

use core::sync::atomic::{AtomicBool, Ordering};

/// A discovered function's stable id: its slot in the boot table.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct DeviceId(pub u16);

/// Identifies who owns a claim. Slot 0 is the kernel; the userspace tier
/// (D3) maps a process's task slot into this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaskSlot(pub usize);

impl TaskSlot {
    /// The kernel itself is the owner of boot-time driver claims.
    pub const KERNEL: TaskSlot = TaskSlot(0);
}

/// Where a device was found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BusId {
    /// A non-discoverable platform/ISA seed (the ATA controller).
    Platform,
    /// A PCI function at bus/device/function coordinates.
    Pci(pci::Address),
}

/// One discovered function.
#[derive(Clone, Copy, Debug)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub bus: BusId,
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub resources: Resources,
}

impl DeviceInfo {
    /// The ISA ATA controller the block layer has always probed directly.
    /// Seeding it as a platform device lets the driver table attach it through
    /// the same path as a bus-discovered function, with its fixed ports/IRQ.
    pub fn platform_ata() -> DeviceInfo {
        let mut resources = Resources::empty();
        for index in 0..2u8 {
            let base = if index == 0 { 0x1F0 } else { 0x3F6 };
            resources.set_bar(Bar {
                index,
                kind: BarKind::Io,
                base,
                len: 8,
                is_64: false,
                prefetchable: false,
            });
        }
        resources.set_irq(Irq { line: 14 });
        DeviceInfo {
            id: DeviceId(0),
            bus: BusId::Platform,
            vendor: 0x8086,
            device: 0x7010,
            subsystem_vendor: 0,
            subsystem_device: 0,
            class: 0x01,
            subclass: 0x01,
            prog_if: 0x80,
            revision: 0,
            resources,
        }
    }
}

/// The boot-time device table.
static TABLE: spin::Mutex<DeviceTable> = spin::Mutex::new(DeviceTable::new());
static INITED: AtomicBool = AtomicBool::new(false);

/// The global device table, for callers that need to inspect or claim.
pub fn table() -> &'static spin::Mutex<DeviceTable> {
    &TABLE
}

/// Enumerate devices and attach in-kernel drivers. Idempotent; called at boot
/// and, through [`crate::block::init`], on every block probe path. Prints the
/// `DEV:ENUM:PASS` line the acceptance checks.
pub fn init() {
    if INITED.swap(true, Ordering::SeqCst) {
        return;
    }
    let (total, found) = {
        let mut table = TABLE.lock();
        let _ = table.insert(DeviceInfo::platform_ata());
        let found = PciBus.enumerate(&mut table);
        (table.len(), found)
    };
    let pci_count = found.inserted;
    let attached = probe();
    selfcheck::log_irq_routes();
    if found.dropped > 0 {
        serial_println!(
            "DEV:ENUM:FAIL:device table full, {} PCI function(s) dropped ({} recorded)",
            found.dropped,
            pci_count
        );
    } else if pci_count > 0 {
        serial_println!(
            "DEV:ENUM:PASS:{} devices ({} PCI, {} drivers attached)",
            total,
            pci_count,
            attached
        );
    } else {
        serial_println!("DEV:ENUM:FAIL:no PCI devices enumerated");
    }
}
