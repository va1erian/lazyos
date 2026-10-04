//! `healthd` (`/system/bin/healthd`): service health aggregation and the retained
//! `system/health/*` topic (issue #93).
//!
//! `healthd` is the S2 health service from the platform plan section 4.3. It:
//!
//! * registers [`services::HEALTHD_NAME`] and serves `Report` (a service
//!   publishes `health/<name>` with status/detail) and `Status` (a snapshot of
//!   the retained rows plus the aggregate);
//! * reconciles `init`'s supervision table and derives a health row per
//!   service from its phase and dependency state, so a crash that `init` is
//!   restarting shows up here as `degraded` and a dependency that is not `ok`
//!   degrades its dependents;
//! * publishes every row retained on `system/health/<name>` through its
//!   [`router::TopicBroker`], and the aggregate on `system/health/summary`;
//! * prints one `HEALTH:SVC:PASS <name>` / `HEALTH:SVC:FAIL <name> (<status>)`
//!   serial line per transition, which is the headless evidence that the
//!   system came up and that the crash test recovered.
//!
//! Status order is `ok` < `degraded` < `down`; a reported heartbeat wins over
//! the derived row unless the derived one is worse (a service cannot report
//! itself healthy while the supervisor sees it crash).
//!
//! The supervision poll is deliberately slow and skips rows whose phase and
//! detail did not change: the user runtime's bump allocator (`user/src/heap.rs`)
//! never reclaims memory, so a per-tick reconcile would leak. The retained
//! topics are the fast path; the poll is the safety net.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "healthd/aggregate.rs"]
mod aggregate;
#[path = "healthd/handler.rs"]
mod handler;

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::{self, registry, router, services, wait, Endpoint, Error};
use user::sys;

use aggregate::{apply_state, records, refresh, summary};
use handler::dispatch;

/// How often the supervisor table is reconciled (PIT ticks). Service events
/// are the fast path; this poll only catches detail changes and lost events,
/// and it is slow because the user runtime's bump allocator (`user/src/heap.rs`)
/// never reclaims the reply buffers each call allocates.
const POLL_TICKS: u64 = 20_000;
/// How soon to look again while `init` cannot be reached or subscribed to
/// (it registers before it starts any service, so this is a fallback).
const CONNECT_RETRY_TICKS: u64 = 5;
/// Age at which a `Report` stops overriding the derived row (PIT ticks).
const REPORT_TTL: u64 = 250;

/// One aggregated health row.
struct HealthRow {
    name: String,
    /// Supervision phase from the last reconcile (`running`, `restarting`, ...).
    state: String,
    /// Task slot from the last reconcile.
    pid: u64,
    /// Restart count from the last reconcile.
    restarts: u64,
    /// Comma-separated dependency names from the last reconcile.
    deps: String,
    /// Aggregated status (`ok`/`degraded`/`down`).
    status: String,
    /// Human-readable detail.
    detail: String,
    /// Tick the row was last updated.
    tick: u64,
    /// Tick of the last `Report` for this row (`0` = none).
    reported_at: u64,
}

impl HealthRow {
    fn record(&self) -> services::HealthRecord {
        services::HealthRecord {
            name: self.name.clone(),
            status: self.status.clone(),
            detail: self.detail.clone(),
            tick: self.tick,
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("healthd: aggregating service health (issue #93)\n");
    if let Err(error) = run() {
        sys::write_str("healthd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the service and aggregate until the system ends.
fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(
        services::HEALTHD_NAME,
        &published,
        &[services::HEALTHD_INTERFACE, router::INTERFACE],
        0,
    )?;
    let mut broker = router::TopicBroker::new("os.lazy.health.sink");
    let mut rows: Vec<HealthRow> = Vec::new();
    let mut init: Option<Endpoint> = None;
    let mut bus: Option<router::Bus> = None;
    let mut events: Option<router::Subscriber> = None;
    let mut next_poll = 0u64;
    let mut summary_state: Option<(String, String)> = None;
    // Reused receive buffers: the user bump allocator never reclaims per-call
    // buffers, so long-lived loops must not allocate one per message.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut reply_buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        // Service state changes arrive on `init`'s retained topic; subscribe
        // as soon as the supervisor is up (the replay seeds the rows).
        if events.is_none() {
            bus = connect_or_keep(bus, services::INIT_NAME);
            if let Some(bus) = &mut bus {
                // Every service: the `+` wildcard of the declared pattern.
                events = services::init::wire::subscribe_system_events_service(bus, "+").ok();
            }
        }

        // Resolve the supervisor when it becomes available (logd/healthd can
        // start before it has finished registering itself).
        if init.is_none() {
            init = registry::resolve(services::INIT_NAME).ok();
        }
        let now = sys::clock();
        if now >= next_poll {
            let supervisor = init;
            if let Some(endpoint) = &supervisor {
                match services::fetch_services_with(endpoint, &mut reply_buffer) {
                    Ok(statuses) => refresh(&mut rows, &statuses, &mut broker, &mut summary_state),
                    // `init` is gone: drop the handle and retry the resolution.
                    Err(_) => init = None,
                }
            }
            next_poll = sys::clock() + POLL_TICKS;
        }

        // Park on requests and `init`'s service events at once: both are
        // served as they arrive, and an idle `healthd` does not wake (P7).
        let deadline = if events.is_none() || init.is_none() {
            sys::clock() + CONNECT_RETRY_TICKS
        } else {
            next_poll
        };
        let ready = match &events {
            Some(sub) => wait::wait_any(&[server, sub.endpoint()], 0, Some(deadline)),
            None => wait::wait_any(&[server], 0, Some(deadline)),
        };
        let ready = match ready {
            Ok(ready) => ready,
            Err(Error::Errno(code)) if code == -messenger::errno::ETIMEDOUT => 0,
            Err(error) => return Err(error),
        };
        if ready & 0b10 != 0 {
            let fed = take_event(
                &events,
                &mut buffer,
                &mut rows,
                &mut broker,
                &mut summary_state,
            );
            if !fed {
                // The feed died (`init`'s broker is gone): subscribe again.
                events = None;
            }
        }
        if ready & 1 == 0 {
            continue;
        }
        // Serve the queued message (the wait says one is there).
        match server.recv_with(&mut buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(message) => {
                let reply = match dispatch(&mut rows, &mut broker, &message) {
                    Ok(parcel) => parcel,
                    Err(_) => {
                        services::health_reply(&summary(&rows), &records(&rows)).unwrap_or_default()
                    }
                };
                if let Some(txn) = message.txn {
                    // A caller whose deadline passed is a normal scheduling
                    // race, not a service failure: the kernel expired the
                    // transaction, and replying to it is `-ENOENT`. Keep
                    // serving instead of taking the whole service down (a
                    // restart would also collide with the supervisor's slot
                    // reuse until registry teardown lands).
                    if let Err(error) = server.reply(txn, &reply) {
                        if error.errno() != Some(-messenger::errno::ENOENT) {
                            return Err(error);
                        }
                    }
                }
            }
            Err(Error::Errno(code)) if code == -messenger::errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
    }
}

/// Reuse a cached bus, or connect once when the service appears.
fn connect_or_keep(bus: Option<router::Bus>, name: &str) -> Option<router::Bus> {
    match bus {
        Some(bus) => Some(bus),
        None => router::Bus::connect(name).ok(),
    }
}

/// Apply one queued `system/events/service/<name>` state event; `false` when
/// the feed failed (its broker is gone).
fn take_event(
    events: &Option<router::Subscriber>,
    buffer: &mut [u8],
    rows: &mut Vec<HealthRow>,
    broker: &mut router::TopicBroker,
    summary_state: &mut Option<(String, String)>,
) -> bool {
    let Some(events) = events else {
        return true;
    };
    let event = match events.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
        Ok(Some(event)) => event,
        Ok(None) => return true,
        Err(_) => return false,
    };
    // The broker owns the topic, so its service name is trusted over any
    // payload field; a malformed payload is dropped.
    let Some(name) = services::service_event_name(&event.topic) else {
        return true;
    };
    if let Ok(payload) = services::init::wire::decode_system_events_service(&event.payload) {
        apply_state(rows, name, &payload, broker, summary_state);
    }
    true
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
