//! Telling the desktop that an app failed (issue #549).
//!
//! A launched app that ends failed (`svcpolicy::tells_desktop`: it failed
//! while starting, kept crashing, or failed with no restart policy) becomes
//! one `system/events/app/<id>` event on the central broker, carrying its
//! display name, its exit status in words and the reason it reported itself
//! through `init.ReportFailure`. LazyShell, subscribed for its own session,
//! turns it into the one message the user sees.
//!
//! The broker is `messengerd`, a child of this supervisor that never calls
//! it, and every publish carries a deadline, so a wedged broker costs a
//! notice, never the supervisor. Serial: `INIT:APP:FAILED app=<id>
//! status=<n> startup=<0|1> reason=<text>` for every failure, then
//! `INIT:APP:NOTICE:PASS app=<id> matched=<n>` once published (`FAIL
//! err=<errno>` otherwise).

use alloc::format;
use alloc::string::ToString;

use svcpolicy::{clean_reason, describe_status, Cause, Outcome};
use user::central;
use user::messenger::{registry, services, topics_client, Message};
use user::sys;

use super::state::{Phase, Service};

/// Ticks one publish may take (100 Hz): half a second.
const PUBLISH_TICKS: u64 = 50;

/// One failed app, ready to publish.
pub(super) struct Failure {
    app: &'static str,
    event: services::init::wire::AppFailure,
}

impl Failure {
    /// The failure of `row`, which just ended as `outcome`.
    pub(super) fn of(row: &Service, outcome: &Outcome) -> Failure {
        let status = row.last_status.unwrap_or(0);
        let startup = matches!(
            outcome,
            Outcome::Failed {
                cause: Cause::StartUp,
                ..
            }
        );
        Failure {
            app: row.name,
            event: services::init::wire::AppFailure {
                name: if row.title.is_empty() {
                    row.name.to_string()
                } else {
                    row.title.clone()
                },
                status,
                summary: describe_status(status),
                reason: row.reason.clone().unwrap_or_default(),
                session: row.cred.map_or(0, |cred| cred.session),
                startup,
                at: sys::clock(),
            },
        }
    }
}

/// The central-broker connection the notices go out on.
pub(super) struct Notices {
    bus: Option<central::Bus>,
}

impl Notices {
    pub(super) const fn new() -> Notices {
        Notices { bus: None }
    }

    /// Log `failure` and publish it, reconnecting once to a restarted broker.
    pub(super) fn publish(&mut self, failure: &Failure) {
        let event = &failure.event;
        sys::write_str(&format!(
            "INIT:APP:FAILED app={} status={} startup={} reason={}\n",
            failure.app,
            event.status,
            u8::from(event.startup),
            event.reason
        ));
        let mut outcome = self.try_publish(failure);
        if outcome.is_err() {
            self.bus = None;
            outcome = self.try_publish(failure);
        }
        match outcome {
            Ok(matched) => sys::write_str(&format!(
                "INIT:APP:NOTICE:PASS app={} matched={matched}\n",
                failure.app
            )),
            Err(code) => sys::write_str(&format!(
                "INIT:APP:NOTICE:FAIL app={} err={}\n",
                failure.app, -code
            )),
        }
    }

    fn try_publish(&mut self, failure: &Failure) -> Result<u64, i64> {
        use services::init::wire;
        let topic = wire::name_system_events_app(failure.app).map_err(|_| -22i64)?;
        let payload = wire::encode_system_events_app(&failure.event).map_err(|_| -22i64)?;
        if self.bus.is_none() {
            // A resolve is one registry lookup: no retry loop that could
            // hold the supervisor while `messengerd` is away.
            let endpoint = registry::resolve(topics_client::NAME).map_err(errno)?;
            self.bus = Some(central::Bus::from_endpoint(endpoint));
        }
        let bus = self.bus.as_mut().ok_or(-1)?;
        bus.publish_by(
            &topic,
            &payload,
            wire::TOPIC_SYSTEM_EVENTS_APP_RETAINED,
            Some(sys::clock() + PUBLISH_TICKS),
        )
        .map_err(errno)
    }
}

/// `ReportFailure`: remember why the sender's run is failing, when the sender
/// is the running task of a launched row. Anything else is ignored.
pub(super) fn report(services: &mut [Service], message: &Message) {
    let Ok(args) = services::init::wire::decode_report_failure_args(&message.parcel.body) else {
        return;
    };
    let Some(row) = services
        .iter_mut()
        .find(|row| row.launched && row.phase == Phase::Running && row.pid == message.sender)
    else {
        return;
    };
    let reason = clean_reason(&args.reason);
    sys::write_str(&format!(
        "INIT:APP:REASON app={} pid={} reason={reason}\n",
        row.name, row.pid
    ));
    row.reason = (!reason.is_empty()).then_some(reason);
}

/// A messenger error as a negative errno.
fn errno(error: user::messenger::Error) -> i64 {
    error.errno().unwrap_or(-5)
}
