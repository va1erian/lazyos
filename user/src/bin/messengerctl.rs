//! `messengerctl` (`MSGCTL.ELF`): render the Messenger fabric snapshot and
//! browse the name registry (issues #70 and #89). The image name is 8.3
//! because the kernel's FAT reader only resolves short names.
//!
//! Calls the native `messenger` syscall's `stats` op with a snapshot-sized
//! buffer, so the kernel returns the versioned `FabricStats` block (ABI v2),
//! and prints it as a small table grouped by subsystem: services/channels,
//! messages, buffers, audit, and per-slot usage. It then offers the registry
//! commands `list` and `resolve <name>`, typed at the prompt (native programs
//! do not receive argv; the tool is interactive like `sh`).
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
//! Boot it with `LAZYOS_MESSENGERCTL=1` (see the kernel build script): the
//! demo then runs this program in the hello window.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};
use user::messenger::{self, registry, topics, FabricStats};
use user::sys;

/// The interactive command set, printed at startup and by `help`.
const HELP: &str =
    "commands: list | resolve <name> | topics | tail <filter> [count] | stats | help | quit\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("messengerctl: Messenger fabric snapshot\n");
    match messenger::fabric_stats() {
        Ok(stats) => print_report(&stats),
        Err(error) => report(error.message()),
    }
    topic_selftest();
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
            "stats" => match messenger::fabric_stats() {
                Ok(stats) => print_report(&stats),
                Err(error) => report(error.message()),
            },
            "topics" => print_topics(),
            _ if text.starts_with("resolve ") => resolve(text[8..].trim()),
            _ if text.starts_with("tail ") => tail(text[5..].trim()),
            _ => {
                report("unknown command; try list, resolve <name>, topics, tail, stats, help, quit")
            }
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

/// `topics`: list the topics the broker has seen, with subscriber counts and
/// whether a retained value is held.
fn print_topics() {
    let client = match topics::Client::connect() {
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
    let client = match topics::Client::connect() {
        Ok(client) => client,
        Err(error) => return report(error.message()),
    };
    let subscription = match client.subscribe(filter, topics::Qos::Latest) {
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
fn describe_payload(event: &topics::Event) -> String {
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
    let client = match topics::Client::connect() {
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
fn payload_text(event: &topics::Event) -> Result<String, String> {
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
fn selftest_fanout(client: &topics::Client) -> Result<(), String> {
    let first = client
        .subscribe("topics/fanout", topics::Qos::Latest)
        .map_err(err_text)?;
    let second = client
        .subscribe("topics/fanout", topics::Qos::Buffered(4))
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
fn selftest_wildcard(client: &topics::Client) -> Result<(), String> {
    let one = client
        .subscribe("system/+/up", topics::Qos::Latest)
        .map_err(err_text)?;
    let any = client
        .subscribe("system/#", topics::Qos::Buffered(8))
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
fn selftest_retained(client: &topics::Client) -> Result<(), String> {
    let payload = test_parcel("netd-up")?;
    let matched = client
        .publish_retained("system/health/netd", &payload)
        .map_err(err_text)?;
    if matched != 0 {
        return Err(format!("retained publish matched {matched}, expected 0"));
    }
    let subscription = client
        .subscribe("system/health/netd", topics::Qos::Latest)
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
fn selftest_drop(client: &topics::Client) -> Result<(), String> {
    let subscription = client
        .subscribe("topics/drop", topics::Qos::Buffered(1))
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
fn selftest_qos(client: &topics::Client) -> Result<(), String> {
    let reliable = client
        .subscribe("topics/reliable", topics::Qos::Reliable)
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
        .subscribe("topics/conflate", topics::Qos::Conflate)
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
fn selftest_unsubscribe(client: &topics::Client) -> Result<(), String> {
    let subscription = client
        .subscribe("topics/unsub", topics::Qos::Latest)
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
