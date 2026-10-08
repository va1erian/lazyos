//! Messenger endpoints as Linux descriptors (issue #667; design note
//! docs/architecture/endpoint-fd.md).
//!
//! The Messenger op `ENDPOINT_FD` turns one of the caller's channel handles
//! into a descriptor in its Linux table, so `poll`, `select` and `epoll`
//! watch an endpoint beside sockets and pipes. The descriptor only *reports*:
//! it cannot receive, send or close anything, and every `read`/`write` on it
//! fails. What it reports, level-triggered (with `EPOLLET` edges from an
//! arrival counter):
//!
//! * `POLLIN`: a message is queued, or the peer closed (a `recv` would not
//!   block);
//! * `POLLOUT`: a `send` would find room in the peer's inbox;
//! * `POLLHUP`: the peer closed;
//! * `POLLHUP | POLLERR`: the endpoint itself is gone: the handle the
//!   descriptor was made from was closed (by its owner, or by the owner's
//!   exit), the side was closed, or the channel no longer exists.
//!
//! The descriptor holds no reference into the channel registry and no
//! handle: it names `(owner slot, handle, object)` and re-checks on every
//! scan that the owner's table still maps that handle to that object. So it
//! never outlives the handle (it hangs up instead) and never widens it: a
//! copy inherited across `fork`, or shared with `CLONE_FILES`, still watches
//! the owner's handle and gains no way to use it.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::ipc::channels;
use crate::ipc::handles::{self, rights, HandleKind};
use crate::ipc::pipe::{POLLERR, POLLHUP, POLLIN, POLLOUT};

/// Live watches; when zero, [`ring`] costs one load.
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Tags a channel object id as a keyed-wakeup key (`task::pollwait`): the
/// other keys are kernel heap addresses, and 0 means "no key".
const KEY_TAG: u64 = 1 << 62;

/// The freshness reported once the endpoint is gone (never an arrival count
/// a live side reaches).
const GONE_GEN: u64 = u64::MAX;

/// Why `ENDPOINT_FD` refused a handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenError {
    /// No such handle in the caller's table.
    NoHandle,
    /// The handle is not a channel endpoint.
    WrongKind,
    /// The handle lacks `CALL`, the right `recv` needs.
    MissingRight,
    /// The caller's descriptor table is full.
    TableFull,
}

/// One endpoint descriptor's open file.
pub struct EndpointWatch {
    owner: usize,
    handle: u64,
    object_id: u64,
}

impl EndpointWatch {
    /// Watch `handle` in the current task's table.
    pub fn new(handle: u64) -> Result<Arc<EndpointWatch>, OpenError> {
        let owner = crate::task::current();
        let entry = handles::get_for_task(owner, handle).map_err(|_| OpenError::NoHandle)?;
        if entry.kind != HandleKind::Channel {
            return Err(OpenError::WrongKind);
        }
        if entry.rights & rights::CALL == 0 {
            return Err(OpenError::MissingRight);
        }
        LIVE.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(EndpointWatch {
            owner,
            handle,
            object_id: entry.object_id,
        }))
    }

    /// The keyed-wakeup key of the watched side.
    pub fn key(&self) -> u64 {
        key(self.object_id)
    }

    /// Whether the owner's table still maps the handle to the watched side.
    fn handle_alive(&self) -> bool {
        handles::get_for_task(self.owner, self.handle).is_ok_and(|entry| {
            entry.kind == HandleKind::Channel && entry.object_id == self.object_id
        })
    }

    /// `poll` revents for `events` and the edge freshness counter.
    pub fn poll_gen(&self, events: u16) -> (u16, u64) {
        let (channel_id, side) = (self.object_id >> 1, (self.object_id & 1) as usize);
        let state = channels::side_state(channel_id, side);
        let Some(state) = state.filter(|state| !state.closed && self.handle_alive()) else {
            return (POLLHUP | POLLERR, GONE_GEN);
        };
        let mut revents = 0;
        if events & POLLIN != 0 && (state.queued || state.peer_closed) {
            revents |= POLLIN;
        }
        if events & POLLOUT != 0 && state.room && !state.peer_closed {
            revents |= POLLOUT;
        }
        if state.peer_closed {
            revents |= POLLHUP;
        }
        (revents, state.arrivals << 1 | u64::from(state.peer_closed))
    }
}

impl Drop for EndpointWatch {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The keyed-wakeup key of channel object `object_id`.
fn key(object_id: u64) -> u64 {
    KEY_TAG | object_id
}

/// Wake the poll waiters watching channel object `object_id` (a side whose
/// state changed). Called with no channel lock held.
pub fn ring(object_id: u64) {
    if LIVE.load(Ordering::Relaxed) != 0 {
        crate::task::notify_poll_key(key(object_id));
    }
}

/// Live watches (tests and diagnostics).
pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// `ENDPOINT_FD`: a descriptor in the current task's Linux table watching
/// `handle`, close-on-exec when `cloexec`.
pub fn open_fd(handle: u64, cloexec: bool) -> Result<usize, OpenError> {
    let watch = EndpointWatch::new(handle)?;
    let fd =
        crate::task::fd_open(crate::task::Fd::Endpoint { watch }).ok_or(OpenError::TableFull)?;
    if cloexec {
        crate::task::fd_set_cloexec(fd, true);
    }
    Ok(fd)
}
