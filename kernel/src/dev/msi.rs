//! Message-signalled interrupts (issue #616, docs/n150-driver-plan.md K1).
//!
//! A claim whose function has an MSI or MSI-X capability gets one vector of
//! its own at `irq_enable`: the kernel allocates it from [`VECTORS`] vectors
//! above the legacy lines and programs the capability itself (address
//! `0xFEE0_0000 | APIC ID << 12`, data = the vector, fixed, edge), so a
//! driver never writes an interrupt address. The driver-facing contract is
//! the INTx one: the same one-way message from the kernel, the same
//! `irq_ack`; only the masking is per vector instead of per line.
//!
//! Delivery reuses `dev::intx`'s rounds: vector `i` is delivery *source*
//! [`SOURCE_BASE`]` + i`, next to the legacy lines `0..16`. The handler
//! ([`dispatch`]) is lock-free like `dev::irq::dispatch`: it masks the vector
//! in software, records the raise and sends the local APIC EOI. A message
//! that arrives while the vector is masked is *latched* rather than dropped
//! (an MSI is an edge: nothing re-asserts it), and unmasking turns a latched
//! message into a raise, which is how a level line behaves when it is still
//! asserted. Where the hardware can (MSI-X always, MSI when the capability
//! has mask bits) the vector is also masked at the function while a round is
//! open, so a storming device stops at the source.
//!
//! `LAZYOS_MSI=0` turns message interrupts off (every claim on INTx).

use core::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};

use spin::Mutex;

use crate::arch::lapic;

use super::errno::{Errno, ENOSPC, ENOSYS};
use super::msi_hw::{self, Hw};
use super::table::MAX_DEVICES;
use super::{irq, BusId, DeviceId, DeviceInfo};

/// First MSI vector: above the APIC timer (0x30, 0x31), below the syscall
/// gate (0x80).
pub const VECTOR_BASE: u8 = 0x40;
/// MSI vectors, so at most this many claims take message interrupts at
/// once; a claim past it falls back to its INTx line.
pub const VECTORS: u8 = 32;
/// Delivery source of vector 0 (`dev::intx`): after the legacy lines.
pub const SOURCE_BASE: u8 = irq::LINES;

/// How a claim's interrupts arrive (the value `irq_enable` returns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Intx = 0,
    Msi = 1,
    MsiX = 2,
}

/// Owner of each vector: device id + 1, 0 when free. The handler reads it.
static OWNER: [AtomicU16; VECTORS as usize] = [const { AtomicU16::new(0) }; VECTORS as usize];
/// Bit per vector masked in software: a message is latched, not raised.
static SOFT_MASK: AtomicU32 = AtomicU32::new(u32::MAX);
/// Bit per masked vector that received a message meanwhile.
static LATCHED: AtomicU32 = AtomicU32::new(0);
/// Bit per device whose first delivery was reported (`DEV:MSI:PASS`).
static REPORTED: [AtomicU64; MAX_DEVICES.div_ceil(64)] =
    [const { AtomicU64::new(0) }; MAX_DEVICES.div_ceil(64)];

static SPURIOUS: AtomicU32 = AtomicU32::new(0);
static RAISES: AtomicU32 = AtomicU32::new(0);
static LATCHES: AtomicU32 = AtomicU32::new(0);

/// What each vector is programmed into (task context only).
static ROUTES: Mutex<[Option<Hw>; VECTORS as usize]> = Mutex::new([None; VECTORS as usize]);

/// Counters for diagnostics and the suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsiStats {
    /// Messages that raised their vector's source.
    pub raised: u32,
    /// Messages on a vector nobody owns.
    pub spurious: u32,
    /// Messages that arrived while their vector was masked.
    pub latched: u32,
}

pub fn stats() -> MsiStats {
    MsiStats {
        raised: RAISES.load(Ordering::Relaxed),
        spurious: SPURIOUS.load(Ordering::Relaxed),
        latched: LATCHES.load(Ordering::Relaxed),
    }
}

/// Whether claims may take message interrupts: the image allows it and the
/// local APIC is up with an ID an xAPIC message address can name.
pub fn enabled() -> bool {
    option_env!("LAZYOS_MSI") != Some("0")
        && lapic::current_mode().is_some()
        && destination().is_some()
}

/// The APIC ID messages are sent to.
fn destination() -> Option<u8> {
    u8::try_from(lapic::id()).ok()
}

/// The CPU vector of MSI vector `index`.
pub const fn vector(index: u8) -> u8 {
    VECTOR_BASE + index
}

/// The interrupt handler body for MSI vector `index`. Lock-free.
pub fn dispatch(index: u8) {
    if index < VECTORS {
        let bit = 1u32 << index;
        if OWNER[usize::from(index)].load(Ordering::Acquire) == 0 {
            SPURIOUS.fetch_add(1, Ordering::Relaxed);
        } else if SOFT_MASK.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
            LATCHED.fetch_or(bit, Ordering::AcqRel);
            LATCHES.fetch_add(1, Ordering::Relaxed);
        } else {
            RAISES.fetch_add(1, Ordering::Relaxed);
            irq::raise(SOURCE_BASE + index);
        }
    }
    lapic::eoi();
}

/// Whether vector `index` is masked.
pub fn is_masked(index: u8) -> bool {
    index >= VECTORS || SOFT_MASK.load(Ordering::Acquire) & (1 << index) != 0
}

/// Mask or unmask vector `index` (task context, interrupts off: the
/// bottom half and the syscalls). Unmasking a vector that latched a message
/// raises it at once, masked again, as a still-asserted line would.
pub fn set_masked(index: u8, masked: bool) {
    if index >= VECTORS {
        return;
    }
    let bit = 1u32 << index;
    let hw = ROUTES.lock()[usize::from(index)];
    if masked {
        SOFT_MASK.fetch_or(bit, Ordering::AcqRel);
        if let Some(hw) = hw {
            msi_hw::mask(&hw, true);
        }
        return;
    }
    if LATCHED.fetch_and(!bit, Ordering::AcqRel) & bit != 0 {
        // Stay masked and hand the message to the bottom half.
        SOFT_MASK.fetch_or(bit, Ordering::AcqRel);
        RAISES.fetch_add(1, Ordering::Relaxed);
        irq::raise(SOURCE_BASE + index);
        return;
    }
    SOFT_MASK.fetch_and(!bit, Ordering::AcqRel);
    if let Some(hw) = hw {
        msi_hw::mask(&hw, false);
    }
}

/// Give claim `id` a vector and program its function for it, masked: the
/// caller unmasks it when the claim joins delivery. MSI is preferred over
/// MSI-X (no table to map). `ENOSYS` when the function cannot take message
/// interrupts here, `ENOSPC` when every vector is taken.
pub fn route(id: DeviceId, info: &DeviceInfo) -> Result<(u8, Mode), Errno> {
    if !enabled() || !info.resources.message_capable() {
        return Err(ENOSYS);
    }
    let BusId::Pci(address) = info.bus else {
        return Err(ENOSYS);
    };
    let dest = destination().ok_or(ENOSYS)?;
    let owner = id.0.checked_add(1).ok_or(ENOSYS)?;
    let index = (0..VECTORS)
        .find(|&i| {
            OWNER[usize::from(i)]
                .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
        .ok_or(ENOSPC)?;
    let bit = 1u32 << index;
    SOFT_MASK.fetch_or(bit, Ordering::AcqRel);
    LATCHED.fetch_and(!bit, Ordering::AcqRel);
    irq::clear(SOURCE_BASE + index);
    match msi_hw::enable(id, address, info, vector(index), dest) {
        Ok(hw) => {
            let mode = hw.mode();
            ROUTES.lock()[usize::from(index)] = Some(hw);
            Ok((index, mode))
        }
        Err(errno) => {
            OWNER[usize::from(index)].store(0, Ordering::Release);
            Err(errno)
        }
    }
}

/// Take vector `index` back from claim `id`: switch the function's message
/// interrupts off, forget any raise or latch, free the vector. A message
/// already in flight lands on a free vector and is counted spurious.
pub fn unroute(index: u8, id: DeviceId) {
    if index >= VECTORS || OWNER[usize::from(index)].load(Ordering::Acquire) != id.0 + 1 {
        return;
    }
    let hw = ROUTES.lock()[usize::from(index)].take();
    if let Some(hw) = hw {
        msi_hw::disable(&hw);
    }
    let bit = 1u32 << index;
    SOFT_MASK.fetch_or(bit, Ordering::AcqRel);
    LATCHED.fetch_and(!bit, Ordering::AcqRel);
    irq::clear(SOURCE_BASE + index);
    OWNER[usize::from(index)].store(0, Ordering::Release);
}

/// The mode vector `index` was routed with (`Intx` when it is free).
pub fn mode(index: u8) -> Mode {
    ROUTES
        .lock()
        .get(usize::from(index))
        .copied()
        .flatten()
        .map_or(Mode::Intx, |hw| hw.mode())
}

/// Switch off any message interrupt a function was left with (a new claim
/// starts on nothing, like INTx-disable).
pub fn quiesce(info: &DeviceInfo) {
    if let BusId::Pci(address) = info.bus {
        msi_hw::disable_capabilities(address, &info.resources);
    }
}

/// Vectors in use (the suite checks none leak).
pub fn in_use() -> usize {
    OWNER
        .iter()
        .filter(|owner| owner.load(Ordering::Relaxed) != 0)
        .count()
}

/// Print `DEV:MSI:PASS` the first time device `id` is sent an interrupt on
/// vector `index` (once per device and boot): the proof that a message
/// travelled device -> local APIC -> stub -> bottom half -> driver.
pub fn note_delivered(index: u8, id: DeviceId) {
    let (word, bit) = (usize::from(id.0) / 64, 1u64 << (id.0 % 64));
    let Some(reported) = REPORTED.get(word) else {
        return;
    };
    if index >= VECTORS || reported.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
        return;
    }
    let hw = ROUTES.lock()[usize::from(index)];
    if let Some(hw) = hw {
        let a = hw.address;
        crate::serial_println!(
            "DEV:MSI:PASS:{:02x}:{:02x}.{} dev {} {:?} vector {:#x}",
            a.bus,
            a.device,
            a.function,
            id.0,
            hw.mode(),
            vector(index)
        );
    }
}

/// Test-only: forget every vector (the fixture restores a clean slate).
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    for index in 0..VECTORS {
        if let Some(hw) = ROUTES.lock()[usize::from(index)].take() {
            msi_hw::disable(&hw);
        }
        OWNER[usize::from(index)].store(0, Ordering::Release);
        irq::clear(SOURCE_BASE + index);
    }
    SOFT_MASK.store(u32::MAX, Ordering::Release);
    LATCHED.store(0, Ordering::Release);
}
