//! What the service needs of Messenger, as a trait: the program that hosts it implements [`Fabric`]
//! over the kernel's channels and buffers (slice 2), the tests over `mem`. The service never holds a
//! raw handle for longer than it must: every object a request carries is closed, mapped or kept
//! by the service, exactly once.

/// Positive errno values the service answers with (`docs/midl.md`, "Errors").
pub mod errno {
    pub const ENOENT: i64 = 2;
    pub const E2BIG: i64 = 7;
    pub const EAGAIN: i64 = 11;
    pub const EACCES: i64 = 13;
    pub const EBUSY: i64 = 16;
    pub const ENODEV: i64 = 19;
    pub const EINVAL: i64 = 22;
    pub const ENOSPC: i64 = 28;
    pub const EPIPE: i64 = 32;
    pub const ENOTSUP: i64 = 95;
    pub const EDQUOT: i64 = 122;
}

/// A client's slot memory, mapped for writing. Dropping it unmaps and closes the buffer handle.
pub trait Mapping {
    /// Bytes available (the declared range of the buffer, checked against what is really mapped).
    fn capacity(&self) -> usize;
    /// Bytes the kernel mapped for the buffer: all of it, not just the declared range. This is
    /// what the client's slot quota is charged, because it is what webd holds the client's memory
    /// for, whatever the client declared.
    fn mapped_bytes(&self) -> u64;
    /// Copy `bytes` to the start of the range. The service checks `bytes.len() <= capacity()` first.
    fn write(&mut self, bytes: &[u8]);
}

/// The service's side of the Messenger fabric. Errors are negative errno values, as the kernel
/// returns them.
pub trait Fabric {
    /// Send a oneway event `method` with `body` on `channel` (a client's event endpoint). `-EPIPE` means the
    /// client is gone; `-EAGAIN` that its queue is full (the event is lost, the client is slow).
    fn send(&self, channel: u64, method: u32, body: Vec<u8>) -> Result<(), i64>;
    /// Close a channel handle the service owns.
    fn close_channel(&self, channel: u64);
    /// Close a buffer handle the service owns without using it.
    fn close_buffer(&self, handle: u64);
    /// Map the range `offset..offset + len` of `buffer` for writing, taking over the handle: it is
    /// closed when the mapping drops, and at once on failure. `-EINVAL` when the range does not fit
    /// the buffer's real size. The mapping reports that size ([`Mapping::mapped_bytes`]).
    fn map(&self, buffer: libmessenger::Buffer) -> Result<Box<dyn Mapping>, i64>;
    /// Copy the range out of `buffer` and close the handle (also on failure).
    fn read(&self, buffer: libmessenger::Buffer) -> Result<Vec<u8>, i64>;
}
