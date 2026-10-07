//! Who is asking, and password checks behind the brake.
//!
//! Every password check (`Authenticate`, and `SetPassword`'s old password)
//! goes through [`check`]: the attempt is refused at once with `EAGAIN` while
//! the account name or the calling uid is locked (`accountdb::ratelimit`),
//! and only otherwise asks `keyd`. Where a failure counts depends on who
//! asks (`ratelimit::Attempt`, review of #659 H5):
//!
//! * the mediators that check passwords for a person at the keyboard,
//!   `logind` (the login screen) and `elevd` (the trusted prompt), count it
//!   against the account name: each slows its own askers, and a guesser
//!   there locks the name for everyone;
//! * anyone else (a session, a package) counts it against its own uid only,
//!   so flooding `Authenticate("admin", ...)` slows the flooder and never
//!   locks `admin` out of logging in or approving.
//!
//! A success clears the name that authenticated, never the caller's lock.

use accountdb::policy::Who;
use accountdb::ratelimit::Attempt;
use user::messenger::{errno, keyd};
use user::sys::{self, Cred};

use super::{Refusal, State};

/// The [`Who`] of a request, from its kernel-stamped credentials.
pub(crate) fn who(caller: &Cred) -> Who {
    let system = caller.label_id == 0 && caller.session == 0;
    if system && caller.uid == accountdb::ELEVD_UID {
        Who::Elevd
    } else if system && caller.uid == user::messenger::logind::GREETER_UID {
        Who::Greeter
    } else {
        Who::User { uid: caller.uid }
    }
}

/// Whether the caller checks passwords for others (see the module docs):
/// `elevd`, or a system service holding `CAP_SETUID` (`logind`), unlabelled
/// and outside any session.
fn mediator(caller: &Cred) -> bool {
    let service = caller.label_id == 0 && caller.session == 0;
    who(caller) == Who::Elevd || (service && caller.caps & sys::CAP_SETUID != 0)
}

/// `Authenticate`: whether `secret` is `name`'s password, or `EAGAIN` while
/// the brake holds.
pub(crate) fn authenticate(
    state: &mut State,
    caller: &Cred,
    name: &str,
    secret: &str,
) -> Result<bool, Refusal> {
    check(state, caller, name, secret)
}

/// A password check under the brake (see the module docs).
pub(crate) fn check(
    state: &mut State,
    caller: &Cred,
    name: &str,
    secret: &str,
) -> Result<bool, Refusal> {
    let attempt = Attempt::new(name, caller.uid, mediator(caller));
    let now = sys::clock();
    if let Err(wait) = state.limiter.check(&attempt.checked, now) {
        sys::write_str(&alloc::format!(
            "ACCOUNTS:AUTH:SLOWED user={name} caller_uid={} wait_ticks={wait}\n",
            caller.uid
        ));
        return Err(Refusal::new(
            errno::EAGAIN,
            "too many failed attempts; wait a moment and try again",
        ));
    }
    let known = state
        .db
        .as_ref()
        .ok()
        .is_some_and(|db| db.user(name).is_some_and(|user| user.secret.is_some()));
    let ok = known && verify(state, name, secret);
    if ok {
        state.limiter.succeeded(attempt.cleared.as_slice());
    } else {
        state.limiter.failed(&attempt.counted, now);
    }
    Ok(ok)
}

/// `keyd`'s verdict (`docs/security-model.md` section 3), and nothing else
/// (issue #447): an unreachable `keyd` or any error from `Verify` is a
/// refusal. `keyd` is resolved again on every check, so one that registered
/// late or restarted is found without restarting this service.
fn verify(state: &mut State, name: &str, secret: &str) -> bool {
    let Ok(client) = keyd::Client::connect() else {
        if !state.keyd_warned {
            state.keyd_warned = true;
            sys::write_str("accountsd: keyd unreachable; every login is refused\n");
        }
        return false;
    };
    state.keyd_warned = false;
    client.verify(name, secret).unwrap_or(false)
}
