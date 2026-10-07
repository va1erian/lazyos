//! Who is asking, and password checks behind the brake.
//!
//! Every password check (`Authenticate`, and `SetPassword`'s old password)
//! goes through [`check`]: the attempt is refused at once with `EAGAIN` while
//! the account name or the calling uid is locked (`accountdb::ratelimit`),
//! and only otherwise asks `keyd`. The mediators that check passwords for
//! somebody else, `logind` (the login screen) and `elevd` (the trusted
//! prompt), are counted per name only: each slows its own askers, and one
//! guesser must not lock everyone out of logging in or elevating.

use alloc::vec::Vec;

use accountdb::policy::Who;
use accountdb::ratelimit::Key;
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
/// `elevd`, or a system service holding `CAP_SETUID` (`logind`).
fn mediator(caller: &Cred) -> bool {
    who(caller) == Who::Elevd || (caller.label_id == 0 && caller.caps & sys::CAP_SETUID != 0)
}

/// The brake keys of an attempt on `name` by `caller`.
fn keys(caller: &Cred, name: &str) -> Vec<Key> {
    let mut keys = Vec::new();
    // An invalid name names no account: counting it would only let a
    // guesser fill the table with junk keys.
    if accountdb::valid_name(name) {
        keys.push(Key::Name(alloc::string::String::from(name)));
    }
    if !mediator(caller) {
        keys.push(Key::Caller(caller.uid));
    }
    keys
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
    let keys = keys(caller, name);
    let now = sys::clock();
    if let Err(wait) = state.limiter.check(&keys, now) {
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
        state.limiter.succeeded(&keys);
    } else {
        state.limiter.failed(&keys, now);
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
