//! The `list` op of the device syscall (issue #240): every enumerated device
//! as one fixed-size row, for a driver deciding what to claim.
//!
//! Split out of `syscall.rs` (the file-length rule); the op numbers and the
//! row layout constants stay there, next to the dispatch.

use alloc::vec::Vec;
use core::mem::size_of;

use crate::ipc::credentials::{self, CAP_DEV_CLAIM};
use crate::user_ptr;

use super::class::{class_of, method, DEV_INTERFACE};
use super::errno::*;
use super::irq;
use super::syscall::{row_flag, ROW_WORDS};
use super::{table, BarKind, BusId, DeviceInfo};

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
    if info.resources.msi().is_some() {
        flags |= row_flag::MSI;
    }
    if info.resources.msix().is_some() {
        flags |= row_flag::MSIX;
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
pub(super) fn list(slot: usize, buf: u64, capacity: u64) -> Result<u64, Errno> {
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
