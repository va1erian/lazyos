//! `init`'s boot autostart: the desktop shell first, then the installed apps
//! whose manifest sets `autostart` (issue #509), one per stagger interval so
//! their start-up does not all land at once.
//!
//! The apps are packages, and on a fresh image `pkgd` installs them at its
//! first start, so the second half waits for `pkgd`'s provisioning
//! (`INIT:AUTOSTART:WAIT pkgd`): until the packages that open at login are
//! installed (`pkgd` does them first, then announces `ready`; see
//! `provisioning.rs`), bounded at [`PROVISION_WAIT`]: on timeout it opens
//! whatever is installed (`INIT:AUTOSTART:PARTIAL`). The shell and the
//! Terminal are not packages, so the desktop always appears.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{router, services};
use user::sys::{self, Cred as SysCred};

use super::apps::autostart_ids;
use super::installed::InstalledApps;
use super::launch::launch;
use super::state::{
    Service, AUTOSTART_ATTEMPTS, AUTOSTART_DELAY, AUTOSTART_STAGGER, BOOT_SELFTESTS, CAP_SETUID,
    LAUNCH_SELFTEST_RETRY,
};

/// Ticks (100 Hz) the autostart waits for `pkgd`'s provisioning: 30 s.
const PROVISION_WAIT: u64 = 3000;
/// Ticks between two looks at `pkgd`'s progress.
const POLL_EVERY: u64 = 25;
/// Ticks (100 Hz) the registry self-test waits for the whole core set: a
/// first boot writes every package to disk, which is slow on some hosts.
const SELFTEST_WAIT: u64 = 60_000;

/// Where the autostart is.
enum Stage {
    /// Opening the built-in rows (the shell).
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
    due: u64,
    attempts: u64,
}

impl Autostart {
    pub(super) fn new() -> Autostart {
        Autostart {
            stage: Stage::Builtins,
            pending: autostart_ids(),
            due: sys::clock() + AUTOSTART_DELAY,
            attempts: 0,
        }
    }

    /// One step when due: a launch, or one poll of `pkgd`.
    pub(super) fn step(
        &mut self,
        services: &mut Vec<Service>,
        broker: &mut router::TopicBroker,
        installed: &mut InstalledApps,
        now: u64,
    ) {
        if now < self.due {
            return;
        }
        if let Stage::Waiting { until } = self.stage {
            self.poll(installed, now, until);
            return;
        }
        let Some(&id) = self.pending.first() else {
            match self.stage {
                Stage::Builtins => {
                    sys::write_str("INIT:AUTOSTART:WAIT pkgd\n");
                    self.stage = Stage::Waiting {
                        until: now + PROVISION_WAIT,
                    };
                }
                Stage::Apps { selftest_until } if selftest_until != 0 => {
                    self.selftest(installed, now, selftest_until);
                }
                _ => {}
            }
            return;
        };
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
                self.due = now + AUTOSTART_STAGGER;
            }
            Err(_) if self.attempts + 1 >= AUTOSTART_ATTEMPTS => {
                sys::write_str(&format!("INIT:AUTOSTART:FAIL app={id}\n"));
                self.pending.remove(0);
                self.attempts = 0;
            }
            Err(_) => {
                self.attempts += 1;
                self.due = now + LAUNCH_SELFTEST_RETRY;
            }
        }
    }

    /// Ask `pkgd` whether provisioning is over; open the apps when it is, or
    /// when the wait ran out.
    fn poll(&mut self, installed: &mut InstalledApps, now: u64, until: u64) {
        self.due = now + POLL_EVERY;
        // `ready`: the packages that open at login went first and are
        // installed, so the session need not wait for the whole set.
        let ready = super::provisioning::ready();
        if !ready && now < until {
            return;
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
        self.due = now;
    }

    /// The registry self-test, once `pkgd` provisioned the whole core set (or
    /// the wait for it ran out).
    fn selftest(&mut self, installed: &mut InstalledApps, now: u64, until: u64) {
        self.due = now + POLL_EVERY * 4;
        let done = super::provisioning::done();
        if !done && now < until {
            return;
        }
        installed.refresh();
        sys::write_str(&super::selftest::selftest_apps(installed));
        self.stage = Stage::Apps { selftest_until: 0 };
    }
}
