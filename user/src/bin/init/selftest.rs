//! `init`'s boot self-tests: the `top` launch check plus the synthetic policy
//! and launch-cap probes that need no timing-sensitive race.
//!
//! Split out of `init.rs` (issue #194); a pure move, no behavior change.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{self, router, services};
use user::sys::{self, Cred as SysCred};

use super::apps::find_app;
use super::launch::{authorize, launch};
use super::state::{
    Phase, Service, CAP_SETUID, LAUNCH_CAP_PER_SESSION, LAUNCH_SELFTEST_ATTEMPTS,
    LAUNCH_SELFTEST_DELAY, LAUNCH_SELFTEST_RETRY,
};

/// The boot launch self-test: one `Launch("top")` into this supervisor's own
/// session, retried while the task table is full. `top` is `Once`, so it exits
/// after its own `SYS:TOP:PASS`, which proves the launched app really ran.
pub(super) struct LaunchSelftest {
    due: u64,
    attempts: u64,
    done: bool,
}

impl LaunchSelftest {
    pub(super) fn new() -> LaunchSelftest {
        LaunchSelftest {
            due: sys::clock() + LAUNCH_SELFTEST_DELAY,
            attempts: 0,
            done: false,
        }
    }

    /// One attempt when due; schedules the next retry on a full task table.
    pub(super) fn step(
        &mut self,
        services: &mut Vec<Service>,
        broker: &mut router::TopicBroker,
        now: u64,
    ) {
        if self.done || now < self.due {
            return;
        }
        self.attempts += 1;
        let caller = SysCred::new(0, 0, CAP_SETUID, 0, 0);
        let request = services::LaunchRequest {
            app: String::from("top"),
            args: String::new(),
            session: 0,
        };
        match launch(services, broker, &request, &caller) {
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
            let mut row = Service::from_app(app, "", cred);
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
        launch(&mut services, &mut broker, &request, &cred),
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
