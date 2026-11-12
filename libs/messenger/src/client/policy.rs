//! [`Retry`] and [`CallFailure`]: when a call that lost its peer may be sent
//! again.
//!
//! A resend after a service died is only harmless when the service did not
//! run the request, or when running it twice changes nothing. The kernel
//! tells a request it never queued (a closed endpoint: `begin_call` fails
//! `EPIPE`, a stale handle: `ENOENT`) from one the peer took and died
//! holding (the transaction ends `PeerDied`, also `EPIPE`) only if the
//! transport issues the two halves of a call separately, so the transports
//! do and report the difference in [`CallFailure::delivered`].

use super::errno::Errno;

/// Whether a method may be sent again after the peer died while handling it
/// (`retry=` in the `.midl`, `docs/midl.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Retry {
    /// Never resend a request the peer may have run: the error is returned,
    /// the handle is dropped and the next call reconnects. The default: a
    /// launch, a mount or a password change must not run twice.
    #[default]
    Never,
    /// A read or a set-to-a-value write: running it again is harmless, so
    /// the call is resent once to a restarted service.
    Safe,
}

/// A failed [`super::Transport::call`]: the errno, and whether the peer
/// may have received the request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CallFailure {
    /// What the kernel answered.
    pub errno: Errno,
    /// `false` only when the request provably never reached the peer's
    /// queue, so a resend cannot run it twice whatever the method's
    /// [`Retry`]. A transport that cannot tell says `true`.
    pub delivered: bool,
}

impl CallFailure {
    /// The request was never queued: the kernel refused it up front.
    pub const fn undelivered(errno: Errno) -> CallFailure {
        CallFailure {
            errno,
            delivered: false,
        }
    }

    /// The request was queued (or may have been); the failure came after.
    pub const fn delivered(errno: Errno) -> CallFailure {
        CallFailure {
            errno,
            delivered: true,
        }
    }
}
