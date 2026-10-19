//! The sweep of apps' secrets (docs/ai-agent-plan.md section 5.3 "P2b"):
//! `keyd` deletes the secrets of an app `pkgd` no longer lists, so a removed
//! package leaves nothing behind. No new `pkgd`-to-`keyd` call exists: `keyd`
//! asks `pkgd`'s `Installed`, which is readable by an unlabelled service.
//!
//! The sweep only ever deletes, and only for an app `pkgd` *answered* "not
//! installed" for: an unreachable `pkgd`, a provisioning pass still running, a
//! timeout or any error ends the sweep with nothing deleted and a retry soon.
//! It first runs shortly after start (`pkgd` is not up when `keyd` starts) and
//! then every hour.

use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::rng::Entropy;
use user::messenger::pkgd;
use user::sys;
use user::trace;

use crate::appsecrets::AppSecrets;

/// PIT ticks per second.
const SECOND: u64 = 100;
/// The first try after start, and the retry after one that could not finish.
const RETRY: u64 = 5 * SECOND;
/// The period of a completed sweep: one hour.
const PERIOD: u64 = 3600 * SECOND;
/// The longest the whole sweep may take before it gives up (keyd serves no
/// request while it runs).
const BUDGET: u64 = 3 * SECOND;

pub(crate) struct Sweep {
    /// The absolute tick the next sweep is due at.
    next: u64,
    /// Apps `pkgd` said were not installed at the last sweep. One answer is
    /// not enough to delete (a package that failed to provision this boot is
    /// only absent for a moment): an app must be missing twice in a row.
    missed: Vec<String>,
}

impl Sweep {
    pub(crate) fn new() -> Sweep {
        Sweep {
            next: sys::clock() + RETRY,
            missed: Vec::new(),
        }
    }

    /// The tick `recv` must wake at to be on time.
    pub(crate) fn deadline(&self) -> u64 {
        self.next
    }

    /// Run the sweep if it is due.
    pub(crate) fn tick(&mut self, secrets: &mut AppSecrets, entropy: &mut Entropy) {
        if sys::clock() < self.next {
            return;
        }
        let period = match sweep(secrets, entropy, &mut self.missed) {
            Outcome::Done(removed) => {
                trace::line!(trace::Channel::Keyd, "SWEEP:PASS removed={removed}");
                PERIOD
            }
            // Something is missing: ask again soon, before deleting anything.
            Outcome::Confirm | Outcome::Retry => RETRY,
        };
        self.next = sys::clock() + period;
    }
}

enum Outcome {
    /// `pkgd` answered for every app; this many secrets were deleted.
    Done(usize),
    /// Some app was missing for the first time: confirm before deleting.
    Confirm,
    /// `pkgd` could not be asked (or not reliably): nothing was deleted.
    Retry,
}

/// One sweep, within [`BUDGET`]. It deletes nothing unless `pkgd` finished
/// provisioning without a failure (an app that failed to install is absent,
/// not removed) and answered "not installed" for the same app at two sweeps
/// in a row ([`secretstore::confirmed_gone`]).
fn sweep(secrets: &mut AppSecrets, entropy: &mut Entropy, missed: &mut Vec<String>) -> Outcome {
    let apps = secrets.apps();
    if apps.is_empty() {
        missed.clear();
        return Outcome::Done(0);
    }
    let end = sys::clock() + BUDGET;
    let Ok(client) = pkgd::Client::connect() else {
        return Outcome::Retry;
    };
    match client.provisioned_until(Some(end)) {
        Ok(state) if state.done && state.failed == 0 => {}
        _ => return Outcome::Retry,
    }
    let mut now_missing: Vec<String> = Vec::new();
    for app in apps {
        if sys::clock() >= end {
            return Outcome::Retry;
        }
        match client.installed_until(&app, Some(end)) {
            Ok(Some(_)) => {}
            Ok(None) => now_missing.push(app),
            Err(_) => return Outcome::Retry,
        }
    }
    let gone = secretstore::confirmed_gone(missed, &now_missing);
    let first_time = now_missing.len() > gone.len();
    *missed = now_missing;
    let removed = secrets.sweep_gone(entropy, &gone);
    if first_time {
        Outcome::Confirm
    } else {
        Outcome::Done(removed)
    }
}
