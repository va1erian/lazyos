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
//! Calls are bounded by the deadlines `idl/keyd_appsecret.midl` states, so none can park a UI
//! thread for long.

use messenger_generated::os_lazy_keyd_appsecret_v1 as wire;

use super::messenger::{client, Observed};
use crate::sys::errno;

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

/// Run `call` on `keyd`'s client: `Unavailable` when the service cannot be
/// resolved (a denied resolve is `Denied`: the label does not hold the interface).
fn with_keyd<R>(
    call: impl FnOnce(&wire::Client<Observed>) -> Result<R, wire::ClientError>,
) -> Result<R, Failure> {
    let keyd = client(wire::Client::connect).map_err(|error| match -error.code() {
        errno::EPERM | errno::EACCES => Failure::Denied,
        _ => Failure::Unavailable,
    })?;
    call(&keyd).map_err(|error| Failure::of_errno(error.code()))
}

/// Keep `secret` under `name`, replacing one of that name.
pub fn put(name: &str, secret: &[u8]) -> Result<(), Failure> {
    with_keyd(|keyd| keyd.put(name, secret))
}

/// The secret kept under `name`.
pub fn get(name: &str) -> Result<Vec<u8>, Failure> {
    with_keyd(|keyd| keyd.get(name))
}

/// Forget the secret under `name`.
pub fn delete(name: &str) -> Result<(), Failure> {
    with_keyd(|keyd| keyd.delete(name))
}

/// The names of the secrets this app keeps, sorted.
pub fn list() -> Result<Vec<String>, Failure> {
    with_keyd(|keyd| keyd.list())
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
}
