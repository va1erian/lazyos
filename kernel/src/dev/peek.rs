//! `dbgd`'s read-only hardware ops (device syscall ops 13 and 14,
//! `cfg(lazyos_dbgd)` kernels only; docs/dbgd-plan.md, "Hardware reads").
//!
//! A person debugging a PC with no serial port wants a function's
//! configuration space and a few registers of its BAR *without* the driver
//! that claims the device, so the ops take a PCI address, not a `Device`
//! handle, and never claim anything. What keeps that safe:
//!
//! * **Who**: uid 914 (`dbgd`) only, never uid 0 (services authorize by a
//!   kernel-stamped identity, not by being root), and the Messenger policy
//!   for `os.kernel.dev` like the other inspection ops. The in-kernel suite
//!   may also call as the kernel task. `dbgd` authenticates its own peers;
//!   the kernel does not trust it for the *rules* below.
//! * **Read-only**: no write path exists.
//! * **Off unless the box says so**: nothing is answered without `diag.dbg=1`
//!   in `lazyos.cfg` (read here, not told by `dbgd`). Config space is the
//!   64-byte standard header; bytes 0x40..0x100 and MMIO need
//!   `diag.dbg.mmio=1` too (some devices misbehave when their
//!   device-specific config registers are read).
//! * **Allowlist**: MMIO is answered only for dword-aligned spans inside a
//!   range `libs/pciprobe` lists for that exact `vendor:device` and BAR
//!   (built in, or added by the box's own `lazyos.cfg`). Everything else is
//!   `EACCES`, so a register with a side effect on read stays unreachable
//!   until it is listed.
//! * **Not behind a driver's back**: a device a driver holds is `EBUSY`
//!   unless `diag.dbg.mmio.claimed=1`; a device whose memory decode is off
//!   (a quiesced claim, firmware that never enabled it) is `ENXIO`, as is a
//!   BAR that is not memory, not assigned, under a page, overlapping RAM or
//!   past the CPU's physical address width (the checks `map_bar` makes). A
//!   function that reads all-ones (gone, link down) is `ENODEV`: a read that
//!   would complete with a timeout can stall interrupts-off for
//!   milliseconds, so the loop also stops when a dword reads all-ones and a
//!   vendor re-read says the function is gone.
//! * **Bounded**: 4 KiB per call, 64 MiB of kernel window in all (failed
//!   maps count too). Only the 4 KiB pages a request touches are mapped,
//!   read-only and uncached, once, and kept (the kernel window never
//!   unmaps); only bytes inside an allowed range are ever read.
//!
//! **Single CPU.** The claim check and the read are one critical section
//! only because LazyOS runs on CPU 0 and the interrupt windows of
//! `arch::irq_window::poll_point` never switch tasks, so no other task can
//! claim, release or reprogram the device in between. The device table lock
//! is held across the loop on top of that (the window's handlers take no
//! lock but the i8042's, so a poll point is safe under it). A port to SMP
//! must make the claim table and the BAR decode a real critical section.
//!
//! Errors: `EPERM` caller, `EACCES` rules, `EBUSY` claimed, `ENODEV` no such
//! function or gone, `ENXIO` BAR unusable or decode off, `ERANGE` past the
//! BAR's end, `EDOM` malformed request (`EINVAL` means no such op: the
//! kernel has no `LAZYOS_DBGD=1`), `ENOMEM` window budget, `EFAULT` buffer.

use alloc::collections::BTreeMap;
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
const MAP_BUDGET_PAGES: usize = (64 << 20) / PAGE as usize;
/// Dwords between interrupt windows (and liveness re-checks) while reading.
const POLL_EVERY: usize = 64;

/// Method ids the ops are authorized as, on `os.kernel.dev`.
const CONFIG_READ: u32 = fnv1a32("dbg_config_read");
const MMIO_READ: u32 = fnv1a32("dbg_mmio_read");

static POLICY: Mutex<Policy> = Mutex::new(Policy::off());

/// The 4 KiB pages mapped so far: physical page to kernel virtual address,
/// and how many page maps failed (they may leave window space reserved, so
/// they count against the budget too).
struct Maps {
    pages: BTreeMap<u64, u64>,
    failed: usize,
}

static MAPS: Mutex<Maps> = Mutex::new(Maps {
    pages: BTreeMap::new(),
    failed: 0,
});

/// Test hook: treat an all-ones function as present (the suite's synthetic
/// functions sit at an address nothing decodes).
#[cfg(lazyos_tests)]
static ASSUME_PRESENT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Read the `diag.dbg*` lines of `lazyos.cfg` (called by `bootcfg` each time
/// the file is read). A change is logged once.
pub fn apply_config(text: &str) {
    let next = Policy::from_cfg(text);
    let mut policy = POLICY.lock();
    if *policy == next {
        return;
    }
    crate::trace::line!(
        tracefmt::Channel::Dev,
        "PEEK:POLICY enabled={} mmio={} claimed={} extra={} rejected={}",
        u8::from(next.enabled),
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
    ASSUME_PRESENT.store(false, core::sync::atomic::Ordering::Relaxed);
}

/// Test hook: whether an all-ones function counts as present.
#[cfg(lazyos_tests)]
pub fn assume_present_for_test(present: bool) {
    ASSUME_PRESENT.store(present, core::sync::atomic::Ordering::Relaxed);
}

/// Pages of kernel window held by cached mappings and failed maps (the
/// suite's budget checks).
#[cfg(lazyos_tests)]
pub fn mapped_pages_for_test() -> u64 {
    let maps = MAPS.lock();
    (maps.pages.len() + maps.failed) as u64
}

fn authorize(slot: usize, method: u32) -> Result<(), Errno> {
    let uid = credentials::of(slot).uid;
    let allowed = uid == DBGD_UID;
    #[cfg(lazyos_tests)]
    let allowed = allowed || slot == crate::task::KERNEL_TASK;
    if !allowed {
        return Err(EPERM);
    }
    super::inspect::authorized(slot, method)
}

fn alive_assumed() -> bool {
    #[cfg(lazyos_tests)]
    return ASSUME_PRESENT.load(core::sync::atomic::Ordering::Relaxed);
    #[cfg(not(lazyos_tests))]
    false
}

fn address_of(bdf: u64) -> Result<pci::Address, Errno> {
    let bdf = u16::try_from(bdf).map_err(|_| EDOM)?;
    Ok(pci::Address {
        bus: (bdf >> 8) as u8,
        device: ((bdf >> 3) & 0x1F) as u8,
        function: (bdf & 7) as u8,
    })
}

fn copy_out(buf: u64, bytes: &[u8]) -> Result<u64, Errno> {
    crate::user_ptr::try_copy_to(buf, bytes).map_err(|_| EFAULT)?;
    Ok(bytes.len() as u64)
}

/// Op 13: `len` bytes of the function's configuration space from `offset`.
pub fn config_read(slot: usize, bdf: u64, offset: u64, len: u64, buf: u64) -> Result<u64, Errno> {
    authorize(slot, CONFIG_READ)?;
    POLICY
        .lock()
        .check_config(offset, len)
        .map_err(|why| match why {
            Denied::Length | Denied::Alignment | Denied::Bar => EDOM,
            _ => EACCES,
        })?;
    let address = address_of(bdf)?;
    if !table()
        .lock()
        .iter()
        .any(|info| info.bus == BusId::Pci(address))
    {
        return Err(ENODEV);
    }
    // One configuration transaction per dword, not per byte.
    let first = offset & !3;
    let last = (offset + len).next_multiple_of(4);
    let mut bytes = Vec::with_capacity((last - first) as usize);
    for at in (first..last).step_by(4) {
        bytes.extend_from_slice(&pci::read32(address, at as u8).to_le_bytes());
    }
    let skip = (offset - first) as usize;
    copy_out(buf, &bytes[skip..skip + len as usize])
}

/// The kernel address of the 4 KiB page at `phys`, mapped read-only on
/// first use.
fn page_va(phys: u64) -> Result<u64, Errno> {
    let mut maps = MAPS.lock();
    if let Some(va) = maps.pages.get(&phys) {
        return Ok(*va);
    }
    if maps.pages.len() + maps.failed >= MAP_BUDGET_PAGES {
        return Err(ENOMEM);
    }
    match mmio::map_kernel_read_only(phys, PAGE) {
        Ok(va) => {
            maps.pages.insert(phys, va);
            Ok(va)
        }
        Err(_) => {
            maps.failed += 1;
            Err(ENXIO)
        }
    }
}

/// Whether the function still answers: its vendor id is not all-ones.
fn present(address: pci::Address) -> bool {
    alive_assumed() || pci::read16(address, 0) != 0xFFFF
}

/// Op 14: `len` bytes (dwords) of BAR `bar` from `offset`; `packed` is
/// `bar | len << 8`.
pub fn mmio_read(slot: usize, bdf: u64, offset: u64, packed: u64, buf: u64) -> Result<u64, Errno> {
    authorize(slot, MMIO_READ)?;
    let (bar_index, len) = ((packed & 0xFF) as u8, packed >> 8);
    let address = address_of(bdf)?;
    // The table lock is held to the end: see the module comment.
    let devices = table().lock();
    let info: DeviceInfo = devices
        .iter()
        .find(|info| info.bus == BusId::Pci(address))
        .ok_or(ENODEV)?;
    let policy = POLICY.lock().clone();
    let plan = policy
        .plan(info.vendor, info.device, bar_index, offset, len)
        .map_err(|why| match why {
            Denied::Disabled | Denied::Off | Denied::ExtendedConfig | Denied::NotAllowed => EACCES,
            Denied::Length | Denied::Alignment | Denied::Bar => EDOM,
        })?;
    if !policy.claimed && devices.owner(info.id).is_some() {
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
    // A gone function reads all-ones everywhere, command register
    // included, which would look like "decode on".
    if !present(address) {
        return Err(ENODEV);
    }
    if pci::command(address) & COMMAND_MEMORY == 0 {
        return Err(ENXIO);
    }
    if offset + len > bar.len {
        return Err(ERANGE);
    }
    let mut out = Vec::with_capacity(len as usize);
    let mut since_poll = 0;
    let mut checked_gone = false;
    for segment in &plan {
        let mut page = u64::MAX;
        let mut base = 0;
        for at in (segment.offset..segment.offset + segment.len).step_by(4) {
            let phys = (bar.base + at) & !(PAGE - 1);
            if phys != page {
                base = page_va(phys)?;
                page = phys;
            }
            // SAFETY: `base` is the kernel virtual address of the 4 KiB
            // page at `phys`, mapped read-only and uncached for the
            // kernel's lifetime and inside this BAR (`offset + len` was
            // checked against its length, and the segment is inside an
            // allowed range); `at` is a multiple of 4, so the dword lies
            // inside the page and is aligned.
            let word = unsafe {
                core::ptr::read_volatile((base + ((bar.base + at) & (PAGE - 1))) as *const u32)
            };
            if word == u32::MAX && !checked_gone {
                // All-ones may be the register's value or a dead link whose
                // reads complete by timeout: ask the config space once.
                checked_gone = true;
                if !present(address) {
                    return Err(ENODEV);
                }
            }
            out.extend_from_slice(&word.to_le_bytes());
            since_poll += 1;
            if since_poll == POLL_EVERY {
                since_poll = 0;
                checked_gone = false;
                crate::arch::irq_window::poll_point();
            }
        }
    }
    drop(devices);
    copy_out(buf, &out)
}
