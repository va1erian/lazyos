//! The read-only methods: each turns one existing data source into JSON.
//!
//! Nothing here writes, spawns or claims anything; the sources are the ones
//! `top`, `devctl`, `messengerctl` and `dmesg` already read. A handler gets
//! parameters that `dbgwire::methods::validate` has checked.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use dbgwire::json::{self, Object, Value};
use dbgwire::{fsallow, logline, methods};
use user::messenger::services;
use user::sys;

/// A failed call: a JSON-RPC code and its message.
pub(crate) type Failure = (i32, String);

use dbgwire::rpc::code;

/// Bytes the boot-log ring holds (`kernel/src/klog.rs::CAPACITY`).
pub(crate) const KLOG_CAPACITY: usize = 64 * 1024;

/// One look at the boot log: where it ends and its bytes.
pub(crate) struct Klog {
    /// Total bytes ever logged; the text ends at this offset.
    pub total: u64,
    pub text: String,
}

/// Which kernel ring a log method reads.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ring {
    /// The kernel's own boot log (`klog`).
    Kernel,
    /// What the programs and services wrote to the terminal.
    Programs,
}

impl Ring {
    pub fn name(self) -> &'static str {
        match self {
            Ring::Kernel => "kernel",
            Ring::Programs => "programs",
        }
    }

    /// The ring `source` names, if it is one of the kernel's.
    pub fn named(source: &str) -> Option<Ring> {
        match source {
            "kernel" => Some(Ring::Kernel),
            "programs" => Some(Ring::Programs),
            _ => None,
        }
    }
}

/// Read a kernel log ring into `scratch` (kept between calls).
pub(crate) fn read_ring(ring: Ring, scratch: &mut Vec<u8>) -> Result<Klog, Failure> {
    if scratch.len() < 8 + KLOG_CAPACITY {
        scratch.resize(8 + KLOG_CAPACITY, 0);
    }
    let read = match ring {
        Ring::Kernel => sys::klog(scratch),
        Ring::Programs => sys::program_log(scratch),
    };
    let written = read.map_err(|errno| {
        (
            code::UNAVAILABLE,
            format!(
                "the kernel has no {} log for this uid (is it built with LAZYOS_DBGD=1?): errno {}",
                ring.name(),
                -errno
            ),
        )
    })?;
    if written < 8 {
        return Err((code::INTERNAL, String::from("short boot-log read")));
    }
    let total = u64::from_le_bytes(scratch[..8].try_into().unwrap_or([0; 8]));
    let text = String::from_utf8_lossy(&scratch[8..written]).into_owned();
    Ok(Klog { total, text })
}

impl Klog {
    /// Byte offset in the whole log of the first byte of `text`.
    pub fn start(&self) -> u64 {
        self.total - self.text.len() as u64
    }

    /// The log's complete lines with their offsets. A first line cut by the
    /// ring's wrap is dropped, and so is an unfinished last line.
    pub fn lines(&self) -> Vec<(u64, &str)> {
        let mut out = Vec::new();
        let mut pos = self.start();
        let wrapped = self.start() > 0;
        let (complete, _) = logline::split_complete(&self.text);
        for (index, line) in complete.into_iter().enumerate() {
            let at = pos;
            pos += line.len() as u64 + 1;
            if index == 0 && wrapped {
                continue;
            }
            out.push((at, line));
        }
        out
    }
}

fn lines_json(lines: &[(u64, &str)]) -> String {
    json::array(
        lines
            .iter()
            .map(|(pos, text)| logline::parse(text).to_json(Some(*pos))),
    )
}

fn number(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

/// `log.tail`.
pub(crate) fn log_tail(params: &Value, scratch: &mut Vec<u8>) -> Result<String, Failure> {
    let want = number(params, "lines", 50) as usize;
    let source = text(params, "source").unwrap_or("kernel");
    match Ring::named(source) {
        Some(ring) => {
            let log = read_ring(ring, scratch)?;
            let all = log.lines();
            let from = all.len().saturating_sub(want);
            Ok(Object::new()
                .str("source", ring.name())
                .uint("total", log.total)
                .raw("lines", &lines_json(&all[from..]))
                .finish())
        }
        None => {
            let endpoint = services::resolve_service(services::LOGD_NAME)
                .map_err(|e| (code::UNAVAILABLE, format!("logd: {}", e.message())))?;
            let lines = services::fetch_log_tail_file(&endpoint, source, want as u64)
                .map_err(|e| (code::UNAVAILABLE, format!("logd {source}: {}", e.message())))?;
            let rows: Vec<(u64, &str)> = lines.iter().map(|l| (0, l.as_str())).collect();
            Ok(Object::new()
                .str("source", source)
                .raw("lines", &lines_json(&rows))
                .finish())
        }
    }
}

/// `log.sources`.
pub(crate) fn log_sources() -> Result<String, Failure> {
    let endpoint = services::resolve_service(services::LOGD_NAME)
        .map_err(|e| (code::UNAVAILABLE, format!("logd: {}", e.message())))?;
    let sources = services::fetch_log_sources(&endpoint)
        .map_err(|e| (code::UNAVAILABLE, format!("logd: {}", e.message())))?;
    let mut all = vec![String::from("kernel"), String::from("programs")];
    all.extend(sources);
    Ok(Object::new()
        .raw("sources", &json::array(all.iter().map(|s| json::quoted(s))))
        .finish())
}

/// `tasks.list`.
pub(crate) fn tasks_list() -> Result<String, Failure> {
    let snapshot = user::task_snapshot::task_snapshot()
        .map_err(|e| (code::UNAVAILABLE, format!("tasks: {}", e.message())))?;
    let rows = snapshot.rows.iter().filter(|row| row.live).map(|row| {
        Object::new()
            .uint("pid", row.pid)
            .uint("ppid", row.ppid)
            .uint("pgid", row.pgid)
            .uint("sid", row.sid)
            .str(
                "state",
                match row.state {
                    0 => "runnable",
                    1 => "blocked",
                    _ => "done",
                },
            )
            .str(
                "class",
                match row.class {
                    0 => "background",
                    1 => "normal",
                    2 => "interactive",
                    _ => "realtime",
                },
            )
            .uint("weight", row.weight)
            .uint("cpu_ticks", row.cpu_ticks)
            .str("name", &row.name)
            .finish()
    });
    Ok(Object::new()
        .uint("version", snapshot.version)
        .raw("tasks", &json::array(rows))
        .finish())
}

/// `sysinfo` and `mem.stats` share the kernel's statistics snapshot.
pub(crate) fn sysinfo(memory_only: bool) -> Result<String, Failure> {
    let s = user::sysinfo::snapshot()
        .map_err(|errno| (code::UNAVAILABLE, format!("sysinfo: errno {}", -errno)))?;
    let use_ = s.memory_use();
    let memory = Object::new()
        .uint("frames_total", s.frames_total)
        .uint("frames_live", s.frames_live)
        .uint("frames_free", s.frames_free)
        .uint("frames_allocated", s.frames_allocated)
        .uint("frames_freed", s.frames_freed)
        .uint("frames_reserved", s.frames_reserved)
        .uint("frames_double_frees", s.frames_double_frees)
        .uint("frames_invalid_frees", s.frames_invalid_frees)
        .uint("slab_live", s.slab_live)
        .uint("slab_peak", s.slab_peak)
        .uint("slab_oversized", s.slab_oversized)
        .uint("slab_double_frees", s.slab_double_frees)
        .uint("heap_total", s.heap_total)
        .uint("heap_used", s.heap_used)
        .uint("heap_free", s.heap_free)
        .uint("cache_frames", s.cache_frames)
        .uint("slab_frames", s.slab_frames)
        .raw(
            "bytes",
            &Object::new()
                .uint("programs", use_.programs)
                .uint("system", use_.system)
                .uint("cache", use_.cache)
                .uint("free", use_.free)
                .finish(),
        )
        .finish();
    if memory_only {
        return Ok(memory);
    }
    Ok(Object::new()
        .uint("version", s.version)
        .uint("ticks", s.ticks)
        .uint("idle_ticks", s.idle_ticks)
        .uint("tasks_live", s.tasks_live)
        .raw("memory", &memory)
        .finish())
}

/// `fabric.stats`.
pub(crate) fn fabric_stats() -> Result<String, Failure> {
    let s = user::messenger::fabric_stats()
        .map_err(|e| (code::UNAVAILABLE, format!("fabric: {}", e.message())))?;
    let tasks = s
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.live != 0)
        .map(|(slot, t)| {
            Object::new()
                .uint("slot", slot as u64)
                .uint("handles", t.handles as u64)
                .uint("buffers", t.buffers as u64)
                .uint("buffer_bytes", t.buffer_bytes as u64)
                .finish()
        });
    Ok(Object::new()
        .uint("services", s.services as u64)
        .uint("endpoints", s.endpoints as u64)
        .uint("channels", s.channels as u64)
        .uint("queued", s.queued as u64)
        .uint("queued_bytes", s.queued_bytes as u64)
        .uint("outstanding", s.outstanding as u64)
        .uint("calls", s.calls as u64)
        .uint("replies", s.replies as u64)
        .uint("one_way", s.one_way as u64)
        .uint("timeouts", s.timeouts as u64)
        .uint("cancels", s.cancels as u64)
        .uint("drops", s.drops as u64)
        .uint("buffers", s.buffers as u64)
        .uint("buffer_bytes", s.buffer_bytes as u64)
        .uint("buffer_mappings", s.buffer_mappings as u64)
        .uint("handoffs", s.handoffs as u64)
        .uint("audit_denies", s.audit_denies as u64)
        .uint("audit_allows", s.audit_allows as u64)
        .uint("audit_total", s.audit_total as u64)
        .raw("tasks", &json::array(tasks))
        .finish())
}

/// `devices.list`: the PCI inventory with owners and rights.
pub(crate) fn devices_list() -> Result<String, Failure> {
    let devices = user::dev::inspect::inventory().map_err(|errno| {
        (
            code::UNAVAILABLE,
            format!("device inventory: errno {}", -errno),
        )
    })?;
    let rows = devices.iter().map(|d| {
        let owner = d
            .owner
            .map_or(String::from("-"), |uid| format!("{}", devinspect::Uid(uid)));
        Object::new()
            .uint("id", u64::from(d.id))
            .str("class", d.class_name())
            .uint("pci_class", u64::from(d.class))
            .uint("pci_subclass", u64::from(d.subclass))
            .uint("pci_prog_if", u64::from(d.prog_if))
            .str("vendor", &format!("{:04x}", d.vendor))
            .str("device", &format!("{:04x}", d.device))
            .str("owner", &owner)
            .str("rights", &format!("{}", d.rights))
            .finish()
    });
    Ok(Object::new().raw("devices", &json::array(rows)).finish())
}

/// `drivers.list`: `devd`'s view.
pub(crate) fn drivers_list() -> Result<String, Failure> {
    let client = user::messenger::devd::Client::connect()
        .map_err(|_| (code::UNAVAILABLE, String::from("devd is not running")))?;
    let devices = client
        .devices()
        .map_err(|e| (code::UNAVAILABLE, format!("devd: {}", e.message())))?;
    let rows = devices.iter().map(|d| {
        let owner = if d.owner == u32::MAX {
            String::from("-")
        } else {
            format!("{}", devinspect::Uid(d.owner))
        };
        Object::new()
            .uint("id", d.id)
            .str("vendor", &format!("{:04x}", d.vendor))
            .str("device", &format!("{:04x}", d.device))
            .str("class", &d.class)
            .str("driver", &d.driver)
            .str("model", &d.model)
            .str("state", &d.state)
            .str("owner", &owner)
            .uint("pid", d.pid)
            .finish()
    });
    Ok(Object::new().raw("drivers", &json::array(rows)).finish())
}

/// The file `usbd` rewrites after each enumeration and failure snapshot.
pub(crate) const USB_DUMP_PATH: &str = fhs::state::USBD_DUMP;

/// `usb.dump`.
pub(crate) fn usb_dump() -> Result<String, Failure> {
    let bytes = user::files::read_up_to(USB_DUMP_PATH, 64 * 1024).map_err(|_| {
        (
            code::UNAVAILABLE,
            String::from(
                "usbd has not written a dump (no xHCI controller, or usbd is not in this image)",
            ),
        )
    })?;
    let body = String::from_utf8_lossy(&bytes);
    let rows: Vec<(u64, &str)> = body.lines().map(|l| (0, l)).collect();
    Ok(Object::new()
        .str("path", USB_DUMP_PATH)
        .raw("lines", &lines_json(&rows))
        .finish())
}

/// `fs.read`.
pub(crate) fn fs_read(params: &Value) -> Result<String, Failure> {
    let path = text(params, "path")
        .ok_or_else(|| (code::INVALID_PARAMS, String::from("fs.read needs a path")))?;
    fsallow::check(path).map_err(|why| (code::DENIED, String::from(why.text())))?;
    let offset = number(params, "offset", 0);
    let len = number(params, "len", 4096).min(methods::MAX_READ) as usize;
    let (size, kind) = user::files::stat(path).map_err(|errno| {
        (
            code::UNAVAILABLE,
            format!("{path}: {}", user::files::describe(errno)),
        )
    })?;
    if kind == user::files::Kind::Dir {
        let entries = user::files::list(path).map_err(|errno| {
            (
                code::UNAVAILABLE,
                format!("{path}: {}", user::files::describe(errno)),
            )
        })?;
        let names = entries.iter().map(|e| json::quoted(&e.name));
        return Ok(Object::new()
            .str("path", path)
            .str("kind", "dir")
            .raw("entries", &json::array(names))
            .finish());
    }
    let mut data = vec![0u8; len];
    let n = user::files::read_at(path, offset, &mut data).map_err(|errno| {
        (
            code::UNAVAILABLE,
            format!("{path}: {}", user::files::describe(errno)),
        )
    })?;
    data.truncate(n);
    Ok(Object::new()
        .str("path", path)
        .str("kind", "file")
        .uint("size", size)
        .uint("offset", offset)
        .uint("len", n as u64)
        .bool("eof", offset + n as u64 >= size)
        .str("text", &String::from_utf8_lossy(&data))
        .finish())
}

/// `hwreport`: the `HW:*` verdict lines of the boot log.
pub(crate) fn hwreport(scratch: &mut Vec<u8>) -> Result<String, Failure> {
    let log = read_ring(Ring::Kernel, scratch)?;
    let lines = log.lines();
    let verdicts: Vec<(u64, &str)> = lines
        .into_iter()
        .filter(|(_, line)| {
            let stripped = logline::parse(line);
            stripped.text.starts_with("HW:")
        })
        .collect();
    Ok(Object::new()
        .uint("total", log.total)
        .raw("verdicts", &lines_json(&verdicts))
        .finish())
}

/// `msg.registry`: the kernel name registry.
pub(crate) fn msg_registry() -> Result<String, Failure> {
    // Through `messengerd`: the direct list call is for tasks that hold
    // `CAP_IPC_CONTROL`, the daemon lists for everyone.
    let entries = user::messenger::registry::Client::connect()
        .and_then(|client| client.list())
        .map_err(|e| (code::UNAVAILABLE, format!("registry: {}", e.message())))?;
    let rows = entries.iter().map(|e| {
        let interfaces = json::array(
            e.interfaces
                .iter()
                .map(|i| json::quoted(&format!("{i:#x}"))),
        );
        Object::new()
            .str("name", &e.name)
            .uint("object", e.object_id)
            .uint("owner_slot", e.owner_slot)
            .raw("interfaces", &interfaces)
            .uint("lease_ticks", e.lease_remaining)
            .finish()
    });
    Ok(Object::new().raw("services", &json::array(rows)).finish())
}

/// `msg.services`: what `init` supervises.
pub(crate) fn msg_services() -> Result<String, Failure> {
    let endpoint = services::resolve_service(services::INIT_NAME)
        .map_err(|e| (code::UNAVAILABLE, format!("init: {}", e.message())))?;
    let statuses = services::fetch_services(&endpoint)
        .map_err(|e| (code::UNAVAILABLE, format!("init: {}", e.message())))?;
    let rows = statuses.iter().map(|s| {
        Object::new()
            .str("name", &s.name)
            .str("state", &format!("{}", s.state))
            .uint("pid", u64::from(s.pid))
            .uint("restarts", u64::from(s.restarts))
            .str("health", &format!("{}", s.health))
            .str("deps", &format!("{}", s.deps))
            .finish()
    });
    Ok(Object::new().raw("services", &json::array(rows)).finish())
}

/// `msg.topics`: the broker's topic list.
pub(crate) fn msg_topics() -> Result<String, Failure> {
    let client = user::messenger::topics_client::Client::connect()
        .map_err(|e| (code::UNAVAILABLE, format!("broker: {}", e.message())))?;
    let entries = client
        .list()
        .map_err(|e| (code::UNAVAILABLE, format!("broker: {}", e.message())))?;
    let rows = entries.iter().map(|t| {
        let payload = messenger_generated::declared_topic(&t.topic).map_or("-", |d| d.payload);
        Object::new()
            .str("topic", &t.topic)
            .uint("subscribers", t.subscribers)
            .bool("retained", t.retained)
            .str("payload", payload)
            .finish()
    });
    Ok(Object::new().raw("topics", &json::array(rows)).finish())
}

/// Longest payload returned in hex (bytes).
const MAX_TOPIC_PAYLOAD: usize = 1024;

/// `msg.topic`: the retained value an exact topic holds, if any. A
/// subscription replays it at once; none arriving means nothing is held.
pub(crate) fn msg_topic(params: &Value) -> Result<String, Failure> {
    let topic = text(params, "topic").ok_or_else(|| {
        (
            code::INVALID_PARAMS,
            String::from("msg.topic needs a topic"),
        )
    })?;
    if topic.is_empty() || topic.contains(['+', '#']) {
        return Err((
            code::INVALID_PARAMS,
            String::from("an exact topic name, no wildcards"),
        ));
    }
    let client = user::messenger::topics_client::Client::connect()
        .map_err(|e| (code::UNAVAILABLE, format!("broker: {}", e.message())))?;
    let subscription = client
        .subscribe(topic, user::messenger::topics_client::Qos::Latest)
        .map_err(|e| (code::DENIED, format!("broker: {}", e.message())))?;
    let event = subscription.poll_event().ok().flatten();
    let _ = subscription.unsubscribe();
    let Some(event) = event else {
        return Ok(Object::new()
            .str("topic", topic)
            .bool("held", false)
            .finish());
    };
    let shown = &event.payload[..event.payload.len().min(MAX_TOPIC_PAYLOAD)];
    Ok(Object::new()
        .str("topic", &event.topic)
        .bool("held", true)
        .bool("retained", event.retained)
        .uint("sequence", event.sequence)
        .uint("publisher_slot", event.publisher)
        .uint("payload_len", event.payload.len() as u64)
        .str("payload_hex", &dbgwire::auth::hex(shown))
        .finish())
}

/// `methods`.
pub(crate) fn methods_list() -> String {
    let rows = methods::METHODS.iter().map(|m| {
        let params = m.params.iter().map(|p| json::quoted(p.name));
        Object::new()
            .str("name", m.name)
            .str("summary", m.summary)
            .raw("params", &json::array(params))
            .finish()
    });
    Object::new().raw("methods", &json::array(rows)).finish()
}
