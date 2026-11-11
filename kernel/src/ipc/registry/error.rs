//! Why a registry operation failed.

/// Why a registry operation failed. Messages are user-facing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The name is empty, too long, or contains a control character.
    BadName,
    /// The endpoint handle does not name a channel or endpoint.
    BadEndpoint,
    /// The name is already registered by someone else.
    NameTaken,
    /// No name by that spelling is registered.
    UnknownName,
    /// The actor may not unregister this name.
    NotOwner,
    /// More interfaces than [`MAX_INTERFACES`].
    TooManyInterfaces,
    /// The lease length is so large its deadline would overflow the tick clock.
    BadLease,
    /// The registry is full.
    RegistryFull,
    /// The target task slot holds no live task.
    BadTask,
    /// The target task is out of handles or the kernel is out of memory.
    NoResources,
    /// The name policy refuses the caller (a wait re-checks it when the name
    /// appears).
    Denied,
    /// A wait for a name reached its deadline.
    TimedOut,
    /// A wait was cancelled because its task is being killed.
    Canceled,
    /// The task already waits for a name, or the kernel's waiter table is full.
    TooManyWaiters,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::BadName => "that service name is empty, too long, or has control characters",
            Error::BadEndpoint => "a service name must refer to a channel or endpoint handle",
            Error::NameTaken => "that service name is already registered by another owner",
            Error::UnknownName => "no service is registered under that name",
            Error::NotOwner => "only the owner (or an administrator) may unregister that name",
            Error::TooManyInterfaces => "the registration declares too many interfaces",
            Error::BadLease => "that lease is too long for the tick clock to represent",
            Error::RegistryFull => "the kernel name registry is full",
            Error::BadTask => "no task exists in that slot",
            Error::NoResources => "the target process is out of Messenger handles",
            Error::Denied => "the name policy does not let this program use that service name",
            Error::TimedOut => "the service did not register in time",
            Error::Canceled => "the wait for the service was cancelled",
            Error::TooManyWaiters => "too many programs are waiting for services to register",
        }
    }
}
