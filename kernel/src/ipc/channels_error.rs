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
    /// A transferred handle is missing, of the wrong kind, or lacks the right
    /// to be moved.
    BadTransfer,
    /// Transfers are only carried by messages, not by replies (yet).
    UnsupportedTransfer,
    /// The request carries more handles or buffers than its interface and
    /// method declare in `.midl` (issue #516).
    UndeclaredTransfer,
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
            Error::BadTransfer => {
                "a transferred handle does not exist or does not grant the transfer right"
            }
            Error::UnsupportedTransfer => "replies cannot carry handles or buffers yet",
            Error::UndeclaredTransfer => {
                "this request carries handles or buffers its method does not declare"
            }
        }
    }
}
