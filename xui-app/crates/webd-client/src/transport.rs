//! What the client needs of the OS, as a trait: `xui_app::platform::webd` implements [`Transport`]
//! over Messenger on LazyOS, the host tests over the service half's in-memory fabric. The client
//! never holds a raw handle longer than it must: a channel pair is closed by [`Transport::close_channel`],
//! shared memory by dropping its [`SharedMem`].

use libmessenger::Object;

/// The registered name `webd` serves the light interface under.
pub const SERVICE_NAME: &str = "app.os.lazy.webd.light";

/// Positive errno values the client reads (`docs/midl.md`, "Errors"); transports return them
/// negated, as the kernel does.
pub mod errno {
    pub const ENOENT: i64 = 2;
    pub const ENOMEM: i64 = 12;
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
}

/// One event off a view's endpoint: the method id and its TLV body.
pub type RawEvent = (u32, Vec<u8>);

/// How requests reach `webd` and its events reach the app. Errors are negative errno values.
pub trait Transport {
    type Mem: SharedMem;

    /// One call to the service (resolving it first, bounded: a UI thread must not park for long)
    /// carrying `objects`; the reply body, or the negative errno of the transport or of the
    /// service's refusal. `-ENOENT` also means "the service is not running".
    fn call(&mut self, method: u32, body: Vec<u8>, objects: Vec<Object>) -> Result<Vec<u8>, i64>;
    /// A oneway message to the service; `-EPIPE` or `-ENOENT` when it is gone.
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
