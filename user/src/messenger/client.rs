//! The native `libmessenger::client::Transport` over [`Endpoint`] and the
//! registry: what a `user` program builds a
//! [`libmessenger::client::Connection`] on, and the one bounded wait for a
//! service that has not registered yet.

use libmessenger::client::{Deadline, Errno, Handle, Transport};
use libmessenger::Parcel;

use super::{registry, Endpoint, Error, Result};
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
/// The kernel parks the task until the name registers or `until` passes
/// (`WaitFor`), so nothing polls; [`Deadline::Poll`] is one plain `Resolve`.
pub fn resolve_within(name: &str, until: Deadline) -> Result<Endpoint> {
    match until {
        Deadline::Poll => registry::resolve(name),
        _ => registry::wait_for(name, until.absolute_ticks(sys::clock())),
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

    fn call(
        &self,
        handle: Handle,
        request: &Parcel,
        until: Deadline,
    ) -> core::result::Result<Parcel, Errno> {
        let deadline = until.absolute_ticks(sys::clock());
        Endpoint::from_raw(handle)
            .call(request, Some(deadline))
            .map_err(errno_of)
    }

    fn send(&self, handle: Handle, request: &Parcel) -> core::result::Result<(), Errno> {
        Endpoint::from_raw(handle).send(request).map_err(errno_of)
    }

    fn release(&self, handle: Handle) {
        // A handle already gone has nothing left to release.
        let _ = Endpoint::from_raw(handle).release();
    }
}
