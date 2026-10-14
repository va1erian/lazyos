//! The supervisor's half of a removable-media provider: list the mounts,
//! ask one to finish (eject), and say who is keeping one busy
//! (docs/usb-hotplug-plan.md D). The registrant's half is [`super`] and
//! [`super::mount`].
//!
//! `mountd` starts a `fatfuse` per stick and is not its registrant's
//! successor in any sense the kernel knows: it identifies the mount by the
//! id [`list`] gave it. A stop is a request, never a kill: the daemon sees
//! it from `NEXT` ([`super::Next::Stop`]), flushes its volume and
//! unregisters itself, so the filesystem is closed by the code that owns it.
//! Between the request and the unmount the provider refuses new requests
//! (`EIO`), so a file opened in that window fails visibly instead of being
//! written to a volume that is already being closed.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use fused::wire::{ProviderInfo, PROVIDER_ALIVE, PROVIDER_STOPPING};

use super::mount::point_of;
use super::{FuseError, SLOTS};
use crate::fs::openfile;
use crate::task::holders;

/// Why a stop was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopError {
    /// No live removable-media provider has that id.
    NoSuchProvider,
    /// A file is open or a working directory is on the mount, and the stop
    /// was not forced. [`holders_report`] says who.
    Busy,
}

/// Every live removable-media provider, lowest id first.
pub fn list() -> Vec<ProviderInfo> {
    let mut found = Vec::new();
    for slot in SLOTS.iter() {
        let state = slot.state.lock();
        if !(state.registered && state.alive && state.media) {
            continue;
        }
        found.push(ProviderInfo {
            id: slot.index as u64,
            owner: state.owner as u64,
            state: PROVIDER_ALIVE | if state.stopping { PROVIDER_STOPPING } else { 0 },
            name: state.name.clone(),
        });
    }
    found
}

/// The mount point of live media provider `id` registered by task `owner`:
/// a supervisor names both, so an id that another daemon has taken since it
/// listed the providers is [`StopError::NoSuchProvider`].
fn media_point(id: usize, owner: usize) -> Result<String, StopError> {
    let slot = SLOTS.get(id).ok_or(StopError::NoSuchProvider)?;
    let state = slot.state.lock();
    if state.registered && state.alive && state.media && state.owner == owner {
        Ok(point_of(&state.name, true))
    } else {
        Err(StopError::NoSuchProvider)
    }
}

/// Ask provider `id` (registered by task `owner`) to finish and unmount.
/// Unless `force`, refused with [`StopError::Busy`] while anything holds a
/// file open on it. Asking again for a provider already finishing succeeds.
pub fn stop(id: usize, owner: usize, force: bool) -> Result<(), StopError> {
    let point = media_point(id, owner)?;
    // A descriptor scan and the open-file registry: a mapping outlives its
    // descriptor and only the registry sees it.
    if !force && (!holders::under(&point).is_empty() || !openfile::paths_under(&point).is_empty()) {
        return Err(StopError::Busy);
    }
    let slot = SLOTS.get(id).ok_or(StopError::NoSuchProvider)?;
    {
        let mut state = slot.state.lock();
        if !(state.registered && state.alive && state.media && state.owner == owner) {
            return Err(StopError::NoSuchProvider);
        }
        state.stopping = true;
    }
    // The daemon may be parked in NEXT; a requester waiting for the request
    // slot must see the refusal.
    slot.work.notify_all();
    slot.idle.notify_all();
    Ok(())
}

/// A file name from a stick is any text: control characters (a terminal
/// escape, a newline that would forge a line) become `?`.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Text for a person: one line per task keeping provider `id` (registered
/// by task `owner`) busy, then one per open file no task's descriptor table
/// explains (a mapping), at most [`holders::MAX_HOLDERS`] of each. Empty
/// when nothing is.
pub fn holders_report(id: usize, owner: usize) -> Result<String, FuseError> {
    let point = media_point(id, owner).map_err(|_| FuseError::NotOwner)?;
    let tasks = holders::under(&point);
    let mut text = String::new();
    for holder in &tasks {
        let path = clean(&holder.path);
        let _ = if holder.cwd {
            writeln!(
                text,
                "{} ({}): working directory {path}",
                clean(&holder.name),
                holder.pid
            )
        } else {
            writeln!(text, "{} ({}): {path}", clean(&holder.name), holder.pid)
        };
    }
    for path in openfile::paths_under(&point) {
        if !tasks.iter().any(|h| !h.cwd && h.path == path) {
            let _ = writeln!(text, "(mapped): {}", clean(&path));
        }
    }
    Ok(text)
}
