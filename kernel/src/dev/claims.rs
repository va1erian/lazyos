//! Userspace device claims (issue #240).
//!
//! The device table (`dev::table`) records *who owns* a device and its
//! generation. This module holds the rest of a userspace claim: the rights it
//! was granted, the `Device` handle number, the interrupt endpoint and its
//! delivery state, and the BAR mappings that must be undone on release. One
//! claim per device, so the device id doubles as the claim's index and as its
//! bit in the per-line delivery masks.
//!
//! Everything here is protected by the single [`CLAIMS`] lock and is only ever
//! touched from task context. The interrupt handler never looks at it (see
//! `dev::irq`): it must not take a lock the interrupted code may hold.
//! Lock order: `CLAIMS` is never held across a call into another subsystem
//! (channels, handles, quota, audit, the frame allocator); callers copy what
//! they need out, drop the guard, then call.

use spin::Mutex;

use super::class::Class;
use super::resources::MAX_BARS;
use super::table::MAX_DEVICES;
use super::DeviceId;

/// One bit per device id: the claimants a delivery round waits on.
pub type ClaimMask = u128;

const _: () = assert!(
    MAX_DEVICES <= ClaimMask::BITS as usize,
    "claim bitmasks must hold every device id"
);

/// How many live DMA buffers one claim may hold (issue #241). A fixed bound,
/// like [`MAX_BARS`], keeps the claim `Copy` and heap-free.
pub const MAX_DMA_BUFFERS: usize = 16;

/// Delivery sources a round can run on: the legacy lines, then the MSI
/// vectors (`dev::msi`).
pub const SOURCES: usize = super::irq::SOURCES as usize;

/// One BAR mapped into a claimant's address space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mapping {
    /// The PML4 the mapping lives in (the claimant's address space).
    pub table: u64,
    /// User virtual address of the first page.
    pub va: u64,
    /// Physical base of the BAR.
    pub phys: u64,
    /// Length in 4 KiB pages.
    pub pages: u64,
}

/// Where a claim's interrupt notifications go: the inbox of `side` of
/// `channel`, which is the channel the claimant named at `claim`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IrqBinding {
    pub channel: u64,
    pub side: usize,
    /// The claimant opted in to sharing its interrupt line.
    pub shared: bool,
}

/// One live DMA buffer of a claim (issue #241). The buffer is closed by
/// *object id* on release, because a transferred handle number may since have
/// been reused for another object in the same task table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DmaRecord {
    /// Kernel buffer object id (`Buffer::object_id`).
    pub object_id: u64,
    /// Run length in 4 KiB pages.
    pub pages: u64,
    /// Physical address of the first page of the run.
    pub base: u64,
    /// The last reference dropped while the claim was live and the device may
    /// still write the run: the pool return and the `DmaMemory` charge wait for
    /// `release_claim`, which frees them once bus mastering is off.
    pub quarantined: bool,
}

/// One live claim.
#[derive(Clone, Copy)]
pub struct Claim {
    /// Task slot of the owner.
    pub owner: usize,
    /// The owner's uid at claim time: quota is released against it.
    pub uid: u32,
    pub class: &'static Class,
    /// Table generation this claim was minted at.
    pub generation: u32,
    /// Rights fixed at claim time (a `Device` handle can only narrow them).
    pub rights: u32,
    /// The `Device` handle number in the owner's table.
    pub handle: u64,
    /// The legacy line the device's INTx can be delivered on, if routable.
    pub line: Option<u8>,
    /// The MSI vector (index into `dev::msi`) `irq_enable` routed the claim
    /// to; while set it replaces `line` as the claim's delivery source.
    pub msi: Option<u8>,
    pub irq: Option<IrqBinding>,
    /// `irq_enable` was called: the claim takes part in delivery rounds.
    pub armed: bool,
    /// One notification was sent and not yet acknowledged (the claim is "owed"
    /// an ack; no further message is posted until it arrives).
    pub pending: bool,
    /// An interrupt arrived while `pending`: post one fresh message once the
    /// late ack comes in.
    pub missed: bool,
    pub maps: [Option<Mapping>; MAX_BARS],
    /// Live DMA buffers this claim allocated, for release/teardown.
    pub dma: [Option<DmaRecord>; MAX_DMA_BUFFERS],
}

impl Claim {
    /// Where the claim's interrupts arrive: its MSI vector's source once it
    /// has one, else its INTx line.
    pub fn source(&self) -> Option<u8> {
        self.msi
            .map(|index| super::msi::SOURCE_BASE + index)
            .or(self.line)
    }

    /// Record a freshly allocated DMA buffer. Returns false (and changes
    /// nothing) when the per-claim bound is reached.
    pub fn record_dma(&mut self, record: DmaRecord) -> bool {
        match self.dma.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => {
                *slot = Some(record);
                true
            }
            None => false,
        }
    }

    /// Forget the DMA record for `object_id` (the buffer is already gone).
    pub fn forget_dma(&mut self, object_id: u64) {
        for slot in self.dma.iter_mut() {
            if slot.is_some_and(|record| record.object_id == object_id) {
                *slot = None;
            }
        }
    }
}

/// One delivery round on a shared line: the claimants that were sent a message
/// and have not yet acked, and the tick after which they are dropped.
#[derive(Clone, Copy)]
pub struct Round {
    pub waiting: ClaimMask,
    pub deadline: u64,
}

/// Why [`Claims::install`] refused a claim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallError {
    /// The slot is already occupied (a bug: the device table gates this).
    Occupied,
    /// The interrupt line is held exclusively, or the claim asked for
    /// exclusive use of a line another claim already listens on.
    LineBusy,
}

/// All live claims plus the per-line delivery rounds.
pub struct Claims {
    pub(super) slots: [Option<Claim>; MAX_DEVICES],
    pub(super) rounds: [Round; SOURCES],
}

impl Claims {
    pub const fn new() -> Claims {
        Claims {
            slots: [None; MAX_DEVICES],
            rounds: [Round {
                waiting: 0,
                deadline: 0,
            }; SOURCES],
        }
    }

    pub fn get(&self, id: DeviceId) -> Option<&Claim> {
        self.slots.get(usize::from(id.0))?.as_ref()
    }

    pub fn get_mut(&mut self, id: DeviceId) -> Option<&mut Claim> {
        self.slots.get_mut(usize::from(id.0))?.as_mut()
    }

    /// Number of live claims.
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Ids of every claim owned by task `slot`.
    pub fn owned_by(&self, slot: usize) -> ([Option<DeviceId>; MAX_DEVICES], usize) {
        let mut ids = [None; MAX_DEVICES];
        let mut count = 0;
        for (index, claim) in self.slots.iter().enumerate() {
            if claim.is_some_and(|claim| claim.owner == slot) {
                ids[count] = Some(DeviceId(index as u16));
                count += 1;
            }
        }
        (ids, count)
    }

    /// Record `claim` for `id`, enforcing the interrupt-line contract: a line
    /// several claims listen on must have been opted into sharing by every one
    /// of them, so a claim that did not opt in gets the line exclusively.
    pub fn install(&mut self, id: DeviceId, claim: Claim) -> Result<(), InstallError> {
        let index = usize::from(id.0);
        if self.slots.get(index).is_none_or(|slot| slot.is_some()) {
            return Err(InstallError::Occupied);
        }
        if let (Some(line), Some(binding)) = (claim.line, claim.irq) {
            let conflict = self.slots.iter().flatten().any(|other| {
                other.line == Some(line)
                    && other
                        .irq
                        .is_some_and(|theirs| !theirs.shared || !binding.shared)
            });
            if conflict {
                return Err(InstallError::LineBusy);
            }
        }
        self.slots[index] = Some(claim);
        Ok(())
    }
}

impl Default for Claims {
    fn default() -> Self {
        Self::new()
    }
}

/// The claim table. Task context only; see the module docs.
pub static CLAIMS: Mutex<Claims> = Mutex::new(Claims::new());

/// The kernel-side object id a `Device` handle carries: the claim generation
/// and the device id, so a handle can never be confused with a later claim.
pub fn object_id(id: DeviceId, generation: u32) -> u64 {
    (u64::from(generation) << 16) | u64::from(id.0)
}

/// Split [`object_id`] back into `(device, generation)`.
pub fn split_object_id(object: u64) -> (DeviceId, u32) {
    (DeviceId((object & 0xFFFF) as u16), (object >> 16) as u32)
}
