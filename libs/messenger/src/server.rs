//! The server runtime for Messenger interfaces, the counterpart of
//! [`crate::client`] (`docs/client-api-plan.md` 3.3).
//!
//! A generated `Server` trait (one per interface, behind the cargo feature
//! `<interface>_server`) has one method per IDL method; the generated
//! `dispatch` checks the header's interface, decodes the request, calls the
//! handler and frames the reply or the standard error field. What a program
//! hand-writes is only the method bodies. This module is everything that
//! function needs from a runtime and nothing else:
//!
//! * [`Incoming`], the received message as the dispatch sees it. The native
//!   `user` crate and the std `xui-app` server both implement it, so one
//!   generated dispatch serves both runtimes.
//! * [`Caller`], the kernel-stamped identity of the sender. Authorization
//!   stays in the handler; it never parses anything.
//! * [`Fault`], why a handler refused, and [`reply`]/[`fault_reply`], the only
//!   two places a reply parcel is built.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::client::{Errno, ERROR_FIELD};
use crate::{Encoder, Header, Object, Parcel, VERSION};

/// The sender of a request, as the kernel stamped it when the message was
/// queued. Never taken from the request body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caller {
    /// User id.
    pub uid: u32,
    /// Primary group id.
    pub gid: u32,
    /// Capability bits (`CAP_*`).
    pub caps: u32,
    /// Policy label id.
    pub label_id: u32,
    /// Session id; `0` before login.
    pub session: u64,
    /// The sender's task slot: for addressing a per-client table only, since
    /// a slot can be reused. Authorize with the other fields.
    pub sender: u64,
}

/// A received message, as a generated `dispatch` reads it.
pub trait Incoming {
    /// The header's interface id (or an ACL scope id).
    fn interface_id(&self) -> u64;
    /// The header's method id.
    fn method(&self) -> u32;
    /// The sender's kernel-stamped identity.
    fn caller(&self) -> Caller;
    /// Run a generated `decode_<method>_args` over the body and the objects
    /// the kernel installed for the message. On success the decoded value
    /// owns every object (the decoder claims each by its declared slot); on
    /// failure the message keeps and closes them when it drops, so a refused
    /// request leaks nothing.
    fn decode<T>(
        &self,
        decoder: impl FnOnce(&[u8], &[Object]) -> Result<T, crate::Error>,
    ) -> Result<T, crate::Error>;
}

/// Why a handler refused a call: the errno-style or declared code, the
/// sentence the caller sees and the optional detail record of the standard
/// error field (`docs/midl.md`, "Errors").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// Positive errno-style code, or one of the interface's declared codes.
    pub code: u32,
    /// The service's own sentence, ready to show.
    pub message: String,
    /// Which code space `code` belongs to; empty for errno-style codes.
    pub domain: String,
    /// How to fix it.
    pub hint: String,
    /// A documentation id such as `err.messenger.denied`.
    pub docs: String,
}

impl Fault {
    /// A fault with `code` and `message`.
    pub fn new(code: u32, message: &str) -> Fault {
        Fault {
            code,
            message: message.to_string(),
            domain: String::new(),
            hint: String::new(),
            docs: String::new(),
        }
    }

    /// `errno` with the standard friendly text.
    pub fn errno(errno: Errno) -> Fault {
        Fault::new(errno.number().unsigned_abs() as u32, errno.describe())
    }

    /// The refusal of a required method the program does not serve (`ENOSYS`):
    /// every generated method must be implemented, so a daemon that serves
    /// part of an interface says so here.
    pub fn unserved() -> Fault {
        Fault::errno(Errno::NOSYS)
    }

    /// `errno` with the service's own sentence.
    pub fn refuse(errno: Errno, message: &str) -> Fault {
        Fault::new(errno.number().unsigned_abs() as u32, message)
    }

    /// The same fault in `domain`.
    pub fn with_domain(mut self, domain: &str) -> Fault {
        self.domain = domain.to_string();
        self
    }

    /// The same fault with a hint on how to fix it.
    pub fn with_hint(mut self, hint: &str) -> Fault {
        self.hint = hint.to_string();
        self
    }

    /// The same fault with a documentation id.
    pub fn with_docs(mut self, docs: &str) -> Fault {
        self.docs = docs.to_string();
        self
    }

    /// The fault of a syscall return (`-22` is `EINVAL`) with the errno's
    /// friendly text: what a handler returns for a kernel call that failed.
    pub fn from_syscall(code: i64) -> Fault {
        Fault::errno(Errno::from_syscall(code))
    }
}

impl From<Errno> for Fault {
    fn from(errno: Errno) -> Fault {
        Fault::errno(errno)
    }
}

/// A reply header for `interface`/`method`: flags and ids as every service
/// has always sent them (the kernel stamps the transaction).
fn header(interface: u64, method: u32) -> Header {
    Header {
        version: VERSION,
        flags: 0,
        interface_id: interface,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}

/// The success reply of `interface`/`method` carrying `body`.
pub fn reply(interface: u64, method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: header(interface, method),
        body,
        objects: Vec::new(),
    }
}

/// The failure reply: the standard error field with `fault`'s code and
/// message, and its detail record when it states one.
pub fn fault_reply(interface: u64, method: u32, fault: &Fault) -> Parcel {
    let mut body = Encoder::new();
    let detailed = !(fault.domain.is_empty() && fault.hint.is_empty() && fault.docs.is_empty());
    // A fresh encoder takes one bounded error field; an over-long message
    // falls back to the bare code so the caller still hears a refusal.
    let written = if detailed {
        let mut detail = Encoder::new();
        let parts = [(1u16, &fault.domain), (2, &fault.hint), (3, &fault.docs)];
        let built = parts
            .iter()
            .filter(|(_, text)| !text.is_empty())
            .try_for_each(|(id, text)| detail.string(*id, text));
        built.and_then(|()| body.error_detail(ERROR_FIELD, fault.code, &fault.message, &detail))
    } else {
        body.error(ERROR_FIELD, fault.code, &fault.message)
    };
    if written.is_err() {
        body = Encoder::new();
        let _ = body.error(ERROR_FIELD, fault.code, "");
    }
    reply(interface, method, body.finish())
}

/// The reply of a handler's `result`: its value encoded by `encode` into the
/// success reply, or its fault as the standard error field. A value that does
/// not encode is the service's fault ([`bad_reply`]).
pub fn finish<T>(
    interface: u64,
    method: u32,
    result: Result<T, Fault>,
    encode: impl FnOnce(T) -> Result<Vec<u8>, crate::Error>,
) -> Parcel {
    match result {
        Ok(value) => match encode(value) {
            Ok(body) => reply(interface, method, body),
            Err(_) => bad_reply(interface, method),
        },
        Err(fault) => fault_reply(interface, method, &fault),
    }
}

/// The refusal for a request whose body does not decode: `EINVAL`.
pub fn bad_request(interface: u64, method: u32) -> Parcel {
    fault_reply(
        interface,
        method,
        &Fault::refuse(Errno::INVAL, "the request is malformed"),
    )
}

/// The refusal for a reply that does not encode (a field over its limit):
/// the service's fault, `EIO`.
pub fn bad_reply(interface: u64, method: u32) -> Parcel {
    fault_reply(
        interface,
        method,
        &Fault::refuse(Errno::IO, "the reply does not fit its declared shape"),
    )
}

/// The refusal for a header naming another interface: `EINVAL`, as every
/// hand-written dispatch answered.
pub fn wrong_interface(interface: u64, method: u32) -> Parcel {
    fault_reply(
        interface,
        method,
        &Fault::refuse(Errno::INVAL, "this service does not speak that interface"),
    )
}

/// The refusal for a method the interface does not declare: `ENOSYS`.
pub fn unknown_method(interface: u64, method: u32) -> Parcel {
    fault_reply(interface, method, &Fault::errno(Errno::NOSYS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{remote_error, CallError, Detail};

    #[test]
    fn a_fault_reply_reads_back_as_the_standard_error() {
        let fault = Fault::refuse(Errno::PERM, "nope")
            .with_hint("ask an admin")
            .with_domain("x");
        let parcel = fault_reply(7, 3, &fault);
        assert_eq!((parcel.header.interface_id, parcel.header.method), (7, 3));
        match remote_error(&parcel) {
            Some(CallError::Remote {
                code,
                message,
                detail,
            }) => {
                assert_eq!(
                    (code as i64, message.as_str()),
                    (Errno::PERM.number(), "nope")
                );
                assert_eq!(
                    (detail.domain.as_str(), detail.hint.as_str()),
                    ("x", "ask an admin")
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_plain_fault_has_no_detail_record() {
        let parcel = fault_reply(1, 2, &Fault::errno(Errno::NOSYS));
        match remote_error(&parcel) {
            Some(CallError::Remote { code, detail, .. }) => {
                assert_eq!(code as i64, Errno::NOSYS.number());
                assert_eq!(detail, Detail::default());
            }
            other => panic!("{other:?}"),
        }
    }
}
