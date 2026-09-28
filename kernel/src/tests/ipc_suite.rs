//! Messenger handles: open, duplicate, close and quota accounting.

use super::*;
use crate::ipc::handles::{self, rights, Error, HandleKind, MAX_HANDLES};

/// Each handle test starts from an empty table in the kernel task's slot.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    handles::reset_for_task(task::current());
    Ok(())
}

/// Opening returns distinct handles that resolve to what was stored.
pub fn open_distinct() -> Result<(), String> {
    fresh()?;
    let object =
        handles::open(HandleKind::Object, rights::CALL, 1).map_err(|error| error.message())?;
    let buffer =
        handles::open(HandleKind::Buffer, rights::ALL, 2).map_err(|error| error.message())?;
    check!(object != buffer, "open reused handle {object}");
    let entry = handles::get(object).map_err(|error| error.message())?;
    check!(
        entry.kind == HandleKind::Object && entry.rights == rights::CALL && entry.object_id == 1,
        "entry does not match what was opened"
    );
    check!(handles::count() == 2, "count is {}", handles::count());
    handles::reset_for_task(task::current());
    Ok(())
}

/// Duplication requires the right and may only narrow rights.
pub fn duplicate_rights() -> Result<(), String> {
    fresh()?;
    let plain =
        handles::open(HandleKind::Channel, rights::CALL, 7).map_err(|error| error.message())?;
    check!(
        handles::duplicate(plain, rights::CALL) == Err(Error::MissingRight),
        "duplicated a handle without the DUPLICATE right"
    );
    let dupable = handles::open(HandleKind::Channel, rights::CALL | rights::DUPLICATE, 8)
        .map_err(|error| error.message())?;
    check!(
        handles::duplicate(dupable, rights::ALL) == Err(Error::MissingRight),
        "duplication widened the rights (escalation)"
    );
    let copy = handles::duplicate(dupable, rights::CALL).map_err(|error| error.message())?;
    check!(copy != dupable, "duplicate reused the handle");
    check!(
        handles::get(copy).map_err(|error| error.message())?.rights == rights::CALL,
        "duplicate did not take the requested rights"
    );
    handles::reset_for_task(task::current());
    Ok(())
}

/// Closing frees the slot and the lowest free slot is reused.
pub fn close_frees() -> Result<(), String> {
    fresh()?;
    let handle =
        handles::open(HandleKind::Endpoint, rights::CALL, 3).map_err(|error| error.message())?;
    handles::close(handle).map_err(|error| error.message())?;
    check!(
        handles::get(handle) == Err(Error::InvalidHandle),
        "closed handle still resolves"
    );
    check!(
        handles::close(handle) == Err(Error::InvalidHandle),
        "double close succeeded"
    );
    let again =
        handles::open(HandleKind::Endpoint, rights::CALL, 4).map_err(|error| error.message())?;
    check!(again == handle, "freed slot was not reused");
    handles::reset_for_task(task::current());
    Ok(())
}

/// The per-process quota is enforced, and teardown drops every handle.
pub fn quota() -> Result<(), String> {
    fresh()?;
    for index in 0..MAX_HANDLES {
        handles::open(HandleKind::Object, rights::CALL, index as u64)
            .map_err(|error| error.message())?;
    }
    check!(
        handles::open(HandleKind::Object, rights::CALL, 0) == Err(Error::NoFreeHandle),
        "handle quota was not enforced"
    );
    handles::reset_for_task(task::current());
    check!(
        handles::count() == 0,
        "reset left {} handles behind",
        handles::count()
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_open_distinct", open_distinct),
    ("ipc_duplicate_rights", duplicate_rights),
    ("ipc_close_frees", close_frees),
    ("ipc_quota", quota),
];
