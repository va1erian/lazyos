//! [`Errno`]: the errno numbers, named.

use core::fmt;

use crate::errno as table;

/// One errno, kept as the positive number of the shared table
/// ([`crate::errno`]); a syscall returns it negated ([`Errno::from_syscall`],
/// [`Errno::to_syscall`]). A newtype so a caller writes `Errno::INVAL`
/// instead of `Err(-22)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Errno(i64);

macro_rules! errnos {
    ($($name:ident = $value:ident, $text:literal;)*) => {
        impl Errno {
            $(
                #[doc = $text]
                pub const $name: Errno = Errno(table::$value);
            )*

            /// The constant's name without the `E`, or `None` for a number
            /// the table does not name.
            pub fn name(self) -> Option<&'static str> {
                match self.0 {
                    $(table::$value => Some(stringify!($name)),)*
                    _ => None,
                }
            }

            /// Friendly text for the number (`docs/messenger.md` section 12).
            pub fn describe(self) -> &'static str {
                match self.0 {
                    $(table::$value => $text,)*
                    _ => "the call failed",
                }
            }
        }
    };
}

errnos! {
    PERM = EPERM, "not permitted";
    NOENT = ENOENT, "no such service, file or name";
    SRCH = ESRCH, "no such task";
    INTR = EINTR, "interrupted by a signal";
    IO = EIO, "input/output error";
    TOOBIG = E2BIG, "too large for the buffer";
    BADF = EBADF, "bad descriptor";
    AGAIN = EAGAIN, "try again later";
    NOMEM = ENOMEM, "out of memory or quota";
    ACCES = EACCES, "permission denied";
    FAULT = EFAULT, "bad address";
    BUSY = EBUSY, "busy";
    EXIST = EEXIST, "already exists";
    NODEV = ENODEV, "no such device";
    INVAL = EINVAL, "invalid argument";
    MFILE = EMFILE, "too many open handles";
    NOSPC = ENOSPC, "no space left";
    PIPE = EPIPE, "the service is gone";
    DEADLK = EDEADLK, "the call would deadlock";
    NAMETOOLONG = ENAMETOOLONG, "name too long";
    NOSYS = ENOSYS, "not implemented";
    BADMSG = EBADMSG, "malformed message";
    MSGSIZE = EMSGSIZE, "message too large";
    NOTSUP = ENOTSUP, "not supported";
    TIMEDOUT = ETIMEDOUT, "the service did not answer in time";
    DQUOT = EDQUOT, "quota exceeded";
    CANCELED = ECANCELED, "canceled";
}

impl Errno {
    /// The errno with this (positive) number.
    pub const fn new(number: i64) -> Errno {
        Errno(number)
    }

    /// The positive number, as in [`crate::errno`].
    pub const fn number(self) -> i64 {
        self.0
    }

    /// The errno behind a syscall's return value (`-22` is `INVAL`); a
    /// positive value is taken as it is.
    pub const fn from_syscall(code: i64) -> Errno {
        Errno(code.saturating_abs())
    }

    /// The negative value a syscall returns.
    pub const fn to_syscall(self) -> i64 {
        -self.0
    }
}

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => write!(f, "{} (E{name})", self.describe()),
            None => write!(f, "{} (error {})", self.describe(), self.0),
        }
    }
}
