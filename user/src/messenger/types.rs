//! Wire ABI blocks, stats snapshots, and the [`Error`] type shared by every
//! Messenger client and service protocol.
//!
//! The ABI blocks below mirror `kernel/src/ipc/syscalls.rs` field for field;
//! keep them in lockstep (the kernel pins the sizes at compile time).

use super::errno;
use libmessenger::Error as ParcelError;

/// The syscall blocks and the compact counters, mirrored once in
/// `lazyos-sys` (and checked against the kernel there).
pub use lazyos_sys::msg::{MsgArgs, MsgResult, Stats, EXPIRED_DEADLINE};

/// The versioned fabric snapshot and its per-slot rows, decoded once in
/// `lazyos-sys`.
pub use lazyos_sys::msg::{FabricStats, TaskUsage};

/// A Messenger or kernel error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The kernel refused the operation with a negative errno.
    Errno(i64),
    /// The registry daemon refused the request with a positive errno-style
    /// code (`registry::serve_request` carries it across the channel).
    Registry(i64),
    /// The topics broker refused the request with a positive errno-style code
    /// (`topics` carries it in the reply's `ERROR` field).
    Topics(i64),
    /// The MIME service refused the request with a positive errno-style code
    /// (`mimed` carries it in the reply's `ERROR` field).
    Mime(i64),
    /// The supervisor refused the request with a positive errno-style code
    /// (`init` carries it in the reply's `ERROR` field).
    Init(i64),
    /// The configuration registry refused the request with a `CONFD_*` code
    /// (`confd` carries it in the reply's `ERROR` field).
    Confd(i64),
    /// A parcel was malformed on encode or decode.
    Parcel(ParcelError),
}

impl From<messenger_generated::topics::TopicError> for Error {
    /// A malformed generated topic name is `EINVAL`; an encode failure keeps
    /// the parcel error so callers can tell the two apart.
    fn from(error: messenger_generated::topics::TopicError) -> Self {
        match error {
            messenger_generated::topics::TopicError::Encode(parcel) => Error::Parcel(parcel),
            _ => Error::Errno(-errno::EINVAL),
        }
    }
}

impl Error {
    /// The negative errno the kernel returned, if this is a kernel error.
    pub fn errno(self) -> Option<i64> {
        match self {
            Error::Errno(code) => Some(code),
            // Registry and broker codes travel positive; normalise to the
            // syscall shape.
            Error::Registry(code) | Error::Topics(code) | Error::Mime(code) | Error::Init(code) => {
                Some(-code)
            }
            Error::Confd(code) => Some(-code),
            Error::Parcel(_) => None,
        }
    }

    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::Parcel(error) => error.message(),
            Error::Registry(code) => registry_message(code),
            Error::Topics(code) => topics_message(code),
            Error::Mime(code) => mime_message(code),
            Error::Init(code) => init_message(code),
            Error::Confd(code) => confd_message(code),
            // A match guard keeps the named constants readable; a bare
            // `-CONST` is not a valid pattern.
            Error::Errno(code) => match code {
                code if code == -errno::EPERM => {
                    "this app is not allowed to perform that Messenger operation"
                }
                code if code == -errno::ENOENT => "no service is registered under that name",
                code if code == -errno::E2BIG => {
                    "the parcel or reply exceeds the Messenger buffer limit"
                }
                code if code == -errno::EAGAIN => "the peer's queue is full; try again shortly",
                code if code == -errno::ENOMEM => "the kernel is out of Messenger resources",
                code if code == -errno::EACCES => {
                    "this app is not allowed to make that Messenger call"
                }
                code if code == -errno::EFAULT => "a Messenger buffer pointer is invalid",
                code if code == -errno::EBUSY => "the bootstrap endpoint has already been claimed",
                code if code == -errno::EEXIST => "that service name is already registered",
                code if code == -errno::EINVAL => "the Messenger request is malformed",
                code if code == -errno::EPIPE => "the other end of the channel closed",
                code if code == -errno::EDEADLK => {
                    "the call would deadlock with an open transaction"
                }
                code if code == -errno::EBADMSG => {
                    "the wrapped value failed authentication or is malformed"
                }
                code if code == -errno::ETIMEDOUT => "the call timed out before a reply arrived",
                code if code == -errno::ECANCELED => "the call was canceled",
                _ => "the Messenger call failed",
            },
        }
    }
}

/// Friendly text for a registry error code crossing the daemon protocol. The
/// strings mirror `kernel/src/ipc/registry.rs` so both paths explain a failure
/// the same way.
fn registry_message(code: i64) -> &'static str {
    if code == errno::EPERM {
        "only the owner (or an administrator) may unregister that name"
    } else if code == errno::ENOENT {
        "no service is registered under that name"
    } else if code == errno::EEXIST {
        "that service name is already registered by another owner"
    } else if code == errno::ENOMEM {
        "the kernel name registry is full"
    } else if code == errno::EACCES {
        "this app is not allowed to use the name registry"
    } else {
        "the registry request is malformed"
    }
}

/// Friendly text for a topics-broker error code crossing the daemon protocol.
fn topics_message(code: i64) -> &'static str {
    if code == errno::EACCES {
        "this app is not allowed to use that topic segment"
    } else if code == errno::ENOENT {
        "no such topic subscription"
    } else if code == errno::EINVAL {
        "the topic name or filter is malformed"
    } else if code == errno::EPERM {
        "that subscription belongs to another task"
    } else if code == errno::E2BIG {
        "the event payload exceeds the topic broker's limit"
    } else if code == errno::ENOMEM {
        "the topic broker is out of subscription slots"
    } else {
        "the topic request failed"
    }
}

/// Friendly text for a MIME-service error code crossing the daemon protocol.
fn mime_message(code: i64) -> &'static str {
    if code == errno::ENOENT {
        "no application is registered for that file type and verb"
    } else if code == errno::EINVAL {
        "the MIME request is malformed"
    } else if code == errno::E2BIG {
        "the MIME reply exceeds the Messenger buffer limit"
    } else {
        "the MIME request failed"
    }
}

/// Friendly text for a supervisor (`init`) error code crossing the protocol.
fn init_message(code: i64) -> &'static str {
    if code == errno::EPERM {
        "this task may not launch into that session"
    } else if code == errno::ENOENT {
        "no such app, session, or program"
    } else if code == errno::EINVAL {
        "the launch request is malformed"
    } else if code == errno::ENOMEM {
        "the supervisor has no free task slot"
    } else if code == errno::EAGAIN {
        "this session already has too many launched apps running; try again once one exits"
    } else {
        "the supervisor request failed"
    }
}

/// Friendly text for a `confd` error code crossing the protocol. The constants
/// live with the wire module so the service and client agree on one set.
fn confd_message(code: i64) -> &'static str {
    use super::confd;
    if code == confd::CONFD_NOT_FOUND {
        "no value is stored at that path"
    } else if code == confd::CONFD_BAD_PATH {
        "that is not a valid confd path"
    } else if code == confd::CONFD_TOO_LARGE {
        "the value or store exceeds a confd size limit"
    } else if code == confd::CONFD_DENIED {
        "the caller may not access that path"
    } else if code == confd::CONFD_BAD_VALUE {
        "the request carried a malformed confd value"
    } else if code == confd::CONFD_IO {
        "the confd store could not be read or written"
    } else {
        "the confd request failed"
    }
}

/// Result alias for the userspace API.
pub type Result<T> = core::result::Result<T, Error>;

/// Default reply/receive buffer for the convenience methods. A reply that does
/// not fit is refused with `-E2BIG` *after* the transaction completes, so the
/// bytes are lost; a streaming/shared-buffer path is the follow-up for large
/// payloads (`docs/messenger.md` section 2).
pub const DEFAULT_BUFFER: usize = 16 * 1024;
