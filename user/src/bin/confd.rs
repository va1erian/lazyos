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
//! shipped image `/system` is the read-only FAT boot volume and `/tmp` is
//! volatile ramfs, so the store lives on the ext2 data volume when a data disk
//! is attached. The first writable directory of `/data/confd`,
//! `/system/confd` (not writable yet) and `/tmp/confd` wins. The kernel mounts
//! `/data` before userspace starts, but a volume attached later is picked up:
//! while on a lower-ranked location the serve loop re-probes `/data/confd`
//! and, once usable, migrates the live settings there (existing `/data` values
//! win; the old store file is renamed `store.migrated`). Stores left in
//! lower-ranked locations by an earlier run are merged in at startup the same
//! way. A persistent location is reported **ok**; falling
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

#[path = "confd/storage.rs"]
mod storage;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use api::wire;
use confd::{dir, ChangeSink, Confd};
use messenger_generated::topics;
use user::central;
use user::messenger::confd as api;
use user::messenger::{self, errno, registry, services, Error, Message, Parcel};
use user::sys;

use storage::{pick_dir, seed_from_lower, try_upgrade, VfsStoreFs};

/// How long the serve loop parks between demo-child reaps (PIT ticks).
const POLL_TICKS: u64 = 5;
/// How long the serve loop waits before re-probing `/data/confd` while the
/// store sits on a lower-ranked location (PIT ticks).
const UPGRADE_TICKS: u64 = 200;
/// The evidence client `demo=1` spawns at boot (8.3 on-disk name).
const DEMO_PROGRAM: &[u8] = b"CONFCTL.ELF demo\0";

/// The `confd` state a request is dispatched against.
type Service = Confd<VfsStoreFs, TopicSink>;

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
    let (mut dir, mut persistent) = pick_dir();
    let fs = VfsStoreFs::new(&dir);
    let mut service = Service::load(fs, TopicSink::new()).map_err(|_| Error::Errno(-errno::EIO))?;
    // Settings written while a better store was unreachable (an earlier run on
    // `/tmp/confd`) are merged in, never overwriting what is already here.
    seed_from_lower(&mut service, &dir);

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
    // Absolute, so steady client traffic (which never lets a receive time out)
    // cannot postpone the move to `/data/confd` forever.
    let mut next_upgrade = sys::clock().saturating_add(UPGRADE_TICKS);
    loop {
        if demo_pending {
            demo_children = spawn_demo();
            demo_pending = false;
        }
        // While the store is not on the preferred `/data/confd`, wake up
        // periodically to see whether the data volume has become usable.
        let upgrade_due = dir != dir::PREFERRED_DIR;
        let deadline = if demo_children > 0 {
            Some(sys::clock().saturating_add(POLL_TICKS))
        } else if upgrade_due {
            Some(next_upgrade)
        } else {
            None
        };
        match server.recv_with(&mut buffer, deadline) {
            Ok(message) => {
                let method = message.method();
                let reply = match dispatch(&mut service, &message, &dir, persistent) {
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
        // Checked after every wakeup, not only timeouts, at most once per
        // `UPGRADE_TICKS` (the demo poll wakes far more often than that).
        if upgrade_due && sys::clock() >= next_upgrade {
            next_upgrade = sys::clock().saturating_add(UPGRADE_TICKS);
            if try_upgrade(&mut service) {
                dir = String::from(dir::PREFERRED_DIR);
                persistent = true;
                service
                    .sink_mut()
                    .report_health("ok", &format!("store={dir}"));
                sys::write_str(&format!(
                    "CONFD:MIGRATED dir={dir}
"
                ));
            }
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
            sys::write_str(&format!(
                "CONFD:CTL:FAIL: cannot spawn {}\n",
                fhs::boot::CONFCTL_ELF
            ));
            0
        }
    }
}

/// Route one inbound message to the store, mapping a store rejection to a
/// `CONFD_*` error reply. `dir` and `persistent` describe where the store
/// lives, for `Info`.
fn dispatch(
    service: &mut Service,
    message: &Message,
    dir: &str,
    persistent: bool,
) -> messenger::Result<Parcel> {
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
        wire::METHOD_INFO => {
            let body = wire::encode_info_reply(&wire::InfoReply {
                store_dir: String::from(dir),
                persistent,
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

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
