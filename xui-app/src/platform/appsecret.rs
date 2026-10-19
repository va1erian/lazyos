//! Typed client for `keyd`'s app secrets (`os.lazy.keyd.appsecret.v1`,
//! docs/ai-agent-plan.md section 5.3): an app keeps its own passwords and
//! tokens across reboots. Bodies are built and parsed with the generated
//! stubs, never by hand.
//!
//! Every secret is the calling app's own: `keyd` scopes it by the caller's
//! kernel-stamped uid and label, so a name here can never reach another app's
//! secret. An app needs `os.lazy.keyd.appsecret.v1` in its manifest's
//! `interfaces`; without it (or run outside an installed package) every call
//! fails with [`Failure::Denied`] or [`Failure::Unavailable`].
//!
//! Nothing here logs or formats a secret, and [`Failure`] never carries one.
//! Calls are bounded ([`CALL_TICKS`]) so none can park a UI thread for long.

use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_keyd_appsecret_v1 as wire;

use super::messenger::Service;
use crate::sys::errno;

/// The registered service name (the interface name without `.v1`).
const NAME: &str = "os.lazy.keyd.appsecret";
/// How long one call may wait (3 s at 100 Hz): `keyd` writes a file before it
/// answers a `Put` or a `Delete`.
pub const CALL_TICKS: u64 = 300;

/// Why a call did not do what was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// `keyd` could not be reached, or did not answer in time.
    Unavailable,
    /// This app may not use the service: no permission, or not an installed
    /// app (an unlabelled program, a development run, a helper process).
    Denied,
    /// No such secret.
    NotFound,
    /// A bad name or secret (1..=64 printable ASCII, 1..=1024 bytes).
    Invalid,
    /// The app already keeps as many as it may.
    Full,
    /// `keyd` could not make the change durable; nothing changed.
    Io,
}

impl Failure {
    fn of_errno(code: i64) -> Failure {
        match -code {
            errno::EPERM | errno::EACCES => Failure::Denied,
            errno::ENOENT => Failure::NotFound,
            errno::EINVAL | errno::E2BIG => Failure::Invalid,
            errno::ENOSPC => Failure::Full,
            errno::EIO => Failure::Io,
            _ => Failure::Unavailable,
        }
    }
}

/// One call on `keyd`: `Unavailable` when the service cannot be resolved
/// (a denied resolve is `Denied`: the label does not hold the interface).
fn call(method: u32, body: Vec<u8>) -> Result<Vec<u8>, Failure> {
    let service = Service::connect(NAME).map_err(|code| match -code {
        errno::EPERM | errno::EACCES => Failure::Denied,
        _ => Failure::Unavailable,
    })?;
    service
        .call(wire::INTERFACE_ID, method, ERROR_FIELD, body, CALL_TICKS)
        .map(|reply| reply.body)
        .map_err(Failure::of_errno)
}

/// Keep `secret` under `name`, replacing one of that name.
pub fn put(name: &str, secret: &[u8]) -> Result<(), Failure> {
    let body = wire::encode_put_args(&wire::PutArgs {
        name: name.to_owned(),
        secret: secret.to_vec(),
    })
    .map_err(|_| Failure::Invalid)?;
    call(wire::METHOD_PUT, body).map(drop)
}

/// The secret kept under `name`.
pub fn get(name: &str) -> Result<Vec<u8>, Failure> {
    let body = wire::encode_get_args(&wire::GetArgs {
        name: name.to_owned(),
    })
    .map_err(|_| Failure::Invalid)?;
    let reply = call(wire::METHOD_GET, body)?;
    wire::decode_get_reply(&reply)
        .map(|reply| reply.secret)
        .map_err(|_| Failure::Unavailable)
}

/// Forget the secret under `name`.
pub fn delete(name: &str) -> Result<(), Failure> {
    let body = wire::encode_delete_args(&wire::DeleteArgs {
        name: name.to_owned(),
    })
    .map_err(|_| Failure::Invalid)?;
    call(wire::METHOD_DELETE, body).map(drop)
}

/// The names of the secrets this app keeps, sorted.
pub fn list() -> Result<Vec<String>, Failure> {
    let reply = call(wire::METHOD_LIST, Vec::new())?;
    wire::decode_list_reply(&reply)
        .map(|reply| reply.names)
        .map_err(|_| Failure::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_onto_what_the_app_can_act_on() {
        assert_eq!(Failure::of_errno(-errno::EPERM), Failure::Denied);
        assert_eq!(Failure::of_errno(-errno::ENOENT), Failure::NotFound);
        assert_eq!(Failure::of_errno(-errno::EINVAL), Failure::Invalid);
        assert_eq!(Failure::of_errno(-errno::ENOSPC), Failure::Full);
        assert_eq!(Failure::of_errno(-errno::EIO), Failure::Io);
        assert_eq!(Failure::of_errno(-errno::ETIMEDOUT), Failure::Unavailable);
    }

    #[test]
    fn a_request_is_encoded_with_the_generated_stubs() {
        let body = wire::encode_put_args(&wire::PutArgs {
            name: "token".to_owned(),
            secret: b"s3cret".to_vec(),
        })
        .unwrap();
        let back = wire::decode_put_args(&body).unwrap();
        assert_eq!((back.name.as_str(), back.secret.as_slice()), ("token", &b"s3cret"[..]));
    }
}
