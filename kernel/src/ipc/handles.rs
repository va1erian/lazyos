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
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::NoFreeHandle => "the process is holding too many Messenger handles",
            Error::InvalidHandle => "that Messenger handle does not exist",
            Error::MissingRight => "this handle does not grant the required right",
            Error::BadTask => "no task exists in that slot",
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

/// Allocate a handle to `object_id` with the given rights.
pub fn open(kind: HandleKind, rights: u32, object_id: u64) -> Result<u64, Error> {
    with_table(|table| table.open(kind, rights, object_id))?
}

/// Duplicate `handle` into a new handle with equal or narrower rights.
pub fn duplicate(handle: u64, rights: u32) -> Result<u64, Error> {
    with_table(|table| table.duplicate(handle, rights))?
}

/// Drop a handle. The object itself is released when its last handle closes.
pub fn close(handle: u64) -> Result<(), Error> {
    with_table(|table| table.close(handle))?
}

/// Copy out an entry (the fabric never hands out mutable references).
pub fn get(handle: u64) -> Result<HandleEntry, Error> {
    with_table(|table| table.entry(handle))?
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
pub fn reset_for_task(slot: usize) {
    if let Some(table) = TABLES.lock().get_mut(slot) {
        *table = Table::new();
    }
}
