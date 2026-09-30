//! `healthd`'s aggregation core: deriving, ranking, storing and publishing one
//! health row per supervised service.
//!
//! Split out of `healthd.rs` (issue #194). These are the state/table functions
//! the main loop drives; [`HealthRow`] itself stays in the crate root next to
//! the run loop, mirroring `init`'s `Service`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use user::messenger::{router, services};
use user::sys;

use super::{HealthRow, REPORT_TTL};

/// Update one service's supervision phase from a decoded service event. The
/// topic already named the service; `deps` stays whatever the last reconcile
/// learned.
pub(crate) fn apply_state(
    rows: &mut Vec<HealthRow>,
    name: &str,
    event: &services::ServiceEvent,
    broker: &mut router::TopicBroker,
    summary_state: &mut Option<(String, String)>,
) {
    if event.state.is_empty() {
        return;
    }
    if apply_supervision(
        rows,
        name,
        &event.state,
        event.pid,
        event.restarts,
        None,
        broker,
    ) {
        publish_summary(broker, rows, summary_state);
    }
}

/// Reconcile the supervised services, updating only rows whose phase or detail
/// changed; allocations happen on transitions, not on every poll.
pub(crate) fn refresh(
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
    publish_summary(broker, rows, summary_state);
}

/// Publish the aggregate row retained on `system/health/summary`, skipping the
/// write while the status/detail fingerprint is unchanged.
fn publish_summary(
    broker: &mut router::TopicBroker,
    rows: &[HealthRow],
    summary_state: &mut Option<(String, String)>,
) {
    let summary = summary(rows);
    let fingerprint = (summary.status.clone(), summary.detail.clone());
    if summary_state.as_ref() != Some(&fingerprint) {
        let _ = services::health::wire::publish_system_health_summary(broker, &summary);
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
pub(crate) fn report(
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

/// Publish one row retained on `system/health/<name>`. Best-effort: the topic
/// name is validated by the generated helper, so a malformed service name
/// drops the row instead of corrupting the broker.
fn publish_row(broker: &mut router::TopicBroker, name: &str, row: &services::HealthRecord) {
    let _ = services::health::wire::publish_system_health(broker, name, row);
}

/// The aggregate row: the worst status across every known service.
pub(crate) fn summary(rows: &[HealthRow]) -> services::HealthRecord {
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
pub(crate) fn records(rows: &[HealthRow]) -> Vec<services::HealthRecord> {
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
