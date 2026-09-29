//! `demo=1` boot evidence, in two steps over the real Messenger fabric.
//!
//! 1. The `confd` zone key and its change notification move the service: it
//!    writes `Europe/Paris` through its own `confd` link (what `SetZone`
//!    does), deletes the key, and requires the *notification* to bring the
//!    zone back to UTC (nothing else in the service reverts it).
//! 2. It spawns `timectl selftest` (`TIMECTL.ELF`), which calls every
//!    `os.lazy.timed.v1` method as a client, and reaps it.

use alloc::format;
use user::sys;

use crate::state::State;

/// Ticks (100 Hz) each step may take before it is reported as failed. Boot
/// under software emulation is slow, so this is generous.
const TIMEOUT_TICKS: u64 = 3000;
/// The client self-test, spawned with its command as the argument string.
const CLIENT: &[u8] = b"TIMECTL.ELF selftest\0";

enum Phase {
    /// Not requested.
    Off,
    /// Waiting for `confd` to be reachable.
    Start,
    /// Wrote Paris; delete the key next.
    Wrote(u64),
    /// Deleted the key; waiting for the notification to reset the zone.
    AwaitReset(u64),
    /// The `timectl` child is running.
    Client(u64),
    Done,
}

pub(super) struct Demo {
    phase: Phase,
}

impl Demo {
    pub(super) fn from_args() -> Demo {
        let mut buffer = [0u8; 128];
        let len = sys::service_args(&mut buffer).min(buffer.len());
        let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
        let on = text.split_whitespace().any(|part| part == "demo=1");
        Demo {
            phase: if on { Phase::Start } else { Phase::Off },
        }
    }

    pub(super) fn step(&mut self, state: &mut State) {
        let now = sys::clock();
        match self.phase {
            Phase::Off | Phase::Done => {}
            Phase::Start => {
                if !state.has_confd() {
                    return;
                }
                let Some(paris) = timezone::find("Europe/Paris") else {
                    return self.fail("Europe/Paris is missing from the table");
                };
                match state.store_zone(paris) {
                    Ok(()) => self.phase = Phase::Wrote(now + TIMEOUT_TICKS),
                    Err(_) => self.fail("could not write the zone to confd"),
                }
            }
            Phase::Wrote(deadline) => match state.clear_zone() {
                Ok(()) => self.phase = Phase::AwaitReset(deadline.max(now + TIMEOUT_TICKS)),
                Err(_) => self.fail("could not delete the zone key"),
            },
            Phase::AwaitReset(deadline) => {
                if state.zone.name == timezone::DEFAULT_ZONE {
                    sys::write_str("TIMED:DEMO:PASS confd write -> notify -> zone follows\n");
                    self.spawn_client(now);
                } else if now > deadline {
                    self.fail("the confd change was not observed");
                }
            }
            Phase::Client(deadline) => {
                if sys::wait(now).is_some() {
                    self.phase = Phase::Done;
                } else if now > deadline {
                    self.fail("timectl did not finish");
                }
            }
        }
    }

    fn spawn_client(&mut self, now: u64) {
        match sys::spawn(CLIENT) {
            Some(pid) => {
                sys::write_str(&format!("TIMED:CLIENT:START pid={pid}\n"));
                self.phase = Phase::Client(now + TIMEOUT_TICKS);
            }
            None => self.fail("cannot spawn TIMECTL.ELF"),
        }
    }

    fn fail(&mut self, why: &str) {
        sys::write_str("TIMED:DEMO:FAIL ");
        sys::write_str(why);
        sys::write_str("\n");
        self.phase = Phase::Done;
    }
}
