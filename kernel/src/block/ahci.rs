//! AHCI (SATA) driver (docs/ahci-plan.md A2): every implemented port with
//! an ATA disk attached becomes a block device (`ahci0`, `ahci1`, ...).
//!
//! The protocol lives in `libs/ahci` (handoff, port bring-up, command
//! tables, PRDT planning, IDENTIFY, error recovery), host-tested against a
//! model HBA; this module is the machine side of its `Platform` seam: ABAR
//! mapped uncached into the kernel ([`crate::mem::mmio::map_kernel`]),
//! command structures in static DMA pages, a TSC clock for bring-up waits,
//! and the block-layer face (`ahci0`, partitions `ahci0p<n>`).
//!
//! # Requests
//!
//! The HBA reads and writes the caller's buffers directly, each page
//! translated with [`super::virt_to_phys`]. A transfer keeps up to
//! [`ahci::MAX_SLOTS`] commands of at most 256 KiB in flight. The port lock
//! is a [`YieldMutex`] held for the whole transfer: a caller that may sleep
//! parks in [`iowait`] while the disk works, and a contender yields instead
//! of spinning. Buffers the HBA cannot be pointed at (an odd address) go
//! through one bounce page per port.
//!
//! # Failure
//!
//! Every wait is bounded. A command unanswered for [`TIMEOUT_NS`] or
//! answered with an error stops the port and restarts it (`ahci::reset`);
//! two failed restarts in a row detach it (at once when the port will not stop
//! at all: the controller's bus mastering is then cut so the HBA cannot touch
//! the caller's buffers): its device stays registered and
//! answers [`BlockError::Io`]. Boot never waits on it.
//!
//! A disk with anything but 512-byte logical sectors is refused with a log
//! line: the block layer is 512 everywhere ([`SECTOR_SIZE`]).

mod hw;

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ahci::{Hba, Op, Platform, Port, PortPages, Skip, MAX_SLOTS};
use alloc::vec::Vec;
use x86_64::VirtAddr;

use super::dma::{gather, scatter, Page};
use super::iowait::{self, Expect};
use super::stats::IoStats;
use super::{BlockDevice, BlockError, Wait, SECTOR_SIZE};
use crate::dev::pci;
use crate::task::relax::YieldMutex;
use hw::Hw;

/// How long a command may stay unanswered before the port is restarted.
const TIMEOUT_NS: u64 = 10_000_000_000;
/// How long the cache flush and the standby of a power-off may take each.
const POWER_TIMEOUT_NS: u64 = 30_000_000_000;
/// How many ports can be driven at once (one [`Slot`] each), across all
/// controllers.
const MAX_AHCI: usize = 4;
/// Registry names, indexed by slot.
const NAMES: [&str; MAX_AHCI] = ["ahci0", "ahci1", "ahci2", "ahci3"];
/// Most of ABAR this driver maps: 32 ports of registers.
const MAX_BAR_MAP: u64 = ahci::regs::bar_bytes(ahci::regs::MAX_PORTS - 1);

/// Pages per slot: the command list and received-FIS area share the first,
/// then one command table per command slot, IDENTIFY, and the bounce page.
const PAGES: usize = 1 + MAX_SLOTS + 2;
const TABLES: usize = 1;
const IDENTIFY: usize = 1 + MAX_SLOTS;
const BOUNCE: usize = PAGES - 1;
/// Offset of the received-FIS area in the first page (256-byte aligned,
/// after the 1 KiB command list).
const FIS_OFFSET: u64 = 0x400;

/// A live port.
struct Live {
    hw: Hw,
    port: Port,
    /// The controller, to cut its bus mastering if a port cannot be stopped.
    address: pci::Address,
}

/// One driven port: its DMA memory and the registry-facing device.
struct Slot {
    /// Held from the start of a bring-up until it fails, or for good once a
    /// port is attached.
    claimed: AtomicBool,
    pages: [Page; PAGES],
    device: AhciDisk,
}

/// The registry-facing device of one [`Slot`].
pub struct AhciDisk {
    index: usize,
    state: YieldMutex<Option<Live>>,
    sectors: AtomicU64,
    expect_read: Expect,
    expect_write: Expect,
    stats: IoStats,
}

impl Slot {
    const fn new(index: usize) -> Slot {
        Slot {
            claimed: AtomicBool::new(false),
            pages: [const { Page::new() }; PAGES],
            device: AhciDisk {
                index,
                state: YieldMutex::new(None),
                sectors: AtomicU64::new(0),
                expect_read: Expect::new(),
                expect_write: Expect::new(),
                stats: IoStats::new(),
            },
        }
    }

    fn claim() -> Option<&'static Slot> {
        SLOTS.iter().find(|slot| {
            slot.claimed
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        })
    }
}

static SLOTS: [Slot; MAX_AHCI] = [Slot::new(0), Slot::new(1), Slot::new(2), Slot::new(3)];

/// Bring up the controller at `function` (class 01:06:01) with ABAR at
/// `bar` (`len` bytes), and return a block device for each port with a disk.
/// Empty when the controller is unusable or has no disk (with a log line).
pub fn attach_function(
    function: pci::Function,
    bar: u64,
    len: u64,
) -> Vec<&'static dyn BlockDevice> {
    let mut disks: Vec<&'static dyn BlockDevice> = Vec::new();
    let min = ahci::regs::bar_bytes(0);
    if len < min {
        serial_println!("ahci: ABAR of {len:#x} bytes is too small");
        return disks;
    }
    pci::enable_memory(function.address);
    pci::enable_bus_master(function.address);
    let mapped = len.min(MAX_BAR_MAP);
    let regs = match crate::mem::mmio::map_kernel(bar, mapped) {
        Ok(regs) => regs,
        Err(why) => {
            serial_println!("ahci: cannot map ABAR at {bar:#x}: {why}");
            return disks;
        }
    };
    let hw = Hw { regs, mapped };
    let hba = match Hba::init(&hw) {
        Ok(hba) => hba,
        Err(error) => {
            serial_println!("ahci: {:?} does not answer: {error}", function.address);
            return disks;
        }
    };
    let (major, minor) = ahci::regs::version(hw.read32(ahci::regs::VS));
    let highest = hba.highest_port();
    if highest.is_none_or(|highest| ahci::regs::bar_bytes(highest) > mapped) {
        serial_println!(
            "ahci: AHCI {major}.{minor} at {bar:#x}: {} ports, none usable in {len:#x} bytes",
            hba.cap.ports
        );
        return disks;
    }
    for index in 0..=highest.unwrap_or(0) {
        if !hba.has_port(index) {
            continue;
        }
        let Some(slot) = Slot::claim() else {
            serial_println!("ahci: more than {MAX_AHCI} disks; port {index} ignored");
            break;
        };
        match open_port(slot, &hba, &hw, function.address, index) {
            Some(device) => disks.push(device),
            None => {
                slot.claimed.store(false, Ordering::Release);
            }
        }
    }
    serial_println!(
        "ahci: AHCI {major}.{minor} at {bar:#x}: {} ports, {} implemented, {} disk(s)",
        hba.cap.ports,
        hba.implemented.count_ones(),
        disks.len()
    );
    if disks.is_empty() {
        // Nothing to drive: stop the controller's DMA.
        pci::clear_command(function.address, pci::COMMAND_BUS_MASTER);
    }
    disks
}

/// Bring port `index` up in `slot`. `None` for an empty port (silently), a
/// skipped device or a failure (with a log line).
fn open_port(
    slot: &'static Slot,
    hba: &Hba,
    hw: &Hw,
    address: pci::Address,
    index: usize,
) -> Option<&'static dyn BlockDevice> {
    let name = NAMES[slot.device.index];
    let mut phys = [0u64; PAGES];
    for (page, out) in slot.pages.iter().zip(phys.iter_mut()) {
        // SAFETY: the slot was just claimed and no port owns its pages yet.
        unsafe { (*page.0.get()).fill(0) };
        let Some(address) = page.phys() else {
            serial_println!("{name}: DMA page has no physical address");
            return None;
        };
        *out = address;
    }
    // The bounce page is where the HBA is pointed when a caller's page cannot
    // be: without 64-bit addressing it must itself lie below 4 GiB.
    if !hba.cap.s64a && phys[BOUNCE] + 4096 > 1 << 32 {
        serial_println!("{name}: bounce page above 4 GiB on a 32-bit HBA; port {index} refused");
        return None;
    }
    let pages = PortPages {
        list: phys[0],
        fis: phys[0] + FIS_OFFSET,
        tables: core::array::from_fn(|index| phys[TABLES + index]),
        identify: phys[IDENTIFY],
    };
    let port = match hba.open_port(hw, index, pages) {
        Ok(port) => port,
        Err(Skip::Empty) => return None,
        Err(skip) => {
            serial_println!("{name}: port {index}: {skip}");
            return None;
        }
    };
    let disk = port.disk;
    serial_println!(
        "{name}: port {index}: {} (serial {}, firmware {}), {} MiB, {}-byte physical sectors{}",
        disk.model.as_str(),
        disk.serial.as_str(),
        disk.firmware.as_str(),
        disk.bytes() >> 20,
        disk.physical_bytes,
        if disk.write_cache {
            ", write cache"
        } else {
            ""
        }
    );
    let device = &slot.device;
    device.sectors.store(disk.sectors, Ordering::Relaxed);
    *device.state.lock() = Some(Live {
        hw: *hw,
        port,
        address,
    });
    Some(device)
}

/// Flush every live port's write cache, after the filesystems synced.
pub fn flush_all() {
    for slot in &SLOTS {
        let device = &slot.device;
        let mut guard = device.state.lock();
        let Some(live) = guard.as_mut() else {
            continue;
        };
        let mut wait = |ready: &dyn Fn() -> bool| {
            iowait::wait_until(Wait::Spin, &device.expect_write, POWER_TIMEOUT_NS, ready)
        };
        match live.port.flush(&live.hw, &mut wait) {
            Ok(()) => serial_println!("{}: cache flushed", NAMES[device.index]),
            Err(error) => serial_println!("{}: flush: {error}", NAMES[device.index]),
        }
    }
}

/// Before power is cut: flush again, then park each drive with `STANDBY
/// IMMEDIATE` so it commits before the rails drop. The ports are detached.
pub fn standby_all() {
    for slot in &SLOTS {
        let device = &slot.device;
        let mut guard = device.state.lock();
        let Some(live) = guard.as_mut() else {
            continue;
        };
        let mut wait = |ready: &dyn Fn() -> bool| {
            iowait::wait_until(Wait::Spin, &device.expect_write, POWER_TIMEOUT_NS, ready)
        };
        match live.port.shutdown(&live.hw, &mut wait) {
            Ok(()) => serial_println!("{}: flushed and in standby", NAMES[device.index]),
            Err(error) => serial_println!("{}: standby: {error}", NAMES[device.index]),
        }
        *guard = None;
    }
}

/// `(address, length)` of each buffer.
fn spans(bufs: impl Iterator<Item = (*const u8, usize)>) -> Result<Vec<(u64, usize)>, BlockError> {
    let mut out = Vec::new();
    for (ptr, len) in bufs {
        out.try_reserve(1).map_err(|_| BlockError::Io)?;
        out.push((ptr as u64, len));
    }
    Ok(out)
}

fn translate(virt: u64) -> Option<u64> {
    super::virt_to_phys(VirtAddr::new(virt)).map(|phys| phys.as_u64())
}

impl AhciDisk {
    fn slot(&self) -> &'static Slot {
        &SLOTS[self.index]
    }

    /// Move the bytes of `segments` (`(address, length)`, back to back) to
    /// (`write`) or from the disk at `lba`. The segments come from slices
    /// the caller holds (mutably, for a read) until this returns.
    fn transfer(
        &self,
        write: bool,
        lba: u64,
        segments: &[(u64, usize)],
        wait: Wait,
    ) -> Result<(), BlockError> {
        let total: usize = segments.iter().map(|&(_, len)| len).sum();
        super::check_range(SECTOR_SIZE, self.sector_count(), lba, total)?;
        if total == 0 {
            return Ok(());
        }
        let mut guard = self.state.lock();
        let live = guard.as_mut().ok_or(BlockError::Io)?;
        let aligned = segments
            .iter()
            .all(|&(address, len)| address.is_multiple_of(2) && len.is_multiple_of(2));
        let result = if aligned {
            match self.direct(live, write, lba, segments, wait) {
                Err(ahci::Error::Misaligned) => self.bounced(live, write, lba, segments, wait),
                other => other,
            }
        } else {
            self.bounced(live, write, lba, segments, wait)
        };
        match result {
            Ok(()) => {
                self.stats.count(write, total);
                Ok(())
            }
            Err(error) => {
                serial_println!(
                    "{}: {} of {} bytes at lba {lba} failed: {error}",
                    NAMES[self.index],
                    if write { "write" } else { "read" },
                    total
                );
                if live.port.dma_unsafe() {
                    // The port would not stop, so the HBA may still be using
                    // the caller's buffers: cut the controller's DMA (every
                    // port on it) before they are handed back.
                    pci::clear_command(live.address, pci::COMMAND_BUS_MASTER);
                    serial_println!(
                        "{}: port would not stop; bus mastering disabled on its controller",
                        NAMES[self.index]
                    );
                }
                if live.port.is_detached() {
                    serial_println!("{}: port detached", NAMES[self.index]);
                    *guard = None;
                }
                Err(match error {
                    ahci::Error::Bounds => BlockError::Bounds,
                    ahci::Error::Unmapped => BlockError::Unsupported,
                    _ => BlockError::Io,
                })
            }
        }
    }

    /// The HBA moves `segments` itself.
    fn direct(
        &self,
        live: &mut Live,
        write: bool,
        lba: u64,
        segments: &[(u64, usize)],
        wait: Wait,
    ) -> Result<(), ahci::Error> {
        let (op, expect) = if write {
            (Op::Write, &self.expect_write)
        } else {
            (Op::Read, &self.expect_read)
        };
        let mut waiter =
            |ready: &dyn Fn() -> bool| iowait::wait_until(wait, expect, TIMEOUT_NS, ready);
        live.port
            .transfer(&live.hw, op, lba, segments, &translate, &mut waiter)
    }

    /// Through the bounce page, one page at a time: buffers the PRDT rules
    /// cannot describe.
    fn bounced(
        &self,
        live: &mut Live,
        write: bool,
        lba: u64,
        segments: &[(u64, usize)],
        wait: Wait,
    ) -> Result<(), ahci::Error> {
        let bounce = &self.slot().pages[BOUNCE];
        let page = bounce.virt();
        let total: usize = segments.iter().map(|&(_, len)| len).sum();
        let mut done = 0usize;
        while done < total {
            let chunk = (total - done).min(4096);
            if write {
                // SAFETY: the segments are the caller's live slices and the
                // bounce page is this port's, under the device lock.
                unsafe { gather(segments, done, page as *mut u8, chunk) };
            }
            let at = lba + (done / SECTOR_SIZE) as u64;
            self.direct(live, write, at, &[(page, chunk)], wait)?;
            if !write {
                // SAFETY: as above; a read's segments are mutable slices.
                unsafe { scatter(segments, done, page as *const u8, chunk) };
            }
            done += chunk;
        }
        Ok(())
    }
}

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
