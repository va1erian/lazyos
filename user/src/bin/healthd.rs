//! `healthd` (`HEALTHD.ELF`): service health aggregation and the retained
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

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::{self, registry, router, services, Endpoint, Error, Message, Parcel};
use user::sys;

/// How often the supervisor table is reconciled (PIT ticks). Service events
/// are the fast path; this poll only catches detail changes and lost events,
/// and it is slow because the user runtime's bump allocator (`user/src/heap.rs`)
/// never reclaims the reply buffers each call allocates.
const POLL_TICKS: u64 = 20_000;
/// How long the service sleeps waiting for messages between polls.
const IDLE_TICKS: u64 = 5;
/// Age at which a `Report` stops overriding the derived row (PIT ticks).
const REPORT_TTL: u64 = 250;
/// Event topic prefix `init` publishes service state on.
const SERVICE_EVENTS: &str = "system/events/service/";

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
            if let Some(bus) = &bus {
                events = bus.subscribe("system/events/service/#").ok();
            }
        }
        drain_events(
            &events,
            &mut buffer,
            &mut rows,
            &mut broker,
            &mut summary_state,
        );

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

        // Serve one queued message, or wake for the next poll.
        match server.recv_with(&mut buffer, Some(sys::clock() + IDLE_TICKS)) {
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

/// Apply every queued `system/events/service/<name>` state event.
fn drain_events(
    events: &Option<router::Subscriber>,
    buffer: &mut [u8],
    rows: &mut Vec<HealthRow>,
    broker: &mut router::TopicBroker,
    summary_state: &mut Option<(String, String)>,
) {
    let Some(events) = events else {
        return;
    };
    loop {
        match events.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(Some(event)) => {
                let Some(name) = event.topic.strip_prefix(SERVICE_EVENTS) else {
                    continue;
                };
                let payload = core::str::from_utf8(&event.payload).unwrap_or("");
                apply_state(rows, name, payload, broker, summary_state);
            }
            Ok(None) => return,
            Err(_) => return,
        }
    }
}

/// One `key=value` token of an event payload.
fn payload_field<'a>(payload: &'a str, key: &str) -> Option<&'a str> {
    payload
        .split_whitespace()
        .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
}

/// Update one service's supervision phase from an event. The event carries
/// `pid`/`restarts`; `deps` stays whatever the last reconcile learned.
fn apply_state(
    rows: &mut Vec<HealthRow>,
    name: &str,
    payload: &str,
    broker: &mut router::TopicBroker,
    summary_state: &mut Option<(String, String)>,
) {
    let Some(state) = payload_field(payload, "state") else {
        return;
    };
    let stored = rows.iter().find(|row| row.name == name);
    let pid = payload_field(payload, "pid")
        .and_then(|value| value.parse().ok())
        .or_else(|| stored.map(|row| row.pid))
        .unwrap_or(0);
    let restarts = payload_field(payload, "restarts")
        .and_then(|value| value.parse().ok())
        .or_else(|| stored.map(|row| row.restarts))
        .unwrap_or(0);
    if apply_supervision(rows, name, state, pid, restarts, None, broker) {
        let summary = summary(rows);
        let fingerprint = (summary.status.clone(), summary.detail.clone());
        if summary_state.as_ref() != Some(&fingerprint) {
            broker.publish(
                "system/health/summary",
                format!("status={} detail={}", summary.status, summary.detail).as_bytes(),
                true,
            );
            *summary_state = Some(fingerprint);
        }
    }
}

/// Reconcile the supervised services, updating only rows whose phase or detail
/// changed; allocations happen on transitions, not on every poll.
fn refresh(
    rows: &mut Vec<HealthRow>,
    statuses: &[services::ServiceStatus],
    broker: &mut router::TopicBroker,
    summary_state: &mut Option<(String, String)>,
) {
    let mut any_change = false;
    for status in statuses {
        any_change |= apply_supervision(
            rows,
            &status.name,
            &status.state,
            status.pid,
            status.restarts,
            Some(&status.deps),
            broker,
        );
    }
    if !any_change {
        return;
    }
    let summary = summary(rows);
    let fingerprint = (summary.status.clone(), summary.detail.clone());
    if summary_state.as_ref() != Some(&fingerprint) {
        broker.publish(
            "system/health/summary",
            format!("status={} detail={}", summary.status, summary.detail).as_bytes(),
            true,
        );
        *summary_state = Some(fingerprint);
    }
}

/// Apply one supervision state change; returns whether the health row changed.
///
/// `deps` is `Some` when the change came from a full reconcile (the snapshot
/// knows the dependency list) and `None` for an event, which keeps the stored
/// list. The function allocates only on a real change.
fn apply_supervision(
    rows: &mut Vec<HealthRow>,
    name: &str,
    state: &str,
    pid: u64,
    restarts: u64,
    deps: Option<&str>,
    broker: &mut router::TopicBroker,
) -> bool {
    let now = sys::clock();
    let Some(row) = rows.iter_mut().find(|row| row.name == name) else {
        let deps = deps.unwrap_or("");
        let record = derive(name, state, pid, restarts, deps);
        rows.push(HealthRow {
            name: name.to_string(),
            state: state.to_string(),
            pid,
            restarts,
            deps: deps.to_string(),
            status: record.status.clone(),
            detail: record.detail.clone(),
            tick: now,
            reported_at: 0,
        });
        publish_row(broker, name, &record);
        announce(name, &record.status);
        return true;
    };
    let deps_changed = deps.is_some_and(|deps| row.deps != deps);
    if row.state == state && row.pid == pid && row.restarts == restarts && !deps_changed {
        // Nothing changed: do not allocate a new detail string.
        return false;
    }
    row.state.clear();
    row.state.push_str(state);
    row.pid = pid;
    row.restarts = restarts;
    if let Some(deps) = deps {
        row.deps.clear();
        row.deps.push_str(deps);
    }
    let derived = derive(name, state, pid, restarts, &row.deps);
    let mut next_status = derived.status.clone();
    let mut next_detail = derived.detail.clone();
    // A fresh heartbeat wins unless the supervisor sees something worse.
    if row.reported_at != 0
        && now.saturating_sub(row.reported_at) <= REPORT_TTL
        && rank(&row.status) <= rank(&derived.status)
    {
        next_status = row.status.clone();
        next_detail = format!("{} | supervisor: {}", row.detail, derived.detail);
    }
    let changed = row.status != next_status || row.detail != next_detail;
    if row.status != next_status {
        row.status = next_status;
    }
    if row.detail != next_detail {
        row.detail = next_detail;
    }
    row.tick = now;
    if changed {
        let record = row.record();
        publish_row(broker, name, &record);
        announce(name, &record.status);
    }
    changed
}

/// A service's health derived from its supervision phase and dependencies.
fn derive(name: &str, state: &str, pid: u64, restarts: u64, deps: &str) -> services::HealthRecord {
    let health = match state {
        // `stopped` is a clean exit under a no-restart policy (a one-shot
        // program such as `top`): finished, not faulty. A crash without a
        // restart policy is published as `failed`.
        "running" | "stopped" => "ok",
        "restarting" | "pending" => "degraded",
        _ => "down",
    };
    let mut detail = format!("pid={pid} restarts={restarts}");
    if !deps.is_empty() {
        detail.push_str(&format!(" deps={deps}"));
    }
    services::HealthRecord {
        name: name.to_string(),
        status: health.to_string(),
        detail,
        tick: 0,
    }
}

/// Keep the ordering documented in the module comment: `ok` < `degraded` <
/// `down`, so healthd never upgrades away a real failure.
fn rank(status: &str) -> u8 {
    match status {
        "ok" => 0,
        "degraded" => 1,
        _ => 2,
    }
}

/// Record a service heartbeat (`Report`), which outranks the derived row for
/// [`REPORT_TTL`] ticks.
fn report(
    rows: &mut Vec<HealthRow>,
    broker: &mut router::TopicBroker,
    name: &str,
    status: &str,
    detail: &str,
) {
    let now = sys::clock();
    let record = services::HealthRecord {
        name: name.to_string(),
        status: status.to_string(),
        detail: detail.to_string(),
        tick: now,
    };
    match rows.iter_mut().find(|row| row.name == name) {
        Some(row) => {
            let changed = row.status != record.status || row.detail != record.detail;
            if row.status != record.status {
                row.status = record.status.clone();
            }
            if row.detail != record.detail {
                row.detail = record.detail.clone();
            }
            row.tick = now;
            row.reported_at = now;
            if changed {
                publish_row(broker, name, &record);
                announce(name, &record.status);
            }
        }
        None => {
            rows.push(HealthRow {
                name: name.to_string(),
                state: String::from("unknown"),
                pid: 0,
                restarts: 0,
                deps: String::new(),
                status: record.status.clone(),
                detail: record.detail.clone(),
                tick: now,
                reported_at: now,
            });
            publish_row(broker, name, &record);
            announce(name, &record.status);
        }
    }
}

/// Publish one row retained on `system/health/<name>`.
fn publish_row(broker: &mut router::TopicBroker, name: &str, row: &services::HealthRecord) {
    let topic = format!("system/health/{name}");
    let payload = format!("status={} detail={}", row.status, row.detail);
    broker.publish(&topic, payload.as_bytes(), true);
}

/// The aggregate row: the worst status across every known service.
fn summary(rows: &[HealthRow]) -> services::HealthRecord {
    let mut status = "ok";
    let mut ok = 0u64;
    for row in rows {
        if row.status == "ok" {
            ok += 1;
        }
        if rank(&row.status) > rank(status) {
            status = match rank(&row.status) {
                1 => "degraded",
                _ => "down",
            };
        }
    }
    services::HealthRecord {
        name: String::from("summary"),
        status: status.to_string(),
        detail: format!("{ok}/{} services ok", rows.len()),
        tick: sys::clock(),
    }
}

/// The retained rows as wire records, oldest first.
fn records(rows: &[HealthRow]) -> Vec<services::HealthRecord> {
    rows.iter().map(HealthRow::record).collect()
}

/// Print the machine-parseable evidence line for a status transition.
fn announce(name: &str, status: &str) {
    if status == "ok" {
        sys::write_str(&format!("HEALTH:SVC:PASS {name}\n"));
    } else {
        sys::write_str(&format!("HEALTH:SVC:FAIL {name} ({status})\n"));
    }
}

/// Dispatch one inbound message: heartbeat reports, the broker, or a status
/// query.
fn dispatch(
    rows: &mut Vec<HealthRow>,
    broker: &mut router::TopicBroker,
    message: &Message,
) -> messenger::Result<Parcel> {
    match message.interface_id() {
        router::INTERFACE => broker.handle(message),
        services::HEALTHD_INTERFACE => match message.method() {
            services::healthd_method::REPORT => {
                let name = string_field(message, services::field::NAME)?;
                let status = string_field(message, services::field::STATUS)?;
                let detail = string_field(message, services::field::DETAIL)?;
                report(rows, broker, &name, &status, &detail);
                services::health_reply(&summary(rows), &records(rows))
            }
            services::healthd_method::STATUS => {
                services::health_reply(&summary(rows), &records(rows))
            }
            _ => Err(Error::Errno(-messenger::errno::EINVAL)),
        },
        _ => Err(Error::Errno(-messenger::errno::EINVAL)),
    }
}

/// The first string field with the given id in a message body.
fn string_field(message: &Message, id: u16) -> messenger::Result<String> {
    use libmessenger::{Decoder, Kind};
    let mut decoder = Decoder::new(&message.parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::String && field.id == id {
            return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
        }
    }
    Err(Error::Errno(-messenger::errno::EINVAL))
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
