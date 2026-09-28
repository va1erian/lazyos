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
//! and `health`, and the `log`/`log tail`/`log verify` commands, typed at the
//! prompt (native programs do not receive argv; the tool is interactive like
//! `sh`).
//!
//! Boot it with `LAZYOS_MESSENGERCTL=1` (see the kernel build script): the
//! demo then runs this program in the hello window. With `LAZYOS_SERVICES=1`
//! the supervisor's services provide targets for the new commands.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::panic::PanicInfo;
use user::messenger::{self, registry, services, FabricStats};
use user::sys;

/// The interactive command set, printed at startup and by `help`.
const HELP: &str = "commands: list | resolve <name> | services | health | \
                    log [tail [n]] | log verify | stats | help | quit\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("messengerctl: Messenger fabric snapshot\n");
    match messenger::fabric_stats() {
        Ok(stats) => print_report(&stats),
        Err(error) => report(error.message()),
    }
    commands()
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
            "log" => print_log(10),
            "log verify" => verify_log(),
            "stats" => match messenger::fabric_stats() {
                Ok(stats) => print_report(&stats),
                Err(error) => report(error.message()),
            },
            _ if text.starts_with("resolve ") => resolve(text[8..].trim()),
            _ if text.starts_with("log tail") => {
                let count = text[8..].trim().parse().unwrap_or(10);
                print_log(count)
            }
            _ => report("unknown command; try list, resolve <name>, services, health, log, stats, help, quit"),
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

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
