//! `confd` (`/system/bin/confd`): the configuration registry service (issue #260).
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
//!   `system/confd/changed/<path>`, and a `user/<uid>/<rest>` change on
//!   `user/<uid>/confd/changed/<rest>` (the kernel's per-uid topic namespace,
//!   issue #407); payload `(path, deleted)` and never the value.
//!
//! # Storage
//!
//! The store lives in `/conf` (`fhs::state::CONF_ROOT`) on the OS volume,
//! 0700 root: only `confd` reads the raw store, every other reader goes
//! through this service. `/system` is written only by image updates and is
//! never a store. When `/conf` cannot be created or written (a recovery boot
//! with a read-only `/`), the store falls back to `/transient/conf` (ramfs),
//! `confd` logs why and reports **degraded** to `healthd`; the serve loop then
//! re-probes `/conf` and, once it is usable, migrates the live settings there
//! (existing `/conf` values win; the ramfs store is renamed `store.migrated`).
//! A store left on the ramfs by an earlier run is merged in at startup the
//! same way. A persistent `/conf` is reported **ok** with
//! `CONFD:READY dir=/conf persistent=true`.
//!
//! The F0 to F3 store, `/data/confd` (`fhs::state::LEGACY_DATA_CONFD`), is a
//! **seed**: the first start on `/conf` merges it in without overwriting
//! anything, writes `/conf/.seeded-from-data` and never reads it again, so a
//! setting deleted after the migration does not come back. `/data/confd` is
//! never written; F7 removes it.
//!
//! `/conf/svc/<service>/` is the home of service state that is not
//! key/value (`fhs::state::CONF_SVC`); `confd` creates nothing there.
//!
//! # Change topics
//!
//! The kernel topic policy cannot express "`user/<uid>` is owner-only", so
//! `libs/confd` only announces `sys/` changes. Announcing a `user/` path would
//! leak it to every subscriber; the reviewer-approved fallback for #260 is to
//! stay silent on that subtree. Subscribers should `Get`/`List` after any
//! change and re-read on reconnect.
//!
//! # Shutdown
//!
//! `confd` serves `os.lazy.lifecycle.v1` (docs/shutdown.md): on `init`'s
//! `Shutdown` it flushes the store's volume, prints `CONFD:STOP` and exits 0.
//! It is one of the last services stopped, after every service that could
//! still write a setting.

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
use user::messenger::services::lifecycle;
use user::messenger::{self, errno, registry, services, wait, Error, Message, Parcel};
use user::sys;

use storage::{pick_dir, seed_from_lower, try_upgrade, VfsStoreFs};

/// How long the serve loop waits before re-probing `/conf` while the
/// store sits on a lower-ranked location (PIT ticks).
const UPGRADE_TICKS: u64 = 200;
/// The evidence client `demo=1` spawns at boot.
const DEMO_PROGRAM: (&str, &str) = (fhs::bin::CONFCTL, "demo");

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
        match ::confd::announcement(path) {
            Some(::confd::Announcement::System) => {
                let _ = wire::publish_system_confd_changed(self, path, &value);
            }
            Some(::confd::Announcement::User { uid, rest }) => {
                let uid = format!("{uid}");
                let _ = wire::publish_user_confd_changed(self, &uid, rest, &value);
            }
            None => {}
        }
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
    // `/transient/conf`) are merged in, never overwriting what is already
    // here; on `/conf`, so is `/data/confd` once.
    seed_from_lower(&mut service, &dir);

    let (published, server) = messenger::create_pair()?;
    registry::register(
        api::NAME,
        &published,
        &[api::INTERFACE, lifecycle::INTERFACE],
        0,
    )?;

    let detail = if persistent {
        format!("store={dir}")
    } else {
        format!("store={dir} (ramfs; not persistent)")
    };
    service
        .sink_mut()
        .report_health(if persistent { "ok" } else { "degraded" }, &detail);
    sys::write_str(&format!("CONFD:READY dir={dir} persistent={persistent}\n"));
    // Serving: what waits for this service may start (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();

    // One receive buffer for the whole life of the service: the user bump
    // allocator never reclaims per-call buffers.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    // `demo=1` spawns one `confctl` self-test once `confd` is serving; the loop
    // below parks with a deadline while it is alive so its exit is reaped.
    let mut demo_pending = demo_from_args();
    let mut demo_children = 0u64;
    // Absolute, so steady client traffic (which never lets a receive time out)
    // cannot postpone the move to `/conf` forever.
    let mut next_upgrade = sys::clock().saturating_add(UPGRADE_TICKS);
    loop {
        if demo_pending {
            demo_children = spawn_demo();
            demo_pending = false;
        }
        // While the store is not on the preferred `/conf`, wake up
        // periodically to see whether `/conf` has become writable.
        let upgrade_due = dir != dir::PREFERRED_DIR;
        let deadline = upgrade_due.then_some(next_upgrade);
        // A live demo child's exit rings the child bell (P7): no poll.
        let doorbells = if demo_children > 0 {
            wait::WAIT_CHILD
        } else {
            0
        };
        let ready = match wait::wait_any(&[server], doorbells, deadline) {
            Ok(ready) => ready,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => 0,
            Err(error) => return Err(error),
        };
        if ready & wait::CHILD_READY != 0 && sys::wait(sys::clock().max(1)).is_some() {
            demo_children -= 1;
            sys::write_str("CONFD:CTL:EXIT\n");
        }
        let received = if ready & 1 != 0 {
            server.recv_with(&mut buffer, Some(messenger::EXPIRED_DEADLINE))
        } else {
            Err(Error::Errno(-errno::ETIMEDOUT))
        };
        match received {
            Ok(message) => {
                // An orderly shutdown (docs/shutdown.md): every write is
                // synchronous, so none is in flight between two messages.
                if let Some(reason) = lifecycle::stop_requested(&message) {
                    stop(&dir, &reason);
                    return Ok(());
                }
                let method = message.method();
                let reply = match dispatch(&mut service, &message, &dir, persistent) {
                    Ok(parcel) => parcel,
                    Err(error) => error_reply_for(method, error),
                };
                if let Some(txn) = message.txn {
                    server.reply_or_drop(txn, &reply)?;
                }
            }
            // A wake for the child bell or the upgrade timer only.
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
        // Checked after every wakeup, not only timeouts, at most once per
        // `UPGRADE_TICKS` (requests wake far more often than that).
        if upgrade_due && sys::clock() >= next_upgrade {
            next_upgrade = sys::clock().saturating_add(UPGRADE_TICKS);
            if try_upgrade(&mut service) {
                dir = String::from(dir::PREFERRED_DIR);
                persistent = true;
                seed_from_lower(&mut service, &dir);
                service
                    .sink_mut()
                    .report_health("ok", &format!("store={dir}"));
                sys::write_str(&format!(
                    "CONFD:MIGRATED dir={dir}
"
                ));
            }
        }
    }
}

/// The lifecycle stop: flush the volume holding the store (its last
/// `store.tmp` -> fsync -> rename finished before this message was read), so
/// the store is durable before the machine stops.
fn stop(dir: &str, reason: &str) {
    let synced = match user::files::fsync(dir) {
        Ok(()) => String::from("ok"),
        Err(code) => format!("errno {code}"),
    };
    sys::write_str(&format!(
        "CONFD:STOP dir={dir} sync={synced} reason=\"{reason}\"\n"
    ));
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
    match sys::spawn_native(DEMO_PROGRAM.0, &[DEMO_PROGRAM.1]) {
        Some(pid) => {
            sys::write_str(&format!("CONFD:CTL:START pid={pid}\n"));
            1
        }
        None => {
            sys::write_str(&format!(
                "CONFD:CTL:FAIL: cannot spawn {}\n",
                fhs::bin::CONFCTL
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
    // A system service holds `CAP_SETUID`; `elevd` is one by its identity
    // (docs/accounts-plan.md U2): it writes `sys/**` only for what an
    // administrator approved on the trusted prompt. A session never is.
    let cred = message.caller();
    let caller = confd::Caller {
        uid: caller_uid(message)?,
        system: cred.caps & user::sys::CAP_SETUID != 0
            || elevpolicy::is_elevd(cred.uid, cred.label_id, cred.session),
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

/// The uid of the Messenger sender, from the credentials the kernel stamped
/// on the message (issue #446): never a uid the caller chose.
fn caller_uid(message: &Message) -> messenger::Result<u32> {
    Ok(message.caller().uid)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
