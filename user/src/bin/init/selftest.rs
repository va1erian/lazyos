//! `init`'s boot self-tests: the `top` launch check plus the synthetic policy
//! and launch-cap probes that need no timing-sensitive race.
//!
//! Split out of `init.rs` (issue #194); a pure move, no behavior change.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{self, router, services};
use user::sys::{self, Cred as SysCred};

use super::apps::{available_count, find_app, selftest_builtins, SHELL_APP_ID};
use super::installed::{alias_of, InstalledApps};
use super::launch::{authorize, launch_argument, launch_row, MAX_LAUNCH_PATH};
use super::state::{
    Phase, Restart, Service, CAP_SETUID, LAUNCH_CAP_PER_SESSION, LAUNCH_SELFTEST_ATTEMPTS,
    LAUNCH_SELFTEST_RETRY,
};
use super::supervise::argv;
use svcpolicy::restarts_after;

/// The boot launch self-test: one `Launch("top")` into this supervisor's own
/// session once the boot services are ready, retried while the task table is
/// full. `top` is `Once`, so it exits
/// after its own `SYS:TOP:PASS`, which proves the launched app really ran.
pub(super) struct LaunchSelftest {
    /// When a refused attempt is retried (0: at the next step).
    due: u64,
    attempts: u64,
    done: bool,
}

impl LaunchSelftest {
    pub(super) fn new() -> LaunchSelftest {
        LaunchSelftest {
            due: 0,
            attempts: 0,
            done: false,
        }
    }

    /// When a retry is due, while the test has not run; the first attempt
    /// follows readiness, which wakes the supervisor on its own.
    pub(super) fn next_due(&self) -> Option<u64> {
        (!self.done && self.due != 0).then_some(self.due)
    }

    /// One attempt when due; schedules the next retry on a full task table.
    pub(super) fn step(
        &mut self,
        services: &mut Vec<Service>,
        broker: &mut router::TopicBroker,
        now: u64,
    ) {
        if self.done || now < self.due || !super::ready::settled(services) {
            return;
        }
        self.attempts += 1;
        let caller = SysCred::new(0, 0, CAP_SETUID, 0, 0);
        let request = services::LaunchRequest {
            app: String::from("top"),
            args: String::new(),
            session: 0,
        };
        match launch_row(services, broker, &request, &caller, false) {
            Ok(_) => self.done = true,
            Err(_) if self.attempts >= LAUNCH_SELFTEST_ATTEMPTS => {
                self.done = true;
                sys::write_str("INIT:LAUNCH:FAIL app=top (spawn unavailable)\n");
            }
            Err(_) => self.due = now + LAUNCH_SELFTEST_RETRY,
        }
    }
}

/// The launch-policy self-test: a synthetic session owner may launch into its
/// own session, a foreign non-root caller may not, and root may go anywhere.
/// Prints `INIT:LAUNCH:DENIED:PASS` when the denial holds.
pub(super) fn selftest_launch_policy() {
    let owner = SysCred::new(1000, 1000, 0, 0, 7);
    let foreign = SysCred::new(1000, 1000, 0, 0, 8);
    let root = SysCred::new(0, 0, 0, 0, 3);
    let denied = authorize(&foreign, 7).is_err();
    let owner_ok = authorize(&owner, 7).is_ok();
    let root_ok = authorize(&root, 8).is_ok();
    if denied && owner_ok && root_ok {
        sys::write_str("INIT:LAUNCH:DENIED:PASS\n");
    } else {
        sys::write_str("INIT:LAUNCH:DENIED:FAIL policy check failed\n");
    }
}

/// The launch-cap self-test: a session already holding
/// [`LAUNCH_CAP_PER_SESSION`] reserved launched rows gets `-EAGAIN` for one
/// more, and the refused call leaves the supervision table unchanged (nothing
/// was spawned). One row is `Running` and the other `Restarting` (mid crash
/// backoff), so the test also covers a launched row that no longer holds a
/// task slot but will reclaim one from the supervision loop's backoff sweep
/// without another cap check ([`running_in_session`] must still count it).
/// Exercises the real [`launch`] against a synthetic table, the same way
/// [`selftest_launch_policy`] exercises [`authorize`] directly, so the boot
/// self-test needs no timing-sensitive race against real processes exiting
/// or crashing. Prints `INIT:LAUNCH:CAP:PASS`.
pub(super) fn selftest_launch_cap() {
    // Any always-shipped app works: the call is refused by the cap before it
    // spawns. `top` is the launch self-test's target, which the desktop profile
    // does not ship, so use `messengerctl`, which every services image carries.
    let Some(app) = find_app("messengerctl") else {
        return sys::write_str("INIT:LAUNCH:CAP:FAIL messengerctl is not registered\n");
    };
    const SESSION: u64 = 4243;
    let cred = SysCred::new(1000, 1000, 0, 0, SESSION);
    let reserved_phases = [Phase::Running, Phase::Restarting];
    let mut services: Vec<Service> = reserved_phases
        .into_iter()
        .cycle()
        .take(LAUNCH_CAP_PER_SESSION)
        .map(|phase| {
            let mut row = Service::from_app(app, None, cred);
            row.phase = phase;
            row
        })
        .collect();
    let mut broker = router::TopicBroker::new("os.lazy.selftest.sink");
    let request = services::LaunchRequest {
        app: String::from("messengerctl"),
        args: String::new(),
        session: SESSION,
    };
    let capped = matches!(
        launch_row(&mut services, &mut broker, &request, &cred, false),
        Err(messenger::Error::Errno(code)) if code == -messenger::errno::EAGAIN
    );
    if capped && services.len() == LAUNCH_CAP_PER_SESSION {
        sys::write_str(&format!(
            "INIT:LAUNCH:CAP:PASS session={SESSION} cap={LAUNCH_CAP_PER_SESSION}\n"
        ));
    } else {
        sys::write_str("INIT:LAUNCH:CAP:FAIL cap did not hold\n");
    }
}

/// The launch-argument self-test: `Launch`'s `args` is one absolute path,
/// passed through as a single `argv` item (spaces and quotes included, since
/// `spawnv` splits nothing), and anything else is refused with `-EINVAL`.
/// Prints `INIT:LAUNCH:ARGS:PASS`.
pub(super) fn selftest_launch_args() {
    let long = format!("/{}", "a".repeat(MAX_LAUNCH_PATH));
    let bad = ["relative.txt", "-flag", "/a\0b", "/a\nb", long.as_str()];
    let refused = bad.iter().all(|arg| {
        matches!(
            launch_argument(arg),
            Err(messenger::Error::Errno(code)) if code == -messenger::errno::EINVAL
        )
    });
    let one_item = |path: &str| launch_argument(path).ok().flatten().as_deref() == Some(path);
    let plain = one_item("/u/a.txt");
    let spaced = one_item("/u/my file.txt") && one_item("/u/say \"hi\".txt");
    let empty = matches!(launch_argument(""), Ok(None));
    // The row's `argv` keeps the spaced path as one item after the fixed args.
    let whole = find_app("installer").is_none_or(|app| {
        let row = Service::from_app(app, Some(String::from("/a b/c d.txt")), SysCred::default());
        argv(&row, 0)[1..] == ["--client", "/a b/c d.txt", "attempt=1"]
    });
    if refused && plain && spaced && empty && whole {
        sys::write_str("INIT:LAUNCH:ARGS:PASS\n");
    } else {
        sys::write_str("INIT:LAUNCH:ARGS:FAIL argument validation broke\n");
    }
}

/// The desktop shell's supervision self-test (issue #157): a launched
/// `lazyshell` row keeps the session's stamped credentials, runs as a `xuid`
/// client, and its `Always` policy brings it back after a clean exit, a crash
/// and a kill alike (status 137 is how a SIGKILL'd Linux task reports). The
/// restart itself is the ordinary backoff sweep (`INIT:RESTART:PASS
/// name=lazyshell` when it happens). Prints `INIT:SHELL:PASS`.
pub(super) fn selftest_shell_supervision() {
    let Some(app) = find_app(SHELL_APP_ID) else {
        return sys::write_str("INIT:SHELL:FAIL lazyshell is not registered\n");
    };
    let cred = SysCred::new(1000, 1000, 0, 0, 5);
    let row = Service::from_app(app, None, cred);
    let client = argv(&row, 0).iter().any(|arg| arg == "--client");
    let stamped = row
        .cred
        .is_some_and(|stamped| stamped.session == 5 && stamped.uid == 1000);
    let always = row.restart == Restart::Always
        && [0, 1, 137]
            .iter()
            .all(|status| restarts_after(row.restart, *status));
    if row.launched && client && stamped && always {
        sys::write_str("INIT:SHELL:PASS restart=always\n");
    } else {
        sys::write_str("INIT:SHELL:FAIL shell row is not supervised as always-restart\n");
    }
}

/// The app registry self-test (issue #509), once `pkgd` provisioned: the
/// built-in rows are well formed, and every core package the image ships in
/// `/system/packages` is installed, recorded as core, answers to its short
/// alias and has its program on disk. Prints `INIT:APPS:PASS count=<n>` (the
/// launchable apps) and `INIT:APPS:SHIPPED count=<n>` (the core packages).
pub(super) fn selftest_apps(installed: &InstalledApps) -> String {
    let shipped: Vec<String> = user::files::list(fhs::SYSTEM_PACKAGES)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| pkgstore::provision::package_file_name(&entry.name))
        .map(String::from)
        .collect();
    let mut missing: Vec<&str> = Vec::new();
    for name in &shipped {
        let ok = installed.apps().iter().any(|app| {
            app.id == name.as_str()
                && app.core
                && alias_of(app.id).is_none_or(|short| installed.find(short).is_some())
                && user::files::stat(app.path).is_ok()
        });
        if !ok {
            missing.push(name);
        }
    }
    let count = available_count() + installed.apps().len();
    if !selftest_builtins() {
        String::from("INIT:APPS:FAIL the built-in registry is malformed\n")
    } else if !missing.is_empty() {
        format!("INIT:APPS:FAIL core packages not installed: {missing:?}\n")
    } else {
        format!(
            "INIT:APPS:PASS count={count}\nINIT:APPS:SHIPPED count={}\n",
            shipped.len()
        )
    }
}
