//! [`Transport`] and [`Connection`]: resolve once, call, survive a restart.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use super::deadline::Deadline;
use super::errno::Errno;
use super::error::{remote_error, CallError};
use crate::{flags, Header, Object, Parcel, VERSION};

/// A kernel handle naming a resolved service.
pub type Handle = u64;

/// What a runtime provides for Messenger calls: the native `user` library,
/// the std programs over `lazyos-sys`, and a [`super::Mock`] in tests. It
/// moves parcels and knows nothing of interfaces, errors or retries; that
/// is [`Connection`].
pub trait Transport {
    /// The one bound this runtime waits for a service to register
    /// (`docs/client-api-plan.md` 3.4: [`Transport::resolve`] blocks in the
    /// kernel on the registry for at most this long).
    fn connect_bound(&self) -> Deadline;

    /// The flags every synchronous request of this runtime carries. Native
    /// programs send none, so the kernel refuses a call that would close a
    /// wait cycle (`Deadlock`, `docs/messenger.md` 6); the std runtime's UI
    /// loops send `ALLOW_NESTED`, as they always have. Required, not
    /// defaulted, so a new transport decides it explicitly.
    fn call_flags(&self) -> u16;

    /// Resolve the registry name `name` into a handle, waiting for it to
    /// register no longer than `until` ([`Deadline::Poll`]: one attempt).
    fn resolve(&self, name: &str, until: Deadline) -> Result<Handle, Errno>;

    /// One synchronous call: `request` out, the reply back, no later than
    /// `until`.
    fn call(&self, handle: Handle, request: &Parcel, until: Deadline) -> Result<Parcel, Errno>;

    /// Send `request` one way; it does not wait for the peer.
    fn send(&self, handle: Handle, request: &Parcel) -> Result<(), Errno>;

    /// Give `handle` back to the kernel (a resolved handle is shared with
    /// every other resolver, so this releases, it does not close).
    fn release(&self, handle: Handle);
}

/// A service by registry name, over a [`Transport`]: it resolves on first
/// use, keeps the handle, and when a call finds the peer gone
/// (`EPIPE`, or `ENOENT` from a reaped name) drops it, resolves again once
/// and retries the call once. A remote refusal is never retried.
pub struct Connection<T: Transport> {
    transport: T,
    name: Cow<'static, str>,
    handle: Cell<Option<Handle>>,
}

impl<T: Transport> Connection<T> {
    /// A connection to the service registered as `name`; nothing resolves
    /// until the first call.
    pub const fn new(transport: T, name: &'static str) -> Connection<T> {
        Connection {
            transport,
            name: Cow::Borrowed(name),
            handle: Cell::new(None),
        }
    }

    /// A connection to a name built at run time: a member of a service
    /// family (`SERVICE_INSTANCE_PREFIX` plus an instance, such as
    /// `os.lazy.net.nic/eth0`).
    pub fn named(transport: T, name: String) -> Connection<T> {
        Connection {
            transport,
            name: Cow::Owned(name),
            handle: Cell::new(None),
        }
    }

    /// The registry name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The runtime behind it.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Whether a handle is cached.
    pub fn is_connected(&self) -> bool {
        self.handle.get().is_some()
    }

    /// The service's handle, resolving it (waiting the runtime's connect
    /// bound for the name to register) when none is cached.
    pub fn connect(&self) -> Result<Handle, Errno> {
        self.resolve_within(self.transport.connect_bound())
    }

    /// [`Connection::connect`] with one attempt: for a caller whose
    /// service being absent has a cheap fallback.
    pub fn try_connect(&self) -> Result<Handle, Errno> {
        self.resolve_within(Deadline::Poll)
    }

    fn resolve_within(&self, until: Deadline) -> Result<Handle, Errno> {
        if let Some(handle) = self.handle.get() {
            return Ok(handle);
        }
        let handle = self.transport.resolve(&self.name, until)?;
        self.handle.set(Some(handle));
        Ok(handle)
    }

    /// Forget the cached handle after the peer died, so the next use
    /// resolves a restarted service.
    pub fn evict(&self) {
        if let Some(handle) = self.handle.take() {
            self.transport.release(handle);
        }
    }

    /// The request parcel for `method` of `interface` (or of a scope id,
    /// for a method declared on an ACL scope) with `flag_bits`.
    pub fn request(
        interface: u64,
        method: u32,
        flag_bits: u16,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Parcel {
        Parcel {
            header: Header {
                version: VERSION,
                flags: flag_bits,
                interface_id: interface,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body,
            objects,
        }
    }

    /// One call of `method` on `interface` (or scope id), `body` and
    /// `objects` as the generated encoder returned them, answered within
    /// `until`. A reply that is the standard error field becomes
    /// [`CallError::Remote`].
    ///
    /// The request carries the transport's [`Transport::call_flags`].
    pub fn call(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
        until: Deadline,
    ) -> Result<Parcel, CallError> {
        let request = Self::request(
            interface,
            method,
            self.transport.call_flags(),
            body,
            objects,
        );
        let reply = self
            .with_retry(|handle| self.transport.call(handle, &request, until))
            .map_err(CallError::Transport)?;
        Self::check(reply)
    }

    /// A one-way message (`flags::ONE_WAY`): never waits for the peer.
    /// `objects` move with it, as for [`Connection::call`].
    pub fn send(
        &self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Result<(), CallError> {
        let request = Self::request(interface, method, flags::ONE_WAY, body, objects);
        self.with_retry(|handle| self.transport.send(handle, &request))
            .map_err(CallError::Transport)
    }

    /// `reply` unless it is the standard error field, which is the error.
    /// For a caller that issued the call itself (an asynchronous one).
    pub fn check(reply: Parcel) -> Result<Parcel, CallError> {
        match remote_error(&reply) {
            Some(error) => Err(error),
            None => Ok(reply),
        }
    }

    /// Run `attempt` on the handle; when it reports a dead peer, drop the
    /// handle, resolve once without waiting and run it once more.
    fn with_retry<R>(
        &self,
        mut attempt: impl FnMut(Handle) -> Result<R, Errno>,
    ) -> Result<R, Errno> {
        let handle = self.connect()?;
        match attempt(handle) {
            Err(errno) if is_peer_gone(errno) => {
                self.evict();
                attempt(self.try_connect()?).inspect_err(|errno| {
                    if is_peer_gone(*errno) {
                        self.evict();
                    }
                })
            }
            result => result,
        }
    }
}

impl<T: Transport> Drop for Connection<T> {
    fn drop(&mut self) {
        self.evict();
    }
}

/// `EPIPE` (the endpoint closed) or `ENOENT` (the registration was reaped):
/// the service restarted or is gone, so a cached handle is stale.
pub fn is_peer_gone(errno: Errno) -> bool {
    errno == Errno::PIPE || errno == Errno::NOENT
}
