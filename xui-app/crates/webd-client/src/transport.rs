//! What the client needs of the OS, as a trait: `xui_app::platform::webd` implements [`Transport`]
//! over Messenger on LazyOS, the host tests over the service half's in-memory fabric. The client
//! never holds a raw handle longer than it must: a channel pair is closed by [`Transport::close_channel`],
//! shared memory by dropping its [`SharedMem`].

use libmessenger::Object;

/// The registered name `webd` serves the light interface under.
pub const SERVICE_NAME: &str = "app.os.lazy.webd.light";

/// The registered name `webd` serves the full interface (`os.lazy.webd.view.v1`) and its two scope
/// interfaces (`os.lazy.webd.net.v1`, `os.lazy.webd.browser.v1`) under.
pub const FULL_SERVICE_NAME: &str = "app.os.lazy.webd.view";

/// Positive errno values the client reads (`docs/midl.md`, "Errors"); transports return them
/// negated, as the kernel does.
pub mod errno {
    pub const ENOENT: i64 = 2;
    pub const ENOMEM: i64 = 12;
    pub const EAGAIN: i64 = 11;
    pub const EACCES: i64 = 13;
    pub const ENODEV: i64 = 19;
    pub const EINVAL: i64 = 22;
    pub const EPIPE: i64 = 32;
}

/// Shared pages this task created: a slot for webd to paint into, or a page of HTML to hand over.
/// Dropping it unmaps the pages and closes the handle (the service keeps its own reference for as
/// long as it needs one).
pub trait SharedMem {
    /// The buffer handle that travels in a request.
    fn handle(&self) -> u64;
    /// Bytes the pages hold (at least what was asked for).
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Copy `bytes` to the start; `bytes.len() <= len()`.
    fn write(&mut self, bytes: &[u8]);
    /// Copy the first `into.len()` bytes out; `into.len() <= len()`.
    fn read(&self, into: &mut [u8]);
    /// Copy `into.len()` bytes out from `offset`; `offset + into.len() <= len()`. A full view's
    /// delta reads only its rectangles out of a slot; a transport should copy just those bytes.
    fn read_at(&self, offset: usize, into: &mut [u8]) {
        let mut prefix = vec![0; offset + into.len()];
        self.read(&mut prefix);
        into.copy_from_slice(&prefix[offset..]);
    }
}

/// One event off a view's endpoint: the method id and its TLV body.
pub type RawEvent = (u32, Vec<u8>);

/// The receive buffer a transport offers for one event: the largest event webd sends is a `State`
/// with a title and an error of `EVENT_TEXT_BYTES` each, so this is `limits::EVENT_BYTES_MAX` of the
/// service half (a test keeps the two together). A message larger than the buffer is dropped by the
/// kernel, so the buffer is never smaller than the largest event.
pub const EVENT_BYTES: usize = 100 << 10;

/// Close the channels of a request that never reached the service. `objects` are what the request
/// carried; `held` says whether this task still has a handle by that number (a queued request moved
/// it away) and `close` closes it. A buffer is not touched: its owner closes it.
pub fn close_unqueued(
    objects: &[Object],
    mut held: impl FnMut(u64) -> bool,
    mut close: impl FnMut(u64),
) {
    for object in objects {
        if let Object::Channel(handle) = *object {
            if held(handle) {
                close(handle);
            }
        }
    }
}

/// How requests reach `webd` and its events reach the app. Errors are negative errno values.
pub trait Transport {
    type Mem: SharedMem;

    /// One call to the service (resolving it first, bounded: a UI thread must not park for long)
    /// carrying `objects`; the reply body, or the negative errno of the transport or of the
    /// service's refusal. `-ENOENT` also means "the service is not running".
    ///
    /// The transport takes `objects` over whatever happens: a channel is moved to the service when
    /// the request is queued, and **closed by the transport when the request never was** (the
    /// service is not there, the kernel refused it: [`close_unqueued`]). The client never closes the
    /// end it passed (a request the service refused after queuing it has closed what it carried).
    fn call(&mut self, method: u32, body: Vec<u8>, objects: Vec<Object>) -> Result<Vec<u8>, i64>;
    /// A oneway message to the service; `-EPIPE` or `-ENOENT` when it is gone, `-EAGAIN` when the
    /// service's inbox is full (the message is not delivered; the client keeps the ones that must
    /// arrive and sends them again, `crate::outbox`).
    fn send(&mut self, method: u32, body: Vec<u8>) -> Result<(), i64>;
    /// A fresh channel pair: `(kept, sent)`. `sent` moves to the service with `CreateView`; the
    /// events arrive on `kept`.
    fn create_pair(&mut self) -> Result<(u64, u64), i64>;
    /// The next event waiting on `channel`, or `None` when there is none. Never parks. `-EPIPE`
    /// when the service's end is gone.
    fn recv(&mut self, channel: u64) -> Result<Option<RawEvent>, i64>;
    /// New shared memory of at least `bytes` bytes (`bytes > 0`).
    fn alloc(&mut self, bytes: usize) -> Result<Self::Mem, i64>;
    /// Close a channel handle this task holds.
    fn close_channel(&mut self, handle: u64);
}

/// A transport bound to the full interface (`os.lazy.webd.view.v1`: `call`, `send` and `recv` use
/// it) that can also make one call through another interface of the same service: a full view's
/// `CreateView` goes through the scope it asks for (`os.lazy.webd.net.v1`, `os.lazy.webd.browser.v1`),
/// which the kernel checks against the caller's manifest. Events arrive as `os.lazy.webd.view.v1`.
pub trait ScopedTransport: Transport {
    /// [`Transport::call`] with the request's interface id set to `interface`.
    fn call_through(
        &mut self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Result<Vec<u8>, i64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_channels_still_held_are_closed() {
        let objects = [Object::Channel(4), Object::Buffer(5), Object::Channel(6)];
        let mut closed = Vec::new();
        close_unqueued(&objects, |handle| handle == 4, |handle| closed.push(handle));
        assert_eq!(closed, vec![4], "5 is a buffer, 6 was moved away");
        let mut closed = Vec::new();
        close_unqueued(&objects, |_| true, |handle| closed.push(handle));
        assert_eq!(closed, vec![4, 6]);
    }
}
