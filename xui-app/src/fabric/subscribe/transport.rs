//! The byte-level seam under [`super::Subscriber`]: the real syscall-5
//! transport and the trait a test double implements.

use libmessenger::client::Deadline;

use crate::sys;

use super::super::error::{EINVAL, ENOENT, EPIPE};
use super::super::registry::{release, resolve};
use super::{Errno, ETIMEDOUT};

/// What the subscriber needs from the kernel. Errors are negative errno.
pub trait Transport {
    /// Send the encoded `request` to the broker and read its reply into
    /// `reply`; returns the reply length. `until` is the method's own bound
    /// (`topics::METHOD_*_DEADLINE`, from `idl/topics.midl`), or
    /// [`Deadline::Poll`]: the broker answers in its service turn or the call
    /// ends at once with `-ETIMEDOUT` ("nothing ready").
    fn call(&mut self, request: &[u8], reply: &mut [u8], until: Deadline) -> Result<usize, Errno>;

    /// A fresh channel pair, both ends open in this task.
    fn create_pair(&mut self) -> Result<(u64, u64), Errno>;

    /// Close a channel end this task owns (never the shared broker handle).
    fn close(&mut self, handle: u64);

    /// Take one queued message from `handle` without blocking: its length,
    /// or `None` when nothing is queued. A closed peer is `-EPIPE`.
    fn recv(&mut self, handle: u64, buf: &mut [u8]) -> Result<Option<usize>, Errno>;
}

/// The topics-broker well-known name (same as [`super::super::TOPICS_NAME`]).
const BROKER_NAME: &str = super::super::TOPICS_NAME;
/// The poll deadline: never blocks, see `docs/architecture/ipc-core.md`.
const POLL_DEADLINE: u64 = sys::EXPIRED_DEADLINE;

/// The syscall-5 transport. The broker endpoint is resolved on first use and
/// cached: it is a handle every client of the service shares, so it is
/// `release`d, never closed (a close would end the broker's receiving side
/// for everyone).
#[derive(Default)]
pub struct SysTransport {
    endpoint: Option<u64>,
}

impl SysTransport {
    /// Nothing resolved yet.
    pub const fn new() -> SysTransport {
        SysTransport { endpoint: None }
    }

    fn endpoint(&mut self) -> Result<u64, Errno> {
        match self.endpoint {
            Some(handle) => Ok(handle),
            None => {
                let handle = resolve(BROKER_NAME)?;
                self.endpoint = Some(handle);
                Ok(handle)
            }
        }
    }
}

impl Transport for SysTransport {
    fn call(&mut self, request: &[u8], reply: &mut [u8], until: Deadline) -> Result<usize, Errno> {
        let handle = self.endpoint()?;
        // Never unbounded on a UI thread. `check_call_bounds.py` names this
        // file as the one raw caller: its bound is the method's generated
        // deadline.
        let deadline = match until {
            Deadline::Never => return Err(-EINVAL),
            until => until.absolute_ticks(sys::clock_ticks()),
        };
        let result = lazyos_sys::msg::call(handle, request, reply, deadline);
        if let Err(code) = result {
            // A handle naming a dead endpoint cannot recover: drop the alias
            // and resolve afresh next time. A timeout keeps it. `EPIPE` also
            // answers a request the broker dropped as too large while it is
            // alive, which is why this is a `release` and not a `close`.
            if code == -ENOENT || code == -EPIPE {
                self.endpoint = None;
                let _ = release(handle);
            }
        }
        result
    }

    fn create_pair(&mut self) -> Result<(u64, u64), Errno> {
        lazyos_sys::msg::create_pair()
    }

    fn close(&mut self, handle: u64) {
        let _ = lazyos_sys::msg::close(handle);
    }

    fn recv(&mut self, handle: u64, buf: &mut [u8]) -> Result<Option<usize>, Errno> {
        // The counters first: an expired-deadline `recv` on an empty queue
        // parks until the next tick, which a UI thread must not pay on every
        // look at a bell. (An error from the counters falls through to the
        // `recv`, which reports a closed peer.)
        if lazyos_sys::msg::queued(handle) == Ok(0) {
            return Ok(None);
        }
        match lazyos_sys::msg::recv(handle, buf, POLL_DEADLINE) {
            Ok(result) => Ok(Some(result.bytes as usize)),
            Err(code) if code == -ETIMEDOUT => Ok(None),
            Err(code) => Err(code),
        }
    }
}

impl Drop for SysTransport {
    fn drop(&mut self) {
        if let Some(handle) = self.endpoint.take() {
            let _ = release(handle);
        }
    }
}
