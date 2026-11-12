//! The native `libmessenger::client::Transport` over [`Endpoint`] and the
//! registry: what a `user` program builds a
//! [`libmessenger::client::Connection`] on, and the one bounded wait for a
//! service that has not registered yet.

use libmessenger::client::{
    resolve_waiting as wait_resolving, CallFailure, Deadline, Errno, Handle, Retry, Transport,
};
use libmessenger::{flags, Parcel};

use super::{errno, park_tick, registry, Endpoint, Error, Result};
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

/// The native transport with `ALLOW_NESTED` on every request.
///
/// For a service every client resolves to the same endpoint and that answers
/// each request before servicing the next, calling out only on a different
/// channel: the clipboard (a paste by one task can overlap an offer by
/// another) and the compositor (an app's event poll must not trip the
/// kernel's per-channel cycle check while a `Commit` is in flight). Nesting
/// cannot form a cycle there; without the flag the kernel would refuse the
/// second call with `EDEADLK`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Nested;

impl Transport for Nested {
    fn connect_bound(&self) -> Deadline {
        Native.connect_bound()
    }

    fn call_flags(&self) -> u16 {
        flags::ALLOW_NESTED
    }

    fn resolve(&self, name: &str, until: Deadline) -> core::result::Result<Handle, Errno> {
        Native.resolve(name, until)
    }

    fn call(
        &self,
        handle: Handle,
        request: &Parcel,
        until: Deadline,
        retry: Retry,
    ) -> core::result::Result<Parcel, CallFailure> {
        Native.call(handle, request, until, retry)
    }

    fn send(&self, handle: Handle, request: &Parcel) -> core::result::Result<(), Errno> {
        Native.send(handle, request)
    }

    fn release(&self, handle: Handle) {
        Native.release(handle);
    }
}

/// A service reached over an endpoint this task already holds, instead of one
/// the registry resolves: `inputd`'s private shell channel, or an endpoint
/// resolved once and kept. The connection's "resolve" hands back that handle;
/// dropping the connection releases it only when [`Held::resolved`] says the
/// handle came from name resolution (a handle obtained by name is released,
/// never closed, or the service's own endpoint would report a closed peer).
#[derive(Clone, Copy, Debug)]
pub struct Held {
    handle: Handle,
    release: bool,
}

impl Held {
    /// A channel this task owns: closing it is the owner's business.
    pub fn private(endpoint: Endpoint) -> Held {
        Held {
            handle: endpoint.handle(),
            release: false,
        }
    }

    /// An endpoint obtained by name: released with the connection.
    pub fn resolved(endpoint: Endpoint) -> Held {
        Held {
            handle: endpoint.handle(),
            release: true,
        }
    }
}

impl Transport for Held {
    fn connect_bound(&self) -> Deadline {
        Native.connect_bound()
    }

    fn call_flags(&self) -> u16 {
        0
    }

    fn resolve(&self, _name: &str, _until: Deadline) -> core::result::Result<Handle, Errno> {
        Ok(self.handle)
    }

    fn call(
        &self,
        handle: Handle,
        request: &Parcel,
        until: Deadline,
        retry: Retry,
    ) -> core::result::Result<Parcel, CallFailure> {
        Native.call(handle, request, until, retry)
    }

    fn send(&self, handle: Handle, request: &Parcel) -> core::result::Result<(), Errno> {
        Native.send(handle, request)
    }

    fn release(&self, handle: Handle) {
        if self.release {
            Native.release(handle);
        }
    }
}

/// A failed local Messenger operation as the call error the generated clients
/// return, for code that mixes the two (a malformed parcel is `EINVAL`).
pub fn call_error(error: Error) -> libmessenger::client::CallError {
    libmessenger::client::CallError::Transport(errno_of(error))
}

/// Resolve `name`, waiting for it to register no longer than `until`.
///
/// The kernel parks the task until the name registers or `until` passes
/// (`WaitFor`), so nothing polls; [`Deadline::Poll`] is one plain `Resolve`.
pub fn resolve_within(name: &str, until: Deadline) -> Result<Endpoint> {
    if until == Deadline::Poll {
        return registry::resolve(name);
    }
    // Fixed once, so a re-issued wait keeps the caller's bound.
    let stop = until.absolute_ticks(sys::clock());
    let mut last = Error::Errno(-errno::EAGAIN);
    let found = wait_resolving(
        || {
            registry::wait_for(name, stop)
                .map(Endpoint::handle)
                .map_err(|error| {
                    last = error;
                    errno_of(error)
                })
        },
        // The overload path (`EAGAIN`): a plain resolve every tick.
        || {
            registry::resolve(name)
                .map(Endpoint::handle)
                .map_err(errno_of)
        },
        || stop != 0 && sys::clock() >= stop,
        park_tick,
        // A native task has no handlers: a fatal signal ended it on the
        // `EINTR` return itself (`deliver_native`), so there is nothing to run.
        || {},
    );
    found.map(Endpoint::from_raw).map_err(|code| match code {
        Errno::TIMEDOUT => Error::Errno(-errno::ETIMEDOUT),
        _ => last,
    })
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

    /// A `retry=never` method issues the two halves of the kernel call
    /// (`begin_call`, `await_reply`), so a request the kernel refused before
    /// queuing is told from one the peer had when it died; a `retry=safe`
    /// one is resent either way and takes the single call.
    fn call(
        &self,
        handle: Handle,
        request: &Parcel,
        until: Deadline,
        retry: Retry,
    ) -> core::result::Result<Parcel, CallFailure> {
        let deadline = until.absolute_ticks(sys::clock());
        let endpoint = Endpoint::from_raw(handle);
        if retry == Retry::Safe {
            return endpoint
                .call(request, Some(deadline))
                .map_err(|error| CallFailure::delivered(errno_of(error)));
        }
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
