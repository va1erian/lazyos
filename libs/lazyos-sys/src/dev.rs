//! The device syscall (23, issue #240; `kernel/src/dev/syscall.rs`). The
//! native runtime's `user::dev` builds the driver API (claim, map, PIO,
//! config space, interrupts) on [`dev_syscall`]; the read-only inspection
//! ops (issue #481) are typed here (feature `devinspect`), because both
//! `devctl` and the Devices app read them.

use crate::nr;

/// Operation codes, `dev::syscall::OP_*` (`tests/kernel_tables.rs` checks
/// them against the kernel).
pub mod op {
    pub const LIST: u64 = 0;
    pub const CLAIM: u64 = 1;
    pub const MAP_BAR: u64 = 2;
    pub const PIO: u64 = 3;
    pub const CFG_READ: u64 = 4;
    pub const CFG_WRITE: u64 = 5;
    pub const IRQ_ENABLE: u64 = 6;
    pub const IRQ_ACK: u64 = 7;
    pub const RELEASE: u64 = 8;
    /// Allocate contiguous DMA memory.
    pub const DMA_ALLOC: u64 = 9;
    /// Inspection: every device, its owner and rights (issue #481).
    pub const INVENTORY: u64 = 10;
    /// Inspection: the installed class rules.
    pub const POLICY: u64 = 11;
    /// Inspection: refused claims in the audit ring.
    pub const DENIALS: u64 = 12;
    /// `dbgd`'s read of configuration space, no claim (`LAZYOS_DBGD=1`
    /// kernels; others answer `-EINVAL`).
    pub const DBG_CONFIG_READ: u64 = 13;
    /// `dbgd`'s read of allowlisted MMIO ranges, no claim.
    pub const DBG_MMIO_READ: u64 = 14;
}

/// `claim`'s "no interrupt endpoint" argument (a polling driver).
pub const NO_ENDPOINT: u64 = u64::MAX;
/// `claim`'s "interrupts on a channel the kernel makes" argument (issue
/// #496): the kernel writes the channel's receive handle through `a4`.
pub const KERNEL_CHANNEL: u64 = u64::MAX - 1;
/// `claim` flag: accept sharing the interrupt line with other drivers.
pub const FLAG_SHARED_IRQ: u64 = 1;
/// `u64` words per `list` row.
pub const ROW_WORDS: usize = 13;

/// One device syscall.
///
/// # Safety
///
/// As [`crate::raw::syscall5`]: each pointer argument of `op` must be valid
/// for the kernel's access through it.
pub unsafe fn dev_syscall(op: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    // SAFETY: forwarded; the caller upholds the pointer contract.
    unsafe { crate::raw::syscall5(nr::DEV, op, a1, a2, a3, a4) }
}

#[cfg(feature = "devinspect")]
pub use inspect::{config_read, denials, inventory, mmio_read, policy};

#[cfg(feature = "devinspect")]
mod inspect {
    use alloc::vec;
    use alloc::vec::Vec;

    use devinspect::{Denial, Device, Rule, DENIAL_WORDS, INVENTORY_WORDS, RULE_WORDS};

    use crate::errno::ENOENT;

    /// Run inspection `op` until the buffer holds every row; the flat words
    /// and the row count, or the negative errno.
    fn read_all(op: u64, words_per_row: usize) -> Result<(Vec<u64>, usize), i64> {
        let mut capacity = 16;
        loop {
            let mut words = vec![0u64; capacity * words_per_row];
            // SAFETY: the kernel writes at most `capacity` rows of
            // `words_per_row` words, the size of `words`.
            let code =
                unsafe { super::dev_syscall(op, words.as_mut_ptr() as u64, capacity as u64, 0, 0) };
            let total = crate::value(code)? as usize;
            if total <= capacity {
                return Ok((words, total));
            }
            capacity = total;
        }
    }

    /// Every device, with its owner and rights.
    pub fn inventory() -> Result<Vec<Device>, i64> {
        let (words, count) = read_all(super::op::INVENTORY, INVENTORY_WORDS)?;
        Ok(devinspect::rows::<INVENTORY_WORDS>(&words, count)
            .map(|row| Device::from_words(&row))
            .collect())
    }

    /// The installed class rules; `Ok(None)` before the kernel installed them.
    pub fn policy() -> Result<Option<Vec<Rule>>, i64> {
        match read_all(super::op::POLICY, RULE_WORDS) {
            Ok((words, count)) => Ok(Some(
                devinspect::rows::<RULE_WORDS>(&words, count)
                    .map(|row| Rule::from_words(&row))
                    .collect(),
            )),
            Err(code) if code == -ENOENT => Ok(None),
            Err(code) => Err(code),
        }
    }

    /// Refused claims still in the audit ring, newest first
    /// (`CAP_AUDIT_READ`).
    pub fn denials() -> Result<Vec<Denial>, i64> {
        let (words, count) = read_all(super::op::DENIALS, DENIAL_WORDS)?;
        Ok(devinspect::rows::<DENIAL_WORDS>(&words, count)
            .map(|row| Denial::from_words(&row))
            .collect())
    }

    /// `len` bytes of configuration space of the function at `bdf`
    /// (`bus << 8 | device << 3 | function`), without claiming it. Only
    /// `dbgd` and root may ask (`kernel/src/dev/peek.rs`).
    pub fn config_read(bdf: u16, offset: u64, len: usize) -> Result<Vec<u8>, i64> {
        let mut bytes = vec![0u8; len];
        // SAFETY: the kernel writes at most `len` bytes, the size of `bytes`.
        let code = unsafe {
            super::dev_syscall(
                super::op::DBG_CONFIG_READ,
                u64::from(bdf),
                offset,
                len as u64,
                bytes.as_mut_ptr() as u64,
            )
        };
        bytes.truncate(crate::value(code)? as usize);
        Ok(bytes)
    }

    /// `len` bytes of BAR `bar` of the function at `bdf` from `offset`,
    /// inside the ranges the box allows (`libs/pciprobe`).
    pub fn mmio_read(bdf: u16, bar: u8, offset: u64, len: usize) -> Result<Vec<u8>, i64> {
        let mut bytes = vec![0u8; len];
        // SAFETY: the kernel writes at most `len` bytes, the size of `bytes`.
        let code = unsafe {
            super::dev_syscall(
                super::op::DBG_MMIO_READ,
                u64::from(bdf),
                offset,
                u64::from(bar) | (len as u64) << 8,
                bytes.as_mut_ptr() as u64,
            )
        };
        bytes.truncate(crate::value(code)? as usize);
        Ok(bytes)
    }
}
