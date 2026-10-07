//! What a session may ask for at all, whatever an administrator would
//! answer (review of #659).
//!
//! [`Operation::parse`] checks an operation's *shape* (`EINVAL` when wrong);
//! [`Operation::permitted`] checks the *policy* (`EPERM`): a request that
//! fails it is refused before any prompt, so no administrator is ever asked
//! to approve it.

use crate::Operation;

/// The services a session may ask `elevd` to restart (`service.restart`):
/// drivers and services that hold no security state and come back as they
/// were, so restarting one can unstick a device but never weaken a check.
///
/// Never on it: `elevd`, `logind`, `accountsd` and `keyd` (identity,
/// passwords, approvals), `xuid` (the trusted prompt; it is not an `init`
/// service row either), `init` and `messengerd` (the supervisor and the
/// fabric), `confd` (every setting and its policy), `logd` (the audit
/// trail), `pkgd` (a restart mid-install), `timed` (the clock), and the
/// test service `flaky`. `init`'s `RestartService` checks the same list.
pub const RESTARTABLE: &[&str] = &[
    "inputd",
    "audiod",
    "sndd",
    "netd",
    "netdrv",
    "usbd",
    "devd",
    "mountd",
    "printd",
    "clipboardd",
    "mimed",
    "healthd",
    "sysmond",
];

/// Whether a session may ask `elevd` to restart the service `name`.
pub fn restartable(name: &str) -> bool {
    RESTARTABLE.contains(&name)
}

impl Operation {
    /// Whether the operation may be asked for at all; `Err` says why not.
    pub fn permitted(&self) -> Result<(), &'static str> {
        match self {
            Operation::ServiceRestart { name } if !restartable(name) => {
                Err("that service may not be restarted from a session")
            }
            _ => Ok(()),
        }
    }
}
