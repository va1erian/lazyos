//! Per-process handle tables and capability rights (issue #64).
//!
//! A handle is a small integer that names a kernel object *for one process*.
//! Holding a handle is the capability: there is no way to call an object without
//! one, and the rights bits say what the holder may do with it. The table lives
//! in the `ipc` module keyed by task slot (like the VMA/bump registries) rather
//! than in `Task`, so this lands without churning the scheduler; moving it into
//! `Task` later is a pure refactor.

use alloc::vec::Vec;
use spin::Mutex;

use crate::quota::{self, Resource};
use crate::task::MAX_TASKS;

/// What a handle refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandleKind {
    /// A listening endpoint that accepts new connections.
    Endpoint,
    /// A duplex connection between two processes.
    Channel,
    /// A callable service object.
    Object,
    /// A shared buffer.
    Buffer,
}

/// Capability rights carried by a handle.
pub mod rights {
    /// Invoke methods on the object.
    pub const CALL: u32 = 1 << 0;
    /// Create more handles to the object.
    pub const DUPLICATE: u32 = 1 << 1;
    /// Move the handle to another process.
    pub const TRANSFER: u32 = 1 << 2;
    /// Receive health/state events and stats.
    pub const MONITOR: u32 = 1 << 3;
    /// Administrative operations (revoke, reconfigure).
    pub const CONTROL: u32 = 1 << 4;
    /// Every right.
    pub const ALL: u32 = CALL | DUPLICATE | TRANSFER | MONITOR | CONTROL;
}

/// Kernel handles per process. A quota keeps a malicious sender from exhausting
/// kernel memory with handles (see `docs/messenger.md` section 9).
pub const MAX_HANDLES: usize = 256;

/// One occupied handle slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HandleEntry {
    pub kind: HandleKind,
    pub rights: u32,
    /// Kernel object this handle refers to (opaque to userspace).
    pub object_id: u64,
}

/// Why an operation on the handle table failed. Messages are user-facing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The process already holds [`MAX_HANDLES`] handles.
    NoFreeHandle,
    /// The handle is unused or out of range.
    InvalidHandle,
    /// The handle exists but lacks the required right.
    MissingRight,
    /// No task runs in the queried slot.
    BadTask,
    /// The handle's user is over its per-uid handle quota (issue #103).
    Quota,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::NoFreeHandle => "the process is holding too many Messenger handles",
            Error::InvalidHandle => "that Messenger handle does not exist",
            Error::MissingRight => "this handle does not grant the required right",
            Error::BadTask => "no task exists in that slot",
            Error::Quota => "this user is holding too many Messenger handles",
        }
    }
}

/// Slot table for one process: handle index -> entry.
struct Table {
    slots: Vec<Option<HandleEntry>>,
}

impl Table {
    const fn new() -> Self {
        Table { slots: Vec::new() }
    }

    fn entry(&self, handle: u64) -> Result<HandleEntry, Error> {
        self.slots
            .get(handle as usize)
            .and_then(|slot| *slot)
            .ok_or(Error::InvalidHandle)
    }

    fn open(&mut self, kind: HandleKind, rights: u32, object_id: u64) -> Result<u64, Error> {
        let entry = HandleEntry {
            kind,
            rights,
            object_id,
        };
        // Reuse the lowest free slot so handle numbers stay small.
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(entry);
                return Ok(index as u64);
            }
        }
        if self.slots.len() >= MAX_HANDLES {
            return Err(Error::NoFreeHandle);
        }
        self.slots.push(Some(entry));
        Ok((self.slots.len() - 1) as u64)
    }

    fn duplicate(&mut self, handle: u64, rights: u32) -> Result<u64, Error> {
        let entry = self.entry(handle)?;
        if entry.rights & rights::DUPLICATE == 0 {
            return Err(Error::MissingRight);
        }
        // Duplication may only narrow rights: a superset would be an escalation.
        if rights & !entry.rights != 0 {
            return Err(Error::MissingRight);
        }
        self.open(entry.kind, rights, entry.object_id)
    }

    fn close(&mut self, handle: u64) -> Result<(), Error> {
        match self.slots.get_mut(handle as usize) {
            Some(slot) if slot.is_some() => {
                *slot = None;
                Ok(())
            }
            _ => Err(Error::InvalidHandle),
        }
    }

    fn count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }
}

/// One table per task slot.
static TABLES: Mutex<[Table; MAX_TASKS]> = Mutex::new([const { Table::new() }; MAX_TASKS]);

/// Run `f` on the current task's handle table.
fn with_table<R>(f: impl FnOnce(&mut Table) -> R) -> Result<R, Error> {
    let slot = crate::task::current();
    let mut tables = TABLES.lock();
    let table = tables.get_mut(slot).ok_or(Error::BadTask)?;
    Ok(f(table))
}

/// Charge one handle to `slot`'s uid (issue #103), mapping a quota refusal
/// onto the handle-table vocabulary.
fn charge_handle(slot: usize) -> Result<(), Error> {
    quota::charge_for_slot(slot, Resource::Handles, 1).map_err(|_| Error::Quota)
}

/// Give one handle back to `slot`'s uid. Saturating, so teardown of an
/// unbalanced table cannot underflow.
fn release_handle(slot: usize, count: u64) {
    quota::release_for_slot(slot, Resource::Handles, count);
}

/// Allocate a handle to `object_id` with the given rights.
///
/// The per-process [`MAX_HANDLES`] check stays the first line inside the table;
/// the per-uid aggregate (issue #103) is charged here, so two tasks of the same
/// user share one limit.
pub fn open(kind: HandleKind, rights: u32, object_id: u64) -> Result<u64, Error> {
    let slot = crate::task::current();
    charge_handle(slot)?;
    match with_table(|table| table.open(kind, rights, object_id))? {
        Ok(handle) => Ok(handle),
        Err(error) => {
            release_handle(slot, 1);
            Err(error)
        }
    }
}

/// Duplicate `handle` into a new handle with equal or narrower rights.
pub fn duplicate(handle: u64, rights: u32) -> Result<u64, Error> {
    let slot = crate::task::current();
    charge_handle(slot)?;
    match with_table(|table| table.duplicate(handle, rights))? {
        Ok(copy) => Ok(copy),
        Err(error) => {
            release_handle(slot, 1);
            Err(error)
        }
    }
}

/// Drop a handle. The object itself is released when its last handle closes.
pub fn close(handle: u64) -> Result<(), Error> {
    let slot = crate::task::current();
    let result = with_table(|table| table.close(handle))?;
    if result.is_ok() {
        release_handle(slot, 1);
    }
    result
}

/// Drop a handle from a specific task slot's table.
///
/// The counterpart of [`close`] for teardown: the caller is not the owner (the
/// dead task's slot is being reclaimed), so the per-uid handle charge is
/// released against the slot's own stamped uid.
pub fn close_for_task(slot: usize, handle: u64) -> Result<(), Error> {
    let result = {
        let mut tables = TABLES.lock();
        let table = tables.get_mut(slot).ok_or(Error::BadTask)?;
        table.close(handle)
    };
    if result.is_ok() {
        release_handle(slot, 1);
    }
    result
}

/// How many handles, across every task's table, still name `object_id`.
///
/// Endpoint handles are not reference counted by the channel registry: a name
/// resolve opens a fresh handle to the *same* endpoint side in each client. Task
/// teardown asks this before declaring a side dead, so one client exiting does
/// not fail the in-flight calls of every other client of the same service.
pub fn object_refs(kind: HandleKind, object_id: u64) -> usize {
    let tables = TABLES.lock();
    tables
        .iter()
        .flat_map(|table| table.slots.iter())
        .filter(|slot| slot.is_some_and(|entry| entry.kind == kind && entry.object_id == object_id))
        .count()
}

/// Every occupied handle in a task slot's table, as `(handle, entry)`.
///
/// Teardown walks this to close each object through its own subsystem (channel
/// endpoints, shared buffers) before the table is dropped.
pub fn entries_for_task(slot: usize) -> Vec<(u64, HandleEntry)> {
    let tables = TABLES.lock();
    match tables.get(slot) {
        Some(table) => table
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.map(|entry| (index as u64, entry)))
            .collect(),
        None => Vec::new(),
    }
}

/// Copy out an entry (the fabric never hands out mutable references).
pub fn get(handle: u64) -> Result<HandleEntry, Error> {
    with_table(|table| table.entry(handle))?
}

/// Copy out an entry from a specific task slot's table.
///
/// Kernel-side only: the name registry uses it to record the endpoint a task
/// published (`messengerd` forwards a client's handle number with the client's
/// slot). Userspace still cannot name another process's handles.
pub fn get_for_task(slot: usize, handle: u64) -> Result<HandleEntry, Error> {
    let mut tables = TABLES.lock();
    let table = tables.get_mut(slot).ok_or(Error::BadTask)?;
    table.entry(handle)
}

/// Open a handle in a specific task slot's table.
///
/// This is the counterpart of [`get_for_task`]: `registry::resolve` opens the
/// discovered endpoint in the *receiving* task's table, which is the caller for
/// a direct resolve and the requesting client for the `messengerd` proxy.
pub fn open_for_task(
    slot: usize,
    kind: HandleKind,
    rights: u32,
    object_id: u64,
) -> Result<u64, Error> {
    charge_handle(slot)?;
    let opened = {
        let mut tables = TABLES.lock();
        match tables.get_mut(slot) {
            Some(table) => table.open(kind, rights, object_id),
            None => Err(Error::BadTask),
        }
    };
    match opened {
        Ok(handle) => Ok(handle),
        Err(error) => {
            release_handle(slot, 1);
            Err(error)
        }
    }
}

/// The rights of `handle`, if it exists.
pub fn rights(handle: u64) -> Option<u32> {
    get(handle).ok().map(|entry| entry.rights)
}

/// Number of handles held by the current task.
pub fn count() -> usize {
    with_table(|table| table.count()).unwrap_or(0)
}

/// Number of handles held by a specific task slot.
pub fn count_for_task(slot: usize) -> usize {
    TABLES
        .lock()
        .get(slot)
        .map(|table| table.count())
        .unwrap_or(0)
}

/// Drop every handle of a task (process teardown; wires into `Task` later).
///
/// The dropped handles are released from the slot's per-uid aggregate too, so
/// teardown cannot strand a user above its handle quota.
pub fn reset_for_task(slot: usize) {
    let dropped = {
        let mut tables = TABLES.lock();
        match tables.get_mut(slot) {
            Some(table) => {
                let count = table.count();
                *table = Table::new();
                count
            }
            None => 0,
        }
    };
    if dropped > 0 {
        release_handle(slot, dropped as u64);
    }
}
