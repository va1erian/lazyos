//! A disk's life: registration into a free or reclaimed slot, its
//! generation, and the raw claim a filesystem daemon takes on it
//! (docs/usb-hotplug-plan.md B and F, docs/architecture/usb-storage.md).
//!
//! # Slots are reused, safely
//!
//! A slot (`usb<n>`) is given to the next registration once its disk is
//! dead and nothing refers to it any more ([`State::reclaimable`]): no
//! requester is in the middle of a request, no filesystem daemon holds a
//! claim, and no partition view was made of it (those, and the ext2 mount
//! of the home volume through them, hold `&'static` references that outlive
//! the disk; a scanned disk keeps its slot until reboot, as every disk
//! once did).
//!
//! The danger of reuse is a holder from before an unplug writing to the
//! stick plugged in next. Every [`super::UserDisk`] operation therefore
//! takes the slot's **generation** when it starts and every request of that
//! operation checks it: an operation in flight across a reclaim fails with
//! `Io` instead of reaching the new disk. Tags keep counting across
//! generations, so a late completion of an old request cannot match a new
//! one.
//!
//! # Claims
//!
//! A filesystem daemon (`fatfuse`) reads and writes a disk's sectors itself:
//! it takes an exclusive **claim** on the disk and then reads, writes and
//! flushes through [`raw_read`], [`raw_write`] and [`raw_flush`], which are
//! the same requests the kernel's own filesystems make. The claim names the
//! generation it was taken on, is dropped when the task releases it or dies
//! ([`release_task`]), and keeps the slot from being reclaimed meanwhile.

use alloc::vec::Vec;

use super::{
    disk, ProviderError, State, Stats, DISKS, MAX_REQUEST_BYTES, NAMES,
};
use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};

/// One live disk, as a claimant sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskInfo {
    pub id: usize,
    pub generation: u64,
    pub sectors: u64,
    pub writable: bool,
    /// Claimed by a daemon, or used by a kernel mount: not available.
    pub claimed: bool,
}

/// Why a raw operation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawError {
    /// No such disk.
    NoDisk,
    /// The disk is a different one than the claimant meant, or is dead.
    Stale,
    /// Someone else holds the claim.
    Busy,
    /// The caller holds no claim on the disk.
    NotOwner,
    /// The medium is gone.
    Gone,
    /// The device failed the request.
    Io,
    /// The medium is write protected.
    ReadOnly,
    /// The range lies outside the disk, or is not whole sectors.
    Range,
}

impl State {
    /// Whether the slot can take a new disk: the old one is dead and nothing
    /// refers to it.
    pub(super) fn reclaimable(&self) -> bool {
        self.registered
            && !self.alive
            && !self.busy
            && !self.scanned
            && !self.in_use
            && self.claim.is_none()
    }
}

/// Register a disk of `sectors` 512-byte sectors served by task `owner`, in
/// the lowest free or reclaimable slot. Returns its id (the `n` of
/// `usb<n>`).
pub fn register(owner: usize, sectors: u64, writable: bool) -> Result<usize, ProviderError> {
    if sectors == 0 || sectors.checked_mul(SECTOR_SIZE as u64).is_none() {
        return Err(ProviderError::Invalid);
    }
    let mut generation = 0;
    let disk = DISKS
        .iter()
        .find(|disk| {
            let mut state = disk.state.lock();
            if state.registered && !state.reclaimable() {
                return false;
            }
            // The whole slot is made over under this one acquisition, so no
            // one sees it half new: a second registration finds it taken,
            // and a holder of the old disk finds a new generation.
            if state.bounce.len() != MAX_REQUEST_BYTES {
                state.bounce.resize(MAX_REQUEST_BYTES, 0);
            }
            state.generation += 1;
            generation = state.generation;
            state.registered = true;
            state.alive = true;
            state.owner = owner;
            state.sectors = sectors;
            state.writable = writable;
            state.busy = false;
            state.phase = super::Phase::Idle;
            state.taken_at = 0;
            state.timeouts = 0;
            state.scanned = false;
            state.in_use = false;
            state.claim = None;
            state.stats = Stats::default();
            // Tags carry the slot, so one disk's tag never matches
            // another's, and they only ever count up: a late completion of
            // an earlier generation's request names no request in flight.
            if state.next_tag == 0 {
                state.next_tag = (disk.index as u64) << 56;
            }
            // Nothing of a disk that used this slot before may be served.
            disk.cache.lock().clear();
            true
        })
        .ok_or(ProviderError::Full)?;
    // The registry never forgets a device: the slot is already there under
    // its name once it has been used.
    let known = crate::block::device(NAMES[disk.index]).is_some();
    if !known && crate::block::register(disk).is_err() {
        disk.state.lock().alive = false;
        return Err(ProviderError::Full);
    }
    serial_println!(
        "block: {} registered by task {owner}: {sectors} sectors ({}), generation {generation}",
        NAMES[disk.index],
        if writable { "rw" } else { "ro" }
    );
    Ok(disk.index)
}

/// The handle a provider names disk `index` by: the slot in the low byte and
/// the generation above it. Every provider operation (`NEXT`, `COMPLETE`,
/// `REMOVE`) takes it, so a provider that still holds the handle of a disk
/// that has died cannot serve, complete or remove the disk its slot was
/// given to next, even when both are the same task (`usbd` registers the
/// next stick before it has noticed the old one is gone).
pub fn handle(index: usize) -> u64 {
    let generation = DISKS.get(index).map_or(0, |disk| disk.state.lock().generation);
    generation << 8 | index as u64
}

/// The slot `handle` names, if the disk it was made for still holds it.
pub fn resolve(handle: u64) -> Result<usize, ProviderError> {
    let index = (handle & 0xFF) as usize;
    let disk = DISKS.get(index).ok_or(ProviderError::NotOwner)?;
    let state = disk.state.lock();
    if state.registered && state.generation == handle >> 8 {
        Ok(index)
    } else {
        Err(ProviderError::NotOwner)
    }
}

/// Every live disk.
pub fn list() -> Vec<DiskInfo> {
    DISKS
        .iter()
        .filter_map(|disk| {
            let state = disk.state.lock();
            (state.registered && state.alive).then_some(DiskInfo {
                id: disk.index,
                generation: state.generation,
                sectors: state.sectors,
                writable: state.writable,
                claimed: state.claim.is_some() || state.in_use,
            })
        })
        .collect()
}

/// Take the exclusive claim on disk `id` as generation `generation` for task
/// `owner`.
pub fn claim(id: usize, generation: u64, owner: usize) -> Result<(), RawError> {
    let disk = disk(id).ok_or(RawError::NoDisk)?;
    let mut state = disk.state.lock();
    if !state.alive || state.generation != generation {
        return Err(RawError::Stale);
    }
    // A volume the kernel mounted through this disk is the kernel's: a
    // daemon reading it raw would see every user's files, and one writing it
    // would corrupt a mounted filesystem.
    if state.in_use || state.claim.is_some() {
        return Err(RawError::Busy);
    }
    state.claim = Some(owner);
    drop(state);
    serial_println!("block: {} claimed by task {owner}", NAMES[id]);
    Ok(())
}

/// The kernel mounted a volume of the device `name` (`usb<n>` or its
/// partition `usb<n>p<k>`): pin its disk (see `State::in_use`).
pub fn mark_in_use(name: &str) {
    let Some(rest) = name.strip_prefix("usb") else {
        return;
    };
    let digits: usize = rest.chars().take_while(char::is_ascii_digit).count();
    let Ok(index) = rest[..digits].parse::<usize>() else {
        return;
    };
    if let Some(disk) = DISKS.get(index) {
        let mut state = disk.state.lock();
        state.in_use = true;
        state.scanned = true;
    }
}

/// Give the claim on disk `id` up.
pub fn release(id: usize, owner: usize) -> Result<(), RawError> {
    let disk = disk(id).ok_or(RawError::NoDisk)?;
    let mut state = disk.state.lock();
    if state.claim != Some(owner) {
        return Err(RawError::NotOwner);
    }
    state.claim = None;
    Ok(())
}

/// Task `slot` is gone: every claim it held is dropped (`teardown_task`).
pub(super) fn release_task(slot: usize) {
    for disk in DISKS.iter() {
        let mut state = disk.state.lock();
        if state.claim == Some(slot) {
            state.claim = None;
        }
    }
}

/// The disk `owner` holds the claim on, which must be alive.
fn claimed(id: usize, owner: usize) -> Result<&'static super::UserDisk, RawError> {
    let disk = disk(id).ok_or(RawError::NoDisk)?;
    let state = disk.state.lock();
    if state.claim != Some(owner) {
        return Err(RawError::NotOwner);
    }
    if !state.alive {
        return Err(RawError::Gone);
    }
    drop(state);
    Ok(disk)
}

fn raw_error(disk: &super::UserDisk, error: BlockError) -> RawError {
    match error {
        BlockError::Bounds | BlockError::Unsupported => RawError::Range,
        BlockError::ReadOnly => RawError::ReadOnly,
        BlockError::Io | BlockError::Exists | BlockError::Full => {
            if disk.state.lock().alive {
                RawError::Io
            } else {
                RawError::Gone
            }
        }
    }
}

fn check_len(bytes: usize) -> Result<(), RawError> {
    if bytes <= MAX_REQUEST_BYTES && bytes.is_multiple_of(SECTOR_SIZE) {
        Ok(())
    } else {
        Err(RawError::Range)
    }
}

/// Read whole sectors from disk `id` (claimed by `owner`) at `lba`.
pub fn raw_read(id: usize, owner: usize, lba: u64, buf: &mut [u8]) -> Result<(), RawError> {
    check_len(buf.len())?;
    let disk = claimed(id, owner)?;
    disk.read_sectors(lba, buf).map_err(|e| raw_error(disk, e))
}

/// Write whole sectors to disk `id` (claimed by `owner`) at `lba`.
pub fn raw_write(id: usize, owner: usize, lba: u64, data: &[u8]) -> Result<(), RawError> {
    check_len(data.len())?;
    let disk = claimed(id, owner)?;
    disk.write_sectors(lba, data).map_err(|e| raw_error(disk, e))
}

/// Flush the device's write cache.
pub fn raw_flush(id: usize, owner: usize) -> Result<(), RawError> {
    let disk = claimed(id, owner)?;
    BlockDevice::flush(disk).map_err(|e| raw_error(disk, e))
}

/// Read `buf` at `lba` as an operation that started on generation
/// `generation` of disk `id` (the suite's stand-in for a holder from before
/// an unplug): it must fail once the slot holds another disk.
#[cfg(lazyos_tests)]
pub fn read_as_generation(
    id: usize,
    generation: u64,
    lba: u64,
    buf: &mut [u8],
) -> Result<(), BlockError> {
    let disk = disk(id).ok_or(BlockError::Io)?;
    disk.transact(generation, super::Op::Read, lba, Some(buf), None)
}

/// [`read_as_generation`], for a write.
#[cfg(lazyos_tests)]
pub fn write_as_generation(
    id: usize,
    generation: u64,
    lba: u64,
    data: &[u8],
) -> Result<(), BlockError> {
    let disk = disk(id).ok_or(BlockError::Io)?;
    disk.transact(generation, super::Op::Write, lba, None, Some(data))
}
