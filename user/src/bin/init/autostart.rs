//! `init`'s boot autostart: the desktop shell first, then the installed apps
//! whose manifest sets `autostart` (issue #509).
//!
//! Nothing here runs on a timer (docs/performance-plan.md P7.3): the session
//! opens as soon as every boot service is ready (`ready::settled`), and each
//! stage moves on when what it waits for is announced, not after a delay.
//!
//! The apps are packages, and on a fresh image `pkgd` installs them at its
//! first start, so the second half waits for `pkgd`'s provisioning
//! (`INIT:AUTOSTART:WAIT pkgd`): until the packages that open at login are
//! installed (`pkgd` does them first, then announces `ready`; see
//! `provisioning.rs`, which observes it from the publish that wakes this
//! supervisor), bounded at [`PROVISION_WAIT`]: on timeout it opens whatever
//! is installed (`INIT:AUTOSTART:PARTIAL`). The shell and the Terminal are
//! not packages, so the desktop always appears.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{router, services};
use user::sys::{self, Cred as SysCred};

use super::apps::autostart_ids;
use super::installed::InstalledApps;
use super::launch::launch;
use super::state::{
    Service, AUTOSTART_ATTEMPTS, BOOT_SELFTESTS, CAP_SETUID, LAUNCH_SELFTEST_RETRY,
};

/// Ticks (100 Hz) the autostart waits for `pkgd`'s provisioning: 30 s.
const PROVISION_WAIT: u64 = 3000;
/// Ticks (100 Hz) the registry self-test waits for the whole core set: a
/// first boot writes every package to disk, which is slow on some hosts.
const SELFTEST_WAIT: u64 = 60_000;

/// Where the autostart is.
enum Stage {
    /// Opening the built-in rows (the shell) once the services are ready.
    Builtins,
    /// Waiting for `pkgd` to finish provisioning, until the absolute tick.
    Waiting { until: u64 },
    /// Opening the installed apps; the registry self-test still waits for
    /// provisioning to be over, until the absolute tick (0: it ran).
    Apps { selftest_until: u64 },
}

/// Opens the desktop session's apps at boot (issues #215, #509). Each launch
/// is `--client` (from the manifest's `args` for a package), so the app
/// connects to `xuid` (which retries until the compositor is up) instead of
/// racing it for the display grant. Prints `INIT:AUTOSTART:PASS app=<id>` per
/// app.
pub(super) struct Autostart {
    stage: Stage,
    pending: Vec<&'static str>,
    /// When a launch refused for a full task table is tried again (0: none).
    retry_at: u64,
    attempts: u64,
}

impl Autostart {
    pub(super) fn new() -> Autostart {
        Autostart {
            stage: Stage::Builtins,
            pending: autostart_ids(),
            retry_at: 0,
            attempts: 0,
        }
    }

    /// The tick the next step is due at with nothing else to wake the
    /// supervisor: a launch retry or a stage's bound. Service readiness and
    /// `pkgd`'s announcements arrive as messages, which wake it anyway.
    pub(super) fn next_due(&self) -> Option<u64> {
        let bound = match self.stage {
            Stage::Builtins => None,
            Stage::Waiting { until } => Some(until),
            Stage::Apps { selftest_until } => (selftest_until != 0).then_some(selftest_until),
        };
        let retry = (self.retry_at != 0).then_some(self.retry_at);
        [bound, retry].into_iter().flatten().min()
    }

    /// Advance as far as the system allows: open the shell once the boot
    /// services are ready, wait for `pkgd`, open the installed apps.
    pub(super) fn step(
        &mut self,
        services: &mut Vec<Service>,
        broker: &mut router::TopicBroker,
        installed: &mut InstalledApps,
        now: u64,
    ) {
        if now < self.retry_at {
            return;
        }
        self.retry_at = 0;
        if matches!(self.stage, Stage::Builtins) {
            if !super::ready::settled(services) {
                return;
            }
            if !self.launch_pending(services, broker, installed, now) {
                return;
            }
            sys::write_str("INIT:AUTOSTART:WAIT pkgd\n");
            self.stage = Stage::Waiting {
                until: now + PROVISION_WAIT,
            };
        }
        if let Stage::Waiting { until } = self.stage {
            if !self.provisioned(installed, now, until) {
                return;
            }
        }
        if !self.launch_pending(services, broker, installed, now) {
            return;
        }
        if let Stage::Apps { selftest_until } = self.stage {
            if selftest_until != 0 {
                self.selftest(installed, now, selftest_until);
            }
        }
    }

    /// Launch every pending app; `false` while one waits for a retry.
    fn launch_pending(
        &mut self,
        services: &mut Vec<Service>,
        broker: &mut router::TopicBroker,
        installed: &mut InstalledApps,
        now: u64,
    ) -> bool {
        while let Some(&id) = self.pending.first() {
            let caller = SysCred::new(0, 0, CAP_SETUID, 0, 0);
            let request = services::LaunchRequest {
                app: String::from(id),
                args: String::new(),
                session: 0,
            };
            match launch(services, broker, installed, &request, &caller, true) {
                Ok(_) => {
                    sys::write_str(&format!("INIT:AUTOSTART:PASS app={id}\n"));
                    self.pending.remove(0);
                    self.attempts = 0;
                }
                Err(_) if self.attempts + 1 >= AUTOSTART_ATTEMPTS => {
                    sys::write_str(&format!("INIT:AUTOSTART:FAIL app={id}\n"));
                    self.pending.remove(0);
                    self.attempts = 0;
                }
                Err(_) => {
                    // The task table is full: try again once a slot may
                    // have been freed.
                    self.attempts += 1;
                    self.retry_at = now + LAUNCH_SELFTEST_RETRY;
                    return false;
                }
            }
        }
        true
    }

    /// Whether `pkgd` has provisioned the apps that open at login (or the
    /// wait ran out); moves on to opening them.
    fn provisioned(&mut self, installed: &mut InstalledApps, now: u64, until: u64) -> bool {
        // `ready`: the packages that open at login went first and are
        // installed, so the session need not wait for the whole set.
        let ready = super::provisioning::ready();
        if !ready && now < until {
            return false;
        }
        if ready {
            sys::write_str("INIT:AUTOSTART:READY pkgd\n");
        } else {
            sys::write_str("INIT:AUTOSTART:PARTIAL pkgd did not finish provisioning\n");
        }
        installed.refresh();
        self.pending = installed.autostart_ids();
        self.stage = Stage::Apps {
            selftest_until: if BOOT_SELFTESTS {
                now + SELFTEST_WAIT
            } else {
                0
            },
        };
        true
    }

    /// The registry self-test, once `pkgd` provisioned the whole core set (or
    /// the wait for it ran out).
    fn selftest(&mut self, installed: &mut InstalledApps, now: u64, until: u64) {
        let done = super::provisioning::done();
        if !done && now < until {
            return;
        }
        installed.refresh();
        sys::write_str(&super::selftest::selftest_apps(installed));
        self.stage = Stage::Apps { selftest_until: 0 };
    }
}
