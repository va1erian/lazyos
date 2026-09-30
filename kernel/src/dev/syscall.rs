//! The device syscall, number 23 (issue #240, driver-plan section 3.2).
//!
//! Userspace drivers reach hardware only through a `Device` handle. The handle
//! is minted by `claim`, lives in the caller's own handle table, cannot be
//! duplicated or transferred, and dies with `release` or the task. Registers:
//!
//! ```text
//!   rax = 23  rdi = op  rsi = a1  rdx = a2  r10 = a3  r8 = a4   -> value | -errno
//!   LIST(0)        a1 -> rows, a2 = capacity in rows   -> total rows
//!   CLAIM(1)       a1 = device id, a2 = irq endpoint handle (or !0),
//!                  a3 = flags (bit 0: share the interrupt line) -> handle
//!   MAP_BAR(2)     a1 = handle, a2 = BAR                -> user virtual address
//!   PIO(3)         a1 = handle, a2 = BAR, a3 = offset,
//!                  a4 = width | write << 8 | value << 32 -> value read
//!   CFG_READ(4)    a1 = handle, a2 = offset, a3 = width -> value
//!   CFG_WRITE(5)   a1 = handle, a2 = offset, a3 = width, a4 = value
//!   IRQ_ENABLE(6)  a1 = handle, a2 = 0
//!   IRQ_ACK(7)     a1 = handle, a2 = 0
//!   RELEASE(8)     a1 = handle
//!   DMA_ALLOC(9)   a1 = handle, a2 = length in bytes, a3 = flags
//!                  (bit 0 share-only, bit 1 64-bit address OK), a4 -> u64
//!                  bus address   -> Buffer handle
//! ```
//!
//! No operation takes a raw physical or port address: everything is resolved
//! from the device's own enumerated resources. See [`super::ops`] for the
//! resource operations and [`super::teardown`] for release.

use alloc::vec::Vec;
use core::mem::size_of;

use crate::ipc::channels;
use crate::ipc::credentials::{self, CAP_DEV_CLAIM};
use crate::ipc::handles::{self, rights, HandleKind};
use crate::quota::{self, Resource};
use crate::user_ptr;

use super::claims::{self, Claim, InstallError, IrqBinding, CLAIMS, MAX_DMA_BUFFERS};
use super::class::{class_of, method, DEV_INTERFACE};
use super::errno::*;
use super::grant;
use super::irq;
use super::pci::{self, COMMAND_BUS_MASTER, COMMAND_INTX_DISABLE, COMMAND_IO, COMMAND_MEMORY};
use super::report::{self, reason};
use super::resources::MAX_BARS;
use super::{dma, intx, ops, table, teardown, BarKind, BusId, DeviceId, DeviceInfo, TaskSlot};

pub const OP_LIST: u64 = 0;
pub const OP_CLAIM: u64 = 1;
pub const OP_MAP_BAR: u64 = 2;
pub const OP_PIO: u64 = 3;
pub const OP_CFG_READ: u64 = 4;
pub const OP_CFG_WRITE: u64 = 5;
pub const OP_IRQ_ENABLE: u64 = 6;
pub const OP_IRQ_ACK: u64 = 7;
pub const OP_RELEASE: u64 = 8;
pub const OP_DMA_ALLOC: u64 = 9;

/// `claim` flag: the claimant accepts sharing its interrupt line.
pub const FLAG_SHARED_IRQ: u64 = 1;
/// `claim` endpoint argument meaning "no interrupt endpoint" (a polling driver).
pub const NO_ENDPOINT: u64 = u64::MAX;

/// `u64` words per `list` row.
pub const ROW_WORDS: usize = 13;
/// `list` flag bits (word 4).
pub mod row_flag {
    pub const OWNED: u64 = 1 << 0;
    pub const PCI: u64 = 1 << 1;
    pub const IRQ_ROUTABLE: u64 = 1 << 2;
}

/// The syscall entry: run `op` for the current task and encode the result as
/// the value or `-errno`.
pub fn dispatch(op: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let slot = crate::task::current();
    let result = match op {
        OP_LIST => list(slot, a1, a2),
        OP_CLAIM => claim(slot, a1, a2, a3),
        OP_MAP_BAR => with_handle(slot, a1, rights::DEV_MMIO, |r| ops::map_bar(r, a2)),
        OP_PIO => with_handle(slot, a1, rights::DEV_PIO, |r| ops::pio(r, a2, a3, a4)),
        OP_CFG_READ => with_handle(slot, a1, rights::DEV_CONFIG, |r| ops::cfg_read(r, a2, a3)),
        OP_CFG_WRITE => with_handle(slot, a1, rights::DEV_CONFIG, |r| {
            ops::cfg_write(r, a2, a3, a4)
        }),
        OP_IRQ_ENABLE => with_handle(slot, a1, rights::DEV_IRQ, |r| irq_enable(r, a2)),
        OP_IRQ_ACK => with_handle(slot, a1, rights::DEV_IRQ, |r| irq_ack(r, a2)),
        OP_RELEASE => release(slot, a1),
        OP_DMA_ALLOC => with_handle(slot, a1, rights::DEV_DMA, |r| dma::dma_alloc(r, a2, a3, a4)),
        _ => Err(EINVAL),
    };
    match result {
        Ok(value) => value,
        Err(errno) => (-errno) as u64,
    }
}

/// A `Device` handle resolved against the calling task, the device table and
/// the claim table, all of which must agree.
pub struct Resolved {
    pub id: DeviceId,
    pub info: DeviceInfo,
    pub claim: Claim,
    /// Handle rights AND claim rights.
    pub rights: u32,
}

/// Resolve `handle` for task `slot`, requiring `need` (0 for none).
///
/// The generation is checked on every call, so a handle from an earlier claim,
/// another task, or a released device fails with `EBADF`; a valid handle that
/// lacks the right fails with `EPERM`.
pub fn resolve(slot: usize, handle: u64, need: u32) -> Result<Resolved, Errno> {
    let entry = handles::get(handle).map_err(|_| EBADF)?;
    if entry.kind != HandleKind::Device {
        return Err(EBADF);
    }
    let (id, generation) = claims::split_object_id(entry.object_id);
    let (info, owner, current) = {
        let devices = table().lock();
        (devices.get(id), devices.owner(id), devices.generation(id))
    };
    let info = info.ok_or(EBADF)?;
    if owner != Some(TaskSlot(slot)) || current != Some(generation) {
        return Err(EBADF);
    }
    let claim = CLAIMS.lock().get(id).copied().ok_or(EBADF)?;
    if claim.generation != generation || claim.owner != slot || claim.handle != handle {
        return Err(EBADF);
    }
    let effective = entry.rights & claim.rights;
    if need & !effective != 0 {
        return Err(EPERM);
    }
    Ok(Resolved {
        id,
        info,
        claim,
        rights: effective,
    })
}

fn with_handle(
    slot: usize,
    handle: u64,
    need: u32,
    op: impl FnOnce(&Resolved) -> Result<u64, Errno>,
) -> Result<u64, Errno> {
    let resolved = resolve(slot, handle, need)?;
    op(&resolved)
}

/// Encode one device as a `list` row (see [`ROW_WORDS`]). Physical BAR bases
/// are deliberately absent: a driver needs sizes and kinds, and maps by index.
fn row(info: &DeviceInfo, owned: bool, generation: u32) -> [u64; ROW_WORDS] {
    let mut words = [0u64; ROW_WORDS];
    words[0] = u64::from(info.id.0);
    words[1] = u64::from(info.vendor)
        | u64::from(info.device) << 16
        | u64::from(info.subsystem_vendor) << 32
        | u64::from(info.subsystem_device) << 48;
    words[2] = u64::from(info.class)
        | u64::from(info.subclass) << 8
        | u64::from(info.prog_if) << 16
        | u64::from(info.revision) << 24;
    words[3] = class_of(info).interface_id;
    let line = info.resources.irq().map_or(0xFF, |irq| irq.line);
    let mut flags = 0;
    if owned {
        flags |= row_flag::OWNED;
    }
    if matches!(info.bus, BusId::Pci(_)) {
        flags |= row_flag::PCI;
    }
    if irq::routable(line) {
        flags |= row_flag::IRQ_ROUTABLE;
    }
    words[4] = flags | u64::from(line) << 8;
    words[5] = u64::from(generation);
    for bar in info.resources.bars() {
        let index = usize::from(bar.index);
        let mut meta = 1u64;
        if bar.kind == BarKind::Io {
            meta |= 2;
        }
        if bar.is_64 {
            meta |= 4;
        }
        if bar.prefetchable {
            meta |= 8;
        }
        words[6] |= meta << (index * 4);
        words[7 + index] = bar.len;
    }
    words
}

/// `list(buf, capacity)`: copy device rows to the caller and return the total
/// number of devices (so a short buffer can be retried larger).
fn list(slot: usize, buf: u64, capacity: u64) -> Result<u64, Errno> {
    if credentials::of(slot).caps & CAP_DEV_CLAIM == 0 {
        return Err(EPERM);
    }
    if acl_denies(slot, DEV_INTERFACE, method::LIST) {
        return Err(EACCES);
    }
    let rows: Vec<[u64; ROW_WORDS]> = {
        let devices = table().lock();
        devices
            .iter()
            .map(|info| {
                row(
                    &info,
                    devices.owner(info.id).is_some(),
                    devices.generation(info.id).unwrap_or(0),
                )
            })
            .collect()
    };
    let count = rows
        .len()
        .min(usize::try_from(capacity).unwrap_or(usize::MAX));
    if count > 0 {
        let words: Vec<u64> = rows[..count].iter().flatten().copied().collect();
        debug_assert_eq!(words.len() * size_of::<u64>(), count * ROW_WORDS * 8);
        user_ptr::try_copy_words(buf, &words).map_err(|_| EFAULT)?;
    }
    Ok(rows.len() as u64)
}

/// Whether the ACL refuses `interface`/`method` for `slot` (recording the
/// denial, as every Messenger decision does).
fn acl_denies(slot: usize, interface: u64, method: u32) -> bool {
    crate::ipc::authorize(slot, interface, method, 0).denied()
}

/// Refuse a claim, recording why. `info` is `None` when the device does not
/// exist (nothing to audit against).
fn deny(slot: usize, info: Option<&DeviceInfo>, why: u32, errno: Errno) -> Errno {
    if let Some(info) = info {
        report::record(slot, info, method::CLAIM, false, why);
    }
    errno
}

/// Give back what a half-finished claim took, in reverse order.
struct ClaimUndo {
    generation: u32,
    id: DeviceId,
    charged_uid: Option<u32>,
    handle: Option<u64>,
}

impl ClaimUndo {
    fn run(self) {
        if let Some(handle) = self.handle {
            let _ = handles::close(handle);
        }
        if let Some(uid) = self.charged_uid {
            quota::release(uid, Resource::DeviceClaims, 1);
        }
        let _ = table().lock().release_generation(self.id, self.generation);
    }
}

/// `claim(id, endpoint, flags)`: take ownership of a device.
///
/// Order matters for failure atomicity: every check that can fail without side
/// effects runs first; ownership, quota and the handle follow, each undone if a
/// later step fails; the claim record is installed last (it enforces the shared
/// interrupt-line contract); only then is the device quiesced.
fn claim(slot: usize, id_raw: u64, endpoint: u64, flags: u64) -> Result<u64, Errno> {
    let cred = credentials::of(slot);
    let id = u16::try_from(id_raw).map(DeviceId).map_err(|_| ENODEV)?;
    let found = table().lock().get(id);
    if slot == crate::task::KERNEL_TASK || cred.caps & CAP_DEV_CLAIM == 0 {
        return Err(deny(slot, found.as_ref(), reason::NO_CAP, EPERM));
    }
    let info = found.ok_or(ENODEV)?;
    if flags & !FLAG_SHARED_IRQ != 0 || (flags & FLAG_SHARED_IRQ != 0 && endpoint == NO_ENDPOINT) {
        return Err(EINVAL);
    }

    // Policy: the class-specific interface id, so a rule for one class says
    // nothing about another (plan section 3.5). A denial is recorded by
    // `authorize`.
    let class = class_of(&info);
    let verdict = crate::ipc::authorize(
        slot,
        class.interface_id,
        method::CLAIM,
        report::correlation(id),
    );
    if verdict.denied() {
        return Err(EACCES);
    }
    let granted = grant::resource_rights(&info) & grant::policy_rights(&cred, class);
    if granted == 0 {
        return Err(deny(slot, Some(&info), reason::NO_RIGHTS, EPERM));
    }

    let binding = if endpoint == NO_ENDPOINT {
        None
    } else {
        if granted & rights::DEV_IRQ == 0 {
            return Err(deny(slot, Some(&info), reason::NO_RIGHTS, EPERM));
        }
        let (channel, side) = channels::private_endpoint_of_task(slot, endpoint)
            .map_err(|_| deny(slot, Some(&info), reason::BAD_ENDPOINT, EBADF))?;
        Some(IrqBinding {
            channel,
            side,
            shared: flags & FLAG_SHARED_IRQ != 0,
        })
    };
    let line = info
        .resources
        .irq()
        .map(|irq| irq.line)
        .filter(|&line| irq::routable(line));

    let minted = table().lock().claim(id, TaskSlot(slot));
    let device = minted.map_err(|_| deny(slot, Some(&info), reason::BUSY, EBUSY))?;
    let mut undo = ClaimUndo {
        generation: device.generation(),
        id,
        charged_uid: None,
        handle: None,
    };
    if quota::charge(cred.uid, Resource::DeviceClaims, 1).is_err() {
        undo.run();
        return Err(deny(slot, Some(&info), reason::QUOTA, EDQUOT));
    }
    undo.charged_uid = Some(cred.uid);
    let object = claims::object_id(id, device.generation());
    let handle = match handles::open(HandleKind::Device, granted, object) {
        Ok(handle) => handle,
        Err(_) => {
            undo.run();
            return Err(EMFILE);
        }
    };
    undo.handle = Some(handle);
    let record = Claim {
        owner: slot,
        uid: cred.uid,
        class,
        generation: device.generation(),
        rights: granted,
        handle,
        line,
        irq: binding,
        armed: false,
        pending: false,
        missed: false,
        maps: [None; MAX_BARS],
        dma: [None; MAX_DMA_BUFFERS],
    };
    let installed = CLAIMS.lock().install(id, record);
    if let Err(error) = installed {
        undo.run();
        let why = match error {
            InstallError::LineBusy => reason::LINE_BUSY,
            InstallError::Occupied => reason::BUSY,
        };
        return Err(deny(slot, Some(&info), why, EBUSY));
    }
    if binding.is_some() {
        channels::seal_endpoint(slot, endpoint);
    }
    quiesce(&info);
    report::record(
        slot,
        &info,
        method::CLAIM,
        true,
        reason::CLAIMED | granted << 8,
    );
    Ok(handle)
}

/// Put a freshly claimed (or just released) PCI function in a known, silent
/// state: memory/I/O decode and bus mastering off, INTx disabled. The driver
/// switches on only what it uses.
pub(super) fn quiesce(info: &DeviceInfo) {
    // Test-only ordering marker: bus mastering is being turned off, which must
    // precede any of this device's DMA frames returning to the pool.
    #[cfg(lazyos_tests)]
    crate::mem::dma::order::note(crate::mem::dma::order::QUIESCE);
    if let BusId::Pci(address) = info.bus {
        let command = pci::command(address);
        let quiet =
            (command & !(COMMAND_IO | COMMAND_MEMORY | COMMAND_BUS_MASTER)) | COMMAND_INTX_DISABLE;
        if quiet != command {
            pci::write_command(address, quiet);
        }
    }
}

/// `irq_enable(handle, 0)`: join interrupt delivery. Returns `ENOSYS` when the
/// device's line cannot be routed through the PIC (the driver then polls).
fn irq_enable(r: &Resolved, index: u64) -> Result<u64, Errno> {
    if index != 0 {
        return Err(EINVAL);
    }
    intx::arm(r.id)?;
    // The claim now listens: let the device assert its line.
    if let BusId::Pci(address) = r.info.bus {
        pci::clear_command(address, COMMAND_INTX_DISABLE);
    }
    Ok(0)
}

/// `irq_ack(handle, 0)`: acknowledge the interrupt message just serviced.
fn irq_ack(r: &Resolved, index: u64) -> Result<u64, Errno> {
    if index != 0 {
        return Err(EINVAL);
    }
    intx::ack(r.id)?;
    Ok(0)
}

/// `release(handle)`: quiesce the device, undo every mapping and return the
/// claim to the pool. The handle is closed.
fn release(slot: usize, handle: u64) -> Result<u64, Errno> {
    let resolved = resolve(slot, handle, 0)?;
    teardown::release_claim(
        resolved.id,
        slot,
        reason::RELEASED,
        ops::current_table().as_u64(),
    );
    let _ = handles::close(handle);
    Ok(0)
}

/// Number of live userspace claims (the boot self-check expects none).
pub(super) fn claim_count() -> usize {
    CLAIMS.lock().len()
}
