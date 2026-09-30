//! `confd` (`CONFD.ELF`): the configuration registry service (issue #260).
//!
//! This is the v1 service from [`docs/confd-plan.md`](../../docs/confd-plan.md)
//! (§1-§5). It loads the store logic from `libs/confd` and serves
//! `os.lazy.confd.v1` over Messenger:
//!
//! * `Get(path)`, `Set(path, value)`, `Delete(path)` and `List(prefix)`;
//! * the **caller uid** comes from the kernel-stamped credential on the
//!   transaction (`sys::cred_get`), never from the request body, so the
//!   `sys/`/`user/<uid>` access rules cannot be spoofed;
//! * a `Set`/`Delete` is persisted before the reply, on a clone of the
//!   committed store, so a write failure leaves the live store untouched;
//! * a committed `sys/` change is announced best-effort on
//!   `system/confd/changed/<path>`, payload `(path, deleted)` and never the
//!   value.
//!
//! # Storage
//!
//! The plan's `/system/confd/store` needs a persistent writable volume. On the
//! shipped image the boot volume is read-only FAT and `/tmp` is volatile
//! ramfs. The first writable directory of `/system/confd` (not mounted yet),
//! `/data/confd` (the ext2 data volume, present when a data disk is attached)
//! and `/tmp/confd` wins. A persistent location is reported **ok**; falling
//! back to `/tmp/confd` logs a warning and reports **degraded** to `healthd`.
//! The store is safe across a `confd` restart either way (the ramfs outlives
//! the task), but only a persistent location survives a reboot.
//!
//! # Change topics
//!
//! The kernel topic policy cannot express "`user/<uid>` is owner-only", so
//! `libs/confd` only announces `sys/` changes. Announcing a `user/` path would
//! leak it to every subscriber; the reviewer-approved fallback for #260 is to
//! stay silent on that subtree. Subscribers should `Get`/`List` after any
//! change and re-read on reconnect.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use api::wire;
use confd::{dir, ChangeSink, Confd, StoreFs};
use messenger_generated::topics;
use user::central;
use user::files::{self, Kind};
use user::messenger::confd as api;
use user::messenger::{self, errno, registry, services, Error, Message, Parcel};
use user::sys;

/// `ENOENT`, spelled out because `files` reports raw errnos.
const ENOENT: i64 = 2;
/// How long the serve loop parks between demo-child reaps (PIT ticks).
const POLL_TICKS: u64 = 5;
/// The evidence client `demo=1` spawns at boot (8.3 on-disk name).
const DEMO_PROGRAM: &[u8] = b"CONFCTL.ELF demo\0";

/// The `confd` state a request is dispatched against.
type Service = Confd<VfsStoreFs, TopicSink>;

/// A [`StoreFs`] binding the store files to one VFS directory.
///
/// The names are the `libs/confd` constants (`store`, `store.tmp`,
/// `store.corrupt`); this type only prefixes the directory.
struct VfsStoreFs {
    dir: String,
}

impl VfsStoreFs {
    fn new(dir: &str) -> VfsStoreFs {
        VfsStoreFs {
            dir: String::from(dir),
        }
    }

    /// The absolute path of one store file.
    fn path(&self, name: &str) -> String {
        let mut path = self.dir.clone();
        path.push('/');
        path.push_str(name);
        path
    }
}

impl StoreFs for VfsStoreFs {
    type Error = i64;

    fn read_file(&mut self, name: &str) -> Result<Option<Vec<u8>>, i64> {
        match files::read_all(&self.path(name)) {
            Ok(data) => Ok(Some(data)),
            Err(errno) if errno == ENOENT => Ok(None),
            Err(errno) => Err(errno),
        }
    }

    fn write_file(&mut self, name: &str, data: &[u8]) -> Result<(), i64> {
        // `write_file` creates-or-replaces, which is all `persist` needs; it
        // only ever points this at `store.tmp`.
        files::write_file(&self.path(name), data)
    }

    fn fsync(&mut self, name: &str) -> Result<(), i64> {
        files::fsync(&self.path(name))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), i64> {
        files::rename(&self.path(from), &self.path(to))
    }

    fn remove(&mut self, name: &str) -> Result<(), i64> {
        match files::remove(&self.path(name)) {
            Ok(()) => Ok(()),
            // A missing file is not an error (the trait contract).
            Err(errno) if errno == ENOENT => Ok(()),
            Err(errno) => Err(errno),
        }
    }
}

/// Publishes committed changes on the central broker and reports health.
///
/// The broker connection is opened lazily and dropped on the first failure, so
/// a `confd` that starts before `messengerd` connects on its first change; a
/// publish failure is swallowed because change topics are best-effort.
struct TopicSink {
    bus: Option<central::Bus>,
}

impl TopicSink {
    fn new() -> TopicSink {
        TopicSink { bus: None }
    }

    /// Best-effort heartbeat to `healthd`.
    fn report_health(&mut self, status: &str, detail: &str) {
        let Ok(endpoint) = services::resolve_service(services::HEALTHD_NAME) else {
            return;
        };
        let Ok(request) = services::health_report_request("confd", status, detail) else {
            return;
        };
        let _ = endpoint.call(&request, None);
    }
}

impl topics::Publish for TopicSink {
    type Error = Error;

    /// Publish through the central broker, reconnecting once when the cached
    /// bus is stale.
    fn publish_topic(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64, Error> {
        if self.bus.is_none() {
            self.bus = central::Bus::connect_retry(4).ok();
        }
        let result = match &mut self.bus {
            Some(bus) => bus.publish(topic, payload, retained),
            None => Err(Error::Errno(-errno::ENOENT)),
        };
        if result.is_err() {
            self.bus = None;
        }
        result
    }
}

impl ChangeSink for TopicSink {
    fn changed(&mut self, path: &str, deleted: bool) {
        let value = wire::Change {
            path: String::from(path),
            deleted,
        };
        // Best-effort: a change topic is an event, not state.
        let _ = wire::publish_system_confd_changed(self, path, &value);
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("confd: configuration registry service (issue #260)\n");
    if let Err(error) = run() {
        sys::write_str("confd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Choose the store directory, load the store, register, then serve forever.
fn run() -> messenger::Result<()> {
    let (dir, persistent) = pick_dir();
    let fs = VfsStoreFs::new(&dir);
    let mut service = Service::load(fs, TopicSink::new()).map_err(|_| Error::Errno(-errno::EIO))?;

    let (published, server) = messenger::create_pair()?;
    registry::register(api::NAME, &published, &[api::INTERFACE], 0)?;

    let detail = if persistent {
        format!("store={dir}")
    } else {
        format!("store={dir} (ramfs; not persistent)")
    };
    service
        .sink_mut()
        .report_health(if persistent { "ok" } else { "degraded" }, &detail);
    sys::write_str(&format!("CONFD:READY dir={dir} persistent={persistent}\n"));

    // One receive buffer for the whole life of the service: the user bump
    // allocator never reclaims per-call buffers.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    // `demo=1` spawns one `confctl` self-test once `confd` is serving; the loop
    // below parks with a deadline while it is alive so its exit is reaped.
    let mut demo_pending = demo_from_args();
    let mut demo_children = 0u64;
    loop {
        if demo_pending {
            demo_children = spawn_demo();
            demo_pending = false;
        }
        let deadline = if demo_children > 0 {
            Some(sys::clock().saturating_add(POLL_TICKS))
        } else {
            None
        };
        match server.recv_with(&mut buffer, deadline) {
            Ok(message) => {
                let method = message.method();
                let reply = match dispatch(&mut service, &message) {
                    Ok(parcel) => parcel,
                    Err(error) => error_reply_for(method, error),
                };
                if let Some(txn) = message.txn {
                    server.reply_or_drop(txn, &reply)?;
                }
            }
            // The demo wakeup that reaps a finished child is not a failure.
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
        while demo_children > 0 && sys::wait(sys::clock()).is_some() {
            demo_children -= 1;
            sys::write_str("CONFD:CTL:EXIT\n");
        }
    }
}

/// Whether the manifest asked for the `confctl` self-test (`demo=1`).
fn demo_from_args() -> bool {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    text.split_whitespace().any(|part| part == "demo=1")
}

/// Spawn the `confctl` self-test as a child of this service; returns how many
/// children are outstanding (0 or 1).
fn spawn_demo() -> u64 {
    match sys::spawn(DEMO_PROGRAM) {
        Some(pid) => {
            sys::write_str(&format!("CONFD:CTL:START pid={pid}\n"));
            1
        }
        None => {
            sys::write_str("CONFD:CTL:FAIL: cannot spawn CONFCTL.ELF\n");
            0
        }
    }
}

/// Route one inbound message to the store, mapping a store rejection to a
/// `CONFD_*` error reply.
fn dispatch(service: &mut Service, message: &Message) -> messenger::Result<Parcel> {
    if message.interface_id() != api::INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    let caller = confd::Caller {
        uid: caller_uid(message)?,
    };
    let method = message.method();
    match method {
        wire::METHOD_GET => {
            let args = wire::decode_get_args(&message.parcel.body).map_err(Error::Parcel)?;
            let value = service.get(&args.path, caller).map_err(service_error)?;
            let body = wire::encode_get_reply(&wire::GetReply {
                value: value.map(api::value_to_wire),
            })
            .map_err(Error::Parcel)?;
            Ok(api::parcel(method, body))
        }
        wire::METHOD_SET => {
            let args = wire::decode_set_args(&message.parcel.body).map_err(Error::Parcel)?;
            let value = api::value_from_wire(&args.value)?;
            service
                .set(&args.path, value, caller)
                .map_err(service_error)?;
            Ok(api::parcel(method, Vec::new()))
        }
        wire::METHOD_DELETE => {
            let args = wire::decode_delete_args(&message.parcel.body).map_err(Error::Parcel)?;
            service.delete(&args.path, caller).map_err(service_error)?;
            Ok(api::parcel(method, Vec::new()))
        }
        wire::METHOD_LIST => {
            let args = wire::decode_list_args(&message.parcel.body).map_err(Error::Parcel)?;
            let paths = service.list(&args.prefix, caller).map_err(service_error)?;
            let body = wire::encode_list_reply(&wire::ListReply {
                paths: paths.into_iter().map(String::from).collect(),
            })
            .map_err(Error::Parcel)?;
            Ok(api::parcel(method, body))
        }
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

/// A store rejection as the error the wire carries.
fn service_error(error: confd::ServiceError) -> Error {
    Error::Confd(api::service_error_code(error))
}

/// The `CONFD_*`/errno reply for a dispatch failure.
fn error_reply_for(method: u32, error: Error) -> Parcel {
    let code = match error {
        Error::Confd(code) => code,
        other => other.errno().map(|code| -code).unwrap_or(errno::EINVAL),
    };
    api::error_reply(method, code, error.message())
}

/// The uid of the Messenger sender, from its kernel-stamped credentials.
///
/// A missing or unreadable credential block is refused rather than guessed:
/// the access rules must never run against a uid the caller chose.
fn caller_uid(message: &Message) -> messenger::Result<u32> {
    let mut cred = sys::Cred::default();
    sys::cred_get(Some(message.sender), &mut cred).map_err(|_| Error::Errno(-errno::EACCES))?;
    Ok(cred.uid)
}

/// The store directory and whether it is persistent.
///
/// A persistent candidate is only accepted if it can be created (or already
/// is a directory) *and* a probe write succeeds. Otherwise `/tmp/confd`
/// (ramfs) is used and the service reports degraded.
fn pick_dir() -> (String, bool) {
    let choice = dir::choose(&dir::PERSISTENT_DIRS, |d| {
        ensure_dir(d) && probe_writable(d)
    });
    if !choice.persistent && !ensure_dir(choice.dir) {
        sys::write_str(
            "confd: warning: could not create /tmp/confd
",
        );
    }
    (String::from(choice.dir), choice.persistent)
}

/// Whether `path` is a directory, creating it when absent.
fn ensure_dir(path: &str) -> bool {
    match files::stat(path) {
        Ok((_, Kind::Dir)) => true,
        Ok(_) => false,
        Err(errno) if errno == ENOENT => files::mkdir(path).is_ok(),
        Err(_) => false,
    }
}

/// Whether a file can be written and removed under `dir`.
fn probe_writable(dir: &str) -> bool {
    let mut probe = String::from(dir);
    probe.push_str("/.probe");
    if files::write_file(&probe, b"ok").is_err() {
        return false;
    }
    let _ = files::remove(&probe);
    true
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
