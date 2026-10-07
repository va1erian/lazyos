//! `init`'s session autostart (issues #509, #623): the apps that open with a
//! desktop session, the built-in rows the build named and the installed apps
//! whose manifest sets `autostart`, opened **in that session, as its user**.
//!
//! Nothing autostarts at boot any more, and nothing here runs as root: a
//! desktop session opens when `logind` has LazyShell launched into it (a
//! typed login or the build's autologin), the launch path calls
//! [`open_session`] with the shell's credentials, and the apps follow with
//! exactly that caller: the session's uid, gid and id, no capability, no
//! label. An app a user installed with `autostart` therefore runs as that
//! user at their next login, never as root at boot (the user-to-root hole
//! of the accounts plan, docs/accounts-plan.md section 2).
//!
//! Nothing here runs on a timer (docs/performance-plan.md P7.3): the apps
//! open as soon as every boot service is ready (`ready::settled`) and `pkgd`
//! has provisioned the core packages, since a fresh image installs them at
//! its start and an app that is not installed yet cannot open. Until then a
//! session's autostart waits (`INIT:AUTOSTART:WAIT pkgd`, then
//! `INIT:AUTOSTART:READY pkgd` once it announces `done`; see
//! `provisioning.rs`). `xuid` holds the screen back for the same reason
//! (`user/src/bin/xuid/provisioning.rs`). The wait is bounded at
//! [`PROVISION_WAIT`]: on timeout the sessions open with whatever is
//! installed (`INIT:AUTOSTART:PARTIAL`), and an image whose `pkgd` is not
//! running does not wait at all. Serial: `INIT:AUTOSTART:PASS app=<id>
//! session=<n>` per app, `INIT:AUTOSTART:FAIL ...` when one cannot start.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use spin::Mutex;
use user::messenger::{router, services};
use user::sys::{self, Cred as SysCred};

use super::apps::autostart_ids;
use super::installed::InstalledApps;
use super::launch::launch;
use super::state::{Service, AUTOSTART_ATTEMPTS, BOOT_SELFTESTS, LAUNCH_SELFTEST_RETRY};

/// Ticks (100 Hz) the autostart waits for `pkgd`'s provisioning: 3 min, for
/// a first boot under TCG that writes every core package.
const PROVISION_WAIT: u64 = 18_000;
/// Ticks (100 Hz) the registry self-test waits for the whole core set: a
/// first boot writes every package to disk, which is slow on some hosts.
const SELFTEST_WAIT: u64 = 60_000;
/// Desktop sessions waiting for their autostart at once (one graphical
/// session at a time; the slack covers a quick logout and login).
const MAX_OPENED: usize = 8;

/// The desktop sessions opened since the last step: the credentials their
/// shell was stamped with (`init` is one task; the lock only makes the static
/// safe).
static OPENED: Mutex<Vec<SysCred>> = Mutex::new(Vec::new());

/// A desktop session opened (its shell was launched into it): its autostart
/// apps open with `shell`'s identity once the system is ready. Called by the
/// launch path, which has no handle on the [`Autostart`] state.
pub(super) fn open_session(shell: SysCred) {
    let mut opened = OPENED.lock();
    if shell.session != 0
        && opened.len() < MAX_OPENED
        && !opened.iter().any(|seen| seen.session == shell.session)
    {
        opened.push(shell);
    }
}

/// Where the autostart is.
enum Stage {
    /// Waiting for every boot service to be ready.
    Services,
    /// Waiting for `pkgd` to finish provisioning, until the absolute tick.
    Waiting { until: u64 },
    /// Opening sessions' apps; the registry self-test still waits for
    /// provisioning to be over, until the absolute tick (0: it ran).
    Apps { selftest_until: u64 },
}

/// One app to open: its registry id and the session's caller identity.
struct Pending {
    id: &'static str,
    caller: SysCred,
}

/// Opens each desktop session's apps (issues #215, #509, #623). Each launch
/// is `--client` (from the manifest's `args` for a package), so the app
/// connects to `xuid` (which retries until the compositor is up) instead of
/// racing it for the display grant.
pub(super) struct Autostart {
    stage: Stage,
    pending: Vec<Pending>,
    /// When a launch refused for a full task table is tried again (0: none).
    retry_at: u64,
    attempts: u64,
}

impl Autostart {
    pub(super) fn new() -> Autostart {
        Autostart {
            stage: Stage::Services,
            pending: Vec::new(),
            retry_at: 0,
            attempts: 0,
        }
    }

    /// The tick the next step is due at with nothing else to wake the
    /// supervisor: a launch retry or a stage's bound. Service readiness,
    /// `pkgd`'s announcements and a session opening arrive as messages, which
    /// wake it anyway.
    pub(super) fn next_due(&self) -> Option<u64> {
        let bound = match self.stage {
            Stage::Services => None,
            Stage::Waiting { until } => Some(until),
            Stage::Apps { selftest_until } => (selftest_until != 0).then_some(selftest_until),
        };
        let retry = (self.retry_at != 0).then_some(self.retry_at);
        [bound, retry].into_iter().flatten().min()
    }

    /// Advance as far as the system allows: once the boot services are ready
    /// and `pkgd` has provisioned the core packages, open the apps of every
    /// desktop session opened so far.
    pub(super) fn step(
        &mut self,
        services: &mut Vec<Service>,
        broker: &mut router::TopicBroker,
        installed: &mut InstalledApps,
        now: u64,
    ) {
        if matches!(self.stage, Stage::Services) {
            if !super::ready::settled(services) {
                return;
            }
            sys::write_str("INIT:AUTOSTART:WAIT pkgd\n");
            self.stage = Stage::Waiting {
                until: now + PROVISION_WAIT,
            };
        }
        if let Stage::Waiting { until } = self.stage {
            if !self.provisioned(services, now, until) {
                return;
            }
        }
        self.take_opened(installed);
        if now >= self.retry_at {
            self.retry_at = 0;
            self.launch_pending(services, broker, installed, now);
        }
        if let Stage::Apps { selftest_until } = self.stage {
            if selftest_until != 0 {
                self.selftest(installed, now, selftest_until);
            }
        }
    }

    /// Queue the apps of every session opened since the last step: the
    /// built-in rows first, then the installed apps that open at login, read
    /// afresh so a package installed during the last session is included.
    fn take_opened(&mut self, installed: &mut InstalledApps) {
        let opened: Vec<SysCred> = core::mem::take(&mut *OPENED.lock());
        if opened.is_empty() {
            return;
        }
        installed.refresh();
        let mut ids = autostart_ids();
        for id in installed.autostart_ids() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        for shell in opened {
            // The session's own identity, exactly: no capability, no label.
            let caller = SysCred::new(shell.uid, shell.gid, 0, 0, shell.session);
            sys::write_str(&format!(
                "INIT:AUTOSTART:SESSION session={} uid={} apps={}\n",
                shell.session,
                shell.uid,
                ids.len()
            ));
            self.pending
                .extend(ids.iter().map(|&id| Pending { id, caller }));
        }
    }

    /// Launch every pending app as its session's user. A session that ended
    /// before its turn (a quick logout) is skipped; a launch refused for a
    /// full task table is tried again later.
    fn launch_pending(
        &mut self,
        services: &mut Vec<Service>,
        broker: &mut router::TopicBroker,
        installed: &mut InstalledApps,
        now: u64,
    ) {
        while let Some(next) = self.pending.first() {
            let (id, caller) = (next.id, next.caller);
            if super::sessions::owner(caller.session) != Some(caller.uid) {
                sys::write_str(&format!(
                    "INIT:AUTOSTART:SKIP app={id} session={} (ended)\n",
                    caller.session
                ));
                self.pending.remove(0);
                continue;
            }
            let request = services::LaunchRequest {
                app: String::from(id),
                args: String::new(),
                session: 0,
            };
            let session = caller.session;
            match launch(services, broker, installed, &request, &caller, true) {
                Ok(_) => {
                    sys::write_str(&format!("INIT:AUTOSTART:PASS app={id} session={session}\n"));
                    self.pending.remove(0);
                    self.attempts = 0;
                }
                Err(_) if self.attempts + 1 >= AUTOSTART_ATTEMPTS => {
                    sys::write_str(&format!("INIT:AUTOSTART:FAIL app={id} session={session}\n"));
                    self.pending.remove(0);
                    self.attempts = 0;
                }
                Err(_) => {
                    // The task table is full: try again once a slot may
                    // have been freed.
                    self.attempts += 1;
                    self.retry_at = now + LAUNCH_SELFTEST_RETRY;
                    return;
                }
            }
        }
    }

    /// Whether `pkgd` has provisioned the core packages (or the wait ran
    /// out, or there is no `pkgd` to wait for); moves on to opening sessions.
    fn provisioned(&mut self, services: &[Service], now: u64, until: u64) -> bool {
        let done = super::provisioning::done();
        let listed = super::provisioning::listed(services);
        let awaited = listed && super::provisioning::awaited(services);
        if !done && awaited && now < until {
            return false;
        }
        // Without a `pkgd` row there is nothing to provision: ready. A `pkgd`
        // that stopped or ran out of time before `done` left the pass short.
        if done || !listed {
            sys::write_str("INIT:AUTOSTART:READY pkgd\n");
        } else if awaited {
            sys::write_str("INIT:AUTOSTART:PARTIAL pkgd did not finish provisioning\n");
        } else {
            sys::write_str("INIT:AUTOSTART:PARTIAL pkgd stopped before provisioning finished\n");
        }
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
