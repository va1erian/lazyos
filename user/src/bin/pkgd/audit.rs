//! `pkgd`'s audit trail: every install, removal and refusal is one record in the
//! hash-chained `/logs/pkg.log` (`pkgstore::audit`) *and* one event on
//! `system/events/pkg/<op>`, which `logd` (subscribed to `system/events/#`)
//! retains independently, so rewriting the file cannot erase what `logd` saw.
//!
//! The record is the generated `PkgEvent`, the same bytes on the wire and in the
//! log. A failed append or publish never undoes the operation it describes (the
//! files are already on disk); it is reported on serial instead.

use alloc::format;
use alloc::string::String;

use pkgstore::audit::{verify, Chain};
use pkgstore::layout;
use user::central;
use user::files;
use user::messenger::{self, pkgd::wire, pkgd::PkgEvent};
use user::sys;

/// The log file is read whole at startup, so it is bounded; a log that grows
/// past this is rotated aside like a broken one.
const MAX_LOG: usize = 8 * 1024 * 1024;
/// Publish attempts before a record goes unannounced.
const PUBLISH_ATTEMPTS: usize = 8;

/// The chain state, the log's availability and the broker connection.
pub(crate) struct Audit {
    chain: Chain,
    /// Whether the log file can be written (`/logs` is writable).
    persistent: bool,
    bus: Option<central::Bus>,
}

impl Audit {
    pub(crate) const fn new() -> Audit {
        Audit {
            chain: Chain {
                count: 0,
                last: pkgstore::audit::GENESIS,
            },
            persistent: false,
            bus: None,
        }
    }

    /// Verify the log on disk and continue its chain. Prints
    /// `PKGD:AUDIT:PASS n=<count>`, or `PKGD:AUDIT:FAIL <why>` after moving the
    /// broken log aside and starting a fresh chain (whose first record says so).
    pub(crate) fn load(&mut self) {
        self.persistent = true;
        let text = match files::read_up_to(layout::LOG_FILE, MAX_LOG) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|_| "the log is not text".into()),
            Err(code) if code == super::store::ENOENT => Ok(String::new()),
            Err(27) => Err(String::from("the log is too large to verify")),
            Err(code) => Err(format!("the log cannot be read: {}", files::describe(code))),
        };
        let verdict = text.and_then(|text| verify(&text).map_err(|error| format!("{error}")));
        match verdict {
            Ok(chain) => {
                self.chain = chain;
                sys::write_str(&format!("PKGD:AUDIT:PASS n={}\n", chain.count));
            }
            Err(reason) => {
                sys::write_str(&format!("PKGD:AUDIT:FAIL {reason}\n"));
                self.rotate(&reason);
            }
        }
    }

    /// Move a log that failed verification aside (kept as evidence) and begin a
    /// new chain with a record naming the failure.
    fn rotate(&mut self, reason: &str) {
        let aside = format!("{}.bad-{}", layout::LOG_FILE, sys::clock());
        let moved = files::rename(layout::LOG_FILE, &aside).is_ok();
        self.chain = Chain::default();
        let detail = if moved {
            format!("the audit log failed verification ({reason}); kept as {aside}")
        } else {
            format!("the audit log failed verification ({reason}) and could not be set aside")
        };
        self.record(&PkgEvent {
            op: String::from("denied"),
            system_name: String::new(),
            version: String::new(),
            install_dir: String::new(),
            digest: String::new(),
            actor_uid: 0,
            ok: false,
            detail,
        });
    }

    /// Mark the volume unusable: records are published but not written.
    pub(crate) fn set_volatile(&mut self) {
        self.persistent = false;
    }

    /// Append `event` to the log and publish it.
    pub(crate) fn record(&mut self, event: &PkgEvent) {
        if let Ok(bytes) = wire::encode_pkg_event(event) {
            if self.persistent {
                let mut next = self.chain;
                let line = next.append(&bytes);
                match files::append_file(layout::LOG_FILE, line.as_bytes()) {
                    Ok(()) => self.chain = next,
                    Err(code) => sys::write_str(&format!(
                        "PKGD:AUDIT:WRITE:FAIL {}\n",
                        files::describe(code)
                    )),
                }
            }
        }
        self.publish(event);
    }

    /// Publish on `system/events/pkg/<op>`; an unreachable broker drops the
    /// event after a few ticks rather than stalling the request.
    pub(crate) fn publish(&mut self, event: &PkgEvent) {
        for _ in 0..PUBLISH_ATTEMPTS {
            if self.bus.is_none() {
                self.bus = central::Bus::connect().ok();
            }
            let Some(bus) = self.bus.as_mut() else {
                messenger::park_tick();
                continue;
            };
            match wire::publish_system_events_pkg(bus, &event.op, event) {
                Ok(_) => return,
                Err(_) => {
                    self.bus = None;
                    messenger::park_tick();
                }
            }
        }
    }
}
