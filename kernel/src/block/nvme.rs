//! NVMe driver (docs/nvme-install-plan.md N1): one polled I/O queue pair per
//! controller, namespace 1, 512-byte blocks.
//!
//! The protocol lives in `libs/nvme` (bring-up, queues, PRP planning,
//! Identify parsing), host-tested against a model controller; this module
//! is the machine side of its `Platform` seam: BAR0 mapped uncached into the
//! kernel ([`crate::mem::mmio::map_kernel`]), queue and PRP-list pages in
//! static memory, a TSC clock for bring-up waits, and the block-layer face
//! (`nvme0`, `nvme1`, partitions `nvme0p<n>`).
//!
//! # Requests
//!
//! The controller reads and writes the caller's buffers directly, each page
//! translated with [`super::virt_to_phys`]. A transfer keeps up to
//! [`nvme::MAX_INFLIGHT`] commands of at most 64 KiB (less when the
//! controller's `MDTS` says so) in the queue. The device lock is a
//! [`YieldMutex`] held for the whole transfer: a caller that may sleep parks
//! in [`iowait`] while the controller works, and a contender yields instead
//! of spinning. Buffers that are not dword aligned (a byte-aligned stack
//! slice) go through one bounce page per controller.
//!
//! # Failure
//!
//! Every wait is bounded. A controller that does not come ready, reports
//! `CSTS.CFS`, or leaves a command unanswered for [`TIMEOUT_NS`] is disabled
//! (after which it touches no memory) and detached: its device stays
//! registered and answers [`BlockError::Io`]. Boot never waits on it.
//!
//! A namespace formatted with anything but 512-byte blocks is refused with a
//! log line naming the format: the block layer is 512 everywhere
//! ([`SECTOR_SIZE`]).

mod bounce;
mod hw;

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use alloc::vec::Vec;
use nvme::{Controller, Op, Pages, Platform, MAX_INFLIGHT};

use bounce::{gather, scatter};
use hw::{Hw, Page};
use x86_64::VirtAddr;

use super::iowait::{self, Expect};
use super::stats::IoStats;
use super::{BlockDevice, BlockError, Wait, SECTOR_SIZE};
use crate::dev::pci;
use crate::task::relax::YieldMutex;

/// How long a command may stay unanswered before the controller is
/// detached.
const TIMEOUT_NS: u64 = 10_000_000_000;
/// How many controllers can be driven at once (one [`Slot`] each).
const MAX_NVME: usize = 2;
/// Registry names, indexed by slot.
const NAMES: [&str; MAX_NVME] = ["nvme0", "nvme1"];
/// Most of BAR0 this driver maps: the registers and the doorbells of queues
/// 0 and 1 at any `CAP.DSTRD` a real controller uses.
const MAX_BAR_MAP: u64 = 64 * 1024;
/// Bytes of BAR0 that hold the registers and the first doorbell.
const MIN_BAR: u64 = 0x2000;

/// Pages per slot: two admin queues, two I/O queues, Identify, the PRP
/// lists and the bounce page.
const PAGES: usize = 5 + MAX_INFLIGHT + 1;
const BOUNCE: usize = PAGES - 1;

/// A live controller.
struct Live {
    hw: Hw,
    controller: Controller,
}

/// One driven controller: its DMA memory and the registry-facing device.
struct Slot {
    pages: [Page; PAGES],
    device: NvmeDisk,
}

/// The registry-facing device of one [`Slot`].
pub struct NvmeDisk {
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
            pages: [const { Page::new() }; PAGES],
            device: NvmeDisk {
                index,
                state: YieldMutex::new(None),
                sectors: AtomicU64::new(0),
                expect_read: Expect::new(),
                expect_write: Expect::new(),
                stats: IoStats::new(),
            },
        }
    }
}

static SLOTS: [Slot; MAX_NVME] = [Slot::new(0), Slot::new(1)];
/// Slots handed out so far.
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Bring up the controller at `function` (class 01:08:02) with BAR0 at
/// `bar` (`len` bytes), and return its block device. `None` (with a log
/// line) when the controller is unusable or every slot is taken.
pub fn attach_function(
    function: pci::Function,
    bar: u64,
    len: u64,
) -> Option<&'static dyn BlockDevice> {
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    let Some(slot) = SLOTS.get(index) else {
        serial_println!(
            "nvme: more than {MAX_NVME} controllers; {:?} ignored",
            function.address
        );
        return None;
    };
    let name = NAMES[index];
    if len < MIN_BAR {
        serial_println!("{name}: BAR0 of {len:#x} bytes is too small");
        return None;
    }
    pci::enable_memory(function.address);
    pci::enable_bus_master(function.address);
    let mapped = len.min(MAX_BAR_MAP);
    let regs = match crate::mem::mmio::map_kernel(bar, mapped) {
        Ok(regs) => regs,
        Err(why) => {
            serial_println!("{name}: cannot map BAR0 at {bar:#x}: {why}");
            return None;
        }
    };
    let hw = Hw { regs, mapped };
    // The doorbells of queues 0 and 1 must lie inside the mapping.
    let cap = nvme::regs::Cap::decode(nvme::regs::read64(&hw, nvme::regs::CAP));
    if nvme::regs::cq_doorbell(1, cap.doorbell_stride) as u64 + 4 > mapped {
        serial_println!(
            "{name}: doorbell stride {} beyond BAR0",
            cap.doorbell_stride
        );
        return None;
    }
    let mut phys = [0u64; PAGES];
    for (page, out) in slot.pages.iter().zip(phys.iter_mut()) {
        // SAFETY: the slot was just claimed and no controller owns its
        // pages yet.
        unsafe { (*page.0.get()).fill(0) };
        let Some(address) = page.phys() else {
            serial_println!("{name}: DMA page has no physical address");
            return None;
        };
        *out = address;
    }
    let pages = Pages {
        admin_sq: phys[0],
        admin_cq: phys[1],
        io_sq: phys[2],
        io_cq: phys[3],
        identify: phys[4],
        prp_lists: core::array::from_fn(|index| phys[5 + index]),
    };
    let mut controller = match Controller::init(&hw, pages) {
        Ok(controller) => controller,
        Err(error) => {
            serial_println!("{name}: bring-up failed: {error}");
            return None;
        }
    };
    let namespace = controller.namespace;
    let info = controller.info;
    if namespace.block_bytes as usize != SECTOR_SIZE {
        serial_println!(
            "{name}: namespace 1 uses LBA format {} with {}-byte blocks; only 512-byte formats are served",
            namespace.format,
            namespace.block_bytes
        );
        controller.detach(&hw);
        return None;
    }
    let (major, minor) = nvme::regs::version(hw.read32(nvme::regs::VS));
    serial_println!(
        "{name}: {} (serial {}, firmware {}), NVMe {major}.{minor}, BAR0 {bar:#x}, {} MiB, {} KiB per command{}",
        info.model.as_str(),
        info.serial.as_str(),
        info.firmware.as_str(),
        namespace.bytes() >> 20,
        controller.max_command_bytes() / 1024,
        if info.volatile_cache {
            ", write cache"
        } else {
            ""
        }
    );
    let device = &slot.device;
    device.sectors.store(namespace.blocks, Ordering::Relaxed);
    *device.state.lock() = Some(Live { hw, controller });
    Some(device)
}

/// Send every live controller the normal shutdown notification, after the
/// filesystems synced, so it writes its cache back before power goes.
pub fn shutdown_all() {
    for slot in SLOTS
        .iter()
        .take(NEXT.load(Ordering::Relaxed).min(MAX_NVME))
    {
        let device = &slot.device;
        let mut guard = device.state.lock();
        let Some(live) = guard.as_mut() else {
            continue;
        };
        match live.controller.shutdown(&live.hw) {
            Ok(()) => serial_println!("{}: shutdown complete", NAMES[device.index]),
            Err(error) => serial_println!("{}: shutdown: {error}", NAMES[device.index]),
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

impl NvmeDisk {
    fn slot(&self) -> &'static Slot {
        &SLOTS[self.index]
    }

    /// Move the bytes of `segments` (`(address, length)`, back to back) to
    /// (`write`) or from the device at `lba`. The segments come from slices
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
            .all(|&(address, _)| address.is_multiple_of(4));
        let result = if aligned {
            match self.direct(live, write, lba, segments, wait) {
                Err(nvme::Error::Misaligned) => self.bounced(live, write, lba, segments, wait),
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
                if live.controller.is_detached() {
                    serial_println!("{}: controller detached", NAMES[self.index]);
                    *guard = None;
                }
                Err(match error {
                    nvme::Error::Bounds => BlockError::Bounds,
                    nvme::Error::Unmapped => BlockError::Unsupported,
                    _ => BlockError::Io,
                })
            }
        }
    }

    /// The controller moves `segments` itself.
    fn direct(
        &self,
        live: &mut Live,
        write: bool,
        lba: u64,
        segments: &[(u64, usize)],
        wait: Wait,
    ) -> Result<(), nvme::Error> {
        let (op, expect) = if write {
            (Op::Write, &self.expect_write)
        } else {
            (Op::Read, &self.expect_read)
        };
        let mut waiter =
            |ready: &dyn Fn() -> bool| iowait::wait_until(wait, expect, TIMEOUT_NS, ready);
        live.controller
            .transfer(&live.hw, op, lba, segments, &translate, &mut waiter)
    }

    /// Through the bounce page, one page at a time: buffers the PRP rules
    /// cannot describe.
    fn bounced(
        &self,
        live: &mut Live,
        write: bool,
        lba: u64,
        segments: &[(u64, usize)],
        wait: Wait,
    ) -> Result<(), nvme::Error> {
        let bounce = &self.slot().pages[BOUNCE];
        let page = bounce.virt();
        let total: usize = segments.iter().map(|&(_, len)| len).sum();
        let mut done = 0usize;
        while done < total {
            let chunk = (total - done).min(4096);
            if write {
                // SAFETY: the segments are the caller's live slices and the
                // bounce page is this controller's, under the device lock.
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

impl BlockDevice for NvmeDisk {
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

    /// The controller writes straight into `bufs`, which stay borrowed until
    /// every command completed (or the controller was disabled).
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
        let result = live.controller.flush(&live.hw, &mut waiter);
        if live.controller.is_detached() {
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
