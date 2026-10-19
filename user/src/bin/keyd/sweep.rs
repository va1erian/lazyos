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
/// The longest one `pkgd` call may take before the sweep gives up.
const CALL: u64 = 2 * SECOND;

pub(crate) struct Sweep {
    /// The absolute tick the next sweep is due at.
    next: u64,
}

impl Sweep {
    pub(crate) fn new() -> Sweep {
        Sweep {
            next: sys::clock() + RETRY,
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
        let period = match sweep(secrets, entropy) {
            Some(removed) => {
                trace::line!(trace::Channel::Keyd, "SWEEP:PASS removed={removed}");
                PERIOD
            }
            None => RETRY,
        };
        self.next = sys::clock() + period;
    }
}

/// One sweep. `Some(removed)` when `pkgd` answered for every app (the table is
/// then swept), `None` when it could not be asked.
fn sweep(secrets: &mut AppSecrets, entropy: &mut Entropy) -> Option<usize> {
    let apps = secrets.apps();
    if apps.is_empty() {
        return Some(0);
    }
    let client = pkgd::Client::connect().ok()?;
    let state = client
        .provisioned_until(Some(sys::clock() + CALL))
        .ok()?;
    if !state.done {
        return None;
    }
    let mut gone: Vec<String> = Vec::new();
    for app in apps {
        match client.installed_until(&app, Some(sys::clock() + CALL)) {
            Ok(Some(_)) => {}
            Ok(None) => gone.push(app),
            Err(_) => return None,
        }
    }
    Some(secrets.sweep_gone(entropy, &gone))
}
