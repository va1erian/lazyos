//! The audit trail: every request, whatever came of it.
//!
//! One serial line (`ELEVD:REQUEST ...`) and one `system/events/elevd/request`
//! record on the central broker, which `logd` appends to `/logs/elevd.log`.
//! A record never carries a password: the summary is built without it
//! (`elevpolicy::Operation::summary`).

use alloc::format;
use alloc::string::String;

use elevpolicy::approvals::Caller;
use messenger_generated::os_lazy_elevd_v1 as wire;
use user::central;
use user::messenger;
use user::sys;

/// Publish attempts before a record is given up (the serial line stays).
const PUBLISH_ATTEMPTS: usize = 3;

/// One request as it is recorded.
pub(crate) struct Entry {
    pub(crate) operation: String,
    pub(crate) summary: String,
    pub(crate) caller: Caller,
    pub(crate) user: String,
    pub(crate) admin: String,
}

impl Entry {
    pub(crate) fn new(operation: &str, caller: Caller) -> Entry {
        // An operation name off the wire is shown, never parsed: keep it
        // short and printable.
        let operation: String = operation
            .chars()
            .take(32)
            .map(|c| if c.is_ascii_graphic() { c } else { '?' })
            .collect();
        Entry {
            operation,
            summary: String::new(),
            caller,
            user: String::new(),
            admin: String::new(),
        }
    }
}

/// The broker connection, made on first use and again after a failure.
pub(crate) struct Audit {
    bus: Option<central::Bus>,
}

impl Audit {
    pub(crate) fn new() -> Audit {
        Audit { bus: None }
    }

    /// Record `entry` with its `outcome`.
    pub(crate) fn log(&mut self, entry: &Entry, outcome: &str) {
        let admin = if entry.admin.is_empty() {
            "-"
        } else {
            entry.admin.as_str()
        };
        sys::write_str(&format!(
            "ELEVD:REQUEST op={} uid={} label={} session={} admin={} outcome={outcome} summary={}\n",
            entry.operation,
            entry.caller.uid,
            entry.caller.label,
            entry.caller.session,
            admin.replace(' ', "_"),
            entry.summary
        ));
        let record = wire::Record {
            operation: entry.operation.clone(),
            summary: entry.summary.clone(),
            uid: entry.caller.uid,
            user: entry.user.clone(),
            label: entry.caller.label,
            admin: entry.admin.clone(),
            outcome: String::from(outcome),
        };
        for _ in 0..PUBLISH_ATTEMPTS {
            if self.bus.is_none() {
                self.bus = central::Bus::connect().ok();
            }
            let Some(bus) = self.bus.as_mut() else {
                messenger::park_tick();
                continue;
            };
            match wire::publish_system_events_elevd_request(bus, &record) {
                Ok(_) => return,
                Err(_) => {
                    self.bus = None;
                    messenger::park_tick();
                }
            }
        }
    }
}
