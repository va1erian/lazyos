//! The device syscall (23, issue #240; `kernel/src/dev/syscall.rs`). The
//! native runtime's `user::dev` builds the driver API (claim, map, PIO,
//! config space, interrupts) on [`dev_syscall`]; the read-only inspection
//! ops (issue #481) are typed here (feature `devinspect`), because both
//! `devctl` and the Devices app read them.

use crate::nr;

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
pub use inspect::{denials, inventory, policy};

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
        let (words, count) = read_all(devinspect::op::INVENTORY, INVENTORY_WORDS)?;
        Ok(devinspect::rows::<INVENTORY_WORDS>(&words, count)
            .map(|row| Device::from_words(&row))
            .collect())
    }

    /// The installed class rules; `Ok(None)` before the kernel installed them.
    pub fn policy() -> Result<Option<Vec<Rule>>, i64> {
        match read_all(devinspect::op::POLICY, RULE_WORDS) {
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
        let (words, count) = read_all(devinspect::op::DENIALS, DENIAL_WORDS)?;
        Ok(devinspect::rows::<DENIAL_WORDS>(&words, count)
            .map(|row| Denial::from_words(&row))
            .collect())
    }
}
