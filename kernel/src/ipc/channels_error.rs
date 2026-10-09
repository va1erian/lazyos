//! The channel error type (split out of `channels.rs`, issue #194).

/// Why a channel operation failed. Messages are user-facing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The handle is unused or out of range.
    InvalidHandle,
    /// The handle exists but does not name a channel endpoint.
    WrongKind,
    /// The handle lacks the right the operation needs.
    MissingRight,
    /// The process is holding the maximum number of handles.
    NoFreeHandle,
    /// No task exists in the current slot.
    BadTask,
    /// The parcel is malformed or exceeds the Messenger size limits.
    BadParcel,
    /// The kernel channel registry is full.
    RegistryFull,
    /// The peer's inbox is at its depth or byte limit.
    QueueFull,
    /// The channel already has [`super::MAX_OUTSTANDING`] pending transactions.
    TooManyOutstanding,
    /// This sender already has [`super::MAX_PENDING_PER_SENDER`] pending transactions.
    Quota,
    /// The sender's *user* is over its per-uid queue quota (issue #103).
    QuotaExceeded,
    /// The call would form a synchronous cycle on this channel pair.
    Deadlock,
    /// No pending transaction has that id.
    NoTransaction,
    /// Only the task that started the transaction may cancel it.
    NotCaller,
    /// The deadline passed before a reply arrived.
    TimedOut,
    /// The caller canceled the transaction.
    Canceled,
    /// The endpoint on the other side was closed.
    PeerDied,
    /// A channel end appears twice in the object list (one handle, one move).
    BadTransfer,
    /// Objects are only carried by requests, never by replies.
    UnsupportedTransfer,
    /// An object-list entry names a handle of the other kind: a buffer in a
    /// channel slot, or a channel end in a buffer slot.
    WrongObjectKind,
    /// The request's object list is not what its interface and method
    /// declare in `.midl` (length, kinds or order; issue #516).
    UndeclaredObject,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::InvalidHandle => "that Messenger handle does not exist",
            Error::WrongKind => "that handle does not name a channel endpoint",
            Error::MissingRight => "this handle does not grant the right to use the channel",
            Error::NoFreeHandle => "the process is holding too many Messenger handles",
            Error::BadTask => "no task exists in that slot",
            Error::BadParcel => "the parcel is malformed or exceeds a Messenger size limit",
            Error::RegistryFull => "the kernel channel registry is full",
            Error::QueueFull => "the peer's message queue is full",
            Error::TooManyOutstanding => "this channel has too many outstanding transactions",
            Error::Quota => "this sender has too many outstanding transactions",
            Error::QuotaExceeded => "this user is over its Messenger queue quota",
            Error::Deadlock => {
                "this call would deadlock: the peer is waiting on a call in the other direction"
            }
            Error::NoTransaction => "no transaction with that id is outstanding",
            Error::NotCaller => "only the task that started a transaction may cancel it",
            Error::TimedOut => "the deadline passed before a reply arrived",
            Error::Canceled => "the caller canceled this transaction",
            Error::PeerDied => "the endpoint on the other side was closed",
            Error::BadTransfer => "a channel end appears twice in the message's objects",
            Error::UnsupportedTransfer => "replies cannot carry objects",
            Error::WrongObjectKind => {
                "an object of the message is not of the kind its slot declares"
            }
            Error::UndeclaredObject => "this request's objects are not what its method declares",
        }
    }
}
