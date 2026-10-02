//! The seam between the `msg` bindings and the Messenger fabric.
//!
//! [`Bus`] is to Messenger what [`crate::Host`] is to files and stdio: the
//! bindings only speak parcel *bodies* through it, so they are tested on the
//! developer's machine against an in-memory fabric (`tests::msg_mock`), and the
//! real implementation (`super::gate`, the native `int 0x80` gate) stays a thin
//! syscall wrapper. Every error carries a ready-to-show message.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A failed fabric operation: a negative errno from the kernel, or the
/// positive code of a service's structured error, plus friendly text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusError {
    pub code: i64,
    pub message: String,
}

impl BusError {
    /// A kernel failure (`code` is a negative errno).
    pub fn errno(code: i64) -> Self {
        let message = match errno_name(code.unsigned_abs()) {
            Some((name, text)) => format!("{text} ({name})"),
            None => format!("error {code}"),
        };
        Self { code, message }
    }

    /// Whether the endpoint is gone and a fresh `resolve` may help.
    pub fn is_dead_peer(&self) -> bool {
        self.code == -EPIPE || self.code == -ENOENT
    }

    pub fn is_timeout(&self) -> bool {
        self.code == -ETIMEDOUT
    }
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

pub const ENOENT: i64 = 2;
pub const EPIPE: i64 = 32;
pub const ETIMEDOUT: i64 = 110;

/// The symbolic name and meaning of the errno values services return.
pub fn errno_name(code: u64) -> Option<(&'static str, &'static str)> {
    Some(match code {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such service or entry"),
        5 => ("EIO", "input/output error"),
        7 => ("E2BIG", "message too large"),
        11 => ("EAGAIN", "try again"),
        12 => ("ENOMEM", "out of memory"),
        13 => ("EACCES", "permission denied"),
        16 => ("EBUSY", "busy"),
        17 => ("EEXIST", "already exists"),
        22 => ("EINVAL", "invalid argument"),
        28 => ("ENOSPC", "no space left"),
        32 => ("EPIPE", "the service went away"),
        38 => ("ENOSYS", "not implemented"),
        110 => ("ETIMEDOUT", "timed out"),
        122 => ("EDQUOT", "quota exceeded"),
        _ => return None,
    })
}

/// How long a call or a receive may wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Until the reply or request comes.
    Forever,
    /// At most this many milliseconds.
    Ms(u64),
    /// Not at all: take what is ready now (the kernel's poll deadline). A
    /// window's timer uses this, so a form never stops painting.
    Poll,
}

impl Wait {
    /// A script's timeout: `0` waits forever.
    pub fn from_timeout_ms(ms: u64) -> Wait {
        if ms == 0 {
            Wait::Forever
        } else {
            Wait::Ms(ms)
        }
    }

    /// The bounded wait in milliseconds (`0` for [`Wait::Forever`] and
    /// [`Wait::Poll`]).
    pub fn millis(self) -> u64 {
        match self {
            Wait::Ms(ms) => ms,
            Wait::Forever | Wait::Poll => 0,
        }
    }
}

/// One request received on a served endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub interface: u64,
    pub method: u32,
    /// The transaction to answer, `None` for a one-way message.
    pub txn: Option<u64>,
    pub body: Vec<u8>,
}

/// Synchronous access to the fabric. Handles are this process's endpoint
/// handles; bodies are TLV bodies (the envelope is the implementation's job).
pub trait Bus {
    /// Resolve a registered service name to an endpoint handle. A resolved
    /// handle must stay open for the life of the process (closing it is peer
    /// death for the service), so callers cache it.
    fn resolve(&self, name: &str) -> Result<u64, BusError>;
    /// Call `method` and wait for the reply body as `wait` allows; a reply
    /// that did not come in time is an `ETIMEDOUT` error.
    fn call(
        &self,
        endpoint: u64,
        interface: u64,
        method: u32,
        body: &[u8],
        wait: Wait,
    ) -> Result<Vec<u8>, BusError>;
    /// Send a one-way message; returns once it is queued.
    fn send(&self, endpoint: u64, interface: u64, method: u32, body: &[u8])
        -> Result<(), BusError>;
    /// Every registered service name.
    fn names(&self) -> Result<Vec<String>, BusError>;

    /// Serving: create an endpoint pair, publish one side under `name`
    /// (declaring `interfaces`), and return the side to receive on.
    fn register(&self, name: &str, interfaces: &[u64]) -> Result<u64, BusError>;
    /// Serving: the next request on `endpoint`, waiting as `wait` allows;
    /// `Ok(None)` when nothing arrived in time.
    fn recv(&self, endpoint: u64, wait: Wait) -> Result<Option<Incoming>, BusError>;
    /// Serving: answer transaction `txn` with `body`. A caller that already
    /// gave up is not an error (the reply is dropped).
    fn reply(&self, txn: u64, interface: u64, method: u32, body: &[u8]) -> Result<(), BusError>;
    /// Serving: withdraw `name` (registered by [`Bus::register`]) and close
    /// its receive side, so callers fail fast instead of timing out.
    fn unregister(&self, name: &str, endpoint: u64) -> Result<(), BusError>;
    /// Milliseconds on a monotonic clock (for `msg::run(ms)`).
    fn clock_ms(&self) -> u64;
}
