//! [`CallError`]: why a call failed, and the standard error field it is
//! decoded from.

use alloc::string::{String, ToString};
use core::convert::Infallible;
use core::fmt;

use super::errno::Errno;
use crate::{Decoder, Kind, Parcel};

/// The id of the standard error field every reply may carry instead of its
/// declared fields (`docs/midl.md`, "Errors"). `midlc` generates the same
/// number as `messenger_generated::errors::ERROR_FIELD`; a test in
/// `libs/generated/tests/` keeps the two equal.
pub const ERROR_FIELD: u16 = 15;

/// The optional record behind an error's message (`docs/midl.md`, "Errors"):
/// each part is empty when the service did not write it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detail {
    /// Which code space `code` belongs to; empty for errno-style codes.
    pub domain: String,
    /// How to fix it.
    pub hint: String,
    /// A documentation id such as `err.messenger.denied`.
    pub docs: String,
}

/// Why a call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError<E = Infallible> {
    /// The call never completed: the service is not registered, died, did not
    /// answer in time, or the request could not be sent.
    Transport(Errno),
    /// The service answered with the standard error field.
    Remote {
        /// Errno-style (positive) unless `detail.domain` says otherwise.
        code: u32,
        /// The service's own sentence, ready to show.
        message: String,
        /// The optional detail record.
        detail: Detail,
    },
    /// The service's declared error, mapped from a `Remote` by a generated
    /// client (`Infallible` for the untyped [`super::Connection`]).
    Service(E),
}

impl<E> CallError<E> {
    /// Re-type the service error.
    pub fn map_service<F>(self, map: impl FnOnce(E) -> F) -> CallError<F> {
        match self {
            CallError::Transport(errno) => CallError::Transport(errno),
            CallError::Remote {
                code,
                message,
                detail,
            } => CallError::Remote {
                code,
                message,
                detail,
            },
            CallError::Service(error) => CallError::Service(map(error)),
        }
    }

    /// The error of a request a generated client could not encode (a field
    /// over its limit): the caller's fault, `EINVAL`, and nothing was sent.
    pub fn bad_request(_: crate::Error) -> CallError<E> {
        CallError::Transport(Errno::INVAL)
    }

    /// The error of a reply that does not decode as the method's declared
    /// reply: the service answered with a malformed message, `EBADMSG`.
    pub fn bad_reply(_: crate::Error) -> CallError<E> {
        CallError::Transport(Errno::BADMSG)
    }

    /// Whether the call timed out: a poll that found nothing, or a service
    /// that did not answer.
    pub fn is_timeout(&self) -> bool {
        matches!(self, CallError::Transport(errno) if *errno == Errno::TIMEDOUT)
    }

    /// The error with its declared service error spelled out as a refusal:
    /// `describe` gives the code and the sentence. For a caller that shows
    /// or forwards any service's failure (`code`, `message` and `Display`
    /// work on the untyped form).
    pub fn flatten(self, describe: impl FnOnce(E) -> (u32, String)) -> CallError {
        match self {
            CallError::Transport(errno) => CallError::Transport(errno),
            CallError::Remote {
                code,
                message,
                detail,
            } => CallError::Remote {
                code,
                message,
                detail,
            },
            CallError::Service(error) => {
                let (code, message) = describe(error);
                CallError::Remote {
                    code,
                    message,
                    detail: Detail::default(),
                }
            }
        }
    }
}

impl CallError {
    /// The error as the negative errno-style number older call sites
    /// matched on: the kernel's errno, or the service's code negated.
    pub fn code(&self) -> i64 {
        match self {
            CallError::Transport(errno) => errno.to_syscall(),
            CallError::Remote { code, .. } => -i64::from(*code),
            CallError::Service(never) => match *never {},
        }
    }

    /// A refusal with this errno-style `code` and friendly `message`, for
    /// a caller that has its own sentence (no detail record).
    pub fn refusal(code: u32, message: &str) -> CallError {
        CallError::Remote {
            code,
            message: message.to_string(),
            detail: Detail::default(),
        }
    }

    /// Plain text to show: the service's sentence for a refusal, the
    /// errno's description for a transport failure.
    pub fn message(&self) -> &str {
        match self {
            CallError::Transport(errno) => errno.describe(),
            CallError::Remote { message, .. } => message,
            CallError::Service(never) => match *never {},
        }
    }

    /// Re-type an untyped error for a client with a declared error enum
    /// (`docs/client-api-plan.md` 3.2): a refusal whose code `from_code`
    /// names becomes [`CallError::Service`]; any other stays what it was.
    pub fn lift<E>(self, from_code: impl FnOnce(u32) -> Option<E>) -> CallError<E> {
        match self {
            CallError::Remote {
                code,
                message,
                detail,
            } => match from_code(code) {
                Some(error) => CallError::Service(error),
                None => CallError::Remote {
                    code,
                    message,
                    detail,
                },
            },
            other => other.widen(),
        }
    }

    /// Re-type an untyped error for a client with a declared error enum.
    pub fn widen<E>(self) -> CallError<E> {
        self.map_service(|never| match never {})
    }
}

/// For callers that report a plain negative errno (`?` in a function that
/// returns `Result<_, i64>`).
impl From<CallError> for i64 {
    fn from(error: CallError) -> i64 {
        error.code()
    }
}

impl<E: fmt::Display> fmt::Display for CallError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Transport(errno) => write!(f, "the call failed: {errno}"),
            CallError::Remote {
                code,
                message,
                detail,
            } => {
                if message.is_empty() {
                    write!(f, "the service refused the call (code {code})")?;
                } else {
                    f.write_str(message)?;
                }
                if !detail.hint.is_empty() {
                    write!(f, " ({})", detail.hint)?;
                }
                Ok(())
            }
            CallError::Service(error) => error.fmt(f),
        }
    }
}

/// The structured error in `reply`, if the service refused the call:
/// [`CallError::Remote`] with the code, the message and the detail record.
/// Scans the top-level fields only and stops quietly at a malformed one (the
/// declared decoder reports that).
pub fn remote_error(reply: &Parcel) -> Option<CallError> {
    let mut decoder = Decoder::new(&reply.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
            let (code, message) = field.error_parts().ok()?;
            return Some(CallError::Remote {
                code,
                message: message.to_string(),
                detail: field.error_detail().map(read_detail).unwrap_or_default(),
            });
        }
    }
    None
}

/// The detail record's strings: `1` domain, `2` hint, `3` docs. A field it
/// does not know, or of another kind, is skipped; a malformed tail ends it.
fn read_detail(mut decoder: Decoder<'_>) -> Detail {
    let mut detail = Detail::default();
    while let Ok(Some(field)) = decoder.next() {
        let Ok(text) = field.as_str() else { continue };
        match field.id {
            1 => detail.domain = text.to_string(),
            2 => detail.hint = text.to_string(),
            3 => detail.docs = text.to_string(),
            _ => {}
        }
    }
    detail
}
