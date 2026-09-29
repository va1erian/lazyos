//! `init`'s boot autostart: opening the desktop session's shipped apps, one
//! per stagger interval so their start-up does not all land at once.
//!
//! Split out of `init.rs` (issue #194); a pure move, no behavior change.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{router, services};
use user::sys::{self, Cred as SysCred};

use super::apps::autostart_ids;
use super::launch::launch_row;
use super::state::{
    Service, AUTOSTART_ATTEMPTS, AUTOSTART_DELAY, AUTOSTART_STAGGER, CAP_SETUID,
    LAUNCH_SELFTEST_RETRY,
};

/// Opens the image's `autostart` apps (the desktop session, issue #215), one
/// per [`AUTOSTART_STAGGER`] ticks so their start-up (each is a `std` program
/// that maps a window buffer) does not all land at once, retrying while the
/// task table is full. Each launch is `--client`, so the app connects to
/// `xuid` (which retries until the compositor is up) instead of racing it for
/// the display grant. Prints `INIT:AUTOSTART:PASS app=<id>` per app.
pub(super) struct Autostart {
    pending: Vec<&'static str>,
    due: u64,
    attempts: u64,
}

impl Autostart {
    pub(super) fn new() -> Autostart {
        Autostart {
            pending: autostart_ids(),
            due: sys::clock() + AUTOSTART_DELAY,
            attempts: 0,
        }
    }

    /// One launch when due; a refusal retries later and gives up after
    /// [`AUTOSTART_ATTEMPTS`] tries so one broken app cannot block the rest.
    pub(super) fn step(
        &mut self,
        services: &mut Vec<Service>,
        broker: &mut router::TopicBroker,
        now: u64,
    ) {
        let Some(&id) = self.pending.first() else {
            return;
        };
        if now < self.due {
            return;
        }
        let caller = SysCred::new(0, 0, CAP_SETUID, 0, 0);
        let request = services::LaunchRequest {
            app: String::from(id),
            args: String::new(),
            session: 0,
        };
        match launch_row(services, broker, &request, &caller, true) {
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
}
