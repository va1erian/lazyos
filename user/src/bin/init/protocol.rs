//! `init`'s request protocol: the cached `Services` reply, the supervision
//! table's wire rows, and the non-blocking dispatch of broker/registry/launch
//! messages on the supervisor endpoint.
//!
//! Split out of `init.rs` (issue #194); `Shutdown` and the `Launch` refusal
//! while it runs are docs/shutdown.md.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use user::central;
use user::sys;
use user::trace;

use messenger_generated::os_lazy_init_app_v1 as app_wire;
use user::messenger::services::init::wire as init_wire;
use user::messenger::{self, services, Endpoint};

use super::installed::InstalledApps;
use super::lifecycle::Stops;
use super::logout::Logouts;
use super::serve::{AppHandler, Handler};
use super::shutdown::Shutdown;
use super::state::Service;

/// Cached `Services` rows.
///
/// The supervisor re-reads its supervision table only when a cheap
/// fingerprint (phase/pid/restarts) says it changed (reading the label of
/// each installed app costs a syscall); callers get a clone of the rows.
#[derive(Default)]
pub(super) struct StatusCache {
    fingerprint: Option<u64>,
    rows: Vec<services::ServiceStatus>,
}

impl StatusCache {
    /// The current rows, re-reading the table when it changed.
    pub(super) fn rows(&mut self, services: &[Service]) -> Vec<services::ServiceStatus> {
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut fingerprint = 0xcbf2_9ce4_8422_2325u64;
        for service in services {
            for byte in service
                .phase
                .label()
                .bytes()
                .chain(service.pid.to_le_bytes())
                .chain(service.restarts.to_le_bytes())
            {
                fingerprint ^= byte as u64;
                fingerprint = fingerprint.wrapping_mul(PRIME);
            }
        }
        if self.fingerprint != Some(fingerprint) {
            self.rows = status_rows(services);
            self.fingerprint = Some(fingerprint);
        }
        self.rows.clone()
    }
}

/// What the supervisor's request handlers share: its tables and the
/// shutdown, if one is running.
pub(super) struct Supervisor<'a> {
    pub(super) services: &'a mut Vec<Service>,
    pub(super) broker: &'a mut central::Publisher,
    pub(super) installed: &'a mut InstalledApps,
    pub(super) cache: &'a mut StatusCache,
    pub(super) shutdown: &'a mut Option<Shutdown>,
    /// `accountsd`'s home changes in flight (their replies are deferred).
    pub(super) homes: &'a mut super::homes::HomeJobs,
    /// The `Stop`s waiting for their targets' exits.
    pub(super) stops: &'a mut Stops,
    /// The sessions being logged out, waiting for their apps to quit.
    pub(super) logouts: &'a mut Logouts,
    /// The login records and provisioning events read off the broker.
    pub(super) feeds: &'a mut super::feeds::Feeds,
}

/// Serve queued control calls without blocking.
pub(super) fn serve_pending(
    state: &mut Supervisor,
    server: &Endpoint,
    buffer: &mut [u8],
) -> messenger::Result<()> {
    while let Some(message) = server.poll_recv_with(buffer)? {
        let mut handler = Handler {
            state: &mut *state,
            txn: message.txn,
            held: false,
        };
        let reply = init_wire::dispatch(&mut handler, &message);
        // A `Stop` and a `Home` are answered later; a one-way has no txn.
        let held = handler.held;
        if let (Some(txn), false) = (message.txn, held) {
            server.reply_or_drop(txn, &reply)?;
        }
    }
    Ok(())
}

/// Serve queued `os.lazy.init.app.v1` calls (`Watch`) on its own endpoint.
pub(super) fn serve_app_pending(
    services: &mut [Service],
    server: &Endpoint,
    buffer: &mut [u8],
) -> messenger::Result<()> {
    while let Some(message) = server.poll_recv_with(buffer)? {
        let reply = app_wire::dispatch(&mut AppHandler { services }, &message);
        if let Some(txn) = message.txn {
            // An app's line must never take the supervisor down: a reply
            // that cannot be delivered is that caller's loss alone.
            if let Err(error) = server.reply_or_drop(txn, &reply) {
                trace::line!(
                    trace::Channel::Init,
                    "APP:REPLY:FAIL err={}",
                    error.errno().unwrap_or(0)
                );
            }
        }
    }
    Ok(())
}

/// The kernel id of the label an installed app's running task carries, read
/// back from the kernel (`ServiceStatus::label`); `0` for a row that is not an
/// installed app or is not running. Called when the table is re-encoded,
/// which a new task (a new pid) triggers.
fn stamped_label(service: &Service) -> u32 {
    if service.label.is_none() || service.pid == 0 {
        return 0;
    }
    sys::cred_get(Some(service.pid)).map_or(0, |cred| cred.label_id)
}

/// Snapshot the supervision table as wire rows.
fn status_rows(services: &[Service]) -> Vec<services::ServiceStatus> {
    services
        .iter()
        .map(|service| {
            let deps = service.deps.iter().fold(String::new(), |mut text, dep| {
                if !text.is_empty() {
                    text.push(',');
                }
                text.push_str(dep);
                text
            });
            services::ServiceStatus {
                name: service.name.to_string(),
                state: service.phase.label().to_string(),
                pid: service.pid,
                restarts: service.restarts,
                deps,
                health: service.phase.health().to_string(),
                label: stamped_label(service),
            }
        })
        .collect()
}
