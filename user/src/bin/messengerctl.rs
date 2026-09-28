//! `messengerctl` (`MSGCTL.ELF`): render the Messenger fabric snapshot and
//! browse the name registry, the service supervisor, health and the event log
//! (issues #70, #89 and #93). The image name is 8.3 because the kernel's FAT
//! reader only resolves short names.
//!
//! Calls the native `messenger` syscall's `stats` op with a snapshot-sized
//! buffer, so the kernel returns the versioned `FabricStats` block (ABI v2),
//! and prints it as a small table grouped by subsystem: services/channels,
//! messages, buffers, audit, and per-slot usage. It then offers the registry
//! commands `list` and `resolve <name>`, the supervisor commands `services`
//! and `health`, `sessions` (the `logind` session table, issue #101), the
//! `log`/`log tail`/`log verify` commands, the shell-integration commands
//! `mime <path>` and `open <path>` (issue #116), and the app commands
//! `apps` and `launch <app> [args]` (issue #158), typed at the prompt (native
//! programs do not receive argv; the tool is interactive like `sh`).
//!
//! When a topics broker is reachable (boot the demo with both
//! `LAZYOS_MESSENGERD=1` and `LAZYOS_MESSENGERCTL=1`) the tool also runs a
//! boot-time topic conformance self-test and prints machine-parseable serial
//! markers (`TOPIC:FANOUT:PASS`, `TOPIC:WILDCARD:PASS`, `TOPIC:RETAINED:PASS`,
//! `TOPIC:DROP:PASS`, `TOPIC:QOS:PASS`, `TOPIC:UNSUB:PASS`), so a headless
//! `qemu_session.py` run proves the pub/sub path end to end. The interactive
//! commands `topics` and `tail <filter> [count]` inspect and stream (issue
//! #92).
//!
//! With services running the boot self-test also exercises the app registry and
//! launch path (issue #158): `MSGCTL:APPS:PASS`, `MSGCTL:LAUNCH:PASS` and the
//! foreign-session `MSGCTL:LAUNCH:DENIED:PASS` probe. The probe runs in a
//! short-lived child (`MSGCTL.ELF probe`, issue #177): the kernel never lets a
//! task widen its own credentials back up, so if the console task dropped its
//! own privilege to run the probe it could never regain it, and every command
//! typed afterward would run as the probe's uid. A disposable child can drop
//! to the probe identity and exit; the console keeps its own credentials.
//!
//! Boot it with `LAZYOS_MESSENGERCTL=1` (see the kernel build script): the
//! demo then runs this program in the hello window. With `LAZYOS_SERVICES=1`
//! the supervisor's services provide targets for the new commands.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};
use user::messenger::{
    self, clipboard, keyd, logind, mime, registry, services, topics_client, FabricStats,
};
use user::sys;
use user::task_snapshot::{self, TaskSnapshot};

/// The interactive command set, printed at startup and by `help`.
const HELP: &str = "commands: list | resolve <name> | services | health | sessions | \
                    apps | launch <app> [args] | \
                    log [tail [n]] | log verify | topics | tail <filter> [count] | \
                    mime <path> | open <path> [verb] | keys | clipboard | stats | stats-json | \
                    tasks-json | help | quit\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    if is_probe_role() {
        probe_role();
    }
    sys::write_str("messengerctl: Messenger fabric snapshot\n");
    match messenger::fabric_stats() {
        Ok(stats) => print_report(&stats),
        Err(error) => report(error.message()),
    }
    topic_selftest();
    keyd_selftest();
    app_selftest();
    commands()
}

/// Read this task's service argument (see [`probe_role`]): `true` for a
/// `MSGCTL.ELF probe` child, `false` for the ordinary console tool.
fn is_probe_role() -> bool {
    let mut buffer = [0u8; 16];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    core::str::from_utf8(&buffer[..len]).unwrap_or("").trim() == "probe"
}

/// The registry command loop; `list` and `resolve <name>` print the name
/// table from the kernel, exactly as the spec's registry interface promises.
fn commands() -> ! {
    sys::write_str(HELP);
    let mut line = [0u8; 256];
    loop {
        sys::write_str("> ");
        let len = read_line(&mut line);
        let text = core::str::from_utf8(&line[..len]).unwrap_or("").trim();
        match text {
            "" => continue,
            "quit" | "exit" => sys::exit(0),
            "help" => sys::write_str(HELP),
            "list" => print_registry(),
            "services" => print_services(),
            "health" => print_health(),
            "sessions" => print_sessions(),
            "apps" => print_apps(),
            "log" => print_log(10),
            "log verify" => verify_log(),
            "stats" => match messenger::fabric_stats() {
                Ok(stats) => print_report(&stats),
                Err(error) => report(error.message()),
            },
            "stats-json" => match messenger::fabric_stats() {
                Ok(stats) => print_report_json(&stats),
                Err(error) => report(error.message()),
            },
            "tasks-json" => match task_snapshot::task_snapshot() {
                Ok(snapshot) => print_tasks_json(&snapshot),
                Err(error) => report(error.message()),
            },
            "topics" => print_topics(),
            "keys" => print_keys(),
            "clipboard" => print_clipboard(),
            _ if text.starts_with("resolve ") => resolve(text[8..].trim()),
            _ if text.starts_with("launch ") => launch_app(text[7..].trim()),
            _ if text.starts_with("log tail") => {
                let count = text[8..].trim().parse().unwrap_or(10);
                print_log(count)
            }
            _ if text.starts_with("tail ") => tail(text[5..].trim()),
            _ if text.starts_with("mime ") => print_mime(text[5..].trim()),
            _ if text.starts_with("open ") => open_path(text[5..].trim()),
            _ => report(
                "unknown command; try list, resolve <name>, services, health, sessions, \
                 apps, launch <app>, log, topics, tail, mime, open, keys, clipboard, \
                 stats, tasks-json, help, quit",
            ),
        }
    }
}

/// `list`: print every registered name with its owner, interfaces and lease.
fn print_registry() {
    match registry::list() {
        Ok(entries) if entries.is_empty() => {
            sys::write_str("registry: no names registered\n");
        }
        Ok(entries) => {
            sys::write_str(&format!("registry: {} name(s)\n", entries.len()));
            for entry in &entries {
                sys::write_str(&format!(
                    "  {}  owner {}  object 0x{:x}\n",
                    entry.name, entry.owner_slot, entry.object_id
                ));
                if entry.lease_remaining == 0 {
                    sys::write_str("    lease permanent\n");
                } else {
                    sys::write_str(&format!("    lease {} ticks\n", entry.lease_remaining));
                }
                for interface in &entry.interfaces {
                    sys::write_str(&format!("    iface 0x{interface:016x}\n"));
                }
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `resolve <name>`: ask the kernel for a handle to the service endpoint and
/// print it. The handle stays open: closing an endpoint closes that *side* of
/// the channel for every holder (the bootstrap listener among them), so a
/// browsing tool must not close what it resolved. The handle dies with the
/// task.
fn resolve(name: &str) {
    match registry::resolve(name) {
        Ok(endpoint) => sys::write_str(&format!(
            "resolved {} -> handle {}\n",
            name,
            endpoint.handle()
        )),
        Err(error) => report(error.message()),
    }
}

/// `services`: the supervisor's supervision table (issue #93).
fn print_services() {
    let endpoint = match services::resolve_service(services::INIT_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_services(&endpoint) {
        Ok(statuses) if statuses.is_empty() => {
            sys::write_str("services: no services supervised\n");
        }
        Ok(statuses) => {
            sys::write_str(&format!("services: {} supervised\n", statuses.len()));
            for status in &statuses {
                sys::write_str(&format!(
                    "  {:<10} {:<10} pid {:<3} restarts {} health {}\n",
                    status.name, status.state, status.pid, status.restarts, status.health
                ));
                if !status.deps.is_empty() {
                    sys::write_str(&format!("    deps {}\n", status.deps));
                }
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `apps`: the supervisor's built-in app registry (issue #158), the table the
/// S5 start menu enumerates and `launch` resolves against.
fn print_apps() {
    let endpoint = match services::resolve_service(services::INIT_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_apps(&endpoint) {
        Ok(apps) if apps.is_empty() => {
            sys::write_str("apps: registry is empty\n");
        }
        Ok(apps) => {
            sys::write_str(&format!("apps: {} registered\n", apps.len()));
            for app in &apps {
                let verbs = if app.verbs.is_empty() {
                    String::from("-")
                } else {
                    app.verbs.join(",")
                };
                sys::write_str(&format!(
                    "  {:<14} {:<20} {:<12} {:<10} {}\n",
                    app.id, app.name, app.path, app.restart, verbs
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `launch <app> [args]`: ask `init` to spawn the app in this task's session
/// (issue #158) and print the child's pid. The app's own output and exit are
/// the evidence that it ran.
fn launch_app(rest: &str) {
    let rest = rest.trim();
    let (app, args) = match rest.find(char::is_whitespace) {
        Some(index) => (&rest[..index], rest[index..].trim()),
        None => (rest, ""),
    };
    if app.is_empty() {
        return report("usage: launch <app> [args]");
    }
    match services::launch_app(app, args, 0) {
        Ok(result) => sys::write_str(&format!(
            "launch {} -> pid {} (session {})\n",
            result.app, result.pid, result.session
        )),
        Err(error) => report(error.message()),
    }
}

/// The boot-time app-registry and launch path self-test (issue #158). Silent
/// when `init` is not reachable, like [`topic_selftest`]; otherwise it:
///
/// 1. lists the registry and checks the ids `mimed` registers are present;
/// 2. launches `top` into this task's (session 0) session and prints
///    `MSGCTL:LAUNCH:PASS` (`init` prints its own `INIT:LAUNCH:PASS`, and the
///    app prints `SYS:TOP:PASS`);
/// 3. spawns a `MSGCTL.ELF probe` child ([`probe_role`]) that restamps
///    *itself* into a foreign session and asks `init` to launch into this
///    console's original session: the supervisor must refuse with `-EPERM`
///    (`INIT:LAUNCH:DENIED:PASS`, `MSGCTL:LAUNCH:DENIED:PASS`). Running the
///    probe in a child, not this task, matters: the kernel never lets a task
///    widen its own credentials back up, so a probe that dropped this
///    console's own privilege could never restore it, and `commands()` would
///    serve the rest of the session as the probe's uid.
fn app_selftest() {
    let Some(endpoint) = resolve_init() else {
        return;
    };
    match services::fetch_apps(&endpoint) {
        Ok(apps) => {
            let has_editor = apps
                .iter()
                .find(|app| app.id == "editor")
                .map(|app| app.verbs.iter().any(|verb| verb == "edit"))
                .unwrap_or(false);
            let has_top = apps
                .iter()
                .any(|app| app.id == "top" && app.path == "TOP.ELF");
            if has_editor && has_top {
                sys::write_str(&format!("MSGCTL:APPS:PASS count={}\n", apps.len()));
            } else {
                sys::write_str("MSGCTL:APPS:FAIL registry is missing editor/top\n");
            }
        }
        Err(error) => sys::write_str(&format!("MSGCTL:APPS:FAIL:{}\n", error.message())),
    }

    match services::launch(&endpoint, "top", "", 0) {
        Ok(result) => sys::write_str(&format!(
            "MSGCTL:LAUNCH:PASS app={} pid={}\n",
            result.app, result.pid
        )),
        Err(error) => sys::write_str(&format!("MSGCTL:LAUNCH:FAIL:{}\n", error.message())),
    }

    // The foreign-session probe runs in a short-lived child (see
    // [`probe_role`]): a task that has dropped to uid 1000 cannot regain
    // root, so this console task must not be the one that self-transitions.
    match sys::spawn(b"MSGCTL.ELF probe\0") {
        Some(pid) => reap_probe(pid),
        None => sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL could not spawn the probe\n"),
    }
}

/// The `MSGCTL.ELF probe` child: become uid 1000 in session 4242 (a
/// self-transition the kernel audits; the dropped capability set means the
/// launch call can no longer pass the supervisor's privilege check), try to
/// launch into the console's original session (0), print the
/// `MSGCTL:LAUNCH:DENIED:*` evidence line, and exit. This never runs in the
/// console task itself: see [`app_selftest`].
fn probe_role() -> ! {
    let endpoint = match resolve_init() {
        Some(endpoint) => endpoint,
        None => {
            sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL could not resolve init\n");
            sys::exit(1);
        }
    };
    let probe = sys::Cred::new(1000, 1000, 0, 0, 4242);
    if sys::cred_set(None, &probe).is_err() {
        sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL could not enter the probe session\n");
        sys::exit(1);
    }
    match services::launch(&endpoint, "top", "", 1) {
        Err(error) if error.errno() == Some(-messenger::errno::EPERM) => {
            sys::write_str("MSGCTL:LAUNCH:DENIED:PASS\n");
            sys::exit(0);
        }
        Ok(_) => {
            sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL foreign launch was allowed\n");
            sys::exit(1);
        }
        Err(error) => {
            sys::write_str(&format!("MSGCTL:LAUNCH:DENIED:FAIL:{}\n", error.message()));
            sys::exit(1);
        }
    }
}

/// Reap the probe child, bounded so a spawn that never runs cannot hang the
/// console forever; other exits (there should be none yet) are reaped and
/// ignored until the probe's own pid turns up.
fn reap_probe(pid: u64) {
    const ATTEMPTS: usize = 200;
    for _ in 0..ATTEMPTS {
        match sys::wait(sys::clock() + 5) {
            Some((exited, _status)) if exited == pid => return,
            Some(_) => {}
            None => {}
        }
    }
    sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL probe did not exit\n");
}

/// Resolve `init`, retrying while the supervisor's registration lands; `None`
/// when it never does.
fn resolve_init() -> Option<user::messenger::Endpoint> {
    const ATTEMPTS: usize = 64;
    for _ in 0..ATTEMPTS {
        if let Ok(endpoint) = services::resolve_service(services::INIT_NAME) {
            return Some(endpoint);
        }
        park_tick();
    }
    None
}

/// `health`: the retained `system/health/*` rows and the aggregate.
fn print_health() {
    let endpoint = match services::resolve_service(services::HEALTHD_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_health(&endpoint) {
        Ok((summary, records)) => {
            sys::write_str(&format!(
                "health: {} ({})\n",
                summary.status, summary.detail
            ));
            for record in &records {
                sys::write_str(&format!(
                    "  {:<10} {:<9} {}\n",
                    record.name, record.status, record.detail
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `sessions`: the `logind` session table (issue #101). Bounded wait: `logind`
/// can be sitting at the console prompt, in which case it answers after the
/// next key and this prints a friendly timeout instead of hanging.
fn print_sessions() {
    let endpoint = match services::resolve_service(logind::NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match logind::fetch_sessions(&endpoint) {
        Ok((_, sessions)) if sessions.is_empty() => {
            sys::write_str("sessions: none yet\n");
        }
        Ok((active, sessions)) => {
            sys::write_str(&format!(
                "sessions: {} active, {} total\n",
                active,
                sessions.len()
            ));
            for session in &sessions {
                sys::write_str(&format!(
                    "  #{:<3} {:<10} uid {:<5} pid {:<3} {:<7} t{}\n",
                    session.id,
                    session.user,
                    session.uid,
                    session.pid,
                    session.state,
                    session.started
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `log [tail [n]]`: the newest records from the structured event log.
fn print_log(count: u64) {
    let endpoint = match services::resolve_service(services::LOGD_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_log_tail(&endpoint, count) {
        Ok(records) if records.is_empty() => sys::write_str("log: no records yet\n"),
        Ok(records) => {
            for record in &records {
                sys::write_str(&format!(
                    "  #{:<4} t{:<6} {:<32} {}\n",
                    record.seq, record.tick, record.topic, record.detail
                ));
                sys::write_str(&format!("       hash 0x{:016x}\n", record.hash));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `log verify`: recompute the hash chain over the retained records.
fn verify_log() {
    let endpoint = match services::resolve_service(services::LOGD_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_log_verify(&endpoint) {
        Ok((true, count)) => {
            sys::write_str(&format!("log: chain intact over {count} record(s)\n"));
        }
        Ok((false, index)) => {
            sys::write_str(&format!("log: CHAIN BROKEN at record {index}\n"));
        }
        Err(error) => report(error.message()),
    }
}

/// Read a line with basic backspace editing. Returns the byte length.
fn read_line(buffer: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let ch = sys::read_char();
        if ch == b'\n' as u64 {
            sys::write_str("\n");
            return len;
        }
        if ch == 8 {
            if len > 0 {
                len -= 1;
                sys::write_str("\u{8} \u{8}");
            }
            continue;
        }
        if (32..127).contains(&ch) && len + 1 < buffer.len() {
            buffer[len] = ch as u8;
            len += 1;
            sys::write(&[ch as u8]);
        }
    }
}

/// Print a friendly error line.
fn report(message: &str) {
    sys::write_str("error: ");
    sys::write_str(message);
    sys::write_str("\n");
}

/// Print the `FabricStats` snapshot as a single machine-parseable JSON line,
/// prefixed `MCP:FABRIC_STATS:` so a host-side tool can find it in the
/// serial log (`SYS_WRITE` mirrors console output to serial — see
/// `kernel/src/process/mod.rs`'s `sys_write`) without scraping the
/// human-oriented table `stats` prints. This is a debug/tooling aid for the
/// prototype MCP debug bridge (see the `MCP Debug Bridge` wiki design doc);
/// it is not part of the Messenger wire protocol.
fn print_report_json(stats: &FabricStats) {
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
            "{{\"slot\":{slot},\"handles\":{},\"buffers\":{},\"buffer_bytes\":{}}}",
            task.handles, task.buffers, task.buffer_bytes
        ));
    }
    tasks.push(']');

    sys::write_str(&format!(
        "MCP:FABRIC_STATS:{{\"services\":{},\"endpoints\":{},\"channels\":{},\
         \"queued\":{},\"queued_bytes\":{},\"outstanding\":{},\
         \"calls\":{},\"replies\":{},\"one_way\":{},\
         \"timeouts\":{},\"cancels\":{},\"drops\":{},\
         \"buffers\":{},\"buffer_bytes\":{},\"buffer_mappings\":{},\
         \"fences_submitted\":{},\"fence_waits\":{},\"fence_timeouts\":{},\
         \"outstanding_fences\":{},\"handoffs\":{},\
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
        stats.cancels,
        stats.drops,
        stats.buffers,
        stats.buffer_bytes,
        stats.buffer_mappings,
        stats.fences_submitted,
        stats.fence_waits,
        stats.fence_timeouts,
        stats.outstanding_fences,
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
fn print_tasks_json(snapshot: &TaskSnapshot) {
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
fn print_report(stats: &FabricStats) {
    sys::write_str(&format!(
        "\n[services]\n  services {}  endpoints {}  channels {}\n",
        stats.services, stats.endpoints, stats.channels
    ));

    sys::write_str(&format!(
        "[channels]\n  queued {} msgs ({} bytes)  outstanding {}\n  \
         calls {}  replies {}  one-way {}\n  \
         timeouts {}  cancels {}  drops {}\n",
        stats.queued,
        stats.queued_bytes,
        stats.outstanding,
        stats.calls,
        stats.replies,
        stats.one_way,
        stats.timeouts,
        stats.cancels,
        stats.drops
    ));

    sys::write_str(&format!(
        "[buffers]\n  buffers {}  bytes {}  mappings {}\n  \
         fences submitted {}  waits {}\n  \
         fence timeouts {}  outstanding {}\n  zero-copy handoffs {}\n",
        stats.buffers,
        stats.buffer_bytes,
        stats.buffer_mappings,
        stats.fences_submitted,
        stats.fence_waits,
        stats.fence_timeouts,
        stats.outstanding_fences,
        stats.handoffs
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

/// `topics`: list the topics the broker has seen, with subscriber counts and
/// whether a retained value is held.
fn print_topics() {
    let client = match topics_client::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    match client.list() {
        Ok(entries) if entries.is_empty() => sys::write_str("topics: none seen\n"),
        Ok(entries) => {
            sys::write_str(&format!("topics: {} known\n", entries.len()));
            for entry in &entries {
                let retained = if entry.retained {
                    "retained"
                } else {
                    "volatile"
                };
                sys::write_str(&format!(
                    "  {}  subs {}  {retained}\n",
                    entry.topic, entry.subscribers
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `keys`: list the `keyd` service's key ids and use counters (issue #102).
/// Key material is never part of the protocol, so this command cannot and
/// does not print any.
fn print_keys() {
    let client = match keyd::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    match client.keys() {
        Ok(keys) if keys.is_empty() => sys::write_str("keyd: no keys held\n"),
        Ok(keys) => {
            sys::write_str(&format!("keyd: {} key(s)\n", keys.len()));
            for key in &keys {
                sys::write_str(&format!(
                    "  #{} {:<8} uses {:<3} last-use tick {}\n",
                    key.id, key.kind, key.uses, key.last_use
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `clipboard`: the current offer's MIME types and owner from `clipboardd`
/// (issue #115). Metadata only: the `Current` method has no content field, so
/// this command cannot and does not print any payload bytes.
fn print_clipboard() {
    let client = match clipboard::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    match client.current() {
        Ok(None) => sys::write_str("clipboard: no offer\n"),
        Ok(Some(offer)) => {
            sys::write_str(&format!(
                "clipboard: token {}  owner {}  session {}\n",
                offer.token, offer.owner, offer.session
            ));
            sys::write_str(&format!(
                "  {}  {} mime(s)  tick {}\n",
                if offer.lazy { "lazy" } else { "eager" },
                offer.mimes.len(),
                offer.tick
            ));
            for mime in &offer.mimes {
                sys::write_str(&format!("    {mime}\n"));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `mime <path>`: the type `mimed`'s database guesses for a path (issue
/// #116). Falls back to `application/octet-stream` when `mimed` is absent.
fn print_mime(path: &str) {
    if path.is_empty() {
        return report("usage: mime <path>");
    }
    sys::write_str(&format!("{}: {}\n", path, mime::guess(path)));
}

/// `open <path> [verb]`: resolve the open-with app for the path and print the
/// launch event `mimed` published (issue #116). The verb defaults to `open`.
fn open_path(rest: &str) {
    let mut parts = rest.split_whitespace();
    let Some(path) = parts.next() else {
        return report("usage: open <path> [verb]");
    };
    let verb = parts.next().unwrap_or(mime::DEFAULT_VERB);
    match mime::open(path, verb) {
        Ok(result) => {
            sys::write_str(&format!(
                "open {} -> {} ({}, topic {})\n",
                path, result.app, result.mime, result.topic
            ));
            if result.launched {
                sys::write_str("  (launched through os.lazy.init)\n");
            }
            if !result.published {
                sys::write_str("  (launch event not published; supervisor unreachable)\n");
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `tail <filter> [count]`: subscribe with `latest` QoS and print up to
/// `count` events (default 5, capped at 64). The task blocks between events;
/// another task's publishes wake it through the broker's deferred reply.
fn tail(rest: &str) {
    let mut parts = rest.split_whitespace();
    let Some(filter) = parts.next() else {
        return report("usage: tail <filter> [count]");
    };
    let count: usize = match parts.next() {
        Some(text) => match text.parse() {
            Ok(value) => core::cmp::min(value, 64usize),
            Err(_) => return report("usage: tail <filter> [count]"),
        },
        None => 5,
    };
    let client = match topics_client::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    let subscription = match client.subscribe(filter, topics_client::Qos::Latest) {
        Ok(subscription) => subscription,
        Err(error) => return report(error.message()),
    };
    sys::write_str(&format!("tail {filter}: {count} event(s)\n"));
    for _ in 0..count {
        match subscription.next_event(None) {
            Ok(Some(event)) => {
                let retained = if event.retained { " retained" } else { "" };
                sys::write_str(&format!(
                    "[{}] {} seq {}{} from slot {}: {}\n",
                    event.topic,
                    event.topic,
                    event.sequence,
                    retained,
                    event.publisher,
                    describe_payload(&event)
                ));
            }
            Ok(None) => {
                report("timed out waiting for an event");
                break;
            }
            Err(error) => {
                report(error.message());
                break;
            }
        }
    }
    let _ = subscription.unsubscribe();
}

/// A readable one-line summary of an event payload: the first string field of
/// the publisher's parcel, or its size when the payload is not text.
fn describe_payload(event: &topics_client::Event) -> String {
    if let Ok(parcel) = event.parcel() {
        let mut decoder = Decoder::new(&parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::String {
                if let Ok(text) = field.as_str() {
                    return String::from(text);
                }
            }
        }
    }
    format!("{} payload byte(s)", event.payload.len())
}

/// The boot-time topic conformance markers (issue #92). Silent when no broker
/// is reachable, so the plain `LAZYOS_MESSENGERCTL=1` demo is unchanged.
fn topic_selftest() {
    let client = match topics_client::Client::connect() {
        Ok(client) => client,
        Err(_) => return,
    };
    sys::write_str("messengerctl: topics broker detected; running selftest\n");
    marker("TOPIC:FANOUT", selftest_fanout(&client));
    marker("TOPIC:WILDCARD", selftest_wildcard(&client));
    marker("TOPIC:RETAINED", selftest_retained(&client));
    marker("TOPIC:DROP", selftest_drop(&client));
    marker("TOPIC:QOS", selftest_qos(&client));
    marker("TOPIC:UNSUB", selftest_unsubscribe(&client));
}

/// Print `TOPIC:<name>:PASS` or `TOPIC:<name>:FAIL:<detail>`.
fn marker(name: &str, outcome: Result<(), String>) {
    match outcome {
        Ok(()) => sys::write_str(&format!("{name}:PASS\n")),
        Err(detail) => sys::write_str(&format!("{name}:FAIL:{detail}\n")),
    }
}

/// The boot-time `keyd` reachability marker: list the service's keys and print
/// `KEYD:KEYS:PASS <count>`. `init` starts `keyd` alongside this tool, so a
/// short retry covers the registration race; a boot with no `keyd` is silent,
/// exactly like [`topic_selftest`].
fn keyd_selftest() {
    // keyd's Argon2id self-test can take seconds under TCG, so the retry
    // window is generous (~0.6 s of parked ticks); `KEYD:SELFTEST:PASS` from
    // the service itself is the authoritative marker, this one is a bonus.
    const ATTEMPTS: usize = 64;
    for _ in 0..ATTEMPTS {
        match keyd::Client::connect() {
            Ok(client) => {
                match client.keys() {
                    Ok(keys) => sys::write_str(&format!("KEYD:KEYS:PASS {}\n", keys.len())),
                    Err(error) => sys::write_str(&format!("KEYD:KEYS:FAIL:{}\n", error.message())),
                }
                return;
            }
            Err(_) => park_tick(),
        }
    }
}

/// Sleep one PIT tick by parking on a private channel pair with an expired
/// deadline (userspace has no sleep syscall; the topics client uses the same
/// trick). The pair is closed again so no channel leaks.
fn park_tick() {
    if let Ok((probe, peer)) = messenger::create_pair() {
        let mut scratch = [0u8; 16];
        let _ = probe.recv_into(&mut scratch, Some(messenger::EXPIRED_DEADLINE));
        let _ = probe.close();
        let _ = peer.close();
    }
}

/// Friendly text for a Messenger API error.
fn err_text(error: messenger::Error) -> String {
    String::from(error.message())
}

/// Friendly text for a parcel codec error.
fn parcel_err_text(error: libmessenger::Error) -> String {
    String::from(error.message())
}

/// A tiny payload parcel for the self-test: one string field.
fn test_parcel(text: &str) -> Result<Parcel, String> {
    let mut body = Encoder::new();
    body.string(1, text).map_err(parcel_err_text)?;
    Ok(Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: 0xfeed_face,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    })
}

/// The first string field of an event payload.
fn payload_text(event: &topics_client::Event) -> Result<String, String> {
    let parcel = event.parcel().map_err(err_text)?;
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(parcel_err_text)? {
        if field.kind == Kind::String {
            return Ok(String::from(field.as_str().map_err(parcel_err_text)?));
        }
    }
    Err(String::from("payload has no string field"))
}

/// Two subscriptions on one topic both receive the same event.
fn selftest_fanout(client: &topics_client::Client) -> Result<(), String> {
    let first = client
        .subscribe("topics/fanout", topics_client::Qos::Latest)
        .map_err(err_text)?;
    let second = client
        .subscribe("topics/fanout", topics_client::Qos::Buffered(4))
        .map_err(err_text)?;
    let payload = test_parcel("fanout-1")?;
    let matched = client
        .publish("topics/fanout", &payload)
        .map_err(err_text)?;
    if matched != 2 {
        return Err(format!("matched {matched}, expected 2"));
    }
    let a = first
        .next_event(None)
        .map_err(err_text)?
        .ok_or("first subscriber got no event")?;
    let b = second
        .next_event(None)
        .map_err(err_text)?
        .ok_or("second subscriber got no event")?;
    if payload_text(&a)? != "fanout-1" || payload_text(&b)? != "fanout-1" {
        return Err(String::from("fanout payloads differ"));
    }
    if a.sequence != b.sequence {
        return Err(String::from("fanout copies disagree on sequence"));
    }
    first.unsubscribe().map_err(err_text)?;
    second.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// `+` matches exactly one segment, trailing `#` matches zero or more.
fn selftest_wildcard(client: &topics_client::Client) -> Result<(), String> {
    let one = client
        .subscribe("system/+/up", topics_client::Qos::Latest)
        .map_err(err_text)?;
    let any = client
        .subscribe("system/#", topics_client::Qos::Buffered(8))
        .map_err(err_text)?;

    // Four segments: `system/+/up` must not match, `system/#` must.
    let deep = test_parcel("network-up")?;
    let matched = client
        .publish("system/events/network/up", &deep)
        .map_err(err_text)?;
    if matched != 1 {
        return Err(format!("deep publish matched {matched}, expected 1"));
    }
    // Three segments: both filters match.
    let shallow = test_parcel("events-up")?;
    let matched = client
        .publish("system/events/up", &shallow)
        .map_err(err_text)?;
    if matched != 2 {
        return Err(format!("shallow publish matched {matched}, expected 2"));
    }
    // One segment: only `system/#` matches; `#` stands for zero segments too.
    let root = test_parcel("system-up")?;
    let matched = client.publish("system", &root).map_err(err_text)?;
    if matched != 1 {
        return Err(format!("root publish matched {matched}, expected 1"));
    }

    let event = one
        .next_event(None)
        .map_err(err_text)?
        .ok_or("`system/+/up` got no event")?;
    if event.topic != "system/events/up" || payload_text(&event)? != "events-up" {
        return Err(format!(
            "`system/+/up` received {} ({})",
            event.topic,
            payload_text(&event)?
        ));
    }
    if one.poll_event().map_err(err_text)?.is_some() {
        return Err(String::from("`system/+/up` matched a second event"));
    }

    let topics = ["system/events/network/up", "system/events/up", "system"];
    for expected in topics {
        let event = any
            .next_event(None)
            .map_err(err_text)?
            .ok_or("`system/#` queue ran dry")?;
        if event.topic != expected {
            return Err(format!(
                "`system/#` got {} expected {expected}",
                event.topic
            ));
        }
    }
    one.unsubscribe().map_err(err_text)?;
    any.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// A retained publish is replayed to a later subscriber.
fn selftest_retained(client: &topics_client::Client) -> Result<(), String> {
    let payload = test_parcel("netd-up")?;
    let matched = client
        .publish_retained("system/health/netd", &payload)
        .map_err(err_text)?;
    if matched != 0 {
        return Err(format!("retained publish matched {matched}, expected 0"));
    }
    let subscription = client
        .subscribe("system/health/netd", topics_client::Qos::Latest)
        .map_err(err_text)?;
    let event = subscription
        .next_event(None)
        .map_err(err_text)?
        .ok_or("new subscriber got no retained value")?;
    if !event.retained {
        return Err(String::from("replayed event is not marked retained"));
    }
    if event.topic != "system/health/netd" || payload_text(&event)? != "netd-up" {
        return Err(String::from("retained value payload changed"));
    }
    subscription.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// `buffered(1)` drops the oldest event on overflow and counts it.
fn selftest_drop(client: &topics_client::Client) -> Result<(), String> {
    let subscription = client
        .subscribe("topics/drop", topics_client::Qos::Buffered(1))
        .map_err(err_text)?;
    for text in ["drop-1", "drop-2", "drop-3"] {
        let payload = test_parcel(text)?;
        client.publish("topics/drop", &payload).map_err(err_text)?;
    }
    let stats = subscription.stats().map_err(err_text)?;
    if stats.drops != 2 {
        return Err(format!("drops {} expected 2", stats.drops));
    }
    if stats.queued != 1 {
        return Err(format!("queued {} expected 1", stats.queued));
    }
    let event = subscription
        .next_event(None)
        .map_err(err_text)?
        .ok_or("dropping subscriber got no event")?;
    if payload_text(&event)? != "drop-3" {
        return Err(format!(
            "oldest was not dropped: got {}",
            payload_text(&event)?
        ));
    }
    subscription.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// `reliable` redelivers the unacked head and retires it on `ack`; `conflate`
/// coalesces a publisher's pending events and counts the replacement.
fn selftest_qos(client: &topics_client::Client) -> Result<(), String> {
    let reliable = client
        .subscribe("topics/reliable", topics_client::Qos::Reliable)
        .map_err(err_text)?;
    for text in ["rel-1", "rel-2"] {
        let payload = test_parcel(text)?;
        client
            .publish("topics/reliable", &payload)
            .map_err(err_text)?;
    }
    let first = reliable
        .next_event(None)
        .map_err(err_text)?
        .ok_or("reliable queue empty")?;
    let retry = reliable
        .next_event(None)
        .map_err(err_text)?
        .ok_or("reliable retry empty")?;
    if retry.sequence != first.sequence || payload_text(&retry)? != "rel-1" {
        return Err(String::from("reliable did not redeliver the head"));
    }
    reliable.ack(first.sequence).map_err(err_text)?;
    let second = reliable
        .next_event(None)
        .map_err(err_text)?
        .ok_or("reliable second event missing")?;
    if payload_text(&second)? != "rel-2" {
        return Err(String::from("reliable did not advance after ack"));
    }
    reliable.ack(second.sequence).map_err(err_text)?;
    reliable.unsubscribe().map_err(err_text)?;

    let conflate = client
        .subscribe("topics/conflate", topics_client::Qos::Conflate)
        .map_err(err_text)?;
    for text in ["conf-1", "conf-2"] {
        let payload = test_parcel(text)?;
        client
            .publish("topics/conflate", &payload)
            .map_err(err_text)?;
    }
    let stats = conflate.stats().map_err(err_text)?;
    if stats.drops != 1 {
        return Err(format!("conflate drops {} expected 1", stats.drops));
    }
    let event = conflate
        .next_event(None)
        .map_err(err_text)?
        .ok_or("conflate queue empty")?;
    if payload_text(&event)? != "conf-2" {
        return Err(String::from("conflate kept the older event"));
    }
    conflate.unsubscribe().map_err(err_text)?;
    Ok(())
}

/// After `unsubscribe` later publishes no longer match.
fn selftest_unsubscribe(client: &topics_client::Client) -> Result<(), String> {
    let subscription = client
        .subscribe("topics/unsub", topics_client::Qos::Latest)
        .map_err(err_text)?;
    subscription.unsubscribe().map_err(err_text)?;
    let payload = test_parcel("gone")?;
    let matched = client.publish("topics/unsub", &payload).map_err(err_text)?;
    if matched != 0 {
        return Err(format!("unsubscribed publish matched {matched}"));
    }
    Ok(())
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
