//! The fixed-capacity device table (issue #239).
//!
//! Enumeration fills a compile-time array; there is no per-device heap
//! allocation and no growth on the hot path. Each slot remembers its `owner`
//! and a `generation` so a handle minted by an earlier claim fails closed
//! instead of acting on a device that has since been released or reused.
//! `DeviceTable::new` is `const`, so the boot table is a plain `static`.

use super::{DeviceId, DeviceInfo, TaskSlot};

/// How many devices the table can hold. Enough for the QEMU q35/`i440fx`
/// device set plus the platform seeds, with headroom.
pub const MAX_DEVICES: usize = 32;

/// Device-core failures. The userspace `dev_*` syscall (D3) maps these to
/// `errno`s; the kernel uses them directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DevError {
    /// The table has no free slot.
    Full,
    /// No device with that id.
    NotFound,
    /// The device already has an owner (double-claim).
    Busy,
    /// The handle's generation is no longer current.
    Stale,
    /// The device is unowned, so there is nothing to release.
    NotOwned,
    /// No driver accepted the device.
    NoDriver,
    /// Hardware-level failure during an operation.
    Io,
    /// The operation is not supported by this device.
    Unsupported,
}

/// A claim handle. It is only valid for the table generation it was minted
/// against; `release` checks that before touching the slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceHandle {
    id: DeviceId,
    generation: u32,
}

impl DeviceHandle {
    pub fn id(self) -> DeviceId {
        self.id
    }

    pub fn generation(self) -> u32 {
        self.generation
    }

    /// Test-only: mint a handle at a chosen generation so the suite can prove
    /// stale rejection without threading a real claim through.
    #[cfg(lazyos_tests)]
    pub fn legacy_for_test(id: DeviceId, generation: u32) -> DeviceHandle {
        DeviceHandle { id, generation }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    info: DeviceInfo,
    owner: Option<TaskSlot>,
    generation: u32,
}

/// The device table. `const`-constructible so it can live in a `static`.
pub struct DeviceTable {
    entries: [Option<Entry>; MAX_DEVICES],
}

impl DeviceTable {
    pub const fn new() -> DeviceTable {
        DeviceTable {
            entries: [None; MAX_DEVICES],
        }
    }

    /// Add `info`, assigning it the next free [`DeviceId`] (its own `id` field
    /// is overwritten with the slot index) and generation 0.
    pub fn insert(&mut self, mut info: DeviceInfo) -> Result<DeviceId, DevError> {
        for (index, slot) in self.entries.iter_mut().enumerate() {
            if slot.is_none() {
                let id = DeviceId(index as u16);
                info.id = id;
                *slot = Some(Entry {
                    info,
                    owner: None,
                    generation: 0,
                });
                return Ok(id);
            }
        }
        Err(DevError::Full)
    }

    /// The device's info, if the id names a present slot.
    pub fn get(&self, id: DeviceId) -> Option<DeviceInfo> {
        self.entry(id).map(|entry| entry.info)
    }

    /// The current owner, if any.
    pub fn owner(&self, id: DeviceId) -> Option<TaskSlot> {
        self.entry(id).and_then(|entry| entry.owner)
    }

    /// The current generation, if the id is present.
    pub fn generation(&self, id: DeviceId) -> Option<u32> {
        self.entry(id).map(|entry| entry.generation)
    }

    /// Number of present devices.
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|slot| slot.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of devices that currently have an owner.
    pub fn owned(&self) -> usize {
        self.entries
            .iter()
            .flatten()
            .filter(|entry| entry.owner.is_some())
            .count()
    }

    /// Every present device, in id order.
    pub fn iter(&self) -> impl Iterator<Item = DeviceInfo> + '_ {
        self.entries.iter().flatten().map(|entry| entry.info)
    }

    /// Claim `id` for `owner`. Fails with [`DevError::Busy`] when it is already
    /// owned (double-claim) and [`DevError::NotFound`] for an unknown id.
    pub fn claim(&mut self, id: DeviceId, owner: TaskSlot) -> Result<DeviceHandle, DevError> {
        let entry = self.entry_mut(id).ok_or(DevError::NotFound)?;
        if entry.owner.is_some() {
            return Err(DevError::Busy);
        }
        entry.owner = Some(owner);
        Ok(DeviceHandle {
            id,
            generation: entry.generation,
        })
    }

    /// Release a claim. The handle must still match the slot's generation
    /// ([`DevError::Stale`] otherwise) and the slot must be owned
    /// ([`DevError::NotOwned`] otherwise). On success the owner is cleared and
    /// the generation bumped, invalidating every older handle.
    pub fn release(&mut self, handle: DeviceHandle) -> Result<(), DevError> {
        let entry = self.entry_mut(handle.id).ok_or(DevError::NotFound)?;
        if entry.generation != handle.generation {
            return Err(DevError::Stale);
        }
        if entry.owner.is_none() {
            return Err(DevError::NotOwned);
        }
        entry.owner = None;
        entry.generation = entry.generation.wrapping_add(1);
        Ok(())
    }

    fn entry(&self, id: DeviceId) -> Option<&Entry> {
        self.entries.get(id.0 as usize)?.as_ref()
    }

    fn entry_mut(&mut self, id: DeviceId) -> Option<&mut Entry> {
        self.entries.get_mut(id.0 as usize)?.as_mut()
    }
}

impl Default for DeviceTable {
    fn default() -> Self {
        Self::new()
    }
}
