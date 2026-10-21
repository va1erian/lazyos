//! The version of the syscall ABI this crate describes (docs/update-plan.md,
//! decision 18).
//!
//! The kernel and the programs that run on it are separate system packages,
//! so each states which ABI it provides or needs. The ABI is the native gate's
//! syscall numbers ([`crate::nr`]), the Messenger ops ([`crate::msg::op`]) and
//! the device ops ([`crate::dev::op`]). It only grows, as Linux's does: an
//! entry is never renumbered or renamed, and an entry leaves only after its
//! `removed` mark has been in the frozen table for two snapshots.
//!
//! `abi/syscalls.abi` is that table, frozen. `tests/abi_freeze.rs` fails when
//! the constants in this crate differ from it, and when a change to the table
//! does not move [`KERNEL_ABI`] with it.

/// The ABI a kernel built from this tree provides, and the newest one a
/// program built from it may use. Bumped by one with any change to the frozen
/// table (a new entry or a removal).
pub const KERNEL_ABI: u32 = 1;

/// The oldest ABI a kernel built from this tree still serves: programs built
/// for `KERNEL_ABI_COMPAT_FROM..=KERNEL_ABI` run on it. It moves up only when
/// an entry is removed, to the ABI that removed it.
pub const KERNEL_ABI_COMPAT_FROM: u32 = 1;
