//! Fabric-stats and task-snapshot rendering (`stats`, `stats-json`,
//! `tasks-json`).

use alloc::format;
use alloc::string::String;
use user::messenger::FabricStats;
use user::sys;
use user::task_snapshot::TaskSnapshot;

/// Print the `FabricStats` snapshot as a single machine-parseable JSON line,
/// prefixed `MCP:FABRIC_STATS:` so a host-side tool can find it in the
/// serial log (`SYS_WRITE` mirrors console output to serial — see
/// `kernel/src/process/mod.rs`'s `sys_write`) without scraping the
/// human-oriented table `stats` prints. This is a debug/tooling aid for the
/// prototype MCP debug bridge (see the `MCP Debug Bridge` wiki design doc);
/// it is not part of the Messenger wire protocol.
pub(crate) fn print_report_json(stats: &FabricStats) {
    let mut tasks = String::from("[");
    let mut first = true;
    for (slot, task) in stats.tasks.iter().enumerate() {
        if task.live == 0 {
            continue;
        }
        if !first {
            tasks.push(',');
        }
        first = false;
        tasks.push_str(&format!(
            "{{\"slot\":{slot},\"handles\":{},\"buffers\":{},\"buffer_bytes\":{},\
             \"calls\":{},\"timeouts\":{},\"polls\":{}}}",
            task.handles, task.buffers, task.buffer_bytes, task.calls, task.timeouts, task.polls
        ));
    }
    tasks.push(']');

    sys::write_str(&format!(
        "MCP:FABRIC_STATS:{{\"services\":{},\"endpoints\":{},\"channels\":{},\
         \"queued\":{},\"queued_bytes\":{},\"outstanding\":{},\
         \"calls\":{},\"replies\":{},\"one_way\":{},\
         \"timeouts\":{},\"polls\":{},\"cancels\":{},\"drops\":{},\
         \"buffers\":{},\"buffer_bytes\":{},\"buffer_mappings\":{},\"handoffs\":{},\
         \"acl_loaded\":{},\"acl_rules\":{},\"audit_trace\":{},\
         \"audit_denies\":{},\"audit_allows\":{},\"audit_count\":{},\"audit_total\":{},\
         \"tasks\":{tasks}}}\n",
        stats.services,
        stats.endpoints,
        stats.channels,
        stats.queued,
        stats.queued_bytes,
        stats.outstanding,
        stats.calls,
        stats.replies,
        stats.one_way,
        stats.timeouts,
        stats.polls,
        stats.cancels,
        stats.drops,
        stats.buffers,
        stats.buffer_bytes,
        stats.buffer_mappings,
        stats.handoffs,
        stats.acl_loaded,
        stats.acl_rules,
        stats.audit_trace,
        stats.audit_denies,
        stats.audit_allows,
        stats.audit_count,
        stats.audit_total,
    ));
}

/// Escape a task name for embedding in a JSON string literal. Names come
/// from static program names (`"sh"`, `"messengerd"`, ...), so this only
/// needs to be defensive, not exhaustive.
fn json_escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out
}

/// Print the `TaskSnapshot` as a single machine-parseable JSON line, prefixed
/// `MCP:TASK_SNAPSHOT:`, the scheduler counterpart of `stats-json`'s
/// `MCP:FABRIC_STATS:` line. Same debug/tooling-only status: see
/// `print_report_json`.
pub(crate) fn print_tasks_json(snapshot: &TaskSnapshot) {
    let mut rows = String::from("[");
    let mut first = true;
    for row in &snapshot.rows {
        if !row.live {
            continue;
        }
        if !first {
            rows.push(',');
        }
        first = false;
        rows.push_str(&format!(
            "{{\"pid\":{},\"ppid\":{},\"pgid\":{},\"sid\":{},\"state\":{},\
             \"class\":{},\"weight\":{},\"cpu_ticks\":{},\"name\":\"{}\"}}",
            row.pid,
            row.ppid,
            row.pgid,
            row.sid,
            row.state,
            row.class,
            row.weight,
            row.cpu_ticks,
            json_escape(&row.name)
        ));
    }
    rows.push(']');
    sys::write_str(&format!(
        "MCP:TASK_SNAPSHOT:{{\"version\":{},\"tasks\":{rows}}}\n",
        snapshot.version
    ));
}

/// Print the snapshot grouped into services/channels, buffers, audit and
/// per-slot usage sections. Rows are kept compact so the default two-column
/// demo window does not scroll the first sections away.
pub(crate) fn print_report(stats: &FabricStats) {
    sys::write_str(&format!(
        "\n[services]\n  services {}  endpoints {}  channels {}\n",
        stats.services, stats.endpoints, stats.channels
    ));

    sys::write_str(&format!(
        "[channels]\n  queued {} msgs ({} bytes)  outstanding {}\n  \
         calls {}  replies {}  one-way {}\n  \
         timeouts {}  polls {}  cancels {}  drops {}\n",
        stats.queued,
        stats.queued_bytes,
        stats.outstanding,
        stats.calls,
        stats.replies,
        stats.one_way,
        stats.timeouts,
        stats.polls,
        stats.cancels,
        stats.drops
    ));

    sys::write_str(&format!(
        "[buffers]\n  buffers {}  bytes {}  mappings {}\n  zero-copy handoffs {}\n",
        stats.buffers, stats.buffer_bytes, stats.buffer_mappings, stats.handoffs
    ));

    let acl = if stats.acl_loaded != 0 {
        "loaded"
    } else {
        "bootstrap window"
    };
    let trace = if stats.audit_trace != 0 { "on" } else { "off" };
    sys::write_str(&format!(
        "[audit]\n  acl {} rules ({acl})\n  trace {trace}\n  \
         denies {}  allows {}  ring {}  total {}\n  last hash 0x{:016x}\n",
        stats.acl_rules,
        stats.audit_denies,
        stats.audit_allows,
        stats.audit_count,
        stats.audit_total,
        stats.audit_last_hash
    ));

    sys::write_str("[tasks]\n");
    for (slot, task) in stats.tasks.iter().enumerate() {
        if task.live != 0 {
            sys::write_str(&format!(
                "  slot {}  handles {}  buffers {} ({} bytes)\n",
                slot, task.handles, task.buffers, task.buffer_bytes
            ));
        }
    }
}
