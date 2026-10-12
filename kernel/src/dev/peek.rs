//! `dbgd`'s read-only hardware ops (device syscall ops 13 and 14,
//! `cfg(lazyos_dbgd)` kernels only; docs/dbgd-plan.md, "Hardware reads").
//!
//! A person debugging a PC with no serial port wants a function's
//! configuration space and a few registers of its BAR *without* the driver
//! that claims the device, so the ops take a PCI address, not a `Device`
//! handle, and never claim anything. What keeps that safe:
//!
//! * **Who**: uid 914 (`dbgd`) or root, and the Messenger policy for
//!   `os.kernel.dev` like the other inspection ops. `dbgd` authenticates
//!   its own peers; the kernel does not trust it for the *rules* below.
//! * **Read-only**: no write path exists; config reads are side-effect free.
//! * **Allowlist**: MMIO is answered only for dword-aligned spans inside a
//!   range `libs/pciprobe` lists for that exact `vendor:device` and BAR
//!   (built in, or added by the box's own `lazyos.cfg`), and only when
//!   `diag.dbg.mmio=1`. Everything else is `EACCES`, so a register with a
//!   side effect on read stays unreachable until it is listed.
//! * **Not behind a driver's back**: a device a driver holds is `EBUSY`
//!   unless `diag.dbg.mmio.claimed=1`; a device whose memory decode is off
//!   (a quiesced claim, firmware that never enabled it) is `ENXIO`, as is a
//!   BAR that is not memory, not assigned, under a page, overlapping RAM or
//!   past the CPU's physical address width (the checks `map_bar` makes).
//! * **Bounded**: 4 KiB per call, 64 MiB of kernel window in all. Ranges are
//!   mapped once, uncached, on first use and kept (the kernel window never
//!   unmaps); only bytes inside an allowed range are ever read.
//!
//! Errors: `EPERM` caller, `EACCES` rules, `EBUSY` claimed, `ENODEV` no such
//! function, `ENXIO` BAR unusable or decode off, `ERANGE` past the BAR's
//! end, `EINVAL` malformed request, `ENOMEM` window budget, `EFAULT`
//! buffer.

use alloc::vec::Vec;
use spin::Mutex;

use pciprobe::{Denied, Policy};

use super::errno::*;
use super::pci::{self, COMMAND_MEMORY};
use super::{table, BarKind, BusId, DeviceInfo};
use crate::ipc::credentials;
use crate::ipc::topics::fnv1a32;
use crate::mem::mmio;

/// `dbgd`'s uid (`libs/dbgwire::config::DBGD_UID`; the kernel keeps its own
/// copy, like `sysinfo`).
const DBGD_UID: u32 = 914;
const PAGE: u64 = 4096;
/// Pages of kernel MMIO window these ops may hold (64 MiB).
const MAP_BUDGET_PAGES: u64 = (64 << 20) / PAGE;
/// Dwords between interrupt windows while reading.
const POLL_EVERY: usize = 64;

/// Method ids the ops are authorized as, on `os.kernel.dev`.
const CONFIG_READ: u32 = fnv1a32("dbg_config_read");
const MMIO_READ: u32 = fnv1a32("dbg_mmio_read");

static POLICY: Mutex<Policy> = Mutex::new(Policy::off());

/// One allowed range mapped into the kernel window.
#[derive(Clone, Copy)]
struct Mapped {
    phys: u64,
    pages: u64,
    va: u64,
}

static MAPS: Mutex<Vec<Mapped>> = Mutex::new(Vec::new());

/// Read the `diag.dbg.mmio*` lines of `lazyos.cfg` (called by `bootcfg`
/// each time the file is read). A change is logged once.
pub fn apply_config(text: &str) {
    let next = Policy::from_cfg(text);
    let mut policy = POLICY.lock();
    if *policy == next {
        return;
    }
    crate::trace::line!(
        tracefmt::Channel::Dev,
        "PEEK:POLICY mmio={} claimed={} extra={} rejected={}",
        u8::from(next.mmio),
        u8::from(next.claimed),
        next.extra().len(),
        next.rejected
    );
    *policy = next;
}

/// Test hook: install `policy` directly.
#[cfg(lazyos_tests)]
pub fn set_policy_for_test(policy: Policy) {
    *POLICY.lock() = policy;
}

/// Test hook: back to the policy of a box that did not ask.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    *POLICY.lock() = Policy::off();
}

/// Pages of kernel window held by cached mappings (the suite's budget
/// checks).
#[cfg(lazyos_tests)]
pub fn mapped_pages_for_test() -> u64 {
    MAPS.lock().iter().map(|m| m.pages).sum()
}

fn authorize(slot: usize, method: u32) -> Result<(), Errno> {
    let uid = credentials::of(slot).uid;
    if uid != 0 && uid != DBGD_UID {
        return Err(EPERM);
    }
    super::inspect::authorized(slot, method)
}

/// The enumerated function at `bdf`, if there is one.
fn find(bdf: u64) -> Result<(DeviceInfo, pci::Address), Errno> {
    let bdf = u16::try_from(bdf).map_err(|_| EINVAL)?;
    let address = pci::Address {
        bus: (bdf >> 8) as u8,
        device: ((bdf >> 3) & 0x1F) as u8,
        function: (bdf & 7) as u8,
    };
    table()
        .lock()
        .iter()
        .find(|info| info.bus == BusId::Pci(address))
        .map(|info| (info, address))
        .ok_or(ENODEV)
}

fn copy_out(buf: u64, bytes: &[u8]) -> Result<u64, Errno> {
    crate::user_ptr::try_copy_to(buf, bytes).map_err(|_| EFAULT)?;
    Ok(bytes.len() as u64)
}

/// Op 13: `len` bytes of the function's configuration space from `offset`.
pub fn config_read(slot: usize, bdf: u64, offset: u64, len: u64, buf: u64) -> Result<u64, Errno> {
    authorize(slot, CONFIG_READ)?;
    pciprobe::check_config(offset, len).map_err(|_| EINVAL)?;
    let (_, address) = find(bdf)?;
    let mut bytes = Vec::with_capacity(len as usize);
    for at in offset..offset + len {
        bytes.push(pci::read8(address, at as u8));
    }
    copy_out(buf, &bytes)
}

/// The page-aligned mapping of `pages` pages at `phys`, made once.
fn mapping(phys: u64, pages: u64) -> Result<u64, Errno> {
    let mut maps = MAPS.lock();
    if let Some(found) = maps.iter().find(|m| m.phys == phys && m.pages == pages) {
        return Ok(found.va);
    }
    let held: u64 = maps.iter().map(|m| m.pages).sum();
    if held + pages > MAP_BUDGET_PAGES {
        return Err(ENOMEM);
    }
    let va = mmio::map_kernel(phys, pages * PAGE).map_err(|_| ENXIO)?;
    maps.push(Mapped { phys, pages, va });
    Ok(va)
}

/// Op 14: `len` bytes (dwords) of BAR `bar` from `offset`; `packed` is
/// `bar | len << 8`.
pub fn mmio_read(slot: usize, bdf: u64, offset: u64, packed: u64, buf: u64) -> Result<u64, Errno> {
    authorize(slot, MMIO_READ)?;
    let (bar_index, len) = ((packed & 0xFF) as u8, packed >> 8);
    let (info, address) = find(bdf)?;
    let policy = POLICY.lock().clone();
    let plan = policy
        .plan(info.vendor, info.device, bar_index, offset, len)
        .map_err(|why| match why {
            Denied::Off | Denied::NotAllowed => EACCES,
            Denied::Length | Denied::Alignment | Denied::Bar => EINVAL,
        })?;
    if !policy.claimed && table().lock().owner(info.id).is_some() {
        return Err(EBUSY);
    }
    let bar = info.resources.bar(bar_index).ok_or(ENXIO)?;
    if bar.kind != BarKind::Mem
        || bar.base == 0
        || !bar.base.is_multiple_of(PAGE)
        || bar.len < PAGE
        || !bar.len.is_multiple_of(PAGE)
    {
        return Err(ENXIO);
    }
    let bar_end = bar.base.checked_add(bar.len).ok_or(ENXIO)?;
    if bar_end > mmio::phys_limit() || mmio::overlaps_ram(bar.base, bar_end) {
        return Err(ENXIO);
    }
    if pci::command(address) & COMMAND_MEMORY == 0 {
        return Err(ENXIO);
    }
    if offset + len > bar.len {
        return Err(ERANGE);
    }
    let mut out = Vec::with_capacity(len as usize);
    let mut since_poll = 0;
    for segment in &plan {
        // The page span of the whole allowed range, so one mapping serves
        // every request that lands in it.
        let first = segment.range.start & !(PAGE - 1);
        let last = segment.range.end.min(bar.len).next_multiple_of(PAGE);
        let va = mapping(bar.base + first, (last - first) / PAGE)?;
        let mut at = va + (segment.offset - first);
        for _ in 0..segment.len / 4 {
            // SAFETY: `at` is dword aligned (offsets and lengths are
            // multiples of 4 and `va` is page aligned) and inside the
            // `[first, last)` window of this BAR that `mapping` mapped
            // uncached for the kernel's lifetime; the segment lies inside
            // the allowed range and the BAR (checked above).
            let word = unsafe { core::ptr::read_volatile(at as *const u32) };
            out.extend_from_slice(&word.to_le_bytes());
            at += 4;
            since_poll += 1;
            if since_poll == POLL_EVERY {
                since_poll = 0;
                crate::arch::irq_window::poll_point();
            }
        }
    }
    copy_out(buf, &out)
}
