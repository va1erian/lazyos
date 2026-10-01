//! Typed client for the Task Manager's services view (issue #489): `init`'s
//! supervision table joined with `healthd`'s retained health rows.
//!
//! Both calls go through the generated `os.lazy.init.v1` and
//! `os.lazy.healthd.v1` stubs, never hand-written field ids. Both services are
//! read without any capability: `Services` and `Status` are snapshots open to
//! every task. Every reply is untrusted, so rows are capped, text is stripped
//! of control characters and shortened before it reaches the painter.
//!
//! Neither service exists in an image built without `LAZYOS_SERVICES=1`, so a
//! missing name is resolved once per refresh and reported as an errno instead
//! of blocking the UI thread (`Service::try_connect`).

use messenger_generated::os_lazy_healthd_v1 as health_wire;
use messenger_generated::os_lazy_init_v1 as init_wire;

use crate::platform::messenger::Service;
use crate::sys::errno;

/// The supervisor's registered name.
const INIT_NAME: &str = "os.lazy.init";
/// The health aggregator's registered name.
const HEALTHD_NAME: &str = "os.lazy.healthd";
/// The structured-error field id every service uses (see
/// `user/src/messenger/services/mod.rs`).
const ERROR_FIELD: u16 = 15;

/// The most rows the view keeps; `init` supervises a few dozen at most, so a
/// longer reply is a broken or hostile peer, not a bigger system.
pub const MAX_ROWS: usize = 128;
/// The longest a displayed name, state or dependency list may be.
const MAX_FIELD: usize = 48;
/// The longest a displayed health detail may be.
const MAX_DETAIL: usize = 96;

/// How a status word should be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Running / `ok`.
    Good,
    /// Starting, restarting / `degraded`.
    Warning,
    /// Stopped by failure / `down`.
    Bad,
    /// No word, or one this client does not know.
    Unknown,
}

impl Tone {
    /// The tone of a `healthd` status word (`ok`/`degraded`/`down`).
    pub fn of_health(status: &str) -> Tone {
        match status {
            "ok" => Tone::Good,
            "degraded" => Tone::Warning,
            "down" => Tone::Bad,
            _ => Tone::Unknown,
        }
    }

    /// The tone of an `init` supervision phase.
    pub fn of_state(state: &str) -> Tone {
        match state {
            "running" => Tone::Good,
            "pending" | "restarting" => Tone::Warning,
            "failed" => Tone::Bad,
            _ => Tone::Unknown,
        }
    }
}

/// One line of the services view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceRow {
    /// Service name.
    pub name: String,
    /// Supervision phase, or empty when `init` does not supervise the row
    /// (a service that only sends heartbeats to `healthd`).
    pub state: String,
    /// Task slot of the running child, or 0.
    pub pid: u64,
    /// Restart count.
    pub restarts: u64,
    /// Comma-separated dependency names.
    pub deps: String,
    /// Health word: `healthd`'s row when it has one, else `init`'s.
    pub health: String,
    /// `healthd`'s human-readable detail, empty when there is none.
    pub detail: String,
}

/// The aggregate health row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub status: String,
    pub detail: String,
}

/// One refresh of the view: the merged rows and why a source was missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Services {
    pub rows: Vec<ServiceRow>,
    /// `healthd`'s aggregate, when it answered.
    pub summary: Option<Summary>,
    /// The errno `init` failed with, when it did.
    pub init_error: Option<i64>,
    /// The errno `healthd` failed with, when it did.
    pub health_error: Option<i64>,
}

impl Services {
    /// Whether both sources answered and at least one service is listed.
    pub fn complete(&self) -> bool {
        self.init_error.is_none() && self.health_error.is_none() && !self.rows.is_empty()
    }

    /// Rows per tone, for the header counts: `(good, warning, bad)`.
    pub fn counts(&self) -> (usize, usize, usize) {
        self.rows.iter().fold((0, 0, 0), |(good, warn, bad), row| {
            match Tone::of_health(&row.health) {
                Tone::Good => (good + 1, warn, bad),
                Tone::Warning => (good, warn + 1, bad),
                Tone::Bad => (good, warn, bad + 1),
                Tone::Unknown => (good, warn, bad),
            }
        })
    }
}

/// Read both services now. Never blocks on a service that is not registered.
pub fn fetch() -> Services {
    let table = call(
        INIT_NAME,
        init_wire::INTERFACE_ID,
        init_wire::METHOD_SERVICES,
    )
    .and_then(|body| {
        init_wire::decode_services_reply(&body)
            .map(|reply| reply.services)
            .map_err(|_| -errno::EINVAL)
    });
    let health = call(
        HEALTHD_NAME,
        health_wire::INTERFACE_ID,
        health_wire::METHOD_STATUS,
    )
    .and_then(|body| health_wire::decode_status_reply(&body).map_err(|_| -errno::EINVAL));
    assemble(table, health)
}

/// One argument-less call on `name`, returning the reply body.
fn call(name: &'static str, interface: u64, method: u32) -> Result<Vec<u8>, i64> {
    let service = Service::try_connect(name).ok_or(-errno::ENOENT)?;
    service
        .call(interface, method, ERROR_FIELD, Vec::new())
        .map(|reply| reply.body)
}

/// Combine the two (possibly failed) replies into one view.
pub fn assemble(
    table: Result<Vec<init_wire::ServiceStatus>, i64>,
    health: Result<health_wire::StatusReply, i64>,
) -> Services {
    let (table, init_error) = split(table);
    let (health, health_error) = split(health);
    let summary = health.as_ref().map(|reply| Summary {
        status: clean(&reply.summary.status, MAX_FIELD),
        detail: clean(&reply.summary.detail, MAX_DETAIL),
    });
    let records = health.map(|reply| reply.records).unwrap_or_default();
    Services {
        rows: merge(&table.unwrap_or_default(), &records),
        summary,
        init_error,
        health_error,
    }
}

fn split<T>(result: Result<T, i64>) -> (Option<T>, Option<i64>) {
    match result {
        Ok(value) => (Some(value), None),
        Err(code) => (None, Some(code)),
    }
}

/// Join the supervision table with the health rows by name: every supervised
/// service in `init`'s order, then any service only `healthd` knows. At most
/// [`MAX_ROWS`] rows; a duplicate name keeps its first row.
pub fn merge(
    table: &[init_wire::ServiceStatus],
    records: &[health_wire::HealthRecord],
) -> Vec<ServiceRow> {
    let mut rows: Vec<ServiceRow> = Vec::new();
    for status in table {
        let name = clean(&status.name, MAX_FIELD);
        if name.is_empty() || rows.iter().any(|row| row.name == name) {
            continue;
        }
        if rows.len() == MAX_ROWS {
            break;
        }
        rows.push(ServiceRow {
            name,
            state: clean(&status.state, MAX_FIELD),
            pid: status.pid,
            restarts: status.restarts,
            deps: clean(&status.deps, MAX_FIELD),
            health: clean(&status.health, MAX_FIELD),
            detail: String::new(),
        });
    }
    for record in records {
        let name = clean(&record.name, MAX_FIELD);
        if name.is_empty() {
            continue;
        }
        let health = clean(&record.status, MAX_FIELD);
        let detail = clean(&record.detail, MAX_DETAIL);
        if let Some(row) = rows.iter_mut().find(|row| row.name == name) {
            // `healthd` folds heartbeats into the phase, so its word wins.
            if !health.is_empty() {
                row.health = health;
            }
            row.detail = detail;
        } else if rows.len() < MAX_ROWS {
            rows.push(ServiceRow {
                name,
                health,
                detail,
                ..ServiceRow::default()
            });
        }
    }
    rows
}

/// `text` without control characters, cut to `max` characters with an
/// ellipsis, so a peer cannot break the table layout.
fn clean(text: &str, max: usize) -> String {
    let mut out: String = text.chars().filter(|c| !c.is_control()).collect();
    if out.chars().count() > max {
        out = out.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(name: &str, state: &str, health: &str) -> init_wire::ServiceStatus {
        init_wire::ServiceStatus {
            name: name.into(),
            state: state.into(),
            pid: 7,
            restarts: 1,
            deps: "logd".into(),
            health: health.into(),
        }
    }

    fn record(name: &str, status: &str, detail: &str) -> health_wire::HealthRecord {
        health_wire::HealthRecord {
            name: name.into(),
            status: status.into(),
            detail: detail.into(),
            tick: 0,
        }
    }

    #[test]
    fn health_rows_join_the_supervision_table_by_name() {
        let rows = merge(
            &[
                status("logd", "running", "ok"),
                status("flaky", "restarting", "degraded"),
            ],
            &[
                record("flaky", "down", "crashed 3x"),
                record("netd", "ok", "heartbeat"),
            ],
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "logd");
        assert_eq!(rows[0].health, "ok");
        assert_eq!(rows[1].health, "down");
        assert_eq!(rows[1].detail, "crashed 3x");
        assert_eq!(rows[1].state, "restarting");
        // A heartbeat-only service is listed after the supervised ones.
        assert_eq!(rows[2].name, "netd");
        assert!(rows[2].state.is_empty());
    }

    #[test]
    fn an_empty_health_word_keeps_inits_word() {
        let rows = merge(
            &[status("keyd", "running", "ok")],
            &[record("keyd", "", "")],
        );
        assert_eq!(rows[0].health, "ok");
    }

    #[test]
    fn hostile_rows_are_capped_cleaned_and_deduplicated() {
        let mut table: Vec<_> = (0..MAX_ROWS + 10)
            .map(|index| status(&format!("svc{index}"), "running", "ok"))
            .collect();
        table.insert(0, status("svc0", "failed", "down"));
        table.insert(0, status("", "running", "ok"));
        let rows = merge(&table, &[record("bad\x1b[2Jname", "ok", &"x".repeat(500))]);
        assert_eq!(rows.len(), MAX_ROWS);
        assert_eq!(rows[0].state, "failed", "the first duplicate wins");
        let long = clean(&"y".repeat(200), MAX_FIELD);
        assert_eq!(long.chars().count(), MAX_FIELD);
        assert!(long.ends_with('…'));
        assert_eq!(clean("bad\x1b[2Jname\n", MAX_FIELD), "bad[2Jname");
    }

    #[test]
    fn a_missing_source_is_reported_not_fatal() {
        let view = assemble(Ok(vec![status("logd", "running", "ok")]), Err(-2));
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.health_error, Some(-2));
        assert!(view.summary.is_none());
        assert!(!view.complete());

        let view = assemble(
            Ok(vec![status("logd", "running", "ok")]),
            Ok(health_wire::StatusReply {
                summary: record("summary", "degraded", "1 degraded"),
                records: vec![record("logd", "degraded", "slow")],
            }),
        );
        assert!(view.complete());
        assert_eq!(view.summary.as_ref().unwrap().status, "degraded");
        assert_eq!(view.counts(), (0, 1, 0));
    }

    #[test]
    fn tones_follow_both_vocabularies() {
        assert_eq!(Tone::of_health("ok"), Tone::Good);
        assert_eq!(Tone::of_health("down"), Tone::Bad);
        assert_eq!(Tone::of_health("weird"), Tone::Unknown);
        assert_eq!(Tone::of_state("restarting"), Tone::Warning);
        assert_eq!(Tone::of_state("failed"), Tone::Bad);
        assert_eq!(Tone::of_state("stopped"), Tone::Unknown);
    }
}
