//! A task's file-descriptor table: a growable vector of slots.
//!
//! The table used to be a fixed `[Fd; 16]` in every task, so the seventeenth
//! `open` failed however much memory the machine had. It is now a `Vec` that
//! starts with the three standard streams and grows on demand (doubling) up
//! to `limit.fd_max` descriptors (Linux's `RLIMIT_NOFILE`, 8192 by default,
//! `crate::limits`), or to the soft limit a process set with `setrlimit`). Growth
//! is fallible: an exhausted heap fails the one `open`/`dup` with
//! `EMFILE`/`ENOMEM`, never the kernel.
//!
//! The API is deliberately small (look up, install, replace, take, copy) so a
//! later `CLONE_FILES` can share one table between tasks by wrapping it,
//! without touching the descriptor operations built on it.
//!
//! Dropping an [`Fd`] may wake a wait queue (a pipe's last writer), which must
//! not happen under the task-table lock: every method that removes entries
//! hands them back to the caller to drop after unlocking.
//!
//! Every open slot holds a per-uid `Fds` quota charge ([`super::fdcharge`]),
//! released when the slot closes or the table is dropped.

use alloc::vec::Vec;

use super::fdcharge;
use super::{Fd, FD_CLOEXEC};

/// One descriptor slot: the entry, its per-descriptor flags, and the uid its
/// `Fds` charge was taken from (`None`: uncharged, a mirror copy).
struct Slot {
    fd: Fd,
    flags: u16,
    charged: Option<u32>,
}

impl Slot {
    const fn closed() -> Slot {
        Slot {
            fd: Fd::Closed,
            flags: 0,
            charged: None,
        }
    }

    /// A freshly opened slot holding `fd`, charged to `charged`.
    const fn open(fd: Fd, charged: Option<u32>) -> Slot {
        Slot {
            fd,
            flags: 0,
            charged,
        }
    }

    fn is_open(&self) -> bool {
        !matches!(self.fd, Fd::Closed)
    }

    /// Give this slot's charge back (it is closing or being dropped).
    fn release_charge(&mut self) {
        if let Some(uid) = self.charged.take() {
            fdcharge::release(uid);
        }
    }
}

/// A process's `RLIMIT_NOFILE` pair, kept in its descriptor table so every
/// path that grows the table sees it, and copied with the table (`fork`,
/// `execve` and `clone` all inherit it, as on Linux). `0` means "the kernel's
/// `limit.fd_max`", so a table nobody configured follows the limit.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct NofileLimit {
    soft: u32,
    hard: u32,
}

/// The descriptor table of one task.
pub struct FdTable {
    slots: Vec<Slot>,
    limit: NofileLimit,
}

impl Drop for FdTable {
    fn drop(&mut self) {
        for slot in &mut self.slots {
            slot.release_charge();
        }
    }
}

/// Slots allocated the first time a table grows past the standard streams.
const FIRST_GROWTH: usize = 16;

/// The most descriptors a table may hold right now (`limit.fd_max`).
pub fn fd_max() -> usize {
    crate::limits::fd_max()
}

impl FdTable {
    /// A table with no descriptors.
    pub const fn empty() -> FdTable {
        FdTable {
            slots: Vec::new(),
            limit: NofileLimit { soft: 0, hard: 0 },
        }
    }

    /// The most descriptors this table may hold: its soft `RLIMIT_NOFILE`,
    /// never past the kernel's `limit.fd_max`.
    pub fn soft_limit(&self) -> usize {
        match self.limit.soft {
            0 => fd_max(),
            soft => (soft as usize).min(fd_max()),
        }
    }

    /// The hard `RLIMIT_NOFILE`: the ceiling the soft limit may be raised to.
    pub fn hard_limit(&self) -> usize {
        match self.limit.hard {
            0 => fd_max(),
            hard => (hard as usize).min(fd_max()),
        }
    }

    /// Install a `(soft, hard)` pair; the caller has checked
    /// `soft <= hard <= fd_max`.
    pub fn set_limits(&mut self, soft: usize, hard: usize) {
        self.limit = NofileLimit {
            soft: soft.min(u32::MAX as usize) as u32,
            hard: hard.min(u32::MAX as usize) as u32,
        };
    }

    /// A table whose 0/1/2 are the task's terminal, charged to the acting uid
    /// when its quota has room (a task always gets its standard streams).
    pub fn standard() -> FdTable {
        let mut table = FdTable::empty();
        if table.slots.try_reserve_exact(3).is_ok() {
            let uid = fdcharge::acting_uid();
            let charged = fdcharge::charge(uid, 3).then_some(uid);
            for _ in 0..3 {
                table.slots.push(Slot::open(Fd::Terminal, charged));
            }
        }
        table
    }

    /// One past the highest slot allocated (open or not).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// The open descriptor `fd`; `None` when closed or out of range.
    pub fn get(&self, fd: usize) -> Option<&Fd> {
        match self.slots.get(fd) {
            Some(Slot { fd: Fd::Closed, .. }) | None => None,
            Some(slot) => Some(&slot.fd),
        }
    }

    /// The open descriptor `fd`, mutably.
    pub fn get_mut(&mut self, fd: usize) -> Option<&mut Fd> {
        match self.slots.get_mut(fd) {
            Some(Slot { fd: Fd::Closed, .. }) | None => None,
            Some(slot) => Some(&mut slot.fd),
        }
    }

    /// Whether `fd` is open.
    pub fn is_open(&self, fd: usize) -> bool {
        self.get(fd).is_some()
    }

    /// The flags of open descriptor `fd` (`None` when closed).
    pub fn flags(&self, fd: usize) -> Option<u16> {
        self.get(fd)?;
        Some(self.slots[fd].flags)
    }

    /// Replace the flags of open descriptor `fd`; false when closed.
    pub fn set_flags(&mut self, fd: usize, flags: u16) -> bool {
        if !self.is_open(fd) {
            return false;
        }
        self.slots[fd].flags = flags;
        true
    }

    /// Every open descriptor with its number, in order.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &Fd)> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !matches!(slot.fd, Fd::Closed))
            .map(|(index, slot)| (index, &slot.fd))
    }

    /// Make room for slot `fd`, growing geometrically. False when `fd` is at
    /// or past the descriptor limit or the heap cannot grow.
    fn reserve_slot(&mut self, fd: usize) -> bool {
        // The slots stay allocated after a close (or a lowered limit), so the
        // limit is checked before the table's length.
        let limit = self.soft_limit();
        if fd >= limit {
            return false;
        }
        if fd < self.slots.len() {
            return true;
        }
        let wanted = (fd + 1)
            .max(self.slots.len().saturating_mul(2))
            .max(FIRST_GROWTH)
            .min(limit);
        if self
            .slots
            .try_reserve_exact(wanted - self.slots.len())
            .is_err()
        {
            return false;
        }
        self.slots.resize_with(wanted, Slot::closed);
        true
    }

    /// Install `entry` in the lowest closed slot at or above `min`, with no
    /// flags. Hands `entry` back when the table is full.
    /// Hands `entry` back when the acting uid is at its `Fds` quota, too.
    pub fn install_lowest(&mut self, min: usize, entry: Fd) -> Result<usize, Fd> {
        let found = (min..self.slots.len()).find(|&fd| !self.slots[fd].is_open());
        let fd = found.unwrap_or(self.slots.len().max(min));
        if !self.reserve_slot(fd) {
            return Err(entry);
        }
        let uid = fdcharge::acting_uid();
        if !fdcharge::charge(uid, 1) {
            return Err(entry);
        }
        self.slots[fd] = Slot::open(entry, Some(uid));
        Ok(fd)
    }

    /// Put `entry` at `fd` (growing the table), clearing its flags; returns
    /// the previous entry (`Fd::Closed` when the slot was free). Hands
    /// `entry` back when `fd` is past the limit, memory ran out, or a free
    /// slot would put the acting uid over its `Fds` quota. Replacing an open
    /// descriptor keeps its charge: the count does not change.
    pub fn put(&mut self, fd: usize, entry: Fd) -> Result<Fd, Fd> {
        self.put_charging(fd, entry, true)
    }

    /// [`put`](FdTable::put) for a `CLONE_FILES` mirror: a slot that was free
    /// stays uncharged, since the share group paid in the acting table.
    pub fn put_mirror(&mut self, fd: usize, entry: Fd) -> Result<Fd, Fd> {
        self.put_charging(fd, entry, false)
    }

    fn put_charging(&mut self, fd: usize, entry: Fd, charge: bool) -> Result<Fd, Fd> {
        if !self.reserve_slot(fd) {
            return Err(entry);
        }
        let charged = if self.slots[fd].is_open() {
            self.slots[fd].charged.take()
        } else if charge {
            let uid = fdcharge::acting_uid();
            if !fdcharge::charge(uid, 1) {
                return Err(entry);
            }
            Some(uid)
        } else {
            None
        };
        let old = core::mem::replace(&mut self.slots[fd], Slot::open(entry, charged));
        Ok(old.fd)
    }

    /// Replace the entry of open descriptor `fd`, keeping its flags; returns
    /// the old entry, or hands `entry` back when `fd` is closed.
    pub fn replace(&mut self, fd: usize, entry: Fd) -> Result<Fd, Fd> {
        match self.get_mut(fd) {
            Some(slot) => Ok(core::mem::replace(slot, entry)),
            None => Err(entry),
        }
    }

    /// Close `fd`, returning its entry (`None` when it was not open).
    pub fn take(&mut self, fd: usize) -> Option<Fd> {
        if !self.is_open(fd) {
            return None;
        }
        let mut old = core::mem::replace(&mut self.slots[fd], Slot::closed());
        old.release_charge();
        Some(old.fd)
    }

    /// Close everything, returning the old table for unlocked dropping.
    pub fn take_all(&mut self) -> FdTable {
        core::mem::replace(self, FdTable::empty())
    }

    /// The descriptors marked `FD_CLOEXEC`.
    pub fn cloexec_fds(&self) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !matches!(slot.fd, Fd::Closed) && slot.flags & FD_CLOEXEC != 0)
            .map(|(index, _)| index)
            .collect()
    }

    /// A copy for `fork`: every entry shares its open file description, pipe
    /// references are retained, flags are kept. Every copied descriptor is
    /// charged to the acting uid. `None` when memory ran out or the uid's
    /// `Fds` quota cannot take the copy.
    pub fn fork_copy(&self) -> Option<FdTable> {
        self.copy_where(|_| true, true)
    }

    /// A copy for a `CLONE_FILES` thread: the same entries, uncharged, since
    /// the share group already paid for them ([`super::fdcharge`]).
    pub fn mirror_copy(&self) -> Option<FdTable> {
        self.copy_where(|_| true, false)
    }

    /// A copy for `execve` of a native program: like [`fork_copy`] but
    /// without the descriptors marked `FD_CLOEXEC` (and with flags cleared).
    ///
    /// [`fork_copy`]: FdTable::fork_copy
    pub fn exec_copy(&self) -> Option<FdTable> {
        let mut copy = self.copy_where(|slot| slot.flags & FD_CLOEXEC == 0, true)?;
        for slot in &mut copy.slots {
            slot.flags = 0;
        }
        Some(copy)
    }

    /// Copy the open slots `keep` selects, charging them all at once (before
    /// any entry is cloned, so a refusal leaves no reference to drop).
    fn copy_where(&self, keep: impl Fn(&Slot) -> bool, charge: bool) -> Option<FdTable> {
        let kept = |slot: &Slot| slot.is_open() && keep(slot);
        // Only up to the last descriptor the copy keeps: a table that once
        // held thousands of descriptors keeps its length after they close,
        // and a thread per copy would otherwise pay for all of it (a
        // hundred threads of a process that touched fd 8191 are ~30 MiB).
        let used = self.slots.iter().rposition(kept).map_or(0, |last| last + 1);
        let mut slots = Vec::new();
        slots.try_reserve_exact(used).ok()?;
        let charged = if charge {
            let uid = fdcharge::acting_uid();
            let count = self.slots.iter().filter(|slot| kept(slot)).count() as u64;
            fdcharge::charge(uid, count).then_some(uid)?;
            Some(uid)
        } else {
            None
        };
        for slot in &self.slots[..used] {
            slots.push(if kept(slot) {
                Slot {
                    fd: slot.fd.clone(),
                    flags: slot.flags,
                    charged,
                }
            } else {
                Slot::closed()
            });
        }
        Some(FdTable {
            slots,
            limit: self.limit,
        })
    }
}
