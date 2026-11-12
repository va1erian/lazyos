//! The native `libmessenger::client::Transport` over [`Endpoint`] and the
//! registry: what a `user` program builds a
//! [`libmessenger::client::Connection`] on, and the one bounded wait for a
//! service that has not registered yet.

use libmessenger::client::{CallFailure, Deadline, Errno, Handle, Transport};
use libmessenger::Parcel;

use super::{park_tick, registry, Endpoint, Error, Result};
use crate::sys;

/// How long a native program waits for a service to register: the one connect
/// bound of this runtime (10 s), instead of one `CONNECT_TICKS` per program.
pub const CONNECT_BOUND: Deadline = Deadline::secs(10);

/// The native transport. Stateless: the handle cache is the `Connection`'s.
#[derive(Clone, Copy, Debug, Default)]
pub struct Native;

/// A Messenger error as the plain errno the transport trait reports
/// (a malformed parcel is `EINVAL`).
fn errno_of(error: Error) -> Errno {
    error.errno().map_or(Errno::INVAL, Errno::from_syscall)
}

/// Resolve `name`, waiting for it to register no longer than `until`.
///
/// The kernel-served registry has no "wait for name" operation, so this is
/// the runtime's one bounded polling loop (one tick between attempts);
/// `docs/client-api-plan.md` 3.4 is the follow-up that wakes on the
/// registration instead. [`Deadline::Poll`] makes one attempt.
pub fn resolve_within(name: &str, until: Deadline) -> Result<Endpoint> {
    let stop = until.absolute_ticks(sys::clock());
    loop {
        match registry::resolve(name) {
            Ok(endpoint) => return Ok(endpoint),
            Err(error) if until == Deadline::Poll || (stop != 0 && sys::clock() >= stop) => {
                return Err(error)
            }
            Err(_) => park_tick(),
        }
    }
}

/// [`registry::resolve`] for a program that may start before the service:
/// waits [`CONNECT_BOUND`].
pub fn resolve_waiting(name: &str) -> Result<Endpoint> {
    resolve_within(name, CONNECT_BOUND)
}

impl Transport for Native {
    fn connect_bound(&self) -> Deadline {
        CONNECT_BOUND
    }

    /// No `ALLOW_NESTED`: a native call that would close a wait cycle is
    /// refused with `Deadlock`, as before the shared runtime.
    fn call_flags(&self) -> u16 {
        0
    }

    fn resolve(&self, name: &str, until: Deadline) -> core::result::Result<Handle, Errno> {
        resolve_within(name, until)
            .map(Endpoint::handle)
            .map_err(errno_of)
    }

    /// The two halves of the kernel call (`begin_call`, `await_reply`), so
    /// a request the kernel refused before queuing is told from one the peer
    /// had when it died.
    fn call(
        &self,
        handle: Handle,
        request: &Parcel,
        until: Deadline,
    ) -> core::result::Result<Parcel, CallFailure> {
        let deadline = until.absolute_ticks(sys::clock());
        let endpoint = Endpoint::from_raw(handle);
        let txn = endpoint
            .begin_call(request, Some(deadline))
            .map_err(|error| CallFailure::undelivered(errno_of(error)))?;
        endpoint
            .await_reply(txn)
            .map_err(|error| CallFailure::delivered(errno_of(error)))
    }

    fn send(&self, handle: Handle, request: &Parcel) -> core::result::Result<(), Errno> {
        Endpoint::from_raw(handle).send(request).map_err(errno_of)
    }

    fn release(&self, handle: Handle) {
        // A handle already gone has nothing left to release.
        let _ = Endpoint::from_raw(handle).release();
    }
}
